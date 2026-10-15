//! The node's event bus: a replay ring plus a broadcast channel, served as SSE.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;

const RING_LEN: usize = 10_000;
const RING_AGE: Duration = Duration::from_secs(600);
const CHANNEL_LEN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub ty: String,
    pub origin_node: String,
    pub resource: String,
    pub ts: String,
    pub op: Op,
    pub payload: serde_json::Value,
}

/// An event with its SSE ID.
#[derive(Debug, Clone)]
pub struct Sequenced {
    pub id: String,
    pub seq: u64,
    pub at: SystemTime,
    pub event: Event,
}

#[derive(Debug)]
struct Ring {
    next_seq: u64,
    items: VecDeque<Sequenced>,
}

#[derive(Debug)]
pub struct Bus {
    node: String,
    boot_epoch: u64,
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Sequenced>,
}

/// What a new subscriber receives before live events.
#[derive(Debug)]
pub enum Resume {
    /// Events after the client's `Last-Event-ID`.
    Replay(Vec<Sequenced>),
    /// The ID is unknown or too old; the client must re-snapshot.
    Reset,
    /// No `Last-Event-ID` was given.
    Fresh,
}

impl Bus {
    pub fn new(node: String, boot_epoch: u64) -> Self {
        Self {
            node,
            boot_epoch,
            ring: Mutex::new(Ring {
                next_seq: 1,
                items: VecDeque::new(),
            }),
            tx: broadcast::channel(CHANNEL_LEN).0,
        }
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn publish(&self, ty: &str, resource: String, op: Op, payload: serde_json::Value) {
        let event = Event {
            ty: ty.to_string(),
            origin_node: self.node.clone(),
            resource,
            ts: rfc3339(SystemTime::now()),
            op,
            payload,
        };
        self.push(event);
    }

    /// Re-publishes an event that originated on another node, keeping its
    /// origin, so this node's stream covers the whole cluster.
    pub fn republish(&self, event: Event) {
        self.push(event);
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    fn push(&self, event: Event) {
        let now = SystemTime::now();
        let mut ring = self.ring();
        let seq = ring.next_seq;
        ring.next_seq += 1;
        let item = Sequenced {
            id: format!("{}.{}.{seq}", self.node, self.boot_epoch),
            seq,
            at: now,
            event,
        };
        ring.items.push_back(item.clone());
        while ring.items.len() > RING_LEN
            || ring
                .items
                .front()
                .is_some_and(|e| now.duration_since(e.at).unwrap_or_default() > RING_AGE)
        {
            ring.items.pop_front();
        }
        // Sent under the ring lock so subscribers never miss or duplicate an event.
        let _ = self.tx.send(item);
    }

    /// The cursor of the last published event.
    pub fn cursor(&self) -> String {
        let ring = self.ring();
        format!("{}.{}.{}", self.node, self.boot_epoch, ring.next_seq - 1)
    }

    pub fn subscribe(
        &self,
        last_event_id: Option<&str>,
    ) -> (Resume, broadcast::Receiver<Sequenced>) {
        let ring = self.ring();
        let rx = self.tx.subscribe();
        let resume = match last_event_id {
            None => Resume::Fresh,
            Some(id) => match self.parse_seq(id) {
                Some(seq) if seq + 1 == ring.next_seq => Resume::Replay(Vec::new()),
                Some(seq)
                    if seq < ring.next_seq
                        && ring.items.front().is_some_and(|f| f.seq <= seq + 1) =>
                {
                    Resume::Replay(ring.items.iter().filter(|e| e.seq > seq).cloned().collect())
                }
                _ => Resume::Reset,
            },
        };
        (resume, rx)
    }

    fn parse_seq(&self, id: &str) -> Option<u64> {
        let mut parts = id.rsplitn(3, '.');
        let seq = parts.next()?.parse().ok()?;
        let epoch: u64 = parts.next()?.parse().ok()?;
        let node = parts.next()?;
        (node == self.node && epoch == self.boot_epoch).then_some(seq)
    }
}

/// Formats a time as RFC 3339 UTC with second precision.
#[allow(clippy::many_single_char_names)] // names follow the published algorithm
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_791_936_067)),
            "2026-10-14T00:01:07Z"
        );
    }

    #[test]
    fn replay_and_reset() {
        let bus = Bus::new("n1".into(), 4);
        bus.publish(
            "vm.state",
            "/vms/a".into(),
            Op::Upsert,
            serde_json::json!({}),
        );
        bus.publish(
            "vm.state",
            "/vms/b".into(),
            Op::Upsert,
            serde_json::json!({}),
        );
        let Resume::Replay(r) = bus.subscribe(Some("n1.4.1")).0 else {
            panic!()
        };
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].event.resource, "/vms/b");
        let Resume::Replay(r) = bus.subscribe(Some("n1.4.2")).0 else {
            panic!()
        };
        assert!(r.is_empty());
        assert!(matches!(bus.subscribe(Some("n1.3.1")).0, Resume::Reset));
        assert!(matches!(bus.subscribe(Some("n2.4.1")).0, Resume::Reset));
        assert!(matches!(bus.subscribe(Some("garbage")).0, Resume::Reset));
        assert!(matches!(bus.subscribe(None).0, Resume::Fresh));
        assert_eq!(bus.cursor(), "n1.4.2");
    }

    #[tokio::test]
    async fn live() {
        let bus = Bus::new("n".into(), 1);
        let (_, mut rx) = bus.subscribe(None);
        bus.publish("x", "/r".into(), Op::Delete, serde_json::Value::Null);
        assert_eq!(rx.recv().await.unwrap().id, "n.1.1");
    }
}
