//! Device, block, snapshot, checkpoint, backup and job procedures.

use crate::Error;
use crate::client::Client;
use crate::proto::{Domain, procs};
use crate::typed::{TYPED_PARAMS_MAX, TypedParam, decode_params, encode_params};
use crate::xdr::{Decoder, Encoder};

const LIST_MAX: usize = 16_384;

/// `remote_nonnull_domain_snapshot` / `remote_nonnull_domain_checkpoint`:
/// a named object belonging to a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainChild {
    pub name: String,
    pub domain: Domain,
}

impl DomainChild {
    fn encode(&self, e: &mut Encoder) {
        e.string(&self.name);
        self.domain.encode(e);
    }

    fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok(Self {
            name: d.string()?,
            domain: Domain::decode(d)?,
        })
    }
}

/// `remote_domain_get_block_job_info_ret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockJobInfo {
    pub job_type: i32,
    pub bandwidth: u64,
    pub cur: u64,
    pub end: u64,
}

/// `remote_domain_get_block_info_ret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockInfo {
    pub allocation: u64,
    pub capacity: u64,
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
    pub ipv6: bool,
    pub addr: String,
    pub prefix: u32,
}

/// A guest network interface with its addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInterface {
    pub name: String,
    pub hwaddr: Option<String>,
    pub addrs: Vec<InterfaceAddr>,
}

/// Flag values for the procedures in this module.
pub mod flags {
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

impl Client {
    async fn dom_xml_flags(
        &self,
        proc: u32,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<Vec<u8>, Error> {
        self.call_with(proc, |e| {
            dom.encode(e);
            e.string(xml).u32(flags);
        })
        .await
    }

    /// Hot-plugs or persistently adds a device.
    pub async fn attach_device(&self, dom: &Domain, xml: &str, flags: u32) -> Result<(), Error> {
        self.dom_xml_flags(procs::DOMAIN_ATTACH_DEVICE_FLAGS, dom, xml, flags)
            .await
            .map(drop)
    }

    /// Requests device removal. Live removal completes asynchronously,
    /// signalled by a `DeviceRemoved` event.
    pub async fn detach_device(&self, dom: &Domain, xml: &str, flags: u32) -> Result<(), Error> {
        self.dom_xml_flags(procs::DOMAIN_DETACH_DEVICE_FLAGS, dom, xml, flags)
            .await
            .map(drop)
    }

    /// The guest's interfaces and addresses from `source`.
    pub async fn interface_addresses(
        &self,
        dom: &Domain,
        source: AddressSource,
    ) -> Result<Vec<GuestInterface>, Error> {
        let r = self
            .call_with(procs::DOMAIN_INTERFACE_ADDRESSES, |e| {
                dom.encode(e);
                e.u32(source as u32).u32(0);
            })
            .await?;
        let mut d = Decoder::new(&r);
        d.array(256, |d| {
            Ok(GuestInterface {
                name: d.string()?,
                hwaddr: d.opt_string()?,
                addrs: d.array(2048, |d| {
                    Ok(InterfaceAddr {
                        ipv6: d.i32()? == 1,
                        addr: d.string()?,
                        prefix: d.u32()?,
                    })
                })?,
            })
        })
    }

    /// Changes a device in place, for example CD-ROM media.
    pub async fn update_device(&self, dom: &Domain, xml: &str, flags: u32) -> Result<(), Error> {
        self.dom_xml_flags(procs::DOMAIN_UPDATE_DEVICE_FLAGS, dom, xml, flags)
            .await
            .map(drop)
    }

    pub async fn set_vcpus(&self, dom: &Domain, n: u32, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_SET_VCPUS_FLAGS, |e| {
            dom.encode(e);
            e.u32(n).u32(flags);
        })
        .await
        .map(drop)
    }

    /// Grows a disk of a running domain; `size` is in bytes with
    /// `BLOCK_RESIZE_BYTES`.
    pub async fn block_resize(
        &self,
        dom: &Domain,
        disk: &str,
        size: u64,
        flags: u32,
    ) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_BLOCK_RESIZE, |e| {
            dom.encode(e);
            e.string(disk).u64(size).u32(flags);
        })
        .await
        .map(drop)
    }

    pub async fn block_info(&self, dom: &Domain, disk: &str) -> Result<BlockInfo, Error> {
        let r = self
            .call_with(procs::DOMAIN_GET_BLOCK_INFO, |e| {
                dom.encode(e);
                e.string(disk).u32(0);
            })
            .await?;
        let mut d = Decoder::new(&r);
        Ok(BlockInfo {
            allocation: d.u64()?,
            capacity: d.u64()?,
            physical: d.u64()?,
        })
    }

    pub async fn block_copy(
        &self,
        dom: &Domain,
        disk: &str,
        dest_xml: &str,
        params: &[TypedParam],
        flags: u32,
    ) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_BLOCK_COPY, |e| {
            dom.encode(e);
            e.string(disk).string(dest_xml);
            encode_params(e, params);
            e.u32(flags);
        })
        .await
        .map(drop)
    }

    pub async fn block_job_abort(&self, dom: &Domain, disk: &str, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_BLOCK_JOB_ABORT, |e| {
            dom.encode(e);
            e.string(disk).u32(flags);
        })
        .await
        .map(drop)
    }

    /// Returns `None` when no block job is active on `disk`.
    pub async fn block_job_info(
        &self,
        dom: &Domain,
        disk: &str,
    ) -> Result<Option<BlockJobInfo>, Error> {
        let r = self
            .call_with(procs::DOMAIN_GET_BLOCK_JOB_INFO, |e| {
                dom.encode(e);
                e.string(disk).u32(0);
            })
            .await?;
        let mut d = Decoder::new(&r);
        if d.i32()? == 0 {
            return Ok(None);
        }
        Ok(Some(BlockJobInfo {
            job_type: d.i32()?,
            bandwidth: d.u64()?,
            cur: d.u64()?,
            end: d.u64()?,
        }))
    }

    pub async fn abort_job(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_ABORT_JOB, dom, None)
            .await
            .map(drop)
    }

    /// Returns `(job type, stats)`; type 0 means no job.
    pub async fn job_stats(
        &self,
        dom: &Domain,
        flags: u32,
    ) -> Result<(i32, Vec<TypedParam>), Error> {
        let r = self
            .call_dom(procs::DOMAIN_GET_JOB_STATS, dom, Some(flags))
            .await?;
        let mut d = Decoder::new(&r);
        Ok((d.i32()?, decode_params(&mut d, TYPED_PARAMS_MAX)?))
    }

    pub async fn fs_freeze(&self, dom: &Domain) -> Result<u32, Error> {
        let r = self
            .call_with(procs::DOMAIN_FSFREEZE, |e| {
                dom.encode(e);
                e.u32(0).u32(0);
            })
            .await?;
        Decoder::new(&r).u32()
    }

    pub async fn fs_thaw(&self, dom: &Domain) -> Result<u32, Error> {
        let r = self
            .call_with(procs::DOMAIN_FSTHAW, |e| {
                dom.encode(e);
                e.u32(0).u32(0);
            })
            .await?;
        Decoder::new(&r).u32()
    }

    pub async fn snapshot_create(
        &self,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        let r = self
            .dom_xml_flags(procs::DOMAIN_SNAPSHOT_CREATE_XML, dom, xml, flags)
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    pub async fn snapshots(&self, dom: &Domain) -> Result<Vec<DomainChild>, Error> {
        let r = self
            .call_with(procs::DOMAIN_LIST_ALL_SNAPSHOTS, |e| {
                dom.encode(e);
                e.i32(1).u32(0);
            })
            .await?;
        Decoder::new(&r).array(LIST_MAX, DomainChild::decode)
    }

    pub async fn snapshot_lookup(&self, dom: &Domain, name: &str) -> Result<DomainChild, Error> {
        let r = self
            .call_with(procs::DOMAIN_SNAPSHOT_LOOKUP_BY_NAME, |e| {
                dom.encode(e);
                e.string(name).u32(0);
            })
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    pub async fn snapshot_xml(&self, snap: &DomainChild, flags: u32) -> Result<String, Error> {
        let r = self
            .call_with(procs::DOMAIN_SNAPSHOT_GET_XML_DESC, |e| {
                snap.encode(e);
                e.u32(flags);
            })
            .await?;
        Decoder::new(&r).string()
    }

    /// Whether `snap` is the domain's current snapshot (the one libvirt
    /// treats as the parent of the next snapshot and as the revert baseline).
    pub async fn snapshot_is_current(&self, snap: &DomainChild) -> Result<bool, Error> {
        let r = self
            .call_with(procs::DOMAIN_SNAPSHOT_IS_CURRENT, |e| {
                snap.encode(e);
                e.u32(0);
            })
            .await?;
        Ok(Decoder::new(&r).i32()? == 1)
    }

    pub async fn snapshot_delete(&self, snap: &DomainChild, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_SNAPSHOT_DELETE, |e| {
            snap.encode(e);
            e.u32(flags);
        })
        .await
        .map(drop)
    }

    pub async fn snapshot_revert(&self, snap: &DomainChild, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_REVERT_TO_SNAPSHOT, |e| {
            snap.encode(e);
            e.u32(flags);
        })
        .await
        .map(drop)
    }

    pub async fn checkpoint_create(
        &self,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        let r = self
            .dom_xml_flags(procs::DOMAIN_CHECKPOINT_CREATE_XML, dom, xml, flags)
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    pub async fn checkpoints(&self, dom: &Domain) -> Result<Vec<DomainChild>, Error> {
        let r = self
            .call_with(procs::DOMAIN_LIST_ALL_CHECKPOINTS, |e| {
                dom.encode(e);
                e.i32(1).u32(0);
            })
            .await?;
        Decoder::new(&r).array(LIST_MAX, DomainChild::decode)
    }

    pub async fn checkpoint_lookup(&self, dom: &Domain, name: &str) -> Result<DomainChild, Error> {
        let r = self
            .call_with(procs::DOMAIN_CHECKPOINT_LOOKUP_BY_NAME, |e| {
                dom.encode(e);
                e.string(name).u32(0);
            })
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    pub async fn checkpoint_xml(&self, cp: &DomainChild, flags: u32) -> Result<String, Error> {
        let r = self
            .call_with(procs::DOMAIN_CHECKPOINT_GET_XML_DESC, |e| {
                cp.encode(e);
                e.u32(flags);
            })
            .await?;
        Decoder::new(&r).string()
    }

    pub async fn checkpoint_delete(&self, cp: &DomainChild, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_CHECKPOINT_DELETE, |e| {
            cp.encode(e);
            e.u32(flags);
        })
        .await
        .map(drop)
    }

    /// Starts a push-mode backup job, optionally creating a checkpoint atomically.
    pub async fn backup_begin(
        &self,
        dom: &Domain,
        backup_xml: &str,
        checkpoint_xml: Option<&str>,
        flags: u32,
    ) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_BACKUP_BEGIN, |e| {
            dom.encode(e);
            e.string(backup_xml).opt_string(checkpoint_xml).u32(flags);
        })
        .await
        .map(drop)
    }

    /// `virConnectBaselineHypervisorCPU`: a CPU model every host can run.
    pub async fn baseline_hypervisor_cpu(
        &self,
        emulator: Option<&str>,
        arch: Option<&str>,
        machine: Option<&str>,
        virttype: Option<&str>,
        cpus: &[String],
        flags: u32,
    ) -> Result<String, Error> {
        let r = self
            .call_with(procs::CONNECT_BASELINE_HYPERVISOR_CPU, |e| {
                e.opt_string(emulator)
                    .opt_string(arch)
                    .opt_string(machine)
                    .opt_string(virttype);
                e.u32(cpus.len() as u32);
                for c in cpus {
                    e.string(c);
                }
                e.u32(flags);
            })
            .await?;
        Decoder::new(&r).string()
    }
}
