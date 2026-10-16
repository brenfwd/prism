//! The raw TCP data plane (port 7444, inside `WireGuard`) for bulk transfers.
//!
//! A gRPC `DataPlane.Open` call registers a single-use session bound to the
//! caller's mesh address and returns a random 128-bit ID; the caller then
//! connects and sends the ID first.
//!
//! Volume transfer frames (all integers big-endian):
//! - `0 | size u64`: header with the file's logical size
//! - `1 | offset u64 | len u32 | data`: a data extent (holes are skipped)
//! - `2 | blake3 [32]`: end marker with BLAKE3 over every data frame's
//!   `offset | len | data` in order

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// A registered session waits this long for its connection.
const SESSION_TTL: Duration = Duration::from_secs(60);
/// Read and write timeout on an established volume transfer, so a stalled
/// sender cannot hold a blocking thread forever.
const IO_TIMEOUT: Duration = Duration::from_secs(120);
const CHUNK: usize = 1 << 20;

type SessionId = [u8; 16];

struct Pending {
    peer: Ipv6Addr,
    expires: Instant,
    tx: oneshot::Sender<TcpStream>,
}

/// Session registry and listener.
#[derive(Default)]
pub struct DataPlane {
    sessions: Mutex<HashMap<SessionId, Pending>>,
}

impl std::fmt::Debug for DataPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataPlane").finish_non_exhaustive()
    }
}

impl DataPlane {
    /// Registers a session for a connection from `peer`.
    pub fn register_session(&self, peer: Ipv6Addr) -> (SessionId, oneshot::Receiver<TcpStream>) {
        let id = *uuid::Uuid::new_v4().as_bytes();
        let (tx, rx) = oneshot::channel();
        let mut s = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        s.retain(|_, p| p.expires > now);
        s.insert(
            id,
            Pending {
                peer,
                expires: now + SESSION_TTL,
                tx,
            },
        );
        (id, rx)
    }

    /// Accepts connections on `addr`, matching each to its session.
    pub async fn serve(self: Arc<Self>, addr: SocketAddr) {
        loop {
            let listener = match TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::debug!("data plane bind {addr}: {e}; retrying");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            tracing::info!("data plane listening on {addr}");
            loop {
                let Ok((mut stream, from)) = listener.accept().await else {
                    break;
                };
                let dp = Arc::clone(&self);
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut id = [0u8; 16];
                    if tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut id))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let pending = dp
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id);
                    match pending {
                        Some(p)
                            if IpAddr::V6(p.peer) == from.ip() && p.expires > Instant::now() =>
                        {
                            let _ = stream.set_nodelay(true);
                            if p.tx.send(stream).is_err() {
                                tracing::debug!(
                                    "data plane: the session from {from} was no longer awaited"
                                );
                            }
                        }
                        _ => {
                            tracing::warn!(
                                "data plane: unknown or misdirected session from {from}"
                            );
                        }
                    }
                });
            }
        }
    }
}

/// Connects to a peer's data plane and presents `session`.
pub async fn connect(addr: Ipv6Addr, session: &[u8]) -> anyhow::Result<TcpStream> {
    use tokio::io::AsyncWriteExt;
    let mut s = TcpStream::connect(SocketAddr::new(addr.into(), prism_proto::DATA_PORT)).await?;
    s.set_nodelay(true)?;
    s.write_all(session).await?;
    Ok(s)
}

fn hash_frame(h: &mut blake3::Hasher, offset: u64, data: &[u8]) {
    h.update(&offset.to_be_bytes());
    h.update(&(data.len() as u32).to_be_bytes());
    h.update(data);
}

/// Streams `file` sparsely over `out`. Blocking; run on a blocking thread.
pub fn send_file(
    file: &std::fs::File,
    out: &mut impl Write,
    progress: &dyn Fn(u64, u64),
) -> anyhow::Result<()> {
    use rustix::fs::SeekFrom;
    let size = file.metadata()?.len();
    out.write_all(&[0])?;
    out.write_all(&size.to_be_bytes())?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut pos = 0u64;
    let mut sent = 0u64;
    while pos < size {
        // Next data region; ENXIO means only a hole remains.
        let start = match rustix::fs::seek(file, SeekFrom::Data(pos)) {
            Ok(s) => s,
            Err(rustix::io::Errno::NXIO) => break,
            Err(e) => return Err(e.into()),
        };
        let end = rustix::fs::seek(file, SeekFrom::Hole(start)).map_or(size, |e| e.min(size));
        let mut off = start;
        while off < end {
            let n = ((end - off) as usize).min(CHUNK);
            let got = file.read_at(&mut buf[..n], off)?;
            if got == 0 {
                anyhow::bail!("file shrank during transfer");
            }
            let data = &buf[..got];
            out.write_all(&[1])?;
            out.write_all(&off.to_be_bytes())?;
            out.write_all(&(got as u32).to_be_bytes())?;
            out.write_all(data)?;
            hash_frame(&mut hasher, off, data);
            off += got as u64;
            sent += got as u64;
            progress(off, size);
        }
        pos = end;
    }
    out.write_all(&[2])?;
    out.write_all(hasher.finalize().as_bytes())?;
    out.flush()?;
    tracing::debug!(size, sent, "sent sparse file");
    Ok(())
}

/// Receives a file into `file` (empty, writable), verifying the checksum.
/// Blocking; run on a blocking thread.
pub fn receive_file(
    input: &mut impl Read,
    file: &std::fs::File,
    progress: &dyn Fn(u64, u64),
) -> anyhow::Result<u64> {
    let mut tag = [0u8; 1];
    let mut u64b = [0u8; 8];
    input.read_exact(&mut tag)?;
    anyhow::ensure!(tag[0] == 0, "data plane: expected a header frame");
    input.read_exact(&mut u64b)?;
    let size = u64::from_be_bytes(u64b);
    file.set_len(size)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        input.read_exact(&mut tag)?;
        match tag[0] {
            1 => {
                input.read_exact(&mut u64b)?;
                let off = u64::from_be_bytes(u64b);
                let mut lenb = [0u8; 4];
                input.read_exact(&mut lenb)?;
                let len = u32::from_be_bytes(lenb) as usize;
                anyhow::ensure!(
                    len <= CHUNK && off + len as u64 <= size,
                    "data plane: bad extent"
                );
                input.read_exact(&mut buf[..len])?;
                file.write_all_at(&buf[..len], off)?;
                hash_frame(&mut hasher, off, &buf[..len]);
                progress(off + len as u64, size);
            }
            2 => {
                let mut sum = [0u8; 32];
                input.read_exact(&mut sum)?;
                anyhow::ensure!(
                    *hasher.finalize().as_bytes() == sum,
                    "data plane: checksum mismatch"
                );
                file.sync_all()?;
                return Ok(size);
            }
            t => anyhow::bail!("data plane: unknown frame {t}"),
        }
    }
}

/// What a data-plane session is for.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Purpose {
    /// Receive a volume into a local pool (cold move, copy).
    ReceiveVolume { pool: String, volume: String },
    /// Splice to a local migration socket that QEMU listens on.
    MigrationSocket {
        migration: uuid::Uuid,
        which: String,
    },
}

/// Directory of a migration's UNIX sockets on either node.
pub fn migration_dir(migration: uuid::Uuid) -> std::path::PathBuf {
    std::path::Path::new("/run/prism/mig").join(migration.simple().to_string())
}

/// `DataPlane.Open`: registers a session for the calling member.
pub struct DataPlaneService {
    /// The cluster the service belongs to.
    pub cluster: Arc<super::Cluster>,
    /// The session table that incoming connections are matched against.
    pub plane: Arc<DataPlane>,
}

#[tonic::async_trait]
impl prism_proto::v1::data_plane_server::DataPlane for DataPlaneService {
    async fn open(
        &self,
        req: tonic::Request<prism_proto::v1::OpenSession>,
    ) -> Result<tonic::Response<prism_proto::v1::SessionReply>, tonic::Status> {
        super::grpc::check_active(&self.cluster, req.remote_addr())?;
        let Some(IpAddr::V6(peer)) = req.remote_addr().map(|a| a.ip()) else {
            return Err(tonic::Status::permission_denied(
                "calls must come over the mesh",
            ));
        };
        let purpose: Purpose = serde_json::from_slice(&req.into_inner().purpose)
            .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;
        let node = self
            .cluster
            .node
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| tonic::Status::unavailable("node is starting"))?;
        match purpose {
            Purpose::ReceiveVolume { pool, volume } => {
                let pool = node
                    .storage
                    .get(&pool)
                    .map_err(|e| tonic::Status::not_found(e.to_string()))?;
                let (partial, file) = pool
                    .begin_partial(&volume)
                    .map_err(|e| tonic::Status::already_exists(e.to_string()))?;
                let (id, rx) = self.plane.register_session(peer);
                tokio::spawn(async move {
                    let res: anyhow::Result<()> = async {
                        let stream = tokio::time::timeout(SESSION_TTL, rx)
                            .await
                            .map_err(|_| anyhow::anyhow!("sender never connected"))??;
                        let stream = stream.into_std()?;
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(IO_TIMEOUT))?;
                        stream.set_write_timeout(Some(IO_TIMEOUT))?;
                        let result = tokio::task::spawn_blocking(move || {
                            let r = receive_file(
                                &mut std::io::BufReader::new(&stream),
                                &file,
                                &|_, _| {},
                            );
                            // One status byte back to the sender.
                            if let Err(e) = (&stream).write_all(&[u8::from(r.is_err())]) {
                                tracing::debug!("sending the volume status byte: {e}");
                            }
                            r
                        })
                        .await??;
                        tracing::info!(volume, bytes = result, "received volume");
                        pool.commit_partial(&partial, &volume)?;
                        Ok(())
                    }
                    .await;
                    if let Err(e) = res {
                        tracing::warn!("receiving volume: {e:#}");
                        pool.abort_partial(&partial);
                    }
                });
                Ok(tonic::Response::new(prism_proto::v1::SessionReply {
                    session_id: id.to_vec(),
                    detail: Vec::new(),
                }))
            }
            Purpose::MigrationSocket { migration, which } => {
                if !matches!(which.as_str(), "mem" | "nbd") {
                    return Err(tonic::Status::invalid_argument("unknown migration socket"));
                }
                let path = migration_dir(migration).join(format!("{which}.sock"));
                let (id, rx) = self.plane.register_session(peer);
                tokio::spawn(async move {
                    let res: anyhow::Result<()> = async {
                        let mut tcp = tokio::time::timeout(SESSION_TTL, rx)
                            .await
                            .map_err(|_| anyhow::anyhow!("source never connected"))??;
                        let mut unix = tokio::net::UnixStream::connect(&path).await?;
                        tokio::io::copy_bidirectional(&mut tcp, &mut unix).await?;
                        Ok(())
                    }
                    .await;
                    if let Err(e) = res {
                        tracing::debug!("migration splice {}: {e:#}", path.display());
                    }
                });
                Ok(tonic::Response::new(prism_proto::v1::SessionReply {
                    session_id: id.to_vec(),
                    detail: Vec::new(),
                }))
            }
        }
    }
}

/// Sends a local file to `member`'s pool as `volume`, verified end to end.
pub async fn send_volume(
    cluster: &super::Cluster,
    member: &super::model::MemberDoc,
    file: std::fs::File,
    pool: &str,
    volume: &str,
    progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> anyhow::Result<()> {
    let ch = cluster.channels.get(&super::peers::grpc_addr(member))?;
    let purpose = serde_json::to_vec(&Purpose::ReceiveVolume {
        pool: pool.into(),
        volume: volume.into(),
    })?;
    let reply = prism_proto::v1::data_plane_client::DataPlaneClient::new(ch)
        .open(prism_proto::v1::OpenSession { purpose })
        .await?
        .into_inner();
    let stream = connect(member.mesh_addr, &reply.session_id).await?;
    let stream = stream.into_std()?;
    stream.set_nonblocking(false)?;
    tokio::task::spawn_blocking(move || {
        let mut w = std::io::BufWriter::with_capacity(CHUNK, &stream);
        send_file(&file, &mut w, &*progress)?;
        drop(w);
        let mut status = [0u8; 1];
        (&stream).read_exact(&mut status)?;
        anyhow::ensure!(status[0] == 0, "the receiver rejected the transfer");
        Ok(())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("src");
        let src = std::fs::File::create(&src_path).unwrap();
        src.set_len(64 << 20).unwrap();
        src.write_all_at(b"hello", 4096).unwrap();
        src.write_all_at(&vec![7u8; 3 << 20], 32 << 20).unwrap();
        drop(src);
        let src = std::fs::File::open(&src_path).unwrap();
        let mut wire = Vec::new();
        send_file(&src, &mut wire, &|_, _| {}).unwrap();
        // Holes are not sent: far less than 64 MiB on the wire.
        assert!(wire.len() < 8 << 20, "sent {} bytes", wire.len());
        let dst = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(dir.path().join("dst"))
            .unwrap();
        let size = receive_file(&mut wire.as_slice(), &dst, &|_, _| {}).unwrap();
        assert_eq!(size, 64 << 20);
        assert_eq!(
            std::fs::read(dir.path().join("src")).unwrap(),
            std::fs::read(dir.path().join("dst")).unwrap()
        );
    }

    #[test]
    fn detects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, vec![1u8; 10_000]).unwrap();
        let mut wire = Vec::new();
        send_file(&std::fs::File::open(&p).unwrap(), &mut wire, &|_, _| {}).unwrap();
        let mid = wire.len() / 2;
        wire[mid] ^= 0xff;
        let dst = std::fs::File::create(dir.path().join("g")).unwrap();
        assert!(receive_file(&mut wire.as_slice(), &dst, &|_, _| {}).is_err());
    }
}
