//! Snapshots and checkpoints. Both are named children of a domain and use
//! the same call shapes, so one set of helpers serves both.

use super::{DomainChild, LIST_MAX};
use crate::Error;
use crate::client::Client;
use crate::proto::{Domain, procs};
use crate::xdr::{Decoder, Encoder};

impl Client {
    async fn child_create(
        &self,
        proc: u32,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        let r = self.dom_xml_call(proc, dom, xml, flags).await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    async fn child_list(&self, proc: u32, dom: &Domain) -> Result<Vec<DomainChild>, Error> {
        let r = self
            .call_with(proc, |e| {
                dom.encode(e);
                e.i32(1).u32(0);
            })
            .await?;
        Decoder::new(&r).array(LIST_MAX, DomainChild::decode)
    }

    async fn child_lookup(
        &self,
        proc: u32,
        dom: &Domain,
        name: &str,
    ) -> Result<DomainChild, Error> {
        let r = self
            .call_with(proc, |e| {
                dom.encode(e);
                e.string(name).u32(0);
            })
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    async fn child_xml(&self, proc: u32, child: &DomainChild, flags: u32) -> Result<String, Error> {
        let r = self
            .call_with(proc, |e| {
                child.encode(e);
                e.u32(flags);
            })
            .await?;
        Decoder::new(&r).string()
    }

    async fn child_flagged(&self, proc: u32, child: &DomainChild, flags: u32) -> Result<(), Error> {
        self.call_with(proc, |e| {
            child.encode(e);
            e.u32(flags);
        })
        .await
        .map(drop)
    }

    /// `virDomainSnapshotCreateXML`; `flags` are `SNAPSHOT_CREATE_*` values.
    pub async fn snapshot_create(
        &self,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        self.child_create(procs::DOMAIN_SNAPSHOT_CREATE_XML, dom, xml, flags)
            .await
    }

    /// Like [`Self::snapshot_create`], with no call timeout. A snapshot that
    /// saves memory or quiesces a busy guest can take as long as the disks
    /// need, so the caller bounds it: it watches the domain job, calls
    /// `abort_job` to stop it, or drops the future.
    pub async fn snapshot_create_unbounded(
        &self,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        let mut e = Encoder::new();
        dom.encode(&mut e);
        e.string(xml).u32(flags);
        let r = self
            .call_unbounded(procs::DOMAIN_SNAPSHOT_CREATE_XML, e.into_inner())
            .await?;
        DomainChild::decode(&mut Decoder::new(&r))
    }

    /// `virDomainListAllSnapshots`: every snapshot of the domain.
    pub async fn snapshots(&self, dom: &Domain) -> Result<Vec<DomainChild>, Error> {
        self.child_list(procs::DOMAIN_LIST_ALL_SNAPSHOTS, dom).await
    }

    /// `virDomainSnapshotLookupByName`.
    pub async fn snapshot_lookup(&self, dom: &Domain, name: &str) -> Result<DomainChild, Error> {
        self.child_lookup(procs::DOMAIN_SNAPSHOT_LOOKUP_BY_NAME, dom, name)
            .await
    }

    /// `virDomainSnapshotGetXMLDesc`; `SNAPSHOT_XML_SECURE` includes secrets.
    pub async fn snapshot_xml(&self, snap: &DomainChild, flags: u32) -> Result<String, Error> {
        self.child_xml(procs::DOMAIN_SNAPSHOT_GET_XML_DESC, snap, flags)
            .await
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

    /// `virDomainSnapshotDelete`; `flags` are `SNAPSHOT_DELETE_*` values.
    pub async fn snapshot_delete(&self, snap: &DomainChild, flags: u32) -> Result<(), Error> {
        self.child_flagged(procs::DOMAIN_SNAPSHOT_DELETE, snap, flags)
            .await
    }

    /// `virDomainRevertToSnapshot`; `flags` are `SNAPSHOT_REVERT_*` values.
    pub async fn snapshot_revert(&self, snap: &DomainChild, flags: u32) -> Result<(), Error> {
        self.child_flagged(procs::DOMAIN_REVERT_TO_SNAPSHOT, snap, flags)
            .await
    }

    /// Like [`Self::snapshot_revert`], with no call timeout. Restoring saved
    /// memory of a large guest can take minutes; the caller bounds it as for
    /// [`Self::snapshot_create_unbounded`].
    pub async fn snapshot_revert_unbounded(
        &self,
        snap: &DomainChild,
        flags: u32,
    ) -> Result<(), Error> {
        let mut e = Encoder::new();
        snap.encode(&mut e);
        e.u32(flags);
        self.call_unbounded(procs::DOMAIN_REVERT_TO_SNAPSHOT, e.into_inner())
            .await
            .map(drop)
    }

    /// `virDomainCheckpointCreateXML`; `flags` are `CHECKPOINT_CREATE_*`
    /// values.
    pub async fn checkpoint_create(
        &self,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<DomainChild, Error> {
        self.child_create(procs::DOMAIN_CHECKPOINT_CREATE_XML, dom, xml, flags)
            .await
    }

    /// `virDomainListAllCheckpoints`: every checkpoint of the domain.
    pub async fn checkpoints(&self, dom: &Domain) -> Result<Vec<DomainChild>, Error> {
        self.child_list(procs::DOMAIN_LIST_ALL_CHECKPOINTS, dom)
            .await
    }

    /// `virDomainCheckpointLookupByName`.
    pub async fn checkpoint_lookup(&self, dom: &Domain, name: &str) -> Result<DomainChild, Error> {
        self.child_lookup(procs::DOMAIN_CHECKPOINT_LOOKUP_BY_NAME, dom, name)
            .await
    }

    /// `virDomainCheckpointGetXMLDesc`; `CHECKPOINT_XML_SIZE` adds disk sizes.
    pub async fn checkpoint_xml(&self, cp: &DomainChild, flags: u32) -> Result<String, Error> {
        self.child_xml(procs::DOMAIN_CHECKPOINT_GET_XML_DESC, cp, flags)
            .await
    }

    /// `virDomainCheckpointDelete`; `CHECKPOINT_DELETE_METADATA_ONLY` keeps
    /// the bitmaps.
    pub async fn checkpoint_delete(&self, cp: &DomainChild, flags: u32) -> Result<(), Error> {
        self.child_flagged(procs::DOMAIN_CHECKPOINT_DELETE, cp, flags)
            .await
    }
}
