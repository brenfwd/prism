//! Running commands on testbed nodes over SSH.

use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use super::node::{NodeSpec, node_ip};
use super::{SSH_USER, Testbed};
use crate::virsh::virsh_ok;

pub(super) const SSH_OPTS: [&str; 6] = [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
];

impl Testbed {
    pub(super) fn remote_line(&self, ip: &str, cmd: &str) -> String {
        match self.ssh_output(ip, cmd, None, Some(3)) {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.is_empty() { "-".into() } else { s }
            }
            _ => "no ssh".into(),
        }
    }

    // ---- ssh ---------------------------------------------------------

    pub(super) fn ssh_base(&self, connect_timeout: Option<u32>) -> Command {
        let mut c = Command::new("ssh");
        c.arg("-i")
            .arg(self.key_path())
            .args(SSH_OPTS)
            .arg("-o")
            .arg(format!("ConnectTimeout={}", connect_timeout.unwrap_or(10)));
        c
    }

    pub(super) fn ssh_output(
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
    pub(super) fn ssh_check(
        &self,
        ip: &str,
        cmd: &str,
        stdin: Option<&[u8]>,
    ) -> anyhow::Result<String> {
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
}
