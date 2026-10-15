//! Other nodes' tasks, learned from their event streams, so any node can
//! list every task in the cluster and route a task request to its owner.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

use super::{Task, TaskState};

/// Most remote tasks kept; the oldest finished ones are dropped beyond it.
const CAPACITY: usize = 2000;

#[derive(Debug, Default)]
pub(super) struct RemoteTasks {
    tasks: Mutex<HashMap<Uuid, Task>>,
}

impl RemoteTasks {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Task>> {
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replaces everything known about `node` with its snapshot.
    pub(super) fn replace(&self, node: Uuid, tasks: Vec<Task>) {
        let mut map = self.lock();
        map.retain(|_, t| t.node != node);
        map.extend(
            tasks
                .into_iter()
                .filter(|t| t.node == node)
                .map(|t| (t.id, t)),
        );
        evict(&mut map);
    }

    pub(super) fn upsert(&self, task: Task) {
        let mut map = self.lock();
        map.insert(task.id, task);
        evict(&mut map);
    }

    pub(super) fn get(&self, id: Uuid) -> Option<Task> {
        self.lock().get(&id).cloned()
    }

    pub(super) fn all(&self) -> Vec<Task> {
        self.lock().values().cloned().collect()
    }
}

fn evict(map: &mut HashMap<Uuid, Task>) {
    let Some(excess) = map.len().checked_sub(CAPACITY).filter(|&n| n > 0) else {
        return;
    };
    let mut finished: Vec<(String, Uuid)> = map
        .values()
        .filter(|t| t.state != TaskState::Running)
        .map(|t| (t.created.clone(), t.id))
        .collect();
    finished.sort_unstable();
    for (_, id) in finished.into_iter().take(excess) {
        map.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(node: Uuid, created: &str, state: TaskState) -> Task {
        Task {
            id: Uuid::now_v7(),
            kind: "x".into(),
            resource: "/r".into(),
            node,
            state,
            progress: None,
            message: String::new(),
            error: None,
            result: None,
            created: created.into(),
            updated: created.into(),
        }
    }

    #[test]
    fn snapshot_replaces_one_node_only() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let cache = RemoteTasks::default();
        let old = task(a, "1", TaskState::Running);
        let other = task(b, "1", TaskState::Running);
        cache.upsert(old.clone());
        cache.upsert(other.clone());
        let new = task(a, "2", TaskState::Succeeded);
        // A snapshot only speaks for its own node.
        cache.replace(a, vec![new.clone(), task(b, "3", TaskState::Running)]);
        assert!(cache.get(old.id).is_none());
        assert!(cache.get(new.id).is_some());
        assert!(cache.get(other.id).is_some());
        assert_eq!(cache.all().len(), 2);
    }

    #[test]
    fn evicts_oldest_finished() {
        let node = Uuid::from_u128(1);
        let cache = RemoteTasks::default();
        let running = task(node, "0000", TaskState::Running);
        let oldest = task(node, "0001", TaskState::Failed);
        cache.upsert(running.clone());
        cache.upsert(oldest.clone());
        for i in 0..CAPACITY - 1 {
            cache.upsert(task(node, &format!("1{i:05}"), TaskState::Succeeded));
        }
        assert_eq!(cache.all().len(), CAPACITY);
        assert!(cache.get(running.id).is_some());
        assert!(cache.get(oldest.id).is_none());
    }
}
