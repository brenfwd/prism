//! The managed (non-peer-to-peer) V3 migration protocol with typed
//! parameters: Prism drives Begin/Prepare/Perform/Finish/Confirm across two
//! daemons the way libvirt's client library does.

use crate::Error;
use crate::client::Client;
use crate::proto::Domain;
use crate::typed::{TypedParam, encode_params};
use crate::xdr::{Decoder, Encoder};

const COOKIE_MAX: usize = 4_194_304;

pub mod procs {
    pub const BEGIN3_PARAMS: u32 = 302;
    pub const PREPARE3_PARAMS: u32 = 303;
    pub const PERFORM3_PARAMS: u32 = 305;
    pub const FINISH3_PARAMS: u32 = 306;
    pub const CONFIRM3_PARAMS: u32 = 307;
    pub const SET_MAX_DOWNTIME: u32 = 166;
    pub const START_POST_COPY: u32 = 364;
}

/// `virDomainMigrateFlags`.
pub mod flags {
    pub const LIVE: u32 = 1;
    pub const PERSIST_DEST: u32 = 8;
    pub const UNDEFINE_SOURCE: u32 = 16;
    pub const PAUSED: u32 = 32;
    pub const NON_SHARED_DISK: u32 = 64;
    pub const NON_SHARED_INC: u32 = 128;
    pub const CHANGE_PROTECTION: u32 = 256;
    pub const UNSAFE: u32 = 512;
    pub const OFFLINE: u32 = 1024;
    pub const ABORT_ON_ERROR: u32 = 4096;
    pub const AUTO_CONVERGE: u32 = 8192;
    pub const POSTCOPY: u32 = 32768;
}

/// `VIR_MIGRATE_PARAM_*` names.
pub mod params {
    pub const URI: &str = "migrate_uri";
    pub const DEST_NAME: &str = "destination_name";
    pub const DEST_XML: &str = "destination_xml";
    pub const PERSIST_XML: &str = "persistent_xml";
    pub const BANDWIDTH: &str = "bandwidth";
    pub const MIGRATE_DISKS: &str = "migrate_disks";
    pub const DISKS_URI: &str = "disks_uri";
}

fn cookie(e: &mut Encoder, c: &[u8]) {
    e.opaque(c);
}

impl Client {
    /// Source: `Begin3Params`. Returns `(cookie, domain XML)`.
    pub async fn migrate_begin3(
        &self,
        dom: &Domain,
        p: &[TypedParam],
        flags: u32,
    ) -> Result<(Vec<u8>, String), Error> {
        let r = self
            .call_with(procs::BEGIN3_PARAMS, |e| {
                dom.encode(e);
                encode_params(e, p);
                e.u32(flags);
            })
            .await?;
        let mut d = Decoder::new(&r);
        Ok((d.opaque(COOKIE_MAX)?.to_vec(), d.string()?))
    }

    /// Destination: `Prepare3Params`. Returns `(cookie, uri)`.
    pub async fn migrate_prepare3(
        &self,
        p: &[TypedParam],
        cookie_in: &[u8],
        flags: u32,
    ) -> Result<(Vec<u8>, Option<String>), Error> {
        let r = self
            .call_with(procs::PREPARE3_PARAMS, |e| {
                encode_params(e, p);
                cookie(e, cookie_in);
                e.u32(flags);
            })
            .await?;
        let mut d = Decoder::new(&r);
        Ok((d.opaque(COOKIE_MAX)?.to_vec(), d.opt_string()?))
    }

    /// Source: `Perform3Params`; runs the whole transfer.
    pub async fn migrate_perform3(
        &self,
        dom: &Domain,
        p: &[TypedParam],
        cookie_in: &[u8],
        flags: u32,
    ) -> Result<Vec<u8>, Error> {
        let r = self
            .call_with(procs::PERFORM3_PARAMS, |e| {
                dom.encode(e);
                e.opt_string(None);
                encode_params(e, p);
                cookie(e, cookie_in);
                e.u32(flags);
            })
            .await?;
        Ok(Decoder::new(&r).opaque(COOKIE_MAX)?.to_vec())
    }

    /// Destination: `Finish3Params`. On success the domain runs there.
    pub async fn migrate_finish3(
        &self,
        p: &[TypedParam],
        cookie_in: &[u8],
        flags: u32,
        cancelled: bool,
    ) -> Result<(Domain, Vec<u8>), Error> {
        let r = self
            .call_with(procs::FINISH3_PARAMS, |e| {
                encode_params(e, p);
                cookie(e, cookie_in);
                e.u32(flags).i32(i32::from(cancelled));
            })
            .await?;
        let mut d = Decoder::new(&r);
        Ok((Domain::decode(&mut d)?, d.opaque(COOKIE_MAX)?.to_vec()))
    }

    /// Source: `Confirm3Params`; kills the source on success, resumes it
    /// when `cancelled`.
    pub async fn migrate_confirm3(
        &self,
        dom: &Domain,
        p: &[TypedParam],
        cookie_in: &[u8],
        flags: u32,
        cancelled: bool,
    ) -> Result<(), Error> {
        self.call_with(procs::CONFIRM3_PARAMS, |e| {
            dom.encode(e);
            encode_params(e, p);
            cookie(e, cookie_in);
            e.u32(flags).i32(i32::from(cancelled));
        })
        .await
        .map(drop)
    }

    /// Maximum tolerated downtime at switch-over, in milliseconds.
    pub async fn migrate_set_max_downtime(&self, dom: &Domain, ms: u64) -> Result<(), Error> {
        self.call_with(procs::SET_MAX_DOWNTIME, |e| {
            dom.encode(e);
            e.u64(ms).u32(0);
        })
        .await
        .map(drop)
    }

    pub async fn migrate_start_post_copy(&self, dom: &Domain) -> Result<(), Error> {
        self.call_with(procs::START_POST_COPY, |e| {
            dom.encode(e);
            e.u32(0);
        })
        .await
        .map(drop)
    }
}
