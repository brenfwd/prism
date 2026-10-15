//! In-process multi-node tests: a router stands in for gRPC.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use uuid::Uuid;

use super::*;

#[derive(Debug, Default)]
struct MemBackend {
    state: Mutex<(Vec<String>, Option<AppliedMeta>)>,
}

impl MemBackend {
    fn applied(&self) -> Vec<String> {
        self.state.lock().unwrap().0.clone()
    }
}

impl Backend for MemBackend {
    fn apply(&self, command: Option<&str>, meta: &AppliedMeta) -> anyhow::Result<String> {
        let mut s = self.state.lock().unwrap();
        if let Some(c) = command {
            s.0.push(c.to_string());
        }
        s.1 = Some(meta.clone());
        Ok(format!("ok:{}", s.0.len()))
    }

    fn applied_meta(&self) -> anyhow::Result<Option<AppliedMeta>> {
        Ok(self.state.lock().unwrap().1.clone())
    }

    fn snapshot(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.state.lock().unwrap().0)?)
    }

    fn restore(&self, data: &[u8], meta: &AppliedMeta) -> anyhow::Result<()> {
        let mut s = self.state.lock().unwrap();
        s.0 = serde_json::from_slice(data)?;
        s.1 = Some(meta.clone());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct Router {
    nodes: Arc<Mutex<HashMap<RaftId, Raft>>>,
    down: Arc<Mutex<HashSet<RaftId>>>,
}

struct Peer {
    from: RaftId,
    target: RaftId,
    node: PrismNode,
    router: Router,
}

impl Router {
    fn get(&self, from: RaftId, to: RaftId) -> Result<Raft, Unreachable> {
        let down = self.down.lock().unwrap();
        if down.contains(&from) || down.contains(&to) {
            return Err(Unreachable::new(&std::io::Error::other("partitioned")));
        }
        self.nodes
            .lock()
            .unwrap()
            .get(&to)
            .cloned()
            .ok_or_else(|| Unreachable::new(&std::io::Error::other("no such node")))
    }
}

#[derive(Clone)]
struct NodeRouter {
    id: RaftId,
    router: Router,
}

impl RaftNetworkFactory<TypeConfig> for NodeRouter {
    type Network = Peer;
    async fn new_client(&mut self, target: RaftId, node: &PrismNode) -> Peer {
        Peer {
            from: self.id,
            target,
            node: node.clone(),
            router: self.router.clone(),
        }
    }
}

impl RaftNetwork<TypeConfig> for Peer {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _: RPCOption,
    ) -> Result<AppendEntriesResponse<RaftId>, RPCError<RaftId, PrismNode, RaftError<RaftId>>> {
        let r = self
            .router
            .get(self.from, self.target)
            .map_err(RPCError::Unreachable)?;
        r.append_entries(rpc).await.map_err(|e| {
            RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.node.clone(),
                e,
            ))
        })
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<RaftId>,
        RPCError<RaftId, PrismNode, RaftError<RaftId, InstallSnapshotError>>,
    > {
        let r = self
            .router
            .get(self.from, self.target)
            .map_err(RPCError::Unreachable)?;
        r.install_snapshot(rpc).await.map_err(|e| {
            RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.node.clone(),
                e,
            ))
        })
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<RaftId>,
        _: RPCOption,
    ) -> Result<VoteResponse<RaftId>, RPCError<RaftId, PrismNode, RaftError<RaftId>>> {
        let r = self
            .router
            .get(self.from, self.target)
            .map_err(RPCError::Unreachable)?;
        r.vote(rpc).await.map_err(|e| {
            RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.node.clone(),
                e,
            ))
        })
    }
}

fn node(id: RaftId) -> PrismNode {
    PrismNode {
        node_id: Uuid::from_u128(u128::from(id)),
        name: format!("n{id}"),
        addr: format!("[fd00::{id}]:7443"),
    }
}

async fn spawn(router: &Router, id: RaftId, snapshot_every: u64) -> (ClusterNode, Arc<MemBackend>) {
    let backend = Arc::new(MemBackend::default());
    let cfg = Config {
        heartbeat_interval: 50,
        election_timeout_min: 200,
        election_timeout_max: 400,
        snapshot_policy: SnapshotPolicy::LogsSinceLast(snapshot_every),
        max_in_snapshot_log_to_keep: 0,
        ..Config::default()
    };
    let n = ClusterNode::start(
        id,
        Arc::new(cfg.validate().unwrap()),
        NodeRouter {
            id,
            router: router.clone(),
        },
        LogStore::memory().unwrap(),
        backend.clone(),
    )
    .await
    .unwrap();
    router.nodes.lock().unwrap().insert(id, n.raft.clone());
    (n, backend)
}

async fn wait_for(f: impl Fn() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not reached");
}

async fn leader_of(nodes: &[&ClusterNode]) -> RaftId {
    for _ in 0..200 {
        if let Some(n) = nodes.iter().find(|n| n.is_leader()) {
            return n.id;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no leader elected");
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_spoke_learners_replicate_and_forward() {
    let router = Router::default();
    let (hub, hub_be) = spawn(&router, 1, 5000).await;
    let (s2, s2_be) = spawn(&router, 2, 5000).await;
    hub.initialize(node(1)).await.unwrap();
    leader_of(&[&hub]).await;
    hub.add_learner(2, node(2)).await.unwrap();

    let (res, idx) = hub.write("a".into()).await.unwrap();
    assert_eq!(res, "ok:1");
    s2.wait_applied(idx, Duration::from_secs(5)).await.unwrap();
    assert_eq!(s2_be.applied(), vec!["a"]);

    // A spoke cannot commit; it learns who the hub is.
    match s2.write("b".into()).await {
        Err(ClusterError::ForwardToLeader(Some((1, n)))) => assert_eq!(n.name, "n1"),
        other => panic!("expected forward, got {other:?}"),
    }
    assert_eq!(s2.leader().map(|l| l.0), Some(1));
    assert_eq!(hub_be.applied(), vec!["a"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn late_learner_catches_up_from_snapshot() {
    let router = Router::default();
    let (hub, _) = spawn(&router, 1, 5).await;
    hub.initialize(node(1)).await.unwrap();
    leader_of(&[&hub]).await;
    let mut last = 0;
    for i in 0..20 {
        last = hub.write(format!("c{i}")).await.unwrap().1;
    }
    // Logs before the snapshot are purged, so the learner needs InstallSnapshot.
    wait_for(|| hub.metrics().snapshot.is_some()).await;
    let (late, late_be) = spawn(&router, 2, 5).await;
    hub.add_learner(2, node(2)).await.unwrap();
    late.wait_applied(last, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(late_be.applied().len(), 20);
    assert_eq!(late_be.applied()[19], "c19");
}

#[tokio::test(flavor = "multi_thread")]
async fn consensus_survives_leader_loss_and_degrades_without_quorum() {
    let router = Router::default();
    let mut nodes = Vec::new();
    for id in 1..=3 {
        nodes.push(spawn(&router, id, 5000).await);
    }
    nodes[0].0.initialize(node(1)).await.unwrap();
    leader_of(&[&nodes[0].0]).await;
    for id in 2..=3 {
        nodes[0].0.add_learner(id, node(id)).await.unwrap();
    }
    nodes[0]
        .0
        .set_voters(BTreeSet::from([1, 2, 3]))
        .await
        .unwrap();
    let (_, idx) = nodes[0].0.write("x".into()).await.unwrap();
    for (n, _) in &nodes {
        n.wait_applied(idx, Duration::from_secs(5)).await.unwrap();
    }

    // Partition the leader: the other two elect a new one and keep committing.
    router.down.lock().unwrap().insert(1);
    let survivors = [&nodes[1].0, &nodes[2].0];
    let new_leader = {
        let mut found = None;
        for _ in 0..400 {
            if let Some(n) = survivors.iter().find(|n| n.is_leader()) {
                found = Some(n.id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        found.expect("survivors elected a leader")
    };
    let leader = survivors.iter().find(|n| n.id == new_leader).unwrap();
    let (_, idx) = leader.write("y".into()).await.unwrap();
    for n in survivors {
        n.wait_applied(idx, Duration::from_secs(5)).await.unwrap();
    }

    // Lose a second voter: no quorum, writes cannot commit.
    let other = survivors.iter().find(|n| n.id != new_leader).unwrap().id;
    router.down.lock().unwrap().insert(other);
    let r =
        tokio::time::timeout(Duration::from_secs(3), leader.raft.client_write("z".into())).await;
    assert!(
        r.is_err() || r.unwrap().is_err(),
        "write must not commit without quorum"
    );

    // Heal: the old leader rejoins and catches up on "y".
    router.down.lock().unwrap().clear();
    nodes[0]
        .0
        .wait_applied(idx, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(nodes[0].1.applied().contains(&"y".to_string()));
}

#[tokio::test]
async fn log_store_survives_reopen() {
    use openraft::storage::RaftLogStorage;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raft.db");
    {
        let mut s = LogStore::open(&path).unwrap();
        s.save_vote(&openraft::Vote::new(3, 1)).await.unwrap();
    }
    let mut s = LogStore::open(&path).unwrap();
    assert_eq!(
        s.read_vote().await.unwrap(),
        Some(openraft::Vote::new(3, 1))
    );
    let st = s.get_log_state().await.unwrap();
    assert_eq!(st.last_log_id, None);
}
