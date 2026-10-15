//! Console tunnelling: VNC and serial consoles of a VM on another node.

use std::sync::Arc;

use futures::StreamExt;
use prism_proto::v1::ConsoleFrame;
use prism_proto::v1::console_client::ConsoleClient;
use prism_proto::v1::console_server::Console;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::{BoxStream, grpc_addr, node_of};
use crate::cluster::model::MemberDoc;
use crate::cluster::runtime::Cluster;

pub struct ConsoleService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl Console for ConsoleService {
    type OpenStream = BoxStream<ConsoleFrame>;

    /// Bridges a peer to a local VM's VNC or serial console.
    async fn open(
        &self,
        req: Request<Streaming<ConsoleFrame>>,
    ) -> Result<Response<Self::OpenStream>, Status> {
        crate::cluster::grpc::check_active(&self.cluster, req.remote_addr())?;
        let node = node_of(&self.cluster)?;
        let mut inbound = req.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("missing console request"))?;
        let vm = Uuid::parse_str(&first.vm).map_err(|_| Status::invalid_argument("bad VM ID"))?;
        let (to_peer, out) = mpsc::channel::<Result<ConsoleFrame, Status>>(64);
        let (from_peer, mut from_peer_rx) = mpsc::channel::<Vec<u8>>(64);
        tokio::spawn(async move {
            while let Ok(Some(f)) = inbound.message().await {
                if from_peer.send(f.data).await.is_err() {
                    return;
                }
            }
        });
        match first.kind.as_str() {
            "vnc" => {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let path = node
                    .vnc_socket(vm)
                    .await
                    .map_err(|e| Status::failed_precondition(e.to_string()))?;
                let sock = tokio::net::UnixStream::connect(&path)
                    .await
                    .map_err(|e| Status::unavailable(e.to_string()))?;
                let (mut rd, mut wr) = sock.into_split();
                tokio::spawn(async move {
                    while let Some(d) = from_peer_rx.recv().await {
                        if wr.write_all(&d).await.is_err() {
                            return;
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match rd.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                let f = ConsoleFrame {
                                    data: buf[..n].to_vec(),
                                    ..ConsoleFrame::default()
                                };
                                if to_peer.send(Ok(f)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
            "serial" => {
                let stream = node
                    .open_serial(vm)
                    .await
                    .map_err(|e| Status::failed_precondition(e.to_string()))?;
                let (mut reader, writer) = stream.into_split();
                tokio::spawn(async move {
                    while let Some(d) = from_peer_rx.recv().await {
                        if writer.send(&d).is_err() {
                            return;
                        }
                    }
                });
                tokio::spawn(async move {
                    while let Some(Ok(d)) = reader.recv().await {
                        let f = ConsoleFrame {
                            data: d,
                            ..ConsoleFrame::default()
                        };
                        if to_peer.send(Ok(f)).await.is_err() {
                            return;
                        }
                    }
                });
            }
            k => {
                return Err(Status::invalid_argument(format!(
                    "unknown console kind {k}"
                )));
            }
        }
        Ok(Response::new(Box::pin(ReceiverStream::new(out))))
    }
}

/// Opens a console on the VM's owner: send bytes on the returned sender,
/// read the VM's output from the stream.
pub async fn open_remote_console(
    cluster: &Cluster,
    member: &MemberDoc,
    vm: Uuid,
    kind: &str,
) -> anyhow::Result<(mpsc::Sender<Vec<u8>>, Streaming<ConsoleFrame>)> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    let first = ConsoleFrame {
        vm: vm.to_string(),
        kind: kind.into(),
        data: Vec::new(),
    };
    let outbound =
        futures::stream::once(async move { first }).chain(ReceiverStream::new(rx).map(|data| {
            ConsoleFrame {
                data,
                ..ConsoleFrame::default()
            }
        }));
    let ch = cluster.channels.get(&grpc_addr(member))?;
    let inbound = ConsoleClient::new(ch).open(outbound).await?.into_inner();
    Ok((tx, inbound))
}
