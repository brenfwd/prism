//! Running commands on testbed nodes over SSH.

use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};

use super::node::{NodeSpec, node_ip};
use super::{SSH_USER, Testbed};
use crate::virsh::virsh_ok;

/// How long ssh waits for the TCP connection unless a caller picks another.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Options that keep ssh non-interactive and quiet. Node host keys change on
/// every rebuild, so they are neither checked nor remembered.
pub(super) const SSH_OPTS: [&str; 6] = [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
];

impl Testbed {
    /// Runs `cmd` on the node and returns its trimmed stdout for the status
    /// table: `-` when it prints nothing, `no ssh` when the command fails or
    /// the node does not answer within 3 seconds.
    pub(super) fn remote_line(&self, ip: &str, cmd: &str) -> String {
        match self.ssh_output(ip, cmd, None, Duration::from_secs(3)) {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.is_empty() { "-".into() } else { s }
            }
            _ => "no ssh".into(),
        }
    }

    /// An `ssh` command with the testbed key and options, not yet pointed at
    /// a host. `connect_timeout` is rounded up to whole seconds.
    pub(super) fn ssh_base(&self, connect_timeout: Duration) -> Command {
        let mut c = Command::new("ssh");
        c.arg("-i")
            .arg(self.key_path())
            .args(SSH_OPTS)
            .arg("-o")
            .arg(format!(
                "ConnectTimeout={}",
                connect_timeout.as_secs().max(1)
            ));
        c
    }

    /// Runs `cmd` on the node with optional stdin and collects both output
    /// streams. A non-zero exit status is returned in the output, not as an
    /// error.
    pub(super) fn ssh_output(
        &self,
        ip: &str,
        cmd: &str,
        stdin: Option<&[u8]>,
        connect_timeout: Duration,
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
        let Some(data) = stdin else {
            return Ok(child.wait_with_output()?);
        };
        let mut pipe = child.stdin.take().context("ssh stdin was not piped")?;
        // Feed stdin from its own thread while the main thread drains stdout
        // and stderr, so a large reply cannot fill a pipe and stall both ends.
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || pipe.write_all(data));
            let output = child.wait_with_output()?;
            // A remote command that exits without reading its stdin closes the
            // pipe; its exit status already tells the story.
            match writer
                .join()
                .map_err(|_| anyhow!("ssh stdin writer panicked"))?
            {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => return Err(e).context("writing ssh stdin"),
            }
            Ok(output)
        })
    }

    /// Runs a remote command, failing with its output.
    pub(super) fn ssh_check(
        &self,
        ip: &str,
        cmd: &str,
        stdin: Option<&[u8]>,
    ) -> anyhow::Result<String> {
        let out = self.ssh_output(ip, cmd, stdin, CONNECT_TIMEOUT)?;
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

    /// Opens an interactive session on node `node`, or runs `cmd` there.
    /// Replaces this process with ssh, so it only returns on failure.
    pub fn ssh(&self, node: u8, cmd: &[String]) -> anyhow::Result<()> {
        let spec = NodeSpec { index: node };
        if !virsh_ok(&["dominfo", &spec.domain()]) {
            bail!("{} does not exist", spec.domain());
        }
        let ip = node_ip(&spec).with_context(|| format!("{} has no address yet", spec.domain()))?;
        let mut c = self.ssh_base(CONNECT_TIMEOUT);
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
