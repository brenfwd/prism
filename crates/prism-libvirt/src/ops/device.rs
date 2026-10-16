//! Device hot-plug, vCPU count, guest addresses and filesystem freeze.

use super::{AddressSource, GuestInterface, InterfaceAddr};
use crate::Error;
use crate::client::Client;
use crate::proto::{Domain, procs};
use crate::xdr::Decoder;

/// Most interfaces an address reply may contain.
const INTERFACES_MAX: usize = 256;
/// Most addresses one interface may have.
const ADDRS_MAX: usize = 2048;

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

    /// `virDomainAttachDeviceFlags`: hot-plugs or persistently adds the
    /// device described by `xml`. `flags` combine `AFFECT_LIVE` and
    /// `AFFECT_CONFIG`.
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
        d.array(INTERFACES_MAX, |d| {
            Ok(GuestInterface {
                name: d.string()?,
                hwaddr: d.opt_string()?,
                addrs: d.array(ADDRS_MAX, |d| {
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

    /// `virDomainSetVcpusFlags`: sets the number of virtual CPUs to `n`.
    pub async fn set_vcpus(&self, dom: &Domain, n: u32, flags: u32) -> Result<(), Error> {
        self.call_with(procs::DOMAIN_SET_VCPUS_FLAGS, |e| {
            dom.encode(e);
            e.u32(n).u32(flags);
        })
        .await
        .map(drop)
    }

    /// `virDomainFSFreeze` through the guest agent. Returns the number of
    /// filesystems frozen.
    pub async fn fs_freeze(&self, dom: &Domain) -> Result<u32, Error> {
        let r = self
            .call_with(procs::DOMAIN_FSFREEZE, |e| {
                dom.encode(e);
                e.u32(0).u32(0);
            })
            .await?;
        Decoder::new(&r).u32()
    }

    /// `virDomainFSThaw` through the guest agent. Returns the number of
    /// filesystems thawed.
    pub async fn fs_thaw(&self, dom: &Domain) -> Result<u32, Error> {
        let r = self
            .call_with(procs::DOMAIN_FSTHAW, |e| {
                dom.encode(e);
                e.u32(0).u32(0);
            })
            .await?;
        Decoder::new(&r).u32()
    }

    pub(super) async fn dom_xml_call(
        &self,
        proc: u32,
        dom: &Domain,
        xml: &str,
        flags: u32,
    ) -> Result<Vec<u8>, Error> {
        self.dom_xml_flags(proc, dom, xml, flags).await
    }
}
