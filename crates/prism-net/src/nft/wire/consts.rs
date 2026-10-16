//! nfnetlink, `nf_tables` and netlink constants from the kernel UAPI headers.

pub(super) const NFNL_SUBSYS_NFTABLES: u16 = 10;
pub(super) const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
pub(super) const NFNL_MSG_BATCH_END: u16 = 0x11;

pub(super) const NFT_MSG_NEWTABLE: u16 = 0;
pub(super) const NFT_MSG_DELTABLE: u16 = 2;
pub(super) const NFT_MSG_NEWCHAIN: u16 = 3;
pub(super) const NFT_MSG_NEWRULE: u16 = 6;

pub(super) const NLM_F_REQUEST: u16 = 0x1;
pub(super) const NLM_F_ACK: u16 = 0x4;
pub(super) const NLM_F_APPEND: u16 = 0x800;
pub(super) const NLM_F_CREATE: u16 = 0x400;
pub(super) const NLM_F_CAPPED: u16 = 0x100;
pub(super) const NLM_F_ACK_TLVS: u16 = 0x200;
pub(super) const NLMSG_ERROR: u16 = 2;
pub(super) const NLMSG_DONE: u16 = 3;
pub(super) const NLMSGERR_ATTR_MSG: u16 = 1;
pub(super) const NLA_F_NESTED: u16 = 0x8000;

pub(super) const NFTA_TABLE_NAME: u16 = 1;
pub(super) const NFTA_CHAIN_TABLE: u16 = 1;
pub(super) const NFTA_CHAIN_NAME: u16 = 3;
pub(super) const NFTA_CHAIN_HOOK: u16 = 4;
pub(super) const NFTA_CHAIN_POLICY: u16 = 5;
pub(super) const NFTA_CHAIN_TYPE: u16 = 7;
pub(super) const NFTA_HOOK_HOOKNUM: u16 = 1;
pub(super) const NFTA_HOOK_PRIORITY: u16 = 2;
pub(super) const NFTA_RULE_TABLE: u16 = 1;
pub(super) const NFTA_RULE_CHAIN: u16 = 2;
pub(super) const NFTA_RULE_EXPRESSIONS: u16 = 4;
pub(super) const NFTA_LIST_ELEM: u16 = 1;
pub(super) const NFTA_EXPR_NAME: u16 = 1;
pub(super) const NFTA_EXPR_DATA: u16 = 2;
pub(super) const NFTA_DATA_VALUE: u16 = 1;
pub(super) const NFTA_DATA_VERDICT: u16 = 2;
pub(super) const NFTA_VERDICT_CODE: u16 = 1;
pub(super) const NFTA_VERDICT_CHAIN: u16 = 2;

pub(super) const NFT_REG_VERDICT: u32 = 0;
pub(super) const NFT_REG_1: u32 = 1;

pub(super) const NF_DROP: i32 = 0;
pub(super) const NF_ACCEPT: i32 = 1;
pub(super) const NFT_JUMP: i32 = -3;
pub(super) const NFT_GOTO: i32 = -4;
pub(super) const NFT_RETURN: i32 = -5;
