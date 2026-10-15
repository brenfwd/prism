//! Async client for the libvirt remote protocol over a UNIX socket.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::Error;
use crate::proto::{
    self, Domain, DomainInfo, DomainState, Header, LifecycleEvent, MessageType, NodeInfo,
    RemoteError, Status, flags, procs,
};
use crate::xdr::{Decoder, Encoder};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest data chunk sent in one stream message.
const STREAM_CHUNK: usize = 256 * 1024;

/// A domain lifecycle event delivered by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEvent {
    pub domain: Domain,
    pub event: LifecycleEvent,
    pub detail: i32,
}

#[derive(Debug)]
enum StreamEvent {
    Data(Vec<u8>),
    Eof,
    Error(Error),
}

type Reply = Result<Vec<u8>, Error>;
/// The receiver for a stream's data, keyed by the call's serial.
type StreamRx = (u32, mpsc::UnboundedReceiver<StreamEvent>);

#[derive(Debug)]
struct Inner {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    serial: AtomicU32,
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

fn frame(header: &Header, body: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.u32((4 + proto::HEADER_LEN + body.len()) as u32);
    header.encode(&mut e);
    let mut buf = e.into_inner();
    buf.extend_from_slice(body);
    buf
}

impl Client {
    /// Connects to the daemon socket at `path` and opens `uri`
    /// (for example `qemu:///system`).
    pub async fn connect(path: impl AsRef<Path>, uri: &str) -> Result<Self, Error> {
        let sock = UnixStream::connect(path.as_ref()).await?;
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
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            events,
            closed,
        });
        tokio::spawn(write_loop(wr, rx));
        tokio::spawn(read_loop(rd, Arc::clone(&inner)));
        Self { inner }
    }

    /// Subscribes to lifecycle events. Call [`Self::register_lifecycle_events`]
    /// once per connection so the daemon sends them.
    pub fn events(&self) -> broadcast::Receiver<DomainEvent> {
        self.inner.events.subscribe()
    }

    /// Resolves when the connection is closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.closed.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }

    pub fn is_closed(&self) -> bool {
        *self.inner.closed.borrow()
    }

    fn next_serial(&self) -> u32 {
        self.inner.serial.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, header: &Header, body: &[u8]) -> Result<(), Error> {
        self.inner
            .tx
            .send(frame(header, body))
            .map_err(|_| Error::Closed)
    }

    /// Performs a raw call and returns the reply body.
    pub async fn call(&self, proc: u32, args: Vec<u8>) -> Result<Vec<u8>, Error> {
        let (rx, _) = self.start_call(proc, &args, false)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    fn start_call(
        &self,
        proc: u32,
        args: &[u8],
        stream: bool,
    ) -> Result<(oneshot::Receiver<Reply>, Option<StreamRx>), Error> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        let serial = self.next_serial();
        let (tx, rx) = oneshot::channel();
        lock(&self.inner.pending).insert(serial, tx);
        let stream = stream.then(|| {
            let (stx, srx) = mpsc::unbounded_channel();
            lock(&self.inner.streams).insert(serial, stx);
            (serial, srx)
        });
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
        Ok((rx, stream))
    }

    async fn call_with(&self, proc: u32, f: impl FnOnce(&mut Encoder)) -> Result<Vec<u8>, Error> {
        let mut e = Encoder::new();
        f(&mut e);
        self.call(proc, e.into_inner()).await
    }

    async fn call_dom(
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

    /// `virConnectOpen`.
    pub async fn open(&self, uri: &str) -> Result<(), Error> {
        self.call_with(procs::CONNECT_OPEN, |e| {
            e.opt_string(Some(uri)).u32(0);
        })
        .await?;
        tokio::spawn(keepalive_loop(Arc::downgrade(&self.inner)));
        Ok(())
    }

    /// `virConnectClose`.
    pub async fn close(&self) -> Result<(), Error> {
        self.call(procs::CONNECT_CLOSE, Vec::new()).await.map(drop)
    }

    /// `virConnectGetLibVersion`, encoded as `major * 1_000_000 + minor * 1000 + micro`.
    pub async fn lib_version(&self) -> Result<u64, Error> {
        let r = self
            .call(procs::CONNECT_GET_LIB_VERSION, Vec::new())
            .await?;
        Decoder::new(&r).u64()
    }

    pub async fn hostname(&self) -> Result<String, Error> {
        let r = self.call(procs::CONNECT_GET_HOSTNAME, Vec::new()).await?;
        Decoder::new(&r).string()
    }

    pub async fn capabilities(&self) -> Result<String, Error> {
        let r = self
            .call(procs::CONNECT_GET_CAPABILITIES, Vec::new())
            .await?;
        Decoder::new(&r).string()
    }

    pub async fn node_info(&self) -> Result<NodeInfo, Error> {
        let r = self.call(procs::NODE_GET_INFO, Vec::new()).await?;
        NodeInfo::decode(&mut Decoder::new(&r))
    }

    /// Free host memory in bytes.
    pub async fn free_memory(&self) -> Result<u64, Error> {
        let r = self.call(procs::NODE_GET_FREE_MEMORY, Vec::new()).await?;
        Decoder::new(&r).u64()
    }

    /// `virConnectListAllDomains`; `flags == 0` lists every domain.
    pub async fn list_all_domains(&self, flags: u32) -> Result<Vec<Domain>, Error> {
        let r = self
            .call_with(procs::CONNECT_LIST_ALL_DOMAINS, |e| {
                e.i32(1).u32(flags);
            })
            .await?;
        let mut d = Decoder::new(&r);
        d.array(proto::DOMAIN_LIST_MAX, Domain::decode)
    }

    pub async fn lookup_by_uuid(&self, uuid: [u8; 16]) -> Result<Domain, Error> {
        let r = self
            .call_with(procs::DOMAIN_LOOKUP_BY_UUID, |e| {
                e.opaque_fixed(&uuid);
            })
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    pub async fn define_xml(&self, xml: &str, flags: u32) -> Result<Domain, Error> {
        let r = self
            .call_with(procs::DOMAIN_DEFINE_XML_FLAGS, |e| {
                e.string(xml).u32(flags);
            })
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    pub async fn undefine(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_UNDEFINE_FLAGS, dom, Some(flags))
            .await
            .map(drop)
    }

    /// Starts a defined domain.
    pub async fn create(&self, dom: &Domain, flags: u32) -> Result<Domain, Error> {
        let r = self
            .call_dom(procs::DOMAIN_CREATE_WITH_FLAGS, dom, Some(flags))
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    /// Hard power-off.
    pub async fn destroy(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_DESTROY_FLAGS, dom, Some(flags))
            .await
            .map(drop)
    }

    /// Requests a guest (ACPI) shutdown.
    pub async fn shutdown(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_SHUTDOWN, dom, None)
            .await
            .map(drop)
    }

    pub async fn reboot(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_REBOOT, dom, Some(flags))
            .await
            .map(drop)
    }

    pub async fn reset(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_RESET, dom, Some(0))
            .await
            .map(drop)
    }

    pub async fn suspend(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_SUSPEND, dom, None)
            .await
            .map(drop)
    }

    pub async fn resume(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_RESUME, dom, None)
            .await
            .map(drop)
    }

    pub async fn get_info(&self, dom: &Domain) -> Result<DomainInfo, Error> {
        let r = self.call_dom(16, dom, None).await?;
        let mut d = Decoder::new(&r);
        // `unsigned char` and `unsigned short` are 4-byte units in XDR.
        Ok(DomainInfo {
            state: DomainState::from(d.i32()?),
            max_mem_kib: d.u64()?,
            memory_kib: d.u64()?,
            nr_virt_cpu: d.u32()? as u16,
            cpu_time_ns: d.u64()?,
        })
    }

    /// Returns `(state, reason)`.
    pub async fn get_state(&self, dom: &Domain) -> Result<(DomainState, i32), Error> {
        let r = self.call_dom(procs::DOMAIN_GET_STATE, dom, Some(0)).await?;
        let mut d = Decoder::new(&r);
        Ok((DomainState::from(d.i32()?), d.i32()?))
    }

    pub async fn get_xml_desc(&self, dom: &Domain, flags: u32) -> Result<String, Error> {
        let r = self
            .call_dom(procs::DOMAIN_GET_XML_DESC, dom, Some(flags))
            .await?;
        Decoder::new(&r).string()
    }

    /// Sets the balloon target (KiB).
    pub async fn set_memory(&self, dom: &Domain, kib: u64, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_SET_MEMORY_FLAGS, |e| {
            dom.encode(e);
            e.u64(kib).u32(flags);
        })
        .await
        .map(drop)
    }

    /// Returns `(tag, value)` pairs; see [`proto::memory_stat`].
    pub async fn memory_stats(&self, dom: &Domain) -> Result<Vec<(i32, u64)>, Error> {
        let r = self
            .call_with(procs::DOMAIN_MEMORY_STATS, |e| {
                dom.encode(e);
                e.u32(flags::DOMAIN_MEMORY_STAT_NR).u32(0);
            })
            .await?;
        Decoder::new(&r).array(proto::MEMORY_STATS_MAX, |d| Ok((d.i32()?, d.u64()?)))
    }

    /// Asks the daemon to deliver lifecycle events for all domains on this
    /// connection. Returns the callback ID.
    pub async fn register_lifecycle_events(&self) -> Result<i32, Error> {
        let r = self
            .call_with(procs::CONNECT_DOMAIN_EVENT_CALLBACK_REGISTER_ANY, |e| {
                e.i32(flags::DOMAIN_EVENT_ID_LIFECYCLE)
                    .option(None::<&Domain>, |e, d| d.encode(e));
            })
            .await?;
        Decoder::new(&r).i32()
    }

    /// Opens a character device (serial console) as a bidirectional stream.
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
        let (reply, stream) = self.start_call(proc, &e.into_inner(), true)?;
        let (serial, rx) = stream.expect("stream requested");
        let res = reply.await.map_err(|_| Error::Closed).and_then(|r| r);
        if let Err(err) = res {
            lock(&self.inner.streams).remove(&serial);
            return Err(err);
        }
        Ok(Stream {
            reader: StreamReader { rx },
            writer: StreamWriter {
                client: self.clone(),
                proc,
                serial,
            },
        })
    }
}

/// A bidirectional libvirt data stream.
#[derive(Debug)]
pub struct Stream {
    reader: StreamReader,
    writer: StreamWriter,
}

impl Stream {
    pub fn into_split(self) -> (StreamReader, StreamWriter) {
        (self.reader, self.writer)
    }
}

/// Receiving half of a [`Stream`].
#[derive(Debug)]
pub struct StreamReader {
    rx: mpsc::UnboundedReceiver<StreamEvent>,
}

impl StreamReader {
    /// Returns the next chunk, or `None` at end of stream.
    pub async fn recv(&mut self) -> Option<Result<Vec<u8>, Error>> {
        match self.rx.recv().await? {
            StreamEvent::Data(d) => Some(Ok(d)),
            StreamEvent::Eof => None,
            StreamEvent::Error(e) => Some(Err(e)),
        }
    }
}

/// Sending half of a [`Stream`]. Dropping it aborts the stream.
#[derive(Debug)]
pub struct StreamWriter {
    client: Client,
    proc: u32,
    serial: u32,
}

impl StreamWriter {
    fn header(&self, status: Status) -> Header {
        Header {
            prog: proto::REMOTE_PROGRAM,
            vers: proto::REMOTE_PROTOCOL_VERSION,
            proc: self.proc,
            ty: MessageType::Stream,
            serial: self.serial,
            status,
        }
    }

    pub fn send(&self, data: &[u8]) -> Result<(), Error> {
        for chunk in data.chunks(STREAM_CHUNK) {
            self.client.send(&self.header(Status::Continue), chunk)?;
        }
        Ok(())
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        let _ = self.client.send(&self.header(Status::Error), &[]);
        lock(&self.client.inner.streams).remove(&self.serial);
    }
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
        if inner.tx.send(frame(&header, &[])).is_err() {
            return;
        }
    }
}

async fn read_message(rd: &mut BufReader<OwnedReadHalf>) -> Result<Vec<u8>, Error> {
    let len = rd.read_u32().await? as usize;
    if !(4 + proto::HEADER_LEN..=proto::MESSAGE_MAX).contains(&len) {
        return Err(Error::Decode(format!("bad message length {len}")));
    }
    let mut buf = vec![0; len - 4];
    rd.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn read_loop(rd: OwnedReadHalf, inner: Arc<Inner>) {
    let mut rd = BufReader::new(rd);
    let mut last_rx = Instant::now();
    let err = loop {
        let deadline = last_rx + KEEPALIVE_TIMEOUT;
        let msg = match tokio::time::timeout_at(deadline, read_message(&mut rd)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => break e,
            Err(_) => break Error::Decode("keepalive timeout".into()),
        };
        last_rx = Instant::now();
        if let Err(e) = dispatch(&inner, &msg) {
            break e;
        }
    };
    tracing::debug!("libvirt connection closed: {err}");
    inner.closed.send_replace(true);
    for (_, tx) in lock(&inner.pending).drain() {
        let _ = tx.send(Err(Error::Closed));
    }
    for (_, tx) in lock(&inner.streams).drain() {
        let _ = tx.send(StreamEvent::Error(Error::Closed));
    }
}

fn dispatch(inner: &Inner, msg: &[u8]) -> Result<(), Error> {
    let mut d = Decoder::new(msg);
    let h = Header::decode(&mut d)?;
    let body = &msg[proto::HEADER_LEN..];
    if h.prog == proto::KEEPALIVE_PROGRAM {
        if h.proc == proto::KEEPALIVE_PROC_PING {
            let pong = Header {
                proc: proto::KEEPALIVE_PROC_PONG,
                ..h
            };
            let _ = inner.tx.send(frame(&pong, &[]));
        }
        return Ok(());
    }
    if h.prog != proto::REMOTE_PROGRAM {
        tracing::debug!("ignoring message for program {:#x}", h.prog);
        return Ok(());
    }
    let remote_err = || -> Error {
        match RemoteError::decode(&mut Decoder::new(body)) {
            Ok(e) => Error::Remote(e),
            Err(e) => e,
        }
    };
    match h.ty {
        MessageType::Reply | MessageType::ReplyWithFds => {
            let Some(tx) = lock(&inner.pending).remove(&h.serial) else {
                tracing::debug!("reply for unknown serial {}", h.serial);
                return Ok(());
            };
            let res = match h.status {
                Status::Ok => Ok(body.to_vec()),
                Status::Error => Err(remote_err()),
                Status::Continue => Err(Error::Decode("CONTINUE status on reply".into())),
            };
            let _ = tx.send(res);
        }
        MessageType::Message => {
            if h.proc == procs::DOMAIN_EVENT_CALLBACK_LIFECYCLE {
                let mut d = Decoder::new(body);
                let _callback_id = d.i32()?;
                let ev = DomainEvent {
                    domain: Domain::decode(&mut d)?,
                    event: LifecycleEvent::from(d.i32()?),
                    detail: d.i32()?,
                };
                let _ = inner.events.send(ev);
            }
        }
        MessageType::Stream => {
            let mut streams = lock(&inner.streams);
            let ev = match h.status {
                Status::Continue => StreamEvent::Data(body.to_vec()),
                Status::Ok => StreamEvent::Eof,
                Status::Error => StreamEvent::Error(remote_err()),
            };
            let done = !matches!(ev, StreamEvent::Data(_));
            if let Some(tx) = streams.get(&h.serial) {
                let _ = tx.send(ev);
            }
            if done {
                streams.remove(&h.serial);
            }
        }
        MessageType::StreamHole | MessageType::Call | MessageType::CallWithFds => {}
    }
    Ok(())
}
