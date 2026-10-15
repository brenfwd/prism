//! nfnetlink encoding and the batch transaction.

use std::time::Duration;

use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_NETFILTER};

use super::{BaseChain, Chain, CmpOp, Expr, Family, PayloadBase, Policy, Rule, Table, Verdict};
use crate::{Error, Result};

const NFNL_SUBSYS_NFTABLES: u16 = 10;
const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
const NFNL_MSG_BATCH_END: u16 = 0x11;

const NFT_MSG_NEWTABLE: u16 = 0;
const NFT_MSG_DELTABLE: u16 = 2;
const NFT_MSG_NEWCHAIN: u16 = 3;
const NFT_MSG_NEWRULE: u16 = 6;

const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_APPEND: u16 = 0x800;
const NLM_F_CREATE: u16 = 0x400;
const NLM_F_CAPPED: u16 = 0x100;
const NLM_F_ACK_TLVS: u16 = 0x200;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSGERR_ATTR_MSG: u16 = 1;
const NLA_F_NESTED: u16 = 0x8000;

const NFTA_TABLE_NAME: u16 = 1;
const NFTA_CHAIN_TABLE: u16 = 1;
const NFTA_CHAIN_NAME: u16 = 3;
const NFTA_CHAIN_HOOK: u16 = 4;
const NFTA_CHAIN_POLICY: u16 = 5;
const NFTA_CHAIN_TYPE: u16 = 7;
const NFTA_HOOK_HOOKNUM: u16 = 1;
const NFTA_HOOK_PRIORITY: u16 = 2;
const NFTA_RULE_TABLE: u16 = 1;
const NFTA_RULE_CHAIN: u16 = 2;
const NFTA_RULE_EXPRESSIONS: u16 = 4;
const NFTA_LIST_ELEM: u16 = 1;
const NFTA_EXPR_NAME: u16 = 1;
const NFTA_EXPR_DATA: u16 = 2;
const NFTA_DATA_VALUE: u16 = 1;
const NFTA_DATA_VERDICT: u16 = 2;
const NFTA_VERDICT_CODE: u16 = 1;
const NFTA_VERDICT_CHAIN: u16 = 2;

const NFT_REG_VERDICT: u32 = 0;
const NFT_REG_1: u32 = 1;

const NF_DROP: i32 = 0;
const NF_ACCEPT: i32 = 1;
const NFT_JUMP: i32 = -3;
const NFT_GOTO: i32 = -4;
const NFT_RETURN: i32 = -5;

/// Builds one netlink message; attribute helpers handle nesting and padding.
struct Msg {
    buf: Vec<u8>,
    nests: Vec<usize>,
}

impl Msg {
    fn new(kind: u16, flags: u16, seq: u32, family: u8, res_id: u16) -> Self {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&0u32.to_ne_bytes()); // length, patched in finish()
        buf.extend_from_slice(&kind.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes()); // port id: kernel
        buf.extend_from_slice(&[family, 0]); // nfgenmsg: family, NFNETLINK_V0
        buf.extend_from_slice(&res_id.to_be_bytes());
        Self {
            buf,
            nests: Vec::new(),
        }
    }

    fn pad(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    fn attr(&mut self, kind: u16, data: &[u8]) -> &mut Self {
        let len = u16::try_from(4 + data.len()).expect("attribute too large");
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        self.buf.extend_from_slice(data);
        self.pad();
        self
    }

    fn str(&mut self, kind: u16, s: &str) -> &mut Self {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.attr(kind, &data)
    }

    fn be32(&mut self, kind: u16, v: u32) -> &mut Self {
        self.attr(kind, &v.to_be_bytes())
    }

    fn begin(&mut self, kind: u16) -> &mut Self {
        self.nests.push(self.buf.len());
        self.buf.extend_from_slice(&0u16.to_ne_bytes());
        self.buf
            .extend_from_slice(&(kind | NLA_F_NESTED).to_ne_bytes());
        self
    }

    fn end(&mut self) -> &mut Self {
        let start = self.nests.pop().expect("unbalanced nest");
        let len = u16::try_from(self.buf.len() - start).expect("nest too large");
        self.buf[start..start + 2].copy_from_slice(&len.to_ne_bytes());
        self
    }

    fn finish(mut self) -> Vec<u8> {
        debug_assert_eq!(self.nests.len(), 0, "unbalanced nest");
        let len = self.buf.len() as u32;
        self.buf[..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }
}

fn data_value(m: &mut Msg, kind: u16, value: &[u8]) {
    m.begin(kind).attr(NFTA_DATA_VALUE, value).end();
}

/// A DNAT is three expressions: the address and port go into registers, then
/// the `nat` expression reads them.
fn dnat(m: &mut Msg, addr: std::net::Ipv4Addr, port: u16) {
    for (reg, value) in [
        (NFT_REG_1, &addr.octets()[..]),
        (2, &port.to_be_bytes()[..]),
    ] {
        m.begin(NFTA_LIST_ELEM)
            .str(NFTA_EXPR_NAME, "immediate")
            .begin(NFTA_EXPR_DATA);
        m.be32(1, reg);
        data_value(m, 2, value);
        m.end().end();
    }
    m.begin(NFTA_LIST_ELEM)
        .str(NFTA_EXPR_NAME, "nat")
        .begin(NFTA_EXPR_DATA);
    m.be32(1, 1) // NFT_NAT_DNAT
        .be32(2, 2) // NFPROTO_IPV4
        .be32(3, NFT_REG_1) // address min
        .be32(5, 2) // proto min
        .be32(7, 3) // MAP_IPS | PROTO_SPECIFIED
        .end()
        .end();
}

fn expr(m: &mut Msg, e: &Expr) {
    if let Expr::Dnat { addr, port } = e {
        dnat(m, *addr, *port);
        return;
    }
    m.begin(NFTA_LIST_ELEM);
    match e {
        Expr::Meta { key } => {
            m.str(NFTA_EXPR_NAME, "meta").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_1).be32(2, key.number()).end();
        }
        Expr::Payload { base, offset, len } => {
            let base = match base {
                PayloadBase::Link => 0,
                PayloadBase::Network => 1,
                PayloadBase::Transport => 2,
            };
            m.str(NFTA_EXPR_NAME, "payload").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_1)
                .be32(2, base)
                .be32(3, *offset)
                .be32(4, *len)
                .end();
        }
        Expr::Cmp { op, data } => {
            let op = match op {
                CmpOp::Eq => 0,
                CmpOp::Neq => 1,
                CmpOp::Lt => 2,
                CmpOp::Lte => 3,
                CmpOp::Gt => 4,
                CmpOp::Gte => 5,
            };
            m.str(NFTA_EXPR_NAME, "cmp").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_1).be32(2, op);
            data_value(m, 3, data);
            m.end();
        }
        Expr::Bitwise { mask, xor } => {
            m.str(NFTA_EXPR_NAME, "bitwise").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_1)
                .be32(2, NFT_REG_1)
                .be32(3, mask.len() as u32);
            data_value(m, 4, mask);
            data_value(m, 5, xor);
            m.end();
        }
        Expr::CtState => {
            m.str(NFTA_EXPR_NAME, "ct").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_1).be32(2, 0).end(); // NFT_CT_STATE
        }
        Expr::Counter => {
            m.str(NFTA_EXPR_NAME, "counter").begin(NFTA_EXPR_DATA);
            m.attr(1, &0u64.to_be_bytes())
                .attr(2, &0u64.to_be_bytes())
                .end();
        }
        Expr::Masquerade => {
            m.str(NFTA_EXPR_NAME, "masq");
        }
        Expr::Dnat { .. } => unreachable!("handled above"),
        Expr::Verdict(v) => {
            let (code, chain) = match v {
                Verdict::Accept => (NF_ACCEPT, None),
                Verdict::Drop => (NF_DROP, None),
                Verdict::Return => (NFT_RETURN, None),
                Verdict::Jump(c) => (NFT_JUMP, Some(c)),
                Verdict::Goto(c) => (NFT_GOTO, Some(c)),
            };
            m.str(NFTA_EXPR_NAME, "immediate").begin(NFTA_EXPR_DATA);
            m.be32(1, NFT_REG_VERDICT).begin(2).begin(NFTA_DATA_VERDICT);
            m.be32(NFTA_VERDICT_CODE, code.cast_unsigned());
            if let Some(c) = chain {
                m.str(NFTA_VERDICT_CHAIN, c);
            }
            m.end().end().end();
        }
    }
    m.end();
}

/// A transaction under construction: messages plus a description of each for errors.
struct Batch {
    msgs: Vec<(u32, String, Vec<u8>)>,
    seq: u32,
}

impl Batch {
    fn new(seq: u32) -> Self {
        Self {
            msgs: Vec::new(),
            seq,
        }
    }

    fn next_seq(&mut self) -> u32 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn push(
        &mut self,
        what: String,
        kind: u16,
        flags: u16,
        family: Family,
        f: impl FnOnce(&mut Msg),
    ) {
        let seq = self.next_seq();
        let mut m = Msg::new(
            (NFNL_SUBSYS_NFTABLES << 8) | kind,
            NLM_F_REQUEST | NLM_F_ACK | flags,
            seq,
            family.nfproto(),
            0,
        );
        f(&mut m);
        self.msgs.push((seq, what, m.finish()));
    }

    fn delete_table(&mut self, family: Family, name: &str) {
        let label = format!("{} {name}", family.as_str());
        // NEWTABLE without EXCL succeeds whether or not the table exists, so the DELTABLE
        // that follows never fails with ENOENT (works on kernels without DESTROYTABLE).
        self.push(
            format!("add table {label}"),
            NFT_MSG_NEWTABLE,
            NLM_F_CREATE,
            family,
            |m| {
                m.str(NFTA_TABLE_NAME, name);
            },
        );
        self.push(
            format!("delete table {label}"),
            NFT_MSG_DELTABLE,
            0,
            family,
            |m| {
                m.str(NFTA_TABLE_NAME, name);
            },
        );
    }

    fn add_table(&mut self, t: &Table) {
        let label = format!("{} {}", t.family.as_str(), t.name);
        self.push(
            format!("add table {label}"),
            NFT_MSG_NEWTABLE,
            NLM_F_CREATE,
            t.family,
            |m| {
                m.str(NFTA_TABLE_NAME, &t.name);
            },
        );
        for c in &t.chains {
            self.add_chain(t, c);
        }
        for c in &t.chains {
            for (i, r) in c.rules.iter().enumerate() {
                self.add_rule(t, c, i, r);
            }
        }
    }

    fn add_chain(&mut self, t: &Table, c: &Chain) {
        let what = format!("add chain {} {} {}", t.family.as_str(), t.name, c.name);
        self.push(what, NFT_MSG_NEWCHAIN, NLM_F_CREATE, t.family, |m| {
            m.str(NFTA_CHAIN_TABLE, &t.name)
                .str(NFTA_CHAIN_NAME, &c.name);
            if let Some(BaseChain {
                kind,
                hook,
                priority,
                policy,
            }) = c.base
            {
                m.begin(NFTA_CHAIN_HOOK)
                    .be32(NFTA_HOOK_HOOKNUM, hook.number())
                    .be32(NFTA_HOOK_PRIORITY, priority.cast_unsigned())
                    .end();
                let policy = match policy {
                    Policy::Accept => NF_ACCEPT,
                    Policy::Drop => NF_DROP,
                };
                m.be32(NFTA_CHAIN_POLICY, policy.cast_unsigned())
                    .str(NFTA_CHAIN_TYPE, kind.as_str());
            }
        });
    }

    fn add_rule(&mut self, t: &Table, c: &Chain, index: usize, r: &Rule) {
        let what = format!(
            "add rule {} {} {} (#{index}: {:?})",
            t.family.as_str(),
            t.name,
            c.name,
            r.exprs()
        );
        self.push(
            what,
            NFT_MSG_NEWRULE,
            NLM_F_CREATE | NLM_F_APPEND,
            t.family,
            |m| {
                m.str(NFTA_RULE_TABLE, &t.name)
                    .str(NFTA_RULE_CHAIN, &c.name);
                m.begin(NFTA_RULE_EXPRESSIONS);
                for e in r.exprs() {
                    expr(m, e);
                }
                m.end();
            },
        );
    }

    /// Wraps the messages in `BATCH_BEGIN`/`BATCH_END`.
    fn encode(&mut self) -> Vec<u8> {
        let begin_seq = self.seq.wrapping_sub(self.msgs.len() as u32);
        let mut out = Msg::new(
            NFNL_MSG_BATCH_BEGIN,
            NLM_F_REQUEST,
            begin_seq,
            0,
            NFNL_SUBSYS_NFTABLES,
        )
        .finish();
        for (_, _, m) in &self.msgs {
            out.extend_from_slice(m);
        }
        let end_seq = self.next_seq();
        out.extend(
            Msg::new(
                NFNL_MSG_BATCH_END,
                NLM_F_REQUEST,
                end_seq,
                0,
                NFNL_SUBSYS_NFTABLES,
            )
            .finish(),
        );
        out
    }
}

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
        b.delete_table(table.family, &table.name);
        b.add_table(table);
        self.commit(b)
    }

    /// Atomically replaces several tables in one transaction.
    pub fn replace_tables(&mut self, tables: &[Table]) -> Result<()> {
        let mut b = Batch::new(self.seq);
        for t in tables {
            b.delete_table(t.family, &t.name);
            b.add_table(t);
        }
        self.commit(b)
    }

    /// Deletes a table if it exists.
    pub fn delete_table(&mut self, family: Family, name: &str) -> Result<()> {
        let mut b = Batch::new(self.seq);
        b.delete_table(family, name);
        self.commit(b)
    }

    /// Async wrapper: runs [`Nft::replace_tables`] on the blocking pool.
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
        let payload = b.encode();
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
                let len = u32::from_ne_bytes(data[0..4].try_into().expect("4 bytes")) as usize;
                if len < 16 || len > data.len() {
                    return Err(Error::Protocol("truncated netlink message".into()));
                }
                let kind = u16::from_ne_bytes([data[4], data[5]]);
                let flags = u16::from_ne_bytes([data[6], data[7]]);
                let seq = u32::from_ne_bytes(data[8..12].try_into().expect("4 bytes"));
                let body = &data[16..len];
                if kind == NLMSG_ERROR && body.len() >= 4 {
                    let code = i32::from_ne_bytes(body[0..4].try_into().expect("4 bytes"));
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

/// Extracts `NLMSGERR_ATTR_MSG` from an error message body.
fn ext_ack_message(body: &[u8], flags: u16) -> Option<String> {
    if flags & NLM_F_ACK_TLVS == 0 || body.len() < 20 {
        return None;
    }
    let echoed_len = if flags & NLM_F_CAPPED != 0 {
        16
    } else {
        u32::from_ne_bytes(body[4..8].try_into().ok()?) as usize
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
    use crate::nft::{Chain, ChainType, Hook, priority};

    #[test]
    fn encodes_attributes_with_padding_and_nesting() {
        let mut m = Msg::new(1, 0, 7, 1, 0);
        m.str(1, "abc").begin(2).be32(1, 5).end();
        let out = m.finish();
        assert_eq!(out.len(), 16 + 4 + 8 + 4 + 8);
        assert_eq!(
            u32::from_ne_bytes(out[0..4].try_into().unwrap()) as usize,
            out.len()
        );
        // "abc\0" attr: len 8, type 1.
        assert_eq!(&out[20..24], &[8, 0, 1, 0]);
        assert_eq!(&out[24..28], b"abc\0");
        // nested attr: len 12, type 2|NESTED.
        assert_eq!(&out[28..32], &[12, 0, 2, 0x80]);
        assert_eq!(&out[36..40], &5u32.to_be_bytes());
    }

    #[test]
    fn batch_has_begin_and_end() {
        let mut t = Table::new(Family::Inet, "x");
        t.chain(Chain::base(
            "c",
            ChainType::Filter,
            Hook::Input,
            priority::FILTER,
        ))
        .push(Rule::new(Family::Inet).iifname("lo").accept());
        let mut b = Batch::new(100);
        b.delete_table(t.family, &t.name);
        b.add_table(&t);
        let msgs = b.msgs.len();
        assert_eq!(msgs, 5);
        let out = b.encode();
        assert_eq!(u16::from_ne_bytes([out[4], out[5]]), NFNL_MSG_BATCH_BEGIN);
        assert_eq!(u32::from_ne_bytes(out[8..12].try_into().unwrap()), 100);
        let tail = &out[out.len() - 20..];
        assert_eq!(u16::from_ne_bytes([tail[4], tail[5]]), NFNL_MSG_BATCH_END);
    }
}
