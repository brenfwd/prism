//! Host and domain lifecycle procedures: version and host info, listing,
//! defining, starting, stopping and inspecting domains, event registration.

use crate::Error;
use crate::client::Client;
use crate::proto::{
    self, Domain, DomainInfo, DomainState, MemoryStat, NodeInfo, StateInfo, flags, procs,
};
use crate::xdr::Decoder;

impl Client {
    /// `virConnectGetLibVersion`, encoded as `major * 1_000_000 + minor * 1000 + micro`.
    pub async fn lib_version(&self) -> Result<u64, Error> {
        let r = self
            .call(procs::CONNECT_GET_LIB_VERSION, Vec::new())
            .await?;
        Decoder::new(&r).u64()
    }

    /// `virConnectGetHostname`.
    pub async fn hostname(&self) -> Result<String, Error> {
        let r = self.call(procs::CONNECT_GET_HOSTNAME, Vec::new()).await?;
        Decoder::new(&r).string()
    }

    /// `virConnectGetCapabilities`: the host and hypervisor capabilities XML.
    pub async fn capabilities(&self) -> Result<String, Error> {
        let r = self
            .call(procs::CONNECT_GET_CAPABILITIES, Vec::new())
            .await?;
        Decoder::new(&r).string()
    }

    /// `virNodeGetInfo`: CPU model, count and memory of the host.
    pub async fn node_info(&self) -> Result<NodeInfo, Error> {
        let r = self.call(procs::NODE_GET_INFO, Vec::new()).await?;
        NodeInfo::decode(&mut Decoder::new(&r))
    }

    /// Free host memory in bytes.
    pub async fn free_memory(&self) -> Result<u64, Error> {
        let r = self.call(procs::NODE_GET_FREE_MEMORY, Vec::new()).await?;
        Decoder::new(&r).u64()
    }

    /// `virConnectListAllDomains`; `flags == 0` lists every domain.
    pub async fn list_all_domains(&self, flags: u32) -> Result<Vec<Domain>, Error> {
        let r = self
            .call_with(procs::CONNECT_LIST_ALL_DOMAINS, |e| {
                e.i32(1).u32(flags);
            })
            .await?;
        let mut d = Decoder::new(&r);
        d.array(proto::DOMAIN_LIST_MAX, Domain::decode)
    }

    /// `virDomainLookupByUUID`. Fails with `NO_DOMAIN` when no domain has
    /// that UUID.
    pub async fn lookup_by_uuid(&self, uuid: [u8; 16]) -> Result<Domain, Error> {
        let r = self
            .call_with(procs::DOMAIN_LOOKUP_BY_UUID, |e| {
                e.opaque_fixed(&uuid);
            })
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    /// `virDomainDefineXMLFlags`: defines or updates a persistent domain from
    /// its XML. `flags` are `DOMAIN_DEFINE_*` values from [`proto::flags`].
    pub async fn define_xml(&self, xml: &str, flags: u32) -> Result<Domain, Error> {
        let r = self
            .call_with(procs::DOMAIN_DEFINE_XML_FLAGS, |e| {
                e.string(xml).u32(flags);
            })
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    /// `virDomainUndefineFlags`: removes the persistent definition. `flags`
    /// are `DOMAIN_UNDEFINE_*` values from [`proto::flags`].
    pub async fn undefine(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_UNDEFINE_FLAGS, dom, Some(flags))
            .await
            .map(drop)
    }

    /// Starts a defined domain.
    pub async fn create(&self, dom: &Domain, flags: u32) -> Result<Domain, Error> {
        let r = self
            .call_dom(procs::DOMAIN_CREATE_WITH_FLAGS, dom, Some(flags))
            .await?;
        Domain::decode(&mut Decoder::new(&r))
    }

    /// Hard power-off.
    pub async fn destroy(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_DESTROY_FLAGS, dom, Some(flags))
            .await
            .map(drop)
    }

    /// Requests a guest (ACPI) shutdown.
    pub async fn shutdown(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_SHUTDOWN, dom, None)
            .await
            .map(drop)
    }

    /// `virDomainReboot`; `flags` is a `virDomainRebootFlagValues` mask, 0
    /// for the hypervisor default.
    pub async fn reboot(&self, dom: &Domain, flags: u32) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_REBOOT, dom, Some(flags))
            .await
            .map(drop)
    }

    /// `virDomainReset`: a hard reset without a guest shutdown.
    pub async fn reset(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_RESET, dom, Some(0))
            .await
            .map(drop)
    }

    /// `virDomainSuspend`: pauses the domain's vCPUs.
    pub async fn suspend(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_SUSPEND, dom, None)
            .await
            .map(drop)
    }

    /// `virDomainResume`: resumes a paused domain.
    pub async fn resume(&self, dom: &Domain) -> Result<(), Error> {
        self.call_dom(procs::DOMAIN_RESUME, dom, None)
            .await
            .map(drop)
    }

    /// `virDomainGetInfo`: state, memory, vCPU count and CPU time.
    pub async fn get_info(&self, dom: &Domain) -> Result<DomainInfo, Error> {
        let r = self.call_dom(procs::DOMAIN_GET_INFO, dom, None).await?;
        let mut d = Decoder::new(&r);
        // `unsigned char` and `unsigned short` are 4-byte units in XDR.
        Ok(DomainInfo {
            state: DomainState::from(d.i32()?),
            max_mem_kib: d.u64()?,
            memory_kib: d.u64()?,
            nr_virt_cpu: d.u32()? as u16,
            cpu_time_ns: d.u64()?,
        })
    }

    /// `virDomainGetState`: the state and the reason for it.
    pub async fn get_state(&self, dom: &Domain) -> Result<StateInfo, Error> {
        let r = self.call_dom(procs::DOMAIN_GET_STATE, dom, Some(0)).await?;
        let mut d = Decoder::new(&r);
        Ok(StateInfo {
            state: DomainState::from(d.i32()?),
            reason: d.i32()?,
        })
    }

    /// `virDomainGetXMLDesc`; `flags` are `DOMAIN_XML_*` values from
    /// [`proto::flags`].
    pub async fn get_xml_desc(&self, dom: &Domain, flags: u32) -> Result<String, Error> {
        let r = self
            .call_dom(procs::DOMAIN_GET_XML_DESC, dom, Some(flags))
            .await?;
        Decoder::new(&r).string()
    }

    /// Sets the balloon target (KiB).
    pub async fn set_memory(&self, dom: &Domain, kib: u64, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_SET_MEMORY_FLAGS, |e| {
            dom.encode(e);
            e.u64(kib).u32(flags);
        })
        .await
        .map(drop)
    }

    /// `virDomainMemoryStats`: the balloon statistics the guest reports.
    /// Tags are listed in [`proto::memory_stat`].
    pub async fn memory_stats(&self, dom: &Domain) -> Result<Vec<MemoryStat>, Error> {
        let r = self
            .call_with(procs::DOMAIN_MEMORY_STATS, |e| {
                dom.encode(e);
                e.u32(flags::DOMAIN_MEMORY_STAT_NR).u32(0);
            })
            .await?;
        Decoder::new(&r).array(proto::MEMORY_STATS_MAX, |d| {
            Ok(MemoryStat {
                tag: d.i32()?,
                value: d.u64()?,
            })
        })
    }

    /// Asks the daemon to deliver lifecycle events for all domains on this
    /// connection. Returns the callback ID.
    pub async fn register_lifecycle_events(&self) -> Result<i32, Error> {
        self.register_event(flags::DOMAIN_EVENT_ID_LIFECYCLE).await
    }

    /// Registers for one `VIR_DOMAIN_EVENT_ID_*` for all domains on this
    /// connection. Returns the callback ID.
    pub async fn register_event(&self, event_id: i32) -> Result<i32, Error> {
        let r = self
            .call_with(procs::CONNECT_DOMAIN_EVENT_CALLBACK_REGISTER_ANY, |e| {
                e.i32(event_id).option(None::<&Domain>, |e, d| d.encode(e));
            })
            .await?;
        Decoder::new(&r).i32()
    }
}
