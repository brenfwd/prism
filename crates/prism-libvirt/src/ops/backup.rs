//! Backup jobs and host CPU baselining.

use crate::Error;
use crate::client::Client;
use crate::proto::{Domain, procs};
use crate::xdr::Decoder;

impl Client {
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

    /// `virConnectBaselineHypervisorCPU`: a CPU model every host in `cpus`
    /// (CPU definition XML, one per host) can run. The optional arguments
    /// narrow the emulator, architecture, machine type and virtualisation type.
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
                e.u32(u32::try_from(cpus.len()).unwrap_or(u32::MAX));
                for c in cpus {
                    e.string(c);
                }
                e.u32(flags);
            })
            .await?;
        Decoder::new(&r).string()
    }
}
