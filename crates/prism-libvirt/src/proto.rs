//! Constants and types from `remote_protocol.x`, `virnetprotocol.x` and
//! `virkeepaliveprotocol.x` (vendored under `proto/`).
//!
//! Only the procedures Prism uses are covered; they are hand-written here
//! until `prism-xdrgen` generates the full set.

use crate::Error;
use crate::xdr::{Decoder, Encoder};

pub const REMOTE_PROGRAM: u32 = 0x2000_8086;
pub const REMOTE_PROTOCOL_VERSION: u32 = 1;
pub const KEEPALIVE_PROGRAM: u32 = 0x6b65_6570;
pub const KEEPALIVE_PROTOCOL_VERSION: u32 = 1;

pub const MESSAGE_MAX: usize = 33_554_432;
pub const HEADER_LEN: usize = 24;
pub const DOMAIN_LIST_MAX: usize = 16_384;
pub const MEMORY_STATS_MAX: usize = 1024;
pub const UUID_LEN: usize = 16;

/// `virNetMessageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MessageType {
    Call = 0,
    Reply = 1,
    Message = 2,
    Stream = 3,
    CallWithFds = 4,
    ReplyWithFds = 5,
    StreamHole = 6,
}

impl TryFrom<u32> for MessageType {
    type Error = Error;
    fn try_from(v: u32) -> Result<Self, Error> {
        Ok(match v {
            0 => Self::Call,
            1 => Self::Reply,
            2 => Self::Message,
            3 => Self::Stream,
            4 => Self::CallWithFds,
            5 => Self::ReplyWithFds,
            6 => Self::StreamHole,
            _ => return Err(Error::Decode(format!("bad message type {v}"))),
        })
    }
}

/// `virNetMessageStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Status {
    Ok = 0,
    Error = 1,
    Continue = 2,
}

impl TryFrom<u32> for Status {
    type Error = Error;
    fn try_from(v: u32) -> Result<Self, Error> {
        Ok(match v {
            0 => Self::Ok,
            1 => Self::Error,
            2 => Self::Continue,
            _ => return Err(Error::Decode(format!("bad message status {v}"))),
        })
    }
}

/// `virNetMessageHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub prog: u32,
    pub vers: u32,
    pub proc: u32,
    pub ty: MessageType,
    pub serial: u32,
    pub status: Status,
}

impl Header {
    pub fn encode(&self, e: &mut Encoder) {
        e.u32(self.prog)
            .u32(self.vers)
            .u32(self.proc)
            .u32(self.ty as u32)
            .u32(self.serial)
            .u32(self.status as u32);
    }

    pub fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok(Self {
            prog: d.u32()?,
            vers: d.u32()?,
            proc: d.u32()?,
            ty: d.u32()?.try_into()?,
            serial: d.u32()?,
            status: d.u32()?.try_into()?,
        })
    }
}

/// Remote procedure numbers used by Prism.
pub mod procs {
    pub const CONNECT_OPEN: u32 = 1;
    pub const CONNECT_CLOSE: u32 = 2;
    pub const NODE_GET_INFO: u32 = 6;
    pub const CONNECT_GET_CAPABILITIES: u32 = 7;
    pub const DOMAIN_GET_XML_DESC: u32 = 14;
    pub const DOMAIN_LOOKUP_BY_UUID: u32 = 24;
    pub const DOMAIN_RESUME: u32 = 28;
    pub const DOMAIN_SHUTDOWN: u32 = 33;
    pub const DOMAIN_SUSPEND: u32 = 34;
    pub const DOMAIN_REBOOT: u32 = 27;
    pub const CONNECT_GET_HOSTNAME: u32 = 59;
    pub const NODE_GET_FREE_MEMORY: u32 = 102;
    pub const CONNECT_GET_LIB_VERSION: u32 = 157;
    pub const DOMAIN_MEMORY_STATS: u32 = 159;
    pub const DOMAIN_CREATE_WITH_FLAGS: u32 = 196;
    pub const DOMAIN_OPEN_CONSOLE: u32 = 201;
    pub const DOMAIN_SET_MEMORY_FLAGS: u32 = 204;
    pub const DOMAIN_GET_STATE: u32 = 212;
    pub const DOMAIN_UNDEFINE_FLAGS: u32 = 231;
    pub const DOMAIN_DESTROY_FLAGS: u32 = 234;
    pub const DOMAIN_RESET: u32 = 245;
    pub const CONNECT_LIST_ALL_DOMAINS: u32 = 273;
    pub const CONNECT_DOMAIN_EVENT_CALLBACK_REGISTER_ANY: u32 = 316;
    pub const DOMAIN_EVENT_CALLBACK_LIFECYCLE: u32 = 318;
    pub const DOMAIN_DEFINE_XML_FLAGS: u32 = 350;
}

pub const KEEPALIVE_PROC_PING: u32 = 1;
pub const KEEPALIVE_PROC_PONG: u32 = 2;

/// `remote_nonnull_domain`: a reference to a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    pub name: String,
    pub uuid: [u8; UUID_LEN],
    /// Runtime ID, or -1 when the domain is not running.
    pub id: i32,
}

impl Domain {
    pub fn encode(&self, e: &mut Encoder) {
        e.string(&self.name).opaque_fixed(&self.uuid).i32(self.id);
    }

    pub fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        let name = d.string()?;
        let mut uuid = [0; UUID_LEN];
        uuid.copy_from_slice(d.opaque_fixed(UUID_LEN)?);
        Ok(Self {
            name,
            uuid,
            id: d.i32()?,
        })
    }
}

/// `virDomainState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainState {
    NoState,
    Running,
    Blocked,
    Paused,
    Shutdown,
    Shutoff,
    Crashed,
    PmSuspended,
    Unknown(i32),
}

impl From<i32> for DomainState {
    fn from(v: i32) -> Self {
        match v {
            0 => Self::NoState,
            1 => Self::Running,
            2 => Self::Blocked,
            3 => Self::Paused,
            4 => Self::Shutdown,
            5 => Self::Shutoff,
            6 => Self::Crashed,
            7 => Self::PmSuspended,
            v => Self::Unknown(v),
        }
    }
}

/// `remote_domain_get_info_ret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainInfo {
    pub state: DomainState,
    pub max_mem_kib: u64,
    pub memory_kib: u64,
    pub nr_virt_cpu: u16,
    pub cpu_time_ns: u64,
}

/// `remote_node_get_info_ret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub model: String,
    pub memory_kib: u64,
    pub cpus: i32,
    pub mhz: i32,
    pub nodes: i32,
    pub sockets: i32,
    pub cores: i32,
    pub threads: i32,
}

impl NodeInfo {
    pub fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        // `char model[32]` is an XDR vector of chars, each a 4-byte unit.
        let mut model = Vec::with_capacity(32);
        for _ in 0..32 {
            model.push(d.u32()? as u8);
        }
        let end = model.iter().position(|&b| b == 0).unwrap_or(model.len());
        Ok(Self {
            model: String::from_utf8_lossy(&model[..end]).into_owned(),
            memory_kib: d.u64()?,
            cpus: d.i32()?,
            mhz: d.i32()?,
            nodes: d.i32()?,
            sockets: d.i32()?,
            cores: d.i32()?,
            threads: d.i32()?,
        })
    }
}

/// `virDomainEventType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    Defined,
    Undefined,
    Started,
    Suspended,
    Resumed,
    Stopped,
    Shutdown,
    PmSuspended,
    Crashed,
    Unknown(i32),
}

impl From<i32> for LifecycleEvent {
    fn from(v: i32) -> Self {
        match v {
            0 => Self::Defined,
            1 => Self::Undefined,
            2 => Self::Started,
            3 => Self::Suspended,
            4 => Self::Resumed,
            5 => Self::Stopped,
            6 => Self::Shutdown,
            7 => Self::PmSuspended,
            8 => Self::Crashed,
            v => Self::Unknown(v),
        }
    }
}

/// `virNetMessageError`, as returned in a reply with status `Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteError {
    pub code: i32,
    pub domain: i32,
    pub message: Option<String>,
    pub level: i32,
}

impl RemoteError {
    pub fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        // Trailing fields (str1..3, int1..2, dom, net) are not needed.
        Ok(Self {
            code: d.i32()?,
            domain: d.i32()?,
            message: d.opt_string()?,
            level: d.i32()?,
        })
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (code {}, domain {})",
            self.message.as_deref().unwrap_or("unknown libvirt error"),
            self.code,
            self.domain
        )
    }
}

/// libvirt error codes Prism reacts to.
pub mod error_codes {
    pub const OPERATION_INVALID: i32 = 55;
    pub const NO_DOMAIN: i32 = 42;
}

/// Flag values used by Prism.
pub mod flags {
    pub const DOMAIN_XML_SECURE: u32 = 1;
    pub const DOMAIN_XML_INACTIVE: u32 = 2;
    pub const DOMAIN_DEFINE_VALIDATE: u32 = 1;
    pub const DOMAIN_UNDEFINE_MANAGED_SAVE: u32 = 1;
    pub const DOMAIN_UNDEFINE_SNAPSHOTS_METADATA: u32 = 2;
    pub const DOMAIN_UNDEFINE_NVRAM: u32 = 4;
    pub const DOMAIN_UNDEFINE_CHECKPOINTS_METADATA: u32 = 16;
    pub const DOMAIN_AFFECT_LIVE: u32 = 1;
    pub const DOMAIN_AFFECT_CONFIG: u32 = 2;
    pub const DOMAIN_CONSOLE_FORCE: u32 = 1;
    pub const DOMAIN_CONSOLE_SAFE: u32 = 2;
    pub const DOMAIN_EVENT_ID_LIFECYCLE: i32 = 0;
    pub const DOMAIN_MEMORY_STAT_NR: u32 = 13;
}

/// `virDomainMemoryStatTags`.
pub mod memory_stat {
    pub const UNUSED: i32 = 4;
    pub const AVAILABLE: i32 = 5;
    pub const ACTUAL_BALLOON: i32 = 6;
    pub const RSS: i32 = 7;
    pub const USABLE: i32 = 8;
    pub const DISK_CACHES: i32 = 10;
}
