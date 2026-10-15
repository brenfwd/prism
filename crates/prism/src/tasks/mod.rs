//! Long-running operations: tracked in `node.db`, reported as SSE
//! `task.progress` events, cancellable.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::NodeDb;
use crate::events::{Bus, Op, rfc3339};

mod remote;

use remote::RemoteTasks;

/// Finished tasks are kept this long.
const RETENTION: Duration = Duration::from_secs(7 * 86_400);
/// Progress events are rate-limited to this interval per task.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub kind: String,
    /// API path of the resource the task acts on.
    pub resource: String,
    pub node: Uuid,
    pub state: TaskState,
    /// 0.0–1.0 when known.
    pub progress: Option<f64>,
    pub message: String,
    pub error: Option<String>,
    /// Result payload on success (for example the ID of what was created).
    pub result: Option<serde_json::Value>,
    pub created: String,
    pub updated: String,
}

#[derive(Debug)]
struct Inner {
    db: NodeDb,
    bus: Arc<Bus>,
    node: Uuid,
    running: Mutex<HashMap<Uuid, CancellationToken>>,
    /// Other nodes' tasks, from their event streams.
    remote: RemoteTasks,
}

/// The node's task registry.
#[derive(Debug, Clone)]
pub struct Tasks {
    inner: Arc<Inner>,
}

/// Handed to a running task to report progress and observe cancellation.
#[derive(Debug, Clone)]
pub struct TaskCtx {
    inner: Arc<Inner>,
    task: Arc<Mutex<(Task, Instant)>>,
    pub cancel: CancellationToken,
}

fn now() -> String {
    rfc3339(std::time::SystemTime::now())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Inner {
    fn save(&self, task: &Task) {
        if let Err(e) = self.db.put_task(&task.id.to_string(), unix_now(), task) {
            tracing::error!(task = %task.id, "saving task: {e:#}");
        }
        self.bus.publish(
            "task.progress",
            format!("/tasks/{}", task.id),
            Op::Upsert,
            serde_json::to_value(task).unwrap_or_default(),
        );
    }
}

impl Tasks {
    /// Opens the registry. Tasks left `running` by a previous process are
    /// marked failed.
    pub fn new(db: NodeDb, bus: Arc<Bus>, node: Uuid) -> Self {
        let inner = Arc::new(Inner {
            db,
            bus,
            node,
            running: Mutex::default(),
            remote: RemoteTasks::default(),
        });
        if let Ok(tasks) = inner.db.tasks::<Task>(1000) {
            for mut t in tasks.into_iter().filter(|t| t.state == TaskState::Running) {
                t.state = TaskState::Failed;
                t.error = Some("interrupted: prism restarted".into());
                t.updated = now();
                inner.save(&t);
            }
        }
        let _ = inner
            .db
            .prune_tasks(unix_now() - RETENTION.as_secs() as i64);
        Self { inner }
    }

    pub fn list(&self, limit: usize) -> anyhow::Result<Vec<Task>> {
        self.inner.db.tasks(limit)
    }

    pub fn get(&self, id: Uuid) -> anyhow::Result<Option<Task>> {
        self.inner.db.task(&id.to_string())
    }

    /// The newest `limit` tasks across the cluster: this node's, plus every
    /// peer's as last heard from its event stream.
    pub fn list_cluster(&self, limit: usize) -> anyhow::Result<Vec<Task>> {
        let mut all = self.list(limit)?;
        all.extend(self.inner.remote.all());
        all.sort_unstable_by(|a, b| (&b.created, b.id).cmp(&(&a.created, a.id)));
        all.truncate(limit);
        Ok(all)
    }

    /// The peer that runs task `id`, if it is known and not this node.
    pub fn remote_owner(&self, id: Uuid) -> Option<Uuid> {
        self.inner
            .remote
            .get(id)
            .map(|t| t.node)
            .filter(|&n| n != self.inner.node)
    }

    /// Replaces the cached tasks of peer `node` with its snapshot.
    pub fn apply_remote_snapshot(&self, node: Uuid, tasks: Vec<Task>) {
        if node != self.inner.node {
            self.inner.remote.replace(node, tasks);
        }
    }

    /// Caches a peer's task from its `task.progress` event.
    pub fn apply_remote_event(&self, task: Task) {
        if task.node != self.inner.node {
            self.inner.remote.upsert(task);
        }
    }

    /// Requests cancellation. Returns false if the task is not running here.
    pub fn cancel(&self, id: Uuid) -> bool {
        let running = self
            .inner
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        running.get(&id).is_some_and(|c| {
            c.cancel();
            true
        })
    }

    /// Whether a running task already acts on `resource` with this `kind`.
    pub fn busy(&self, resource: &str) -> bool {
        self.inner.db.tasks::<Task>(200).is_ok_and(|ts| {
            ts.iter()
                .any(|t| t.state == TaskState::Running && t.resource == resource)
        })
    }

    /// Starts `f` as a background task and returns its initial record.
    pub fn spawn<F, Fut>(&self, kind: &str, resource: String, f: F) -> Task
    where
        F: FnOnce(TaskCtx) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
    {
        let ts = now();
        let task = Task {
            id: Uuid::now_v7(),
            kind: kind.into(),
            resource,
            node: self.inner.node,
            state: TaskState::Running,
            progress: None,
            message: String::new(),
            error: None,
            result: None,
            created: ts.clone(),
            updated: ts,
        };
        self.inner.save(&task);
        let cancel = CancellationToken::new();
        self.inner
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(task.id, cancel.clone());
        let ctx = TaskCtx {
            inner: Arc::clone(&self.inner),
            task: Arc::new(Mutex::new((task.clone(), Instant::now()))),
            cancel,
        };
        let id = task.id;
        tracing::info!(task = %id, kind, "task started");
        tokio::spawn(async move {
            let result = f(ctx.clone()).await;
            let mut t = ctx.snapshot();
            t.updated = now();
            match result {
                Ok(v) => {
                    t.state = TaskState::Succeeded;
                    t.progress = Some(1.0);
                    t.result = Some(v);
                }
                Err(e) if ctx.cancel.is_cancelled() => {
                    t.state = TaskState::Cancelled;
                    t.error = Some(format!("{e:#}"));
                }
                Err(e) => {
                    t.state = TaskState::Failed;
                    t.error = Some(format!("{e:#}"));
                }
            }
            tracing::info!(task = %id, state = ?t.state, error = t.error.as_deref().unwrap_or(""), "task finished");
            ctx.inner.save(&t);
            ctx.inner
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        });
        task
    }
}

impl TaskCtx {
    fn snapshot(&self) -> Task {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    pub fn task_id(&self) -> Uuid {
        self.snapshot().id
    }

    /// Reports progress (0.0–1.0) and a status message; rate-limited.
    pub fn progress(&self, fraction: Option<f64>, message: impl Into<String>) {
        let mut guard = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let message = message.into();
        let changed_msg = guard.0.message != message;
        guard.0.progress = fraction.map(|f| f.clamp(0.0, 1.0));
        guard.0.message = message;
        if changed_msg || guard.1.elapsed() >= PROGRESS_INTERVAL {
            guard.0.updated = now();
            guard.1 = Instant::now();
            let t = guard.0.clone();
            drop(guard);
            self.inner.save(&t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks() -> Tasks {
        Tasks::new(
            NodeDb::memory().unwrap(),
            Arc::new(Bus::new("n".into(), 1)),
            Uuid::nil(),
        )
    }

    async fn wait(t: &Tasks, id: Uuid) -> Task {
        for _ in 0..200 {
            let task = t.get(id).unwrap().unwrap();
            if task.state != TaskState::Running {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("task did not finish");
    }

    #[tokio::test]
    async fn success_and_failure() {
        let t = tasks();
        let ok = t.spawn("x", "/r".into(), |ctx| async move {
            ctx.progress(Some(0.5), "half");
            Ok(serde_json::json!({"id": 1}))
        });
        let done = wait(&t, ok.id).await;
        assert_eq!(done.state, TaskState::Succeeded);
        assert_eq!(done.result.unwrap()["id"], 1);

        let bad = t.spawn("x", "/r".into(), |_| async { anyhow::bail!("boom") });
        let done = wait(&t, bad.id).await;
        assert_eq!(done.state, TaskState::Failed);
        assert_eq!(done.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn cancellation() {
        let t = tasks();
        let task = t.spawn("x", "/r".into(), |ctx| async move {
            ctx.cancel.cancelled().await;
            Err::<serde_json::Value, _>(anyhow::anyhow!("cancelled"))
        });
        assert!(t.busy("/r"));
        assert!(t.cancel(task.id));
        assert_eq!(wait(&t, task.id).await.state, TaskState::Cancelled);
        assert!(!t.cancel(task.id));
    }

    #[tokio::test]
    async fn cluster_view() {
        let t = tasks();
        let local = t.spawn("x", "/r".into(), |_| async { Ok(serde_json::Value::Null) });
        wait(&t, local.id).await;
        let peer = Uuid::from_u128(7);
        let remote = Task {
            id: Uuid::now_v7(),
            node: peer,
            created: "9999-01-01T00:00:00Z".into(),
            ..local.clone()
        };
        t.apply_remote_snapshot(peer, vec![remote.clone()]);
        // Our own tasks are never cached as remote.
        t.apply_remote_event(Task {
            id: Uuid::now_v7(),
            ..local.clone()
        });

        assert_eq!(t.remote_owner(remote.id), Some(peer));
        assert_eq!(t.remote_owner(local.id), None);
        let ids: Vec<Uuid> = t.list_cluster(10).unwrap().iter().map(|t| t.id).collect();
        assert_eq!(ids, [remote.id, local.id]);
        assert_eq!(t.list_cluster(1).unwrap().len(), 1);
    }
}
