//! Installing a Prism binary and its systemd unit on every node.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};

use super::node::{NodeSpec, existing_nodes, node_ip};
use super::ssh::SSH_OPTS;
use super::{PRISM_PORT, SSH_USER, Testbed, domstate, info, step, wait_for};

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

impl Testbed {
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
            "sudo install -m 0755 /tmp/prism /usr/local/bin/prism && rm -f /tmp/prism && sync",
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
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
