//! The transfer step of a live migration and the source-side recovery after
//! a failed one.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prism_libvirt::{Params, TypedParam};

use super::migrate::PrepareResp;
use super::{Node, VmState, domain_ref};
use crate::spec::VmSpec;

/// Maximum pause at switch-over.
const MAX_DOWNTIME_MS: u64 = 300;

impl Transfer {
    pub(super) fn failed(error: String) -> Self {
        Self {
            finish_cookie: Vec::new(),
            error: Some(error),
        }
    }
}

/// The outcome of the RAM and disk transfer.
pub(super) struct Transfer {
    /// Cookie from `Perform3Params`, passed to Finish; empty on failure.
    pub(super) finish_cookie: Vec<u8>,
    /// Why the transfer failed or was cancelled, if it did.
    pub(super) error: Option<String>,
}

impl Node {
    /// Runs `Perform3Params`, watching the migration job. A failed, stalled
    /// or cancelled transfer is reported in the result rather than as an
    /// error, because Finish and Confirm must still run to clean up. In the
    /// stalled and cancelled cases the job on the source is aborted first.
    pub(super) async fn perform_transfer(
        &self,
        c: &prism_libvirt::Client,
        dom: &prism_libvirt::Domain,
        params: &[TypedParam],
        prep: &PrepareResp,
        flags: u32,
        ctx: &crate::tasks::TaskCtx,
    ) -> Transfer {
        if let Err(e) = c.migrate_set_max_downtime(dom, MAX_DOWNTIME_MS).await {
            tracing::warn!("could not set the maximum migration downtime: {e}");
        }
        let perform_cookie = match B64.decode(&prep.cookie) {
            Ok(b) => b,
            Err(e) => return Transfer::failed(format!("bad cookie from the destination: {e}")),
        };
        let report = |stats: &[TypedParam]| {
            if let Some(f) = super::job_watch::progress_fraction(stats) {
                let iter = stats.u64("memory_iteration").unwrap_or(0);
                ctx.progress(Some(f), format!("migrating (pass {iter})"));
            }
        };
        let call = c.migrate_perform3(dom, params, &perform_cookie, flags);
        match super::job_watch::watch_job(c, dom, ctx, self.cfg.job_stall_timeout(), report, call)
            .await
        {
            super::job_watch::Watched::Done(Ok(finish_cookie)) => Transfer {
                finish_cookie,
                error: None,
            },
            super::job_watch::Watched::Done(Err(e)) => {
                tracing::warn!(vm = %dom.name, "perform failed: {e}");
                // The call may have failed on a lost connection while the
                // job runs on; make sure nothing keeps going.
                if let Err(ae) = c.abort_job(dom).await
                    && !crate::node::not_running(&ae)
                {
                    tracing::debug!(vm = %dom.name, "no migration job to abort: {ae}");
                }
                Transfer::failed(e.to_string())
            }
            super::job_watch::Watched::Stopped(why) => {
                tracing::warn!(vm = %dom.name, "migration stopped: {}", why.describe());
                Transfer::failed(why.describe())
            }
        }
    }

    /// Resumes the source domain after a failed migration if it is paused,
    /// which a migration that fails late can leave behind.
    pub(super) async fn resume_source(&self, spec: &VmSpec) {
        let Ok(c) = self.client() else { return };
        let dom = domain_ref(spec);
        let paused = c
            .get_info(&dom)
            .await
            .map(|i| VmState::from(i.state) == VmState::Paused);
        if paused.is_ok_and(|p| p) {
            match c.resume(&dom).await {
                Ok(()) => {
                    tracing::info!(vm = %spec.name, "resumed the source after a failed migration");
                }
                Err(e) => tracing::error!(vm = %spec.name, "could not resume the source: {e}"),
            }
            self.refresh(spec).await;
        }
    }
}
