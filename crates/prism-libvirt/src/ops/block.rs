//! Block devices and domain jobs: resize, copy, block jobs and job
//! statistics.

use super::{BlockInfo, BlockJobInfo};
use crate::Error;
use crate::client::Client;
use crate::proto::{Domain, procs};
use crate::typed::{TYPED_PARAMS_MAX, TypedParam, decode_params, encode_params};
use crate::xdr::Decoder;

impl Client {
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

    /// `virDomainGetBlockInfo` for the disk with target name `disk`.
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

    /// `virDomainBlockCopy`: starts copying `disk` to the destination
    /// described by `dest_xml`. `flags` are `BLOCK_COPY_*` values.
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

    /// `virDomainBlockJobAbort`: cancels the block job on `disk`, or pivots
    /// to the copy with `BLOCK_JOB_ABORT_PIVOT`.
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

    /// `virDomainAbortJob`: cancels the domain's current job, such as a
    /// migration or a backup.
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
}
