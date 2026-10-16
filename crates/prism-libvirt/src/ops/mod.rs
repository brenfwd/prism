//! Device, block, snapshot, checkpoint, backup and job procedures, with the
//! data types and flag values they use.
//!
//! The procedures are methods on [`Client`](crate::Client), grouped by
//! concern in the submodules. Flags are raw `u32` masks; the constants in
//! [`flags`] name the bits.

mod backup;
mod block;
mod device;
mod snapshot;

use crate::Error;
use crate::proto::Domain;
use crate::xdr::{Decoder, Encoder};

/// Most snapshots or checkpoints one list reply may contain.
const LIST_MAX: usize = 16_384;

/// `remote_nonnull_domain_snapshot` / `remote_nonnull_domain_checkpoint`:
/// a named object belonging to a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainChild {
    /// Snapshot or checkpoint name, unique within the domain.
    pub name: String,
    /// The domain it belongs to.
    pub domain: Domain,
}

impl DomainChild {
    pub(super) fn encode(&self, e: &mut Encoder) {
        e.string(&self.name);
        self.domain.encode(e);
    }

    pub(super) fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok(Self {
            name: d.string()?,
            domain: Domain::decode(d)?,
        })
    }
}

/// `remote_domain_get_block_job_info_ret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockJobInfo {
    /// `virDomainBlockJobType`.
    pub job_type: i32,
    /// Bandwidth limit in MiB/s, or 0 for unlimited.
    pub bandwidth: u64,
    /// Progress so far, in bytes.
    pub cur: u64,
    /// Progress at completion, in bytes.
    pub end: u64,
}

/// `remote_domain_get_block_info_ret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockInfo {
    /// Bytes allocated in the image.
    pub allocation: u64,
    /// Virtual size of the disk in bytes.
    pub capacity: u64,
    /// Size of the backing file or device in bytes.
    pub physical: u64,
}

/// Where `interface_addresses` learns addresses from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSource {
    /// The hypervisor's DHCP leases.
    Lease = 0,
    /// The QEMU guest agent.
    Agent = 1,
    /// The host ARP table.
    Arp = 2,
}

/// One address of a guest interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceAddr {
    /// Whether `addr` is IPv6 (otherwise IPv4).
    pub ipv6: bool,
    /// The address in text form.
    pub addr: String,
    /// Prefix length in bits.
    pub prefix: u32,
}

/// A guest network interface with its addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInterface {
    /// Interface name inside the guest, such as `eth0`.
    pub name: String,
    /// MAC address, if known.
    pub hwaddr: Option<String>,
    /// The interface's addresses.
    pub addrs: Vec<InterfaceAddr>,
}

/// Flag values for the procedures in this module.
pub mod flags {
    #![allow(missing_docs)] // Names mirror the `VIR_DOMAIN_*` flag constants.

    pub const AFFECT_LIVE: u32 = 1;
    pub const AFFECT_CONFIG: u32 = 2;

    pub const SNAPSHOT_CREATE_REDEFINE: u32 = 1;
    pub const SNAPSHOT_CREATE_CURRENT: u32 = 2;
    pub const SNAPSHOT_REDEFINE_CURRENT_FLAGS: u32 = 1 | 2;
    pub const SNAPSHOT_CREATE_NO_METADATA: u32 = 4;
    pub const SNAPSHOT_CREATE_DISK_ONLY: u32 = 16;
    pub const SNAPSHOT_CREATE_REUSE_EXT: u32 = 32;
    pub const SNAPSHOT_CREATE_QUIESCE: u32 = 64;
    pub const SNAPSHOT_CREATE_ATOMIC: u32 = 128;
    pub const SNAPSHOT_CREATE_LIVE: u32 = 256;

    pub const SNAPSHOT_DELETE_CHILDREN: u32 = 1;
    pub const SNAPSHOT_DELETE_METADATA_ONLY: u32 = 2;

    pub const SNAPSHOT_REVERT_RUNNING: u32 = 1;
    pub const SNAPSHOT_REVERT_PAUSED: u32 = 2;
    pub const SNAPSHOT_REVERT_FORCE: u32 = 4;

    pub const CHECKPOINT_CREATE_REDEFINE: u32 = 1;
    pub const CHECKPOINT_CREATE_QUIESCE: u32 = 2;
    pub const CHECKPOINT_DELETE_METADATA_ONLY: u32 = 2;

    pub const BACKUP_BEGIN_REUSE_EXTERNAL: u32 = 1;

    pub const BLOCK_JOB_ABORT_PIVOT: u32 = 2;
    pub const BLOCK_COPY_SHALLOW: u32 = 1;
    pub const BLOCK_COPY_REUSE_EXT: u32 = 2;
    pub const BLOCK_COPY_TRANSIENT_JOB: u32 = 4;

    pub const BLOCK_RESIZE_BYTES: u32 = 1;

    pub const JOB_STATS_COMPLETED: u32 = 1;

    pub const SNAPSHOT_XML_SECURE: u32 = 1;
    pub const CHECKPOINT_XML_SIZE: u32 = 4;
}
