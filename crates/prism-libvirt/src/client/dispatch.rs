//! The read side of the connection: reads frames and routes each to the
//! pending call, event channel or stream it belongs to.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::time::Instant;

use super::stream::StreamEvent;
use super::{Inner, KEEPALIVE_TIMEOUT, frame, lock};
use crate::Error;
use crate::events::DomainEvent;
use crate::proto::{self, Header, MessageType, RemoteError, Status};
use crate::xdr::Decoder;

async fn read_message(rd: &mut BufReader<OwnedReadHalf>) -> Result<Vec<u8>, Error> {
    let len = usize::try_from(rd.read_u32().await?).unwrap_or(usize::MAX);
    if !(4 + proto::HEADER_LEN..=proto::MESSAGE_MAX).contains(&len) {
        return Err(Error::Decode(format!("bad message length {len}")));
    }
    let mut buf = vec![0; len - 4];
    rd.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Reads until the connection fails, then fails every pending call and
/// stream. Silence for [`KEEPALIVE_TIMEOUT`] counts as failure.
pub(super) async fn read_loop(rd: OwnedReadHalf, inner: Arc<Inner>) {
    let mut rd = BufReader::new(rd);
    let mut last_rx = Instant::now();
    let err = loop {
        let deadline = last_rx + KEEPALIVE_TIMEOUT;
        let msg = match tokio::time::timeout_at(deadline, read_message(&mut rd)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => break e,
            Err(_) => break Error::KeepaliveTimeout,
        };
        last_rx = Instant::now();
        if let Err(e) = dispatch(&inner, &msg) {
            break e;
        }
    };
    tracing::debug!("libvirt connection closed: {err}");
    let timed_out = matches!(err, Error::KeepaliveTimeout);
    let reason = || {
        if timed_out {
            Error::KeepaliveTimeout
        } else {
            Error::Closed
        }
    };
    inner.closed.send_replace(true);
    for (_, tx) in lock(&inner.pending).drain() {
        // The caller may have timed out and dropped its receiver.
        let _ = tx.send(Err(reason()));
    }
    for (_, tx) in lock(&inner.streams).drain() {
        // The reader may have been dropped already.
        let _ = tx.send(StreamEvent::Error(reason()));
    }
}

fn dispatch(inner: &Inner, msg: &[u8]) -> Result<(), Error> {
    let h = Header::decode(&mut Decoder::new(msg))?;
    let body = &msg[proto::HEADER_LEN..];
    if h.prog == proto::KEEPALIVE_PROGRAM {
        dispatch_keepalive(inner, h);
        return Ok(());
    }
    if h.prog != proto::REMOTE_PROGRAM {
        tracing::debug!("ignoring message for program {:#x}", h.prog);
        return Ok(());
    }
    match h.ty {
        MessageType::Reply | MessageType::ReplyWithFds => dispatch_reply(inner, h, body),
        MessageType::Message => dispatch_event(inner, h.proc, body),
        MessageType::Stream => dispatch_stream(inner, h, body),
        MessageType::StreamHole | MessageType::Call | MessageType::CallWithFds => {}
    }
    Ok(())
}

fn dispatch_keepalive(inner: &Inner, h: Header) {
    if h.proc != proto::KEEPALIVE_PROC_PING {
        return;
    }
    let pong = Header {
        proc: proto::KEEPALIVE_PROC_PONG,
        ..h
    };
    let sent = frame(&pong, &[]).is_ok_and(|f| inner.tx.send(f).is_ok());
    if !sent {
        // The write task is gone, so the read side is about to fail too.
        tracing::debug!("could not answer libvirt keepalive ping");
    }
}

/// Decodes the error body of a reply or stream message.
fn remote_error(body: &[u8]) -> Error {
    match RemoteError::decode(&mut Decoder::new(body)) {
        Ok(e) => Error::Remote(e),
        Err(e) => e,
    }
}

fn dispatch_reply(inner: &Inner, h: Header, body: &[u8]) {
    let Some(tx) = lock(&inner.pending).remove(&h.serial) else {
        tracing::debug!("reply for unknown serial {}", h.serial);
        return;
    };
    let res = match h.status {
        Status::Ok => Ok(body.to_vec()),
        Status::Error => Err(remote_error(body)),
        Status::Continue => Err(Error::Decode("CONTINUE status on reply".into())),
    };
    // The caller may have timed out and dropped its receiver.
    let _ = tx.send(res);
}

fn dispatch_event(inner: &Inner, proc: u32, body: &[u8]) {
    match DomainEvent::decode(proc, body) {
        Ok(Some(ev)) => {
            // An error only means nobody is subscribed.
            let _ = inner.events.send(ev);
        }
        Ok(None) => tracing::debug!("ignoring event procedure {proc}"),
        Err(e) => tracing::warn!("undecodable event {proc}: {e}"),
    }
}

fn dispatch_stream(inner: &Inner, h: Header, body: &[u8]) {
    let ev = match h.status {
        Status::Continue => StreamEvent::Data(body.to_vec()),
        Status::Ok => StreamEvent::Eof,
        Status::Error => StreamEvent::Error(remote_error(body)),
    };
    let done = !matches!(ev, StreamEvent::Data(_));
    let mut streams = lock(&inner.streams);
    if let Some(tx) = streams.get(&h.serial) {
        // The reader may have been dropped; the writer's Drop cleans up.
        let _ = tx.send(ev);
    }
    if done {
        streams.remove(&h.serial);
    }
}
