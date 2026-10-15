//! Thin wrappers over `virsh -c qemu:///system` and other subprocesses.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

pub const URI: &str = "qemu:///system";

/// Runs a program and returns its stdout, failing with its stderr.
pub fn run<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> anyhow::Result<String> {
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
pub fn run_visible<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> anyhow::Result<()> {
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

pub fn virsh<S: AsRef<OsStr>>(args: &[S]) -> anyhow::Result<String> {
    let mut all: Vec<&OsStr> = vec![OsStr::new("-c"), OsStr::new(URI)];
    all.extend(args.iter().map(AsRef::as_ref));
    run("virsh", &all)
}

/// Runs virsh and reports only whether it succeeded.
pub fn virsh_ok<S: AsRef<OsStr>>(args: &[S]) -> bool {
    virsh(args).is_ok()
}

/// Names printed by a `virsh … --name` listing.
pub fn names(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Volume names from `virsh vol-list` (a two-column table).
pub fn volume_names(out: &str) -> Vec<String> {
    out.lines()
        .skip(2)
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// The first IPv4 address leased to `mac`, from `virsh domifaddr`.
pub fn lease_ipv4(out: &str, mac: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let cols: Vec<_> = l.split_whitespace().collect();
        match cols.as_slice() {
            [_, m, "ipv4", addr] if m.eq_ignore_ascii_case(mac) => {
                Some(addr.split('/').next().unwrap_or(addr).to_owned())
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_domifaddr() {
        let out = " Name       MAC address          Protocol     Address
-------------------------------------------------------------------------------
 vnet3      52:54:00:7b:00:01    ipv4         192.168.122.57/24
 -          52:54:00:7b:00:01    ipv6         fe80::1/64
";
        assert_eq!(
            lease_ipv4(out, "52:54:00:7B:00:01").as_deref(),
            Some("192.168.122.57")
        );
        assert_eq!(lease_ipv4(out, "52:54:00:7b:00:02"), None);
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
