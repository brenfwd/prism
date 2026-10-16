//! Creating testbed resources: the SSH key, networks, storage pool, base
//! volume and node domains. Downloading the cloud image is in `image`.

use std::time::Duration;

use anyhow::{Context, bail};

use super::node::{NodeSpec, node_ip};
use super::ssh::CONNECT_TIMEOUT;
use super::{
    BASE_VOL, DISK_SIZE, IMAGE_FILE, IP_TIMEOUT, MEMORY_MIB, NET_LAN, NET_NAT, POOL, POOL_PATH,
    SSH_TIMEOUT, Testbed, VCPUS, best_effort, domstate, info, path_str, step, wait_for,
};
use crate::cloudinit;
use crate::virsh::{run, virsh, virsh_ok};

impl Testbed {
    pub(super) fn ensure_ssh_key(&self) -> anyhow::Result<String> {
        let key = self.key_path();
        let pubkey = key.with_extension("pub");
        if !key.exists() {
            step("Generating the testbed SSH key");
            run(
                "ssh-keygen",
                &[
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    "prism-testbed",
                    "-f",
                    path_str(&key)?,
                ],
            )?;
        }
        std::fs::read_to_string(&pubkey).with_context(|| format!("reading {}", pubkey.display()))
    }

    pub(super) fn ensure_lan_network(&self) -> anyhow::Result<()> {
        if !virsh_ok(&["net-info", NET_LAN]) {
            step(&format!(
                "Defining the isolated network {NET_LAN} (no forwarding, no DHCP)"
            ));
            let xml = self.dir.join("net-prism-tb.xml");
            std::fs::write(
                &xml,
                format!(
                    "<network>\n  <name>{NET_LAN}</name>\n  <bridge stp='off' delay='0'/>\n</network>\n"
                ),
            )?;
            virsh(&["net-define", path_str(&xml)?])?;
            virsh(&["net-autostart", NET_LAN])?;
        }
        if !net_active(NET_LAN)? {
            step(&format!("Starting network {NET_LAN}"));
            virsh(&["net-start", NET_LAN])?;
        }
        Ok(())
    }

    pub(super) fn ensure_base_volume(&self) -> anyhow::Result<()> {
        if vol_exists(BASE_VOL) {
            return Ok(());
        }
        let image = self.ensure_image()?;
        step(&format!("Uploading {IMAGE_FILE} as volume {BASE_VOL}"));
        let size = std::fs::metadata(&image)?.len().to_string();
        virsh(&[
            "vol-create-as",
            POOL,
            BASE_VOL,
            &size,
            "--allocation",
            "0",
            "--format",
            "raw",
        ])?;
        if let Err(e) = virsh(&["vol-upload", "--pool", POOL, BASE_VOL, path_str(&image)?]) {
            best_effort(
                "delete the half-uploaded base volume",
                virsh(&["vol-delete", "--pool", POOL, BASE_VOL]),
            );
            return Err(e);
        }
        // Let libvirt re-probe the file: it is a qcow2 image.
        virsh(&["pool-refresh", POOL])?;
        Ok(())
    }

    pub(super) fn ensure_node(&self, spec: &NodeSpec, pubkey: &str) -> anyhow::Result<()> {
        let name = spec.domain();
        if virsh_ok(&["dominfo", &name]) {
            if domstate(&name)? == "running" {
                info(&format!("{name}: already running"));
            } else {
                step(&format!("Starting {name}"));
                virsh(&["start", &name])?;
            }
            return Ok(());
        }

        step(&format!("Creating {name}"));
        if vol_exists(&spec.disk_vol()) {
            info(&format!("reusing disk {}", spec.disk_vol()));
        } else {
            virsh(&[
                "vol-create-as",
                POOL,
                &spec.disk_vol(),
                DISK_SIZE,
                "--format",
                "qcow2",
                "--backing-vol",
                BASE_VOL,
                "--backing-vol-format",
                "qcow2",
            ])?;
            info(&format!(
                "disk {} ({DISK_SIZE}, backed by {BASE_VOL})",
                spec.disk_vol()
            ));
        }

        let (hostname, mgmt_mac, lan_mac, lan_addr) = (
            spec.hostname(),
            spec.mgmt_mac(),
            spec.lan_mac(),
            spec.lan_addr(),
        );
        let ci = cloudinit::Node {
            index: spec.index,
            hostname: &hostname,
            ssh_pubkey: pubkey,
            mgmt_mac: &mgmt_mac,
            lan_mac: &lan_mac,
            lan_addr: &lan_addr,
        };
        let iso = prism_iso::IsoBuilder::new("CIDATA")
            .file("user-data", cloudinit::user_data(&ci))
            .file("meta-data", cloudinit::meta_data(&ci))
            .file("network-config", cloudinit::network_config(&ci))
            .build()?;
        let iso_path = self.dir.join(spec.seed_vol());
        std::fs::write(&iso_path, &iso)?;
        let seed = spec.seed_vol();
        if vol_exists(&seed) {
            virsh(&["vol-delete", "--pool", POOL, &seed])?;
        }
        virsh(&[
            "vol-create-as",
            POOL,
            &seed,
            &iso.len().to_string(),
            "--format",
            "raw",
        ])?;
        virsh(&["vol-upload", "--pool", POOL, &seed, path_str(&iso_path)?])?;
        info(&format!("seed {seed} (NoCloud, {} bytes)", iso.len()));

        let xml = self.dir.join(format!("{name}.xml"));
        std::fs::write(&xml, spec.domain_xml())?;
        virsh(&["define", path_str(&xml)?])?;
        if let Err(e) = virsh(&["start", &name]) {
            // Leave nothing half-made, so the next `up` starts from scratch.
            best_effort(&format!("undefine {name}"), virsh(&["undefine", &name]));
            return Err(e);
        }
        info(&format!(
            "started: {MEMORY_MIB} MiB, {VCPUS} vCPUs, NICs {mgmt_mac} ({NET_NAT}) and {lan_mac} ({NET_LAN}, {lan_addr})"
        ));
        Ok(())
    }

    /// Waits for a DHCP lease, SSH, and cloud-init completion.
    pub(super) fn wait_ready(&self, spec: &NodeSpec) -> anyhow::Result<String> {
        let name = spec.domain();
        wait_for(IP_TIMEOUT, || node_ip(spec)).with_context(|| format!("{name}: no DHCP lease"))?;
        info(&format!("{name}: has a lease, waiting for SSH"));
        // A recreated node can show the lease of its earlier incarnation first, so the address
        // is read again on every attempt.
        let ip = wait_for(SSH_TIMEOUT, || {
            let ip = node_ip(spec)?;
            self.ssh_output(&ip, "true", None, Duration::from_secs(5))
                .ok()
                .filter(|o| o.status.success())
                .map(|_| ip)
        })
        .with_context(|| format!("{name}: SSH did not come up"))?;
        info(&format!("{name}: SSH at {ip}"));
        info(&format!(
            "{name}: waiting for cloud-init (package installs take a few minutes)"
        ));
        let out = self.ssh_output(
            &ip,
            "cloud-init status --wait --long",
            None,
            CONNECT_TIMEOUT,
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        match out.status.code() {
            Some(0) => Ok(ip),
            Some(2) => {
                info(&format!(
                    "{name}: cloud-init finished with recoverable errors:\n{}",
                    text.trim()
                ));
                Ok(ip)
            }
            _ => bail!(
                "cloud-init failed on {name}:\n{}{}",
                text.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }
}

pub(super) fn ensure_nat_network() -> anyhow::Result<()> {
    if !virsh_ok(&["net-info", NET_NAT]) {
        bail!("libvirt network {NET_NAT:?} is missing");
    }
    if !net_active(NET_NAT)? {
        step(&format!("Starting network {NET_NAT}"));
        virsh(&["net-start", NET_NAT])?;
    }
    Ok(())
}

pub(super) fn net_active(name: &str) -> anyhow::Result<bool> {
    let out = virsh(&["net-info", name])?;
    Ok(out
        .lines()
        .any(|l| l.starts_with("Active:") && l.ends_with("yes")))
}

pub(super) fn pool_exists() -> bool {
    virsh_ok(&["pool-info", POOL])
}

pub(super) fn ensure_pool() -> anyhow::Result<()> {
    if !pool_exists() {
        step(&format!("Defining storage pool {POOL} at {POOL_PATH}"));
        virsh(&["pool-define-as", POOL, "dir", "--target", POOL_PATH])?;
        virsh(&["pool-build", POOL])?;
        virsh(&["pool-autostart", POOL])?;
    }
    let info = virsh(&["pool-info", POOL])?;
    if !info
        .lines()
        .any(|l| l.starts_with("State:") && l.ends_with("running"))
    {
        step(&format!("Starting storage pool {POOL}"));
        virsh(&["pool-start", POOL])?;
    }
    Ok(())
}

pub(super) fn vol_exists(name: &str) -> bool {
    virsh_ok(&["vol-info", "--pool", POOL, name])
}
