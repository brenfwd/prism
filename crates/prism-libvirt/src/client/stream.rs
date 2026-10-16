//! Console streams: the receiving and sending halves of a libvirt data
//! stream.

use tokio::sync::mpsc;

use super::{Client, lock};
use crate::Error;
use crate::proto::{self, Header, MessageType, Status};

/// Largest data chunk sent in one stream message.
const STREAM_CHUNK: usize = 256 * 1024;

#[derive(Debug)]
pub(super) enum StreamEvent {
    Data(Vec<u8>),
    Eof,
    Error(Error),
}

/// The receiver for a stream's data, keyed by the call's serial.
pub(super) type StreamRx = (u32, mpsc::UnboundedReceiver<StreamEvent>);

/// A bidirectional libvirt data stream.
#[derive(Debug)]
pub struct Stream {
    reader: StreamReader,
    writer: StreamWriter,
}

impl Stream {
    pub(super) fn new(rx: mpsc::UnboundedReceiver<StreamEvent>, writer: StreamWriter) -> Self {
        Self {
            reader: StreamReader { rx },
            writer,
        }
    }

    /// Splits the stream so one task can read while another writes.
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
    /// Returns the next chunk, or `None` at end of stream. A stream error
    /// or a closed connection arrives as `Some(Err(_))` before the end.
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
    pub(super) fn new(client: Client, proc: u32, serial: u32) -> Self {
        Self {
            client,
            proc,
            serial,
        }
    }

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

    /// Queues `data` for the daemon in chunks of at most 256 KiB. The write
    /// queue is unbounded, so this never waits; it fails with
    /// [`Error::Closed`] once the connection has ended.
    pub fn send(&self, data: &[u8]) -> Result<(), Error> {
        for chunk in data.chunks(STREAM_CHUNK) {
            self.client.send(&self.header(Status::Continue), chunk)?;
        }
        Ok(())
    }
}

impl Drop for StreamWriter {
    fn drop(&mut self) {
        if let Err(e) = self.client.send(&self.header(Status::Error), &[]) {
            // The daemon keeps the stream open until the connection ends.
            tracing::debug!(serial = self.serial, "could not abort libvirt stream: {e}");
        }
        lock(&self.client.inner.streams).remove(&self.serial);
    }
}
