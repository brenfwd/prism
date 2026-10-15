//! `cargo xtask testbed`: Ubuntu VMs on the system libvirt that run Prism
//! as root, with nested KVM, for clustering and networking tests.

use std::fmt::Write as _;
use std::io::{IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::cloudinit;
use crate::virsh::{self, run, run_visible, virsh, virsh_ok};

pub const MAX_NODES: i64 = 50;

const POOL: &str = "prism-testbed";
const POOL_PATH: &str = "/var/lib/libvirt/images/prism-testbed";
const NET_NAT: &str = "default";
const NET_LAN: &str = "prism-tb";
const DOMAIN_PREFIX: &str = "prism-tb-";
const IMAGE_BASE_URL: &str = "https://cloud-images.ubuntu.com/resolute/current";
const IMAGE_FILE: &str = "resolute-server-cloudimg-amd64.img";
const BASE_VOL: &str = "ubuntu-26.04-base.qcow2";
const DISK_SIZE: &str = "20G";
const MEMORY_MIB: u32 = 4096;
const VCPUS: u32 = 4;
const SSH_USER: &str = "ubuntu";
const PRISM_PORT: u16 = 8080;

const IP_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Testbed {
    /// `target/testbed` in the workspace.
    dir: PathBuf,
}

/// Per-node names and addresses, all derived from the index.
struct NodeSpec {
    index: u8,
}

impl NodeSpec {
    fn domain(&self) -> String {
        format!("{DOMAIN_PREFIX}{}", self.index)
    }
    fn hostname(&self) -> String {
        format!("tb-{}", self.index)
    }
    fn disk_vol(&self) -> String {
        format!("{}.qcow2", self.domain())
    }
    fn seed_vol(&self) -> String {
        format!("{}-seed.iso", self.domain())
    }
    fn mgmt_mac(&self) -> String {
        format!("52:54:00:7b:00:{:02x}", self.index)
    }
    fn lan_mac(&self) -> String {
        format!("52:54:00:7b:01:{:02x}", self.index)
    }
    fn lan_addr(&self) -> String {
        format!("10.77.0.{}/24", self.index)
    }

    fn from_domain(name: &str) -> Option<Self> {
        let index = name.strip_prefix(DOMAIN_PREFIX)?.parse().ok()?;
        Some(Self { index })
    }

    /// Disks are `type='file'` rather than `type='volume'`: on Ubuntu,
    /// `virt-aa-helper` (the `AppArmor` profile generator) does not grant
    /// access to the backing chain of volume disks, so QEMU could not open
    /// the base image.
    fn domain_xml(&self) -> String {
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
  <cpu mode='host-passthrough' check='none' migratable='off'/>
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

fn step(msg: &str) {
    println!("==> {msg}");
}

fn info(msg: &str) {
    println!("    {msg}");
}

impl Testbed {
    pub fn new() -> anyhow::Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .context("locating the workspace root")?;
        let dir = root.join("target/testbed");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }

    // ---- up ----------------------------------------------------------

    pub fn up(&self, nodes: u8) -> anyhow::Result<()> {
        if !Path::new("/dev/kvm").exists() {
            bail!("/dev/kvm is missing on this host");
        }
        let pubkey = self.ensure_ssh_key()?;
        self.ensure_lan_network()?;
        ensure_nat_network()?;
        ensure_pool()?;
        self.ensure_base_volume()?;

        let specs: Vec<_> = (1..=nodes).map(|index| NodeSpec { index }).collect();
        for spec in &specs {
            self.ensure_node(spec, &pubkey)?;
        }

        step("Waiting for nodes");
        let mut failed = Vec::new();
        for spec in &specs {
            match self.wait_ready(spec) {
                Ok(ip) => info(&format!("{}: ready at {ip}", spec.domain())),
                Err(e) => {
                    info(&format!("{}: {e:#}", spec.domain()));
                    failed.push(spec.domain());
                }
            }
        }
        if !failed.is_empty() {
            bail!(
                "nodes not ready: {} (see `cargo xtask testbed status`)",
                failed.join(", ")
            );
        }
        println!();
        self.status()
    }

    fn ensure_ssh_key(&self) -> anyhow::Result<String> {
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

    fn ensure_lan_network(&self) -> anyhow::Result<()> {
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

    fn ensure_base_volume(&self) -> anyhow::Result<()> {
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
            let _ = virsh(&["vol-delete", "--pool", POOL, BASE_VOL]);
            return Err(e);
        }
        // Let libvirt re-probe the file: it is a qcow2 image.
        virsh(&["pool-refresh", POOL])?;
        Ok(())
    }

    /// Downloads the cloud image into `target/testbed` and verifies it.
    fn ensure_image(&self) -> anyhow::Result<PathBuf> {
        let image = self.dir.join(IMAGE_FILE);
        step("Fetching SHA256SUMS");
        let sums = run("curl", &["-fsSL", &format!("{IMAGE_BASE_URL}/SHA256SUMS")])?;
        let want = sums
            .lines()
            .find_map(|l| {
                let (hash, name) = l.split_once(' ')?;
                (name.trim_start_matches([' ', '*']) == IMAGE_FILE).then(|| hash.to_owned())
            })
            .with_context(|| format!("{IMAGE_FILE} not listed in SHA256SUMS"))?;

        if image.exists() {
            if sha256(&image)? == want {
                info(&format!("cached {} verified", image.display()));
                return Ok(image);
            }
            info("cached image is stale or corrupt; downloading again");
        }
        step(&format!("Downloading {IMAGE_FILE}"));
        let part = image.with_extension("img.part");
        run_visible(
            "curl",
            &[
                "-fL",
                "--progress-bar",
                "-o",
                path_str(&part)?,
                &format!("{IMAGE_BASE_URL}/{IMAGE_FILE}"),
            ],
        )?;
        let got = sha256(&part)?;
        if got != want {
            let _ = std::fs::remove_file(&part);
            bail!("checksum mismatch for {IMAGE_FILE}: got {got}, want {want}");
        }
        std::fs::rename(&part, &image)?;
        info("checksum OK");
        Ok(image)
    }

    fn ensure_node(&self, spec: &NodeSpec, pubkey: &str) -> anyhow::Result<()> {
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
            let _ = virsh(&["undefine", &name]);
            return Err(e);
        }
        info(&format!(
            "started: {MEMORY_MIB} MiB, {VCPUS} vCPUs, NICs {mgmt_mac} ({NET_NAT}) and {lan_mac} ({NET_LAN}, {lan_addr})"
        ));
        Ok(())
    }

    /// Waits for a DHCP lease, SSH, and cloud-init completion.
    fn wait_ready(&self, spec: &NodeSpec) -> anyhow::Result<String> {
        let name = spec.domain();
        let ip = wait_for(IP_TIMEOUT, || node_ip(spec))
            .with_context(|| format!("{name}: no DHCP lease"))?;
        info(&format!("{name}: lease {ip}, waiting for SSH"));
        wait_for(SSH_TIMEOUT, || {
            self.ssh_output(&ip, "true", None, Some(5))
                .ok()
                .filter(|o| o.status.success())
        })
        .with_context(|| format!("{name}: SSH at {ip} did not come up"))?;
        info(&format!(
            "{name}: waiting for cloud-init (package installs take a few minutes)"
        ));
        let out = self.ssh_output(&ip, "cloud-init status --wait --long", None, None)?;
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

    // ---- status ------------------------------------------------------

    pub fn status(&self) -> anyhow::Result<()> {
        let specs = existing_nodes()?;
        if specs.is_empty() {
            println!("No testbed nodes. Run `cargo xtask testbed up`.");
            return Ok(());
        }
        println!(
            "{:<12} {:<9} {:<16} {:<14} {:<10} cloud-init",
            "NODE", "STATE", "MGMT IP", "LAN IP", "PRISM"
        );
        for spec in specs {
            let name = spec.domain();
            let state = domstate(&name).unwrap_or_else(|_| "?".into());
            let ip = node_ip(&spec);
            let (ci, prism) = match &ip {
                Some(ip) if state == "running" => (
                    self.remote_line(
                        ip,
                        "cloud-init status 2>/dev/null | sed -n 's/^status: //p'",
                    ),
                    self.remote_line(ip, "systemctl is-active prism 2>/dev/null || true"),
                ),
                _ => (String::from("-"), String::from("-")),
            };
            let lan = spec.lan_addr();
            println!(
                "{name:<12} {state:<9} {:<16} {:<14} {prism:<10} {ci}",
                ip.as_deref().unwrap_or("-"),
                lan.split('/').next().unwrap_or(&lan),
            );
        }
        println!(
            "\nSSH: cargo xtask testbed ssh N [cmd…]   (key {})",
            self.key_path().display()
        );
        Ok(())
    }

    fn remote_line(&self, ip: &str, cmd: &str) -> String {
        match self.ssh_output(ip, cmd, None, Some(3)) {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.is_empty() { "-".into() } else { s }
            }
            _ => "no ssh".into(),
        }
    }

    // ---- ssh ---------------------------------------------------------

    fn ssh_base(&self, connect_timeout: Option<u32>) -> Command {
        let mut c = Command::new("ssh");
        c.arg("-i")
            .arg(self.key_path())
            .args(SSH_OPTS)
            .arg("-o")
            .arg(format!("ConnectTimeout={}", connect_timeout.unwrap_or(10)));
        c
    }

    fn ssh_output(
        &self,
        ip: &str,
        cmd: &str,
        stdin: Option<&[u8]>,
        connect_timeout: Option<u32>,
    ) -> anyhow::Result<std::process::Output> {
        let mut c = self.ssh_base(connect_timeout);
        c.args(["-o", "BatchMode=yes"])
            .arg(format!("{SSH_USER}@{ip}"))
            .arg("--")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = c.spawn().context("running ssh")?;
        if let Some(data) = stdin {
            child.stdin.take().expect("piped").write_all(data)?;
        }
        Ok(child.wait_with_output()?)
    }

    /// Runs a remote command, failing with its output.
    fn ssh_check(&self, ip: &str, cmd: &str, stdin: Option<&[u8]>) -> anyhow::Result<String> {
        let out = self.ssh_output(ip, cmd, stdin, None)?;
        if !out.status.success() {
            bail!(
                "`{cmd}` on {ip} failed ({}): {}{}",
                out.status,
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn ssh(&self, node: u8, cmd: &[String]) -> anyhow::Result<()> {
        let spec = NodeSpec { index: node };
        if !virsh_ok(&["dominfo", &spec.domain()]) {
            bail!("{} does not exist", spec.domain());
        }
        let ip = node_ip(&spec).with_context(|| format!("{} has no address yet", spec.domain()))?;
        let mut c = self.ssh_base(None);
        if cmd.is_empty() {
            c.arg(format!("{SSH_USER}@{ip}"));
        } else {
            if std::io::stdin().is_terminal() {
                c.arg("-t");
            }
            c.arg(format!("{SSH_USER}@{ip}")).arg("--").args(cmd);
        }
        // Replaces this process; only returns on failure.
        Err(c.exec()).context("exec ssh")
    }

    // ---- deploy ------------------------------------------------------

    pub fn deploy(&self, binary: &Path) -> anyhow::Result<()> {
        let root = self.dir.ancestors().nth(2).expect("target/testbed");
        let binary = if binary.is_absolute() {
            binary.to_owned()
        } else {
            root.join(binary)
        };
        if !binary.is_file() {
            bail!(
                "{} not found; build it with\n  cargo zigbuild --release --target x86_64-unknown-linux-musl -p prism",
                binary.display()
            );
        }
        let specs = existing_nodes()?;
        if specs.is_empty() {
            bail!("no testbed nodes; run `cargo xtask testbed up` first");
        }
        for spec in &specs {
            let name = spec.domain();
            if domstate(&name)? != "running" {
                info(&format!("{name}: not running, skipped"));
                continue;
            }
            let ip = node_ip(spec).with_context(|| format!("{name} has no address"))?;
            step(&format!("Deploying to {name} ({ip})"));
            self.deploy_node(spec, &ip, &binary)
                .with_context(|| format!("deploying to {name}"))?;
        }
        Ok(())
    }

    fn deploy_node(&self, spec: &NodeSpec, ip: &str, binary: &Path) -> anyhow::Result<()> {
        let mut scp = Command::new("scp");
        scp.arg("-q")
            .arg("-i")
            .arg(self.key_path())
            .args(SSH_OPTS)
            .arg(binary)
            .arg(format!("{SSH_USER}@{ip}:/tmp/prism"));
        let status = scp.status().context("running scp")?;
        if !status.success() {
            bail!("scp failed ({status})");
        }
        self.ssh_check(
            ip,
            "sudo install -m 0755 /tmp/prism /usr/local/bin/prism && rm -f /tmp/prism",
            None,
        )?;
        info("installed /usr/local/bin/prism");

        self.ssh_check(
            ip,
            "sudo tee /etc/systemd/system/prism.service >/dev/null",
            Some(UNIT.as_bytes()),
        )?;
        self.ssh_check(
            ip,
            "sudo test -f /etc/prism/prism.toml || sudo /usr/local/bin/prism init --config /etc/prism",
            None,
        )?;
        let current = self.ssh_check(ip, "sudo cat /etc/prism/prism.toml", None)?;
        let patched = patch_http(&current, ip, &spec.hostname())?;
        if patched != current {
            self.ssh_check(
                ip,
                "sudo tee /etc/prism/prism.toml >/dev/null",
                Some(patched.as_bytes()),
            )?;
            info(&format!(
                "http.listen: 127.0.0.1:{PRISM_PORT}, {ip}:{PRISM_PORT}"
            ));
        }
        self.ssh_check(
            ip,
            "sudo systemctl daemon-reload && sudo systemctl enable --quiet prism && sudo systemctl restart prism",
            None,
        )?;
        info("prism.service restarted");

        let addr: SocketAddr = format!("{ip}:{PRISM_PORT}").parse()?;
        let body = wait_for(Duration::from_secs(30), || {
            http_get(addr, "/api/v1/nodes").ok()
        })
        .with_context(|| {
            let log = self
                .ssh_check(ip, "sudo journalctl -u prism -n 20 --no-pager", None)
                .unwrap_or_default();
            format!("http://{addr}/api/v1/nodes did not answer\n{log}")
        })?;
        let connected = body.contains("\"libvirt_connected\":true");
        let kvm = body.contains("\"domain_type\":\"kvm\"");
        info(&format!(
            "http://{addr}/api/v1/nodes OK (libvirt_connected: {connected}, domain_type kvm: {kvm})"
        ));
        Ok(())
    }

    // ---- down --------------------------------------------------------

    pub fn down(&self, all: bool) -> anyhow::Result<()> {
        for spec in existing_nodes()? {
            let name = spec.domain();
            step(&format!("Removing {name}"));
            if domstate(&name)? != "shut off" {
                virsh(&["destroy", &name])?;
            }
            virsh(&["undefine", &name])?;
            let _ = std::fs::remove_file(self.dir.join(format!("{name}.xml")));
            let _ = std::fs::remove_file(self.dir.join(spec.seed_vol()));
        }
        if pool_exists() {
            virsh(&["pool-refresh", POOL])?;
            let vols = virsh::volume_names(&virsh(&["vol-list", POOL])?);
            for vol in vols {
                if vol.starts_with(DOMAIN_PREFIX) || (all && vol == BASE_VOL) {
                    info(&format!("deleting volume {vol}"));
                    virsh(&["vol-delete", "--pool", POOL, &vol])?;
                }
            }
        }
        if all {
            if pool_exists() {
                step(&format!("Removing pool {POOL}"));
                let _ = virsh(&["pool-destroy", POOL]);
                // Removes the (now empty) directory.
                virsh(&["pool-delete", POOL])?;
                virsh(&["pool-undefine", POOL])?;
            }
            if virsh_ok(&["net-info", NET_LAN]) {
                step(&format!("Removing network {NET_LAN}"));
                if net_active(NET_LAN)? {
                    virsh(&["net-destroy", NET_LAN])?;
                }
                virsh(&["net-undefine", NET_LAN])?;
            }
            let image = self.dir.join(IMAGE_FILE);
            if image.exists() {
                info(&format!("deleting {}", image.display()));
                std::fs::remove_file(image)?;
            }
        }
        println!("Done.");
        Ok(())
    }
}

const SSH_OPTS: [&str; 6] = [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
];

const UNIT: &str = "[Unit]
Description=Prism libvirt/KVM manager
Wants=network-online.target
After=network-online.target libvirtd.service

[Service]
ExecStart=/usr/local/bin/prism serve --config /etc/prism
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
";

/// Rewrites `[http] listen` and `allowed_hosts` so the host can reach the
/// node's API at its NAT address.
fn patch_http(toml: &str, ip: &str, hostname: &str) -> anyhow::Result<String> {
    let mut section = "";
    let (mut listen, mut hosts) = (false, false);
    let mut out = String::with_capacity(toml.len() + 64);
    for line in toml.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            section = t;
        }
        if section == "[http]" && (t.starts_with("listen ") || t.starts_with("listen=")) {
            write!(
                out,
                "listen = [\"127.0.0.1:{PRISM_PORT}\", \"{ip}:{PRISM_PORT}\"]"
            )?;
            listen = true;
        } else if section == "[http]" && t.starts_with("allowed_hosts") {
            write!(out, "allowed_hosts = [\"{ip}\", \"{hostname}\"]")?;
            hosts = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !(listen && hosts) {
        bail!("prism.toml has no [http] listen/allowed_hosts lines to patch");
    }
    Ok(out)
}

/// A minimal HTTP/1.1 GET returning the body of a 200 response.
fn http_get(addr: SocketAddr, path: &str) -> anyhow::Result<String> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;
    let mut resp = String::new();
    s.read_to_string(&mut resp)?;
    let (head, body) = resp.split_once("\r\n\r\n").context("malformed response")?;
    if !head.starts_with("HTTP/1.1 200") {
        bail!("{}", head.lines().next().unwrap_or_default());
    }
    Ok(body.to_owned())
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> anyhow::Result<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        if start.elapsed() > timeout {
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn path_str(p: &Path) -> anyhow::Result<&str> {
    p.to_str()
        .with_context(|| format!("non-UTF-8 path {}", p.display()))
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let out = run("sha256sum", &[path_str(path)?])?;
    out.split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum printed nothing")
}

fn ensure_nat_network() -> anyhow::Result<()> {
    if !virsh_ok(&["net-info", NET_NAT]) {
        bail!("libvirt network {NET_NAT:?} is missing");
    }
    if !net_active(NET_NAT)? {
        step(&format!("Starting network {NET_NAT}"));
        virsh(&["net-start", NET_NAT])?;
    }
    Ok(())
}

fn net_active(name: &str) -> anyhow::Result<bool> {
    let out = virsh(&["net-info", name])?;
    Ok(out
        .lines()
        .any(|l| l.starts_with("Active:") && l.ends_with("yes")))
}

fn pool_exists() -> bool {
    virsh_ok(&["pool-info", POOL])
}

fn ensure_pool() -> anyhow::Result<()> {
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

fn vol_exists(name: &str) -> bool {
    virsh_ok(&["vol-info", "--pool", POOL, name])
}

fn domstate(name: &str) -> anyhow::Result<String> {
    Ok(virsh(&["domstate", name])?.trim().to_owned())
}

fn existing_nodes() -> anyhow::Result<Vec<NodeSpec>> {
    let out = virsh(&["list", "--all", "--name"])?;
    let mut specs: Vec<_> = virsh::names(&out)
        .iter()
        .filter_map(|n| NodeSpec::from_domain(n))
        .collect();
    specs.sort_by_key(|s| s.index);
    Ok(specs)
}

fn node_ip(spec: &NodeSpec) -> Option<String> {
    let out = virsh(&["domifaddr", &spec.domain(), "--source", "lease"]).ok()?;
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
        assert!(xml.contains("<cpu mode='host-passthrough'"));
        assert!(xml.contains("/prism-testbed/prism-tb-12-seed.iso'"));
    }

    #[test]
    fn patches_http_section() {
        let toml = r#"[http]
# comment
listen = ["127.0.0.1:8080"]
allowed_hosts = []
allowed_origins = []

[libvirt]
socket = "auto"
"#;
        let out = patch_http(toml, "192.168.122.5", "tb-1").unwrap();
        assert!(out.contains(r#"listen = ["127.0.0.1:8080", "192.168.122.5:8080"]"#));
        assert!(out.contains(r#"allowed_hosts = ["192.168.122.5", "tb-1"]"#));
        assert!(out.contains("[libvirt]\nsocket = \"auto\"\n"));
        assert_eq!(patch_http(&out, "192.168.122.5", "tb-1").unwrap(), out);
        assert!(patch_http("[libvirt]\n", "1.2.3.4", "x").is_err());
    }
}
