//! Thin wrappers over `virsh -c qemu:///system` and other subprocesses.
//!
//! Subprocesses run to completion with no timeout of their own, so a hung
//! libvirt call hangs the task. curl gets explicit time limits at its call
//! sites, and ssh and scp are given connect timeouts.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

/// The libvirt connection URI every `virsh` call uses.
pub(crate) const URI: &str = "qemu:///system";

/// Runs a program and returns its stdout, failing with its stderr.
pub(crate) fn run<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> anyhow::Result<String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {prog}"))?;
    if !out.status.success() {
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        bail!(
            "{prog} {} failed ({}): {}",
            shown.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a program with its output passed through to the terminal.
pub(crate) fn run_visible<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> anyhow::Result<()> {
    let status = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running {prog}"))?;
    if !status.success() {
        bail!("{prog} failed ({status})");
    }
    Ok(())
}

/// Runs `virsh -c qemu:///system` with `args` and returns its stdout, failing
/// with its stderr.
pub(crate) fn virsh<S: AsRef<OsStr>>(args: &[S]) -> anyhow::Result<String> {
    let mut all: Vec<&OsStr> = vec![OsStr::new("-c"), OsStr::new(URI)];
    all.extend(args.iter().map(AsRef::as_ref));
    run("virsh", &all)
}

/// Runs virsh and reports whether it succeeded. The error is dropped, so a
/// missing libvirt looks the same as a missing object; use it only for
/// existence checks.
pub(crate) fn virsh_ok<S: AsRef<OsStr>>(args: &[S]) -> bool {
    virsh(args).is_ok()
}

/// Names printed by a `virsh … --name` listing.
pub(crate) fn names(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Volume names from `virsh vol-list` (a two-column table).
pub(crate) fn volume_names(out: &str) -> Vec<String> {
    out.lines()
        .skip(2)
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// The IPv4 address most recently leased to `mac`, from
/// `virsh net-dhcp-leases`.
///
/// A node holds two leases after every boot: the initramfs DHCP client
/// (client ID `01:<mac>`) takes one, then the real OS (a DUID client ID)
/// takes another. Only the newest one is live, so pick the lease with the
/// latest expiry (timestamps sort lexically).
pub(crate) fn lease_ipv4(out: &str, mac: &str) -> Option<String> {
    out.lines()
        .filter_map(|l| {
            let cols: Vec<_> = l.split_whitespace().collect();
            match cols.as_slice() {
                [date, time, m, "ipv4", addr, ..] if m.eq_ignore_ascii_case(mac) => {
                    let ip = addr.split('/').next().unwrap_or(addr).to_owned();
                    Some((format!("{date} {time}"), ip))
                }
                _ => None,
            }
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, ip)| ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_lease() {
        let out = " Expiry Time           MAC address         Protocol   IP address         Hostname   Client ID or DUID
-----------------------------------------------------------------------------------------------------
 2026-10-15 09:17:35   52:54:00:7b:00:01   ipv4       192.168.122.3/24   tb-1       ff:56:50
 2026-10-15 09:17:31   52:54:00:7b:00:01   ipv4       192.168.122.5/24   -          01:52:54:00:7b:00:01
 2026-10-15 09:05:58   52:54:00:7b:00:02   ipv4       192.168.122.4/24   tb-2       ff:56:50
";
        assert_eq!(
            lease_ipv4(out, "52:54:00:7B:00:01").as_deref(),
            Some("192.168.122.3")
        );
        assert_eq!(lease_ipv4(out, "52:54:00:7b:00:03"), None);
    }

    #[test]
    fn parses_vol_list() {
        let out = " Name                 Path
------------------------------------------------------------------------
 base.qcow2           /var/lib/libvirt/images/prism-testbed/base.qcow2
 prism-tb-1.qcow2     /var/lib/libvirt/images/prism-testbed/prism-tb-1.qcow2

";
        assert_eq!(volume_names(out), ["base.qcow2", "prism-tb-1.qcow2"]);
    }
}
