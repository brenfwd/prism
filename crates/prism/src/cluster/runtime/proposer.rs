//! Routes state commands through raft to the leader.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use prism_cluster::ClusterError;
use prism_proto::v1::WriteRequest;
use prism_proto::v1::cluster_client::ClusterClient;

use super::{Cluster, READ_YOUR_WRITES};
use crate::state::{Propose, StateError};

/// Routes commands to the leader and waits to see them applied locally.
pub(super) struct Proposer {
    pub(super) cluster: Arc<Cluster>,
}

impl Proposer {
    async fn propose_inner(&self, command: String) -> Result<String, StateError> {
        let raft = self
            .cluster
            .raft()
            .ok_or_else(|| StateError::Degraded("raft is not running".into()))?;
        for _ in 0..3 {
            match raft.write(command.clone()).await {
                Ok(outcome) => return Ok(outcome.result),
                Err(ClusterError::ForwardToLeader(Some((_, leader)))) => {
                    let ch = self.cluster.channels.get(&leader.addr).map_err(|e| {
                        StateError::Degraded(format!("cannot reach the leader: {e}"))
                    })?;
                    let mut req = tonic::Request::new(WriteRequest {
                        command: command.clone().into_bytes(),
                    });
                    req.set_timeout(prism_cluster::WRITE_TIMEOUT + Duration::from_secs(2));
                    match ClusterClient::new(ch).forward_write(req).await {
                        Ok(r) => {
                            let r = r.into_inner();
                            raft.wait_applied(r.log_index, READ_YOUR_WRITES)
                                .await
                                .map_err(|e| StateError::Degraded(e.to_string()))?;
                            return String::from_utf8(r.result)
                                .map_err(|e| StateError::Internal(e.into()));
                        }
                        // The leader moved; ask again.
                        Err(s) if s.code() == tonic::Code::FailedPrecondition => {}
                        Err(s)
                            if matches!(
                                s.code(),
                                tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
                            ) =>
                        {
                            return Err(StateError::Degraded(format!(
                                "the leader {} is unreachable",
                                leader.name
                            )));
                        }
                        Err(s) => {
                            return Err(StateError::Internal(anyhow::anyhow!(
                                "forwarding write: {s}"
                            )));
                        }
                    }
                }
                Err(ClusterError::ForwardToLeader(None)) => {
                    return Err(StateError::Degraded("no hub or leader is reachable".into()));
                }
                Err(ClusterError::Degraded(r)) => return Err(StateError::Degraded(r.to_string())),
                Err(ClusterError::Raft(e)) => {
                    return Err(StateError::Internal(anyhow::anyhow!(e)));
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(StateError::Degraded("leadership kept changing".into()))
    }
}

impl Propose for Proposer {
    fn propose(&self, command: String) -> BoxFuture<'_, Result<String, StateError>> {
        Box::pin(self.propose_inner(command))
    }
}
