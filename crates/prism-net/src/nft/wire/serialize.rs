//! Serialises tables, chains, rules and expressions into `nf_tables` netlink messages.

use super::consts::{
    NF_ACCEPT, NF_DROP, NFNL_MSG_BATCH_BEGIN, NFNL_MSG_BATCH_END, NFNL_SUBSYS_NFTABLES, NFT_GOTO,
    NFT_JUMP, NFT_MSG_DELTABLE, NFT_MSG_NEWCHAIN, NFT_MSG_NEWRULE, NFT_MSG_NEWTABLE, NFT_REG_1,
    NFT_REG_VERDICT, NFT_RETURN, NFTA_CHAIN_HOOK, NFTA_CHAIN_NAME, NFTA_CHAIN_POLICY,
    NFTA_CHAIN_TABLE, NFTA_CHAIN_TYPE, NFTA_DATA_VALUE, NFTA_DATA_VERDICT, NFTA_EXPR_DATA,
    NFTA_EXPR_NAME, NFTA_HOOK_HOOKNUM, NFTA_HOOK_PRIORITY, NFTA_LIST_ELEM, NFTA_RULE_CHAIN,
    NFTA_RULE_EXPRESSIONS, NFTA_RULE_TABLE, NFTA_TABLE_NAME, NFTA_VERDICT_CHAIN, NFTA_VERDICT_CODE,
    NLM_F_ACK, NLM_F_APPEND, NLM_F_CREATE, NLM_F_REQUEST,
};
use super::encode::Msg;
use crate::Result;
use crate::nft::{
    BaseChain, Chain, CmpOp, Expr, Family, PayloadBase, Policy, Rule, Table, Verdict,
};

fn data_value(m: &mut Msg, kind: u16, value: &[u8]) {
    m.begin(kind).attr(NFTA_DATA_VALUE, value).end();
}

/// A DNAT is three expressions: the address and port go into registers, then
/// the `nat` expression reads them.
fn dnat(m: &mut Msg, addr: std::net::IpAddr, port: u16) {
    nat(m, 1, addr, Some(port));
}

/// `nat` of kind `NFT_NAT_DNAT` (1) or `NFT_NAT_SNAT` (0); SNAT has no port.
fn nat(m: &mut Msg, kind: u32, addr: std::net::IpAddr, port: Option<u16>) {
    let (proto, octets) = match addr {
        std::net::IpAddr::V4(a) => (2, a.octets().to_vec()), // NFPROTO_IPV4
        std::net::IpAddr::V6(a) => (10, a.octets().to_vec()), // NFPROTO_IPV6
    };
    let mut regs = vec![(NFT_REG_1, octets)];
    if let Some(p) = port {
        regs.push((2, p.to_be_bytes().to_vec()));
    }
    for (reg, value) in regs {
        m.begin(NFTA_LIST_ELEM)
            .str(NFTA_EXPR_NAME, "immediate")
            .begin(NFTA_EXPR_DATA);
        m.be32(1, reg);
        data_value(m, 2, &value);
        m.end().end();
    }
    m.begin(NFTA_LIST_ELEM)
        .str(NFTA_EXPR_NAME, "nat")
        .begin(NFTA_EXPR_DATA);
    m.be32(1, kind).be32(2, proto).be32(3, NFT_REG_1); // address min
    if port.is_some() {
        m.be32(5, 2); // proto min
    }
    // MAP_IPS, plus PROTO_SPECIFIED with a port.
    m.be32(7, if port.is_some() { 3 } else { 1 }).end().end();
}

fn expr(m: &mut Msg, e: &Expr) {
    if let Expr::Dnat { addr, port } = e {
        dnat(m, *addr, *port);
        return;
    }
    if let Expr::Snat { addr } = e {
        nat(m, 0, *addr, None);
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
        Expr::Dnat { .. } | Expr::Snat { .. } => unreachable!("handled above"),
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
pub(super) struct Batch {
    pub(super) msgs: Vec<(u32, String, Vec<u8>)>,
    pub(super) seq: u32,
}

impl Batch {
    pub(super) fn new(seq: u32) -> Self {
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
    ) -> Result<()> {
        let seq = self.next_seq();
        let mut m = Msg::new(
            (NFNL_SUBSYS_NFTABLES << 8) | kind,
            NLM_F_REQUEST | NLM_F_ACK | flags,
            seq,
            family.nfproto(),
            0,
        );
        f(&mut m);
        self.msgs.push((seq, what, m.finish()?));
        Ok(())
    }

    pub(super) fn delete_table(&mut self, family: Family, name: &str) -> Result<()> {
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
        )?;
        self.push(
            format!("delete table {label}"),
            NFT_MSG_DELTABLE,
            0,
            family,
            |m| {
                m.str(NFTA_TABLE_NAME, name);
            },
        )
    }

    pub(super) fn add_table(&mut self, t: &Table) -> Result<()> {
        let label = format!("{} {}", t.family.as_str(), t.name);
        self.push(
            format!("add table {label}"),
            NFT_MSG_NEWTABLE,
            NLM_F_CREATE,
            t.family,
            |m| {
                m.str(NFTA_TABLE_NAME, &t.name);
            },
        )?;
        for c in &t.chains {
            self.add_chain(t, c)?;
        }
        for c in &t.chains {
            for (i, r) in c.rules.iter().enumerate() {
                self.add_rule(t, c, i, r)?;
            }
        }
        Ok(())
    }

    fn add_chain(&mut self, t: &Table, c: &Chain) -> Result<()> {
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
        })
    }

    fn add_rule(&mut self, t: &Table, c: &Chain, index: usize, r: &Rule) -> Result<()> {
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
        )
    }

    /// Wraps the messages in `BATCH_BEGIN`/`BATCH_END`.
    pub(super) fn encode(&mut self) -> Result<Vec<u8>> {
        let begin_seq = self.seq.wrapping_sub(self.msgs.len() as u32);
        let mut out = Msg::new(
            NFNL_MSG_BATCH_BEGIN,
            NLM_F_REQUEST,
            begin_seq,
            0,
            NFNL_SUBSYS_NFTABLES,
        )
        .finish()?;
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
            .finish()?,
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nft::{Chain, ChainType, Hook, priority};

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
        b.delete_table(t.family, &t.name).unwrap();
        b.add_table(&t).unwrap();
        let msgs = b.msgs.len();
        assert_eq!(msgs, 5);
        let out = b.encode().unwrap();
        assert_eq!(u16::from_ne_bytes([out[4], out[5]]), NFNL_MSG_BATCH_BEGIN);
        assert_eq!(u32::from_ne_bytes(out[8..12].try_into().unwrap()), 100);
        let tail = &out[out.len() - 20..];
        assert_eq!(u16::from_ne_bytes([tail[4], tail[5]]), NFNL_MSG_BATCH_END);
    }
}
