//! `NoCloud` seed contents for a testbed node.

/// Packages every node gets.
pub const PACKAGES: &[&str] = &[
    "qemu-system-x86",
    "qemu-utils",
    "libvirt-daemon-system",
    "ovmf",
    "wireguard-tools",
    "nftables",
    "iproute2",
    "bridge-utils",
];

/// The per-node values that go into the seed files.
pub struct Node<'a> {
    /// Node number, from 1.
    pub index: u8,
    /// Hostname inside the guest.
    pub hostname: &'a str,
    /// OpenSSH public key authorised for the `ubuntu` user.
    pub ssh_pubkey: &'a str,
    /// MAC of the NIC on the NAT network.
    pub mgmt_mac: &'a str,
    /// MAC of the NIC on the isolated network.
    pub lan_mac: &'a str,
    /// Static address on the isolated segment, with prefix length.
    pub lan_addr: &'a str,
}

/// The `user-data` file: user, key-only SSH, packages and libvirt setup.
pub fn user_data(n: &Node<'_>) -> String {
    let packages: String = PACKAGES.iter().fold(String::new(), |mut s, p| {
        s.push_str("  - ");
        s.push_str(p);
        s.push('\n');
        s
    });
    format!(
        r#"#cloud-config
hostname: {host}
manage_etc_hosts: true
users:
  - name: ubuntu
    groups: [sudo]
    shell: /bin/bash
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    # Password "ubuntu" is for `virsh console` only; SSH is key-only.
    lock_passwd: false
    plain_text_passwd: ubuntu
    ssh_authorized_keys:
      - {key}
ssh_pwauth: false
package_update: true
packages:
{packages}runcmd:
  - [systemctl, enable, --now, libvirtd]
  - [usermod, -aG, libvirt, ubuntu]
"#,
        host = n.hostname,
        key = n.ssh_pubkey.trim(),
    )
}

/// The `meta-data` file: instance ID and hostname.
pub fn meta_data(n: &Node<'_>) -> String {
    format!(
        "instance-id: prism-tb-{}\nlocal-hostname: {}\n",
        n.index, n.hostname
    )
}

/// Netplan v2: DHCP on the NAT NIC, a static address on the isolated one.
pub fn network_config(n: &Node<'_>) -> String {
    format!(
        r#"version: 2
ethernets:
  mgmt:
    match:
      macaddress: "{mgmt}"
    dhcp4: true
  lan:
    match:
      macaddress: "{lan}"
    dhcp4: false
    addresses: [{addr}]
"#,
        mgmt = n.mgmt_mac,
        lan = n.lan_mac,
        addr = n.lan_addr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders() {
        let n = Node {
            index: 2,
            hostname: "tb-2",
            ssh_pubkey: "ssh-ed25519 AAAA test\n",
            mgmt_mac: "52:54:00:7b:00:02",
            lan_mac: "52:54:00:7b:01:02",
            lan_addr: "10.77.0.2/24",
        };
        let u = user_data(&n);
        assert!(u.starts_with("#cloud-config\nhostname: tb-2\n"));
        assert!(u.contains("      - ssh-ed25519 AAAA test\n"));
        assert!(u.contains("  - bridge-utils\nruncmd:\n"));
        assert!(meta_data(&n).contains("instance-id: prism-tb-2"));
        assert!(network_config(&n).contains("addresses: [10.77.0.2/24]"));
    }
}
