//! The connection: framing, call multiplexing, keepalive and console streams.
//!
//! One socket carries every call. [`Client::call`] assigns a serial number,
//! queues the frame for the write task and waits on a oneshot channel that
//! the read task completes when the reply with the same serial arrives. The
//! read task also routes events to a broadcast channel and stream messages
//! to the stream that owns the serial.

mod dispatch;
mod stream;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::Error;
use crate::events::DomainEvent;
use crate::proto::{self, Domain, Header, MessageType, Status, procs};
use crate::xdr::Encoder;

pub use stream::{Stream, StreamReader, StreamWriter};
use stream::{StreamEvent, StreamRx};

/// How often a ping is sent to the daemon.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// How long the socket may stay silent before the connection is closed.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long [`Client::connect`] waits for the socket to accept.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`Client::open`] waits for the daemon to open the hypervisor.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Default time a call waits for its reply. Some procedures (a migration
/// perform step, a snapshot of a large disk) block in the daemon for minutes.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(600);

type Reply = Result<Vec<u8>, Error>;

#[derive(Debug)]
struct Inner {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    serial: AtomicU32,
    call_timeout_ms: AtomicU64,
    pending: Mutex<HashMap<u32, oneshot::Sender<Reply>>>,
    streams: Mutex<HashMap<u32, mpsc::UnboundedSender<StreamEvent>>>,
    events: broadcast::Sender<DomainEvent>,
    closed: watch::Sender<bool>,
}

/// A connection to a libvirt daemon. Cheap to clone; all clones share one
/// socket and calls are multiplexed by serial number.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// Builds a wire frame: the total length, the header, then `body`.
///
/// Fails when the frame would exceed [`proto::MESSAGE_MAX`], which the daemon
/// rejects by dropping the connection.
fn frame(header: &Header, body: &[u8]) -> Result<Vec<u8>, Error> {
    let len = 4 + proto::HEADER_LEN + body.len();
    if len > proto::MESSAGE_MAX {
        return Err(Error::Decode(format!(
            "message of {len} bytes exceeds the {} byte limit",
            proto::MESSAGE_MAX
        )));
    }
    let mut e = Encoder::new();
    // `len` is at most MESSAGE_MAX (32 MiB), so it fits in u32.
    e.u32(u32::try_from(len).unwrap_or(u32::MAX));
    header.encode(&mut e);
    let mut buf = e.into_inner();
    buf.extend_from_slice(body);
    Ok(buf)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn write_loop(mut wr: OwnedWriteHalf, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(buf) = rx.recv().await {
        if let Err(e) = wr.write_all(&buf).await {
            tracing::debug!("libvirt write failed: {e}");
            break;
        }
    }
}

async fn keepalive_loop(inner: std::sync::Weak<Inner>) {
    let mut tick = tokio::time::interval(KEEPALIVE_INTERVAL);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        if *inner.closed.borrow() {
            return;
        }
        let header = Header {
            prog: proto::KEEPALIVE_PROGRAM,
            vers: proto::KEEPALIVE_PROTOCOL_VERSION,
            proc: proto::KEEPALIVE_PROC_PING,
            ty: MessageType::Message,
            serial: 0,
            status: Status::Ok,
        };
        let sent = frame(&header, &[]).is_ok_and(|f| inner.tx.send(f).is_ok());
        if !sent {
            return;
        }
    }
}

impl Client {
    /// Connects to the daemon socket at `path` and opens `uri`
    /// (for example `qemu:///system`).
    ///
    /// Fails with [`Error::Timeout`] when the socket takes more than 10
    /// seconds to accept or the daemon takes more than 30 seconds to open
    /// the URI.
    pub async fn connect(path: impl AsRef<Path>, uri: &str) -> Result<Self, Error> {
        let sock = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(path.as_ref()))
            .await
            .map_err(|_| Error::Timeout)??;
        let client = Self::from_stream(sock);
        client.open(uri).await?;
        Ok(client)
    }

    /// Wraps an already-connected socket. The caller must call [`Self::open`].
    pub fn from_stream(sock: UnixStream) -> Self {
        let (rd, wr) = sock.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(1024);
        let (closed, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            tx,
            serial: AtomicU32::new(1),
            call_timeout_ms: AtomicU64::new(millis(DEFAULT_CALL_TIMEOUT)),
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            events,
            closed,
        });
        tokio::spawn(write_loop(wr, rx));
        tokio::spawn(dispatch::read_loop(rd, Arc::clone(&inner)));
        Self { inner }
    }

    /// Sets how long [`Self::call`] and the typed procedures wait for a
    /// reply before failing with [`Error::Timeout`]. The default is 10
    /// minutes. The setting is shared by every clone of this client.
    pub fn set_call_timeout(&self, timeout: Duration) {
        self.inner
            .call_timeout_ms
            .store(millis(timeout), Ordering::Relaxed);
    }

    fn call_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.call_timeout_ms.load(Ordering::Relaxed))
    }

    /// Subscribes to lifecycle events. Call [`Self::register_lifecycle_events`]
    /// once per connection so the daemon sends them. A receiver that falls
    /// 1024 events behind loses the oldest ones and sees `Lagged`.
    pub fn events(&self) -> broadcast::Receiver<DomainEvent> {
        self.inner.events.subscribe()
    }

    /// Resolves when the connection is closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.closed.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }

    /// Whether the connection has ended (peer closed, I/O error or
    /// keepalive timeout).
    pub fn is_closed(&self) -> bool {
        *self.inner.closed.borrow()
    }

    fn next_serial(&self) -> u32 {
        self.inner.serial.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, header: &Header, body: &[u8]) -> Result<(), Error> {
        self.inner
            .tx
            .send(frame(header, body)?)
            .map_err(|_| Error::Closed)
    }

    /// Performs a raw call and returns the reply body.
    ///
    /// `args` is the XDR-encoded argument struct of procedure `proc` (see
    /// [`proto::procs`]). Returns [`Error::Remote`] when the daemon answers
    /// with an error, [`Error::Closed`] when the connection ends first,
    /// [`Error::Timeout`] when no reply arrives within the call timeout, and
    /// [`Error::Decode`] when `args` is too large to send.
    pub async fn call(&self, proc: u32, args: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.call_within(proc, args, self.call_timeout()).await
    }

    async fn call_within(
        &self,
        proc: u32,
        args: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        let (serial, rx) = self.start_call(proc, &args)?;
        self.await_reply(serial, rx, timeout).await
    }

    /// Like [`Self::call`], with no timeout. For procedures that block in the
    /// daemon until a long job ends, such as the transfer of a live migration.
    pub(crate) async fn call_unbounded(&self, proc: u32, args: Vec<u8>) -> Result<Vec<u8>, Error> {
        let (_, rx) = self.start_call(proc, &args)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Waits for the reply to `serial`. On timeout the pending entry is
    /// removed, so a late reply is logged as unknown and dropped.
    async fn await_reply(
        &self,
        serial: u32,
        rx: oneshot::Receiver<Reply>,
        timeout: Duration,
    ) -> Reply {
        let Ok(reply) = tokio::time::timeout(timeout, rx).await else {
            lock(&self.inner.pending).remove(&serial);
            lock(&self.inner.streams).remove(&serial);
            return Err(Error::Timeout);
        };
        reply.map_err(|_| Error::Closed)?
    }

    fn start_call(&self, proc: u32, args: &[u8]) -> Result<(u32, oneshot::Receiver<Reply>), Error> {
        self.enqueue_call(proc, args, None)
    }

    /// Like [`Self::start_call`], and also registers the data receiver for
    /// the stream the procedure opens.
    fn start_stream_call(
        &self,
        proc: u32,
        args: &[u8],
    ) -> Result<(oneshot::Receiver<Reply>, StreamRx), Error> {
        let (stx, srx) = mpsc::unbounded_channel();
        let (serial, rx) = self.enqueue_call(proc, args, Some(stx))?;
        Ok((rx, (serial, srx)))
    }

    /// Registers the call (and its stream, if any) under a fresh serial and
    /// queues the frame. Both are registered before the frame is queued, so
    /// a reply or stream message that follows at once is never dropped.
    fn enqueue_call(
        &self,
        proc: u32,
        args: &[u8],
        stream: Option<mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<(u32, oneshot::Receiver<Reply>), Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let serial = self.next_serial();
        let (tx, rx) = oneshot::channel();
        lock(&self.inner.pending).insert(serial, tx);
        if let Some(stx) = stream {
            lock(&self.inner.streams).insert(serial, stx);
        }
        let header = Header {
            prog: proto::REMOTE_PROGRAM,
            vers: proto::REMOTE_PROTOCOL_VERSION,
            proc,
            ty: MessageType::Call,
            serial,
            status: Status::Ok,
        };
        if let Err(e) = self.send(&header, args) {
            lock(&self.inner.pending).remove(&serial);
            lock(&self.inner.streams).remove(&serial);
            return Err(e);
        }
        Ok((serial, rx))
    }

    pub(crate) async fn call_with(
        &self,
        proc: u32,
        f: impl FnOnce(&mut Encoder),
    ) -> Result<Vec<u8>, Error> {
        let mut e = Encoder::new();
        f(&mut e);
        self.call(proc, e.into_inner()).await
    }

    pub(crate) async fn call_dom(
        &self,
        proc: u32,
        dom: &Domain,
        flags: Option<u32>,
    ) -> Result<Vec<u8>, Error> {
        self.call_with(proc, |e| {
            dom.encode(e);
            if let Some(f) = flags {
                e.u32(f);
            }
        })
        .await
    }

    /// `virConnectOpen`: opens the hypervisor `uri` on this connection and
    /// starts the keepalive task. Fails with [`Error::Timeout`] after 30
    /// seconds.
    pub async fn open(&self, uri: &str) -> Result<(), Error> {
        let mut e = Encoder::new();
        e.opt_string(Some(uri)).u32(0);
        self.call_within(procs::CONNECT_OPEN, e.into_inner(), OPEN_TIMEOUT)
            .await?;
        tokio::spawn(keepalive_loop(Arc::downgrade(&self.inner)));
        Ok(())
    }

    /// `virConnectClose`. The daemon closes the socket after replying.
    pub async fn close(&self) -> Result<(), Error> {
        self.call(procs::CONNECT_CLOSE, Vec::new()).await.map(drop)
    }

    /// Opens a character device (serial console) as a bidirectional stream.
    ///
    /// `dev_name` selects a console by alias; `None` opens the first one.
    /// `flags` are [`proto::flags`] `DOMAIN_CONSOLE_*` values.
    pub async fn open_console(
        &self,
        dom: &Domain,
        dev_name: Option<&str>,
        flags: u32,
    ) -> Result<Stream, Error> {
        let mut e = Encoder::new();
        dom.encode(&mut e);
        e.opt_string(dev_name).u32(flags);
        let proc = procs::DOMAIN_OPEN_CONSOLE;
        let (reply, (serial, rx)) = self.start_stream_call(proc, &e.into_inner())?;
        if let Err(e) = self.await_reply(serial, reply, self.call_timeout()).await {
            lock(&self.inner.streams).remove(&serial);
            return Err(e);
        }
        Ok(Stream::new(
            rx,
            StreamWriter::new(self.clone(), proc, serial),
        ))
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
