//! The nfnetlink socket and the batch transaction with its acknowledgement parsing.

use std::time::Duration;

use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_NETFILTER};

use super::consts::{NLM_F_ACK_TLVS, NLM_F_CAPPED, NLMSG_DONE, NLMSG_ERROR, NLMSGERR_ATTR_MSG};
use super::serialize::Batch;
use crate::nft::{Family, Table};
use crate::{Error, Result};

/// An nfnetlink socket for nftables transactions in the current network namespace.
///
/// The socket belongs to the namespace of the thread that created it.
pub struct Nft {
    sock: Socket,
    seq: u32,
}

impl std::fmt::Debug for Nft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nft")
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl Nft {
    /// Opens an nfnetlink socket in the calling thread's network namespace. Receives time
    /// out after 5 seconds.
    pub fn new() -> Result<Self> {
        let op = |e| Error::io("open nfnetlink socket", e);
        let mut sock = Socket::new(NETLINK_NETFILTER).map_err(op)?;
        sock.bind_auto().map_err(op)?;
        sock.connect(&SocketAddr::new(0, 0)).map_err(op)?;
        // Errors then carry the kernel's extended-ack message, and the echoed request only
        // its header.
        sock.set_ext_ack(true).map_err(op)?;
        sock.set_cap_ack(true).map_err(op)?;
        let s2 = socket2::SockRef::from(&sock);
        s2.set_recv_buffer_size(4 << 20).map_err(op)?;
        s2.set_send_buffer_size(4 << 20).map_err(op)?;
        s2.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(op)?;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos());
        Ok(Self { sock, seq: seed })
    }

    /// Atomically replaces `table` (deleting any previous version) in one transaction.
    pub fn replace_table(&mut self, table: &Table) -> Result<()> {
        let mut b = Batch::new(self.seq);
        b.delete_table(table.family, &table.name)?;
        b.add_table(table)?;
        self.commit(b)
    }

    /// Atomically replaces several tables in one transaction.
    pub fn replace_tables(&mut self, tables: &[Table]) -> Result<()> {
        let mut b = Batch::new(self.seq);
        for t in tables {
            b.delete_table(t.family, &t.name)?;
            b.add_table(t)?;
        }
        self.commit(b)
    }

    /// Deletes a table if it exists.
    pub fn delete_table(&mut self, family: Family, name: &str) -> Result<()> {
        let mut b = Batch::new(self.seq);
        b.delete_table(family, name)?;
        self.commit(b)
    }

    /// Runs [`Nft::replace_tables`] on tokio's blocking pool, because the transaction does a
    /// blocking `recv` with a 5 second timeout. The socket is moved in and handed back with
    /// the result.
    pub async fn replace_tables_async(mut self, tables: Vec<Table>) -> (Self, Result<()>) {
        match tokio::task::spawn_blocking(move || {
            let r = self.replace_tables(&tables);
            (self, r)
        })
        .await
        {
            Ok(out) => out,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }

    fn commit(&mut self, mut b: Batch) -> Result<()> {
        let payload = b.encode()?;
        self.seq = b.seq.wrapping_add(1);
        self.sock
            .send(&payload, 0)
            .map_err(|e| Error::kernel("send nftables batch", e))?;

        // The kernel processes the whole batch inside sendmsg(), so every ACK is queued by
        // now; each message we sent with NLM_F_ACK gets exactly one ACK or error.
        let mut pending: std::collections::BTreeSet<u32> = b.msgs.iter().map(|m| m.0).collect();
        let mut first_err: Option<Error> = None;
        let mut buf = vec![0u8; 1 << 16];
        while !pending.is_empty() {
            let mut window = &mut buf[..];
            let n = match self.sock.recv(&mut window, 0) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(first_err.unwrap_or_else(|| {
                        Error::Protocol(format!(
                            "nftables: no answer for {} message(s)",
                            pending.len()
                        ))
                    }));
                }
                Err(e) => return Err(Error::kernel("receive nftables ack", e)),
            };
            let mut data = &buf[..n];
            while data.len() >= 16 {
                let len = read_u32_ne(data, 0).unwrap_or(0) as usize;
                if len < 16 || len > data.len() {
                    return Err(Error::Protocol("truncated netlink message".into()));
                }
                let kind = u16::from_ne_bytes([data[4], data[5]]);
                let flags = u16::from_ne_bytes([data[6], data[7]]);
                let seq = read_u32_ne(data, 8).unwrap_or(0);
                let body = &data[16..len];
                if kind == NLMSG_ERROR && body.len() >= 4 {
                    let code = read_u32_ne(body, 0).unwrap_or(0).cast_signed();
                    pending.remove(&seq);
                    if code != 0 && first_err.is_none() {
                        let what = b
                            .msgs
                            .iter()
                            .find(|m| m.0 == seq)
                            .map_or("nftables batch", |m| m.1.as_str());
                        let detail = ext_ack_message(body, flags)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        first_err = Some(Error::kernel(
                            format!("{what}{detail}"),
                            std::io::Error::from_raw_os_error(-code),
                        ));
                    }
                } else if kind == NLMSG_DONE {
                    pending.clear();
                }
                data = &data[len.next_multiple_of(4).min(data.len())..];
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

/// The native-endian `u32` at `offset`, or `None` when `data` is too short.
fn read_u32_ne(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

/// Extracts `NLMSGERR_ATTR_MSG` from an error message body.
fn ext_ack_message(body: &[u8], flags: u16) -> Option<String> {
    if flags & NLM_F_ACK_TLVS == 0 || body.len() < 20 {
        return None;
    }
    let echoed_len = if flags & NLM_F_CAPPED != 0 {
        16
    } else {
        read_u32_ne(body, 4)? as usize
    };
    let mut tlvs = body.get(4 + echoed_len.next_multiple_of(4)..)?;
    while tlvs.len() >= 4 {
        let len = usize::from(u16::from_ne_bytes([tlvs[0], tlvs[1]]));
        let kind = u16::from_ne_bytes([tlvs[2], tlvs[3]]);
        if len < 4 || len > tlvs.len() {
            break;
        }
        if kind == NLMSGERR_ATTR_MSG {
            let s = &tlvs[4..len];
            let s = s.split(|b| *b == 0).next().unwrap_or(s);
            return Some(String::from_utf8_lossy(s).into_owned());
        }
        tlvs = &tlvs[len.next_multiple_of(4).min(tlvs.len())..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_u32_within_bounds_only() {
        let data = [1, 0, 0, 0, 2, 0, 0];
        assert_eq!(read_u32_ne(&data, 0), Some(1));
        assert_eq!(read_u32_ne(&data, 4), None);
        assert_eq!(read_u32_ne(&data, usize::MAX), None);
    }
}
