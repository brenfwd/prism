//! Testbed nodes: names, addresses and domain XML, all derived from the
//! node index.

use super::{DOMAIN_PREFIX, MEMORY_MIB, NET_LAN, NET_NAT, POOL_PATH, VCPUS};
use crate::virsh::{self, virsh};

/// Per-node names and addresses, all derived from the index.
pub(super) struct NodeSpec {
    pub(super) index: u8,
}

impl NodeSpec {
    pub(super) fn domain(&self) -> String {
        format!("{DOMAIN_PREFIX}{}", self.index)
    }
    pub(super) fn hostname(&self) -> String {
        format!("tb-{}", self.index)
    }
    pub(super) fn disk_vol(&self) -> String {
        format!("{}.qcow2", self.domain())
    }
    pub(super) fn seed_vol(&self) -> String {
        format!("{}-seed.iso", self.domain())
    }
    pub(super) fn mgmt_mac(&self) -> String {
        format!("52:54:00:7b:00:{:02x}", self.index)
    }
    pub(super) fn lan_mac(&self) -> String {
        format!("52:54:00:7b:01:{:02x}", self.index)
    }
    pub(super) fn lan_addr(&self) -> String {
        format!("10.77.0.{}/24", self.index)
    }

    pub(super) fn from_domain(name: &str) -> Option<Self> {
        let index = name.strip_prefix(DOMAIN_PREFIX)?.parse().ok()?;
        Some(Self { index })
    }

    /// Disks are `type='file'` rather than `type='volume'`: on Ubuntu,
    /// `virt-aa-helper` (the `AppArmor` profile generator) does not grant
    /// access to the backing chain of volume disks, so QEMU could not open
    /// the base image.
    ///
    /// The CPU is `host-model`, not `host-passthrough`: passthrough leaks
    /// CPUID bits QEMU does not manage, notably CET shadow stacks, whose
    /// state QEMU then fails to preserve. A vCPU ends up with a stale user
    /// shadow-stack pointer, every process scheduled there dies with a
    /// shadow-stack fault at the same address, and the node drops off the
    /// network.
    pub(super) fn domain_xml(&self) -> String {
        format!(
            r"<domain type='kvm'>
  <name>{name}</name>
  <memory unit='MiB'>{MEMORY_MIB}</memory>
  <vcpu>{VCPUS}</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <boot dev='hd'/>
  </os>
  <features><acpi/><apic/></features>
  <cpu mode='host-model' check='none'/>
  <clock offset='utc'/>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>destroy</on_crash>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2' discard='unmap'/>
      <source file='{POOL_PATH}/{disk}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='{POOL_PATH}/{seed}'/>
      <target dev='sda' bus='sata'/>
      <readonly/>
    </disk>
    <interface type='network'>
      <source network='{NET_NAT}'/>
      <mac address='{mgmt_mac}'/>
      <model type='virtio'/>
    </interface>
    <interface type='network'>
      <source network='{NET_LAN}'/>
      <mac address='{lan_mac}'/>
      <model type='virtio'/>
    </interface>
    <serial type='pty'><target port='0'/></serial>
    <console type='pty'><target type='serial' port='0'/></console>
    <rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>
    <memballoon model='virtio'/>
  </devices>
</domain>
",
            name = self.domain(),
            disk = self.disk_vol(),
            seed = self.seed_vol(),
            mgmt_mac = self.mgmt_mac(),
            lan_mac = self.lan_mac(),
        )
    }
}

pub(super) fn existing_nodes() -> anyhow::Result<Vec<NodeSpec>> {
    let out = virsh(&["list", "--all", "--name"])?;
    let mut specs: Vec<_> = virsh::names(&out)
        .iter()
        .filter_map(|n| NodeSpec::from_domain(n))
        .collect();
    specs.sort_by_key(|s| s.index);
    Ok(specs)
}

pub(super) fn node_ip(spec: &NodeSpec) -> Option<String> {
    let out = virsh(&["net-dhcp-leases", NET_NAT, "--mac", &spec.mgmt_mac()]).ok()?;
    virsh::lease_ipv4(&out, &spec.mgmt_mac())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names() {
        let s = NodeSpec { index: 12 };
        assert_eq!(s.domain(), "prism-tb-12");
        assert_eq!(s.mgmt_mac(), "52:54:00:7b:00:0c");
        assert_eq!(NodeSpec::from_domain("prism-tb-12").unwrap().index, 12);
        assert!(NodeSpec::from_domain("prism-tb-x").is_none());
        assert!(NodeSpec::from_domain("other").is_none());
        let xml = s.domain_xml();
        assert!(xml.contains("<cpu mode='host-model'"));
        assert!(xml.contains("/prism-testbed/prism-tb-12-seed.iso'"));
    }
}
