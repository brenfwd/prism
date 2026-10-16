//! `prism doctor`: checks that the host can run the node.
//!
//! Each check prints one status line (see [`Status`]). The command exits with
//! an error if any required check fails; optional programs only warn.

use std::path::Path;

use anyhow::bail;

use crate::config::Config;
use crate::prereqs;
use crate::storage::Pool;

/// Outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The check passed.
    Ok,
    /// An optional dependency is missing; the node still runs. The string is
    /// a hint on how to install it.
    Warn(String),
    /// A required dependency is missing or broken.
    Fail,
}

/// Formats one report line: a status tag, the check name and its detail.
/// A warning's hint goes on a second, indented line.
pub fn status_line(status: &Status, name: &str, detail: &str) -> String {
    match status {
        Status::Ok => format!("  ok    {name}: {detail}"),
        Status::Fail => format!("  FAIL  {name}: {detail}"),
        Status::Warn(hint) => format!("  warn  {name}: {detail}\n        {hint}"),
    }
}

/// Prints each check as it finishes and remembers whether any failed.
struct Report {
    failed: bool,
}

impl Report {
    fn check(&mut self, name: &str, res: Result<String, String>) {
        let (status, detail) = match res {
            Ok(detail) => (Status::Ok, detail),
            Err(detail) => (Status::Fail, detail),
        };
        self.failed |= status == Status::Fail;
        println!("{}", status_line(&status, name, &detail));
    }
}

/// Runs the checks for the config in `dir` and prints a line for each.
///
/// Fails if the config cannot be loaded or any required check fails.
pub async fn doctor(dir: &Path) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let mut report = Report { failed: false };
    let sock = cfg.libvirt_socket();
    let lv = match prism_libvirt::Client::connect(&sock, &cfg.libvirt.uri).await {
        Ok(c) => c
            .lib_version()
            .await
            .map(|v| format!("{} via {}", crate::node::format_version(v), sock.display()))
            .map_err(|e| e.to_string()),
        Err(e) => Err(format!("{}: {e}", sock.display())),
    };
    report.check("libvirt", lv);
    report.check(
        "kvm",
        kvm_check(Path::new("/dev/kvm").exists(), cfg.domain_type()),
    );
    let qi = tokio::process::Command::new(&cfg.libvirt.qemu_img)
        .arg("--version")
        .output()
        .await;
    report.check(
        "qemu-img",
        match qi {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .into()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into()),
            Err(e) => Err(format!("{}: {e}", cfg.libvirt.qemu_img.display())),
        },
    );
    for p in prereqs::all() {
        match (p.ok, p.required) {
            (true, _) => println!("{}", status_line(&Status::Ok, p.label, &p.detail)),
            (false, true) => report.check(p.label, Err(p.detail)),
            (false, false) => println!(
                "{}",
                status_line(
                    &Status::Warn(p.hint.unwrap_or("").to_string()),
                    p.label,
                    &p.detail
                )
            ),
        }
    }
    for (name, p) in &cfg.pools {
        report.check(
            &format!("pool {name}"),
            Pool::open(name, &p.path, &p.format, true)
                .map(|p| p.path.display().to_string())
                .map_err(|e| e.to_string()),
        );
    }
    if report.failed {
        bail!("some checks failed")
    }
    Ok(())
}

/// Decides the KVM check: `/dev/kvm` must exist unless the domain type is
/// `qemu` (software emulation).
fn kvm_check(present: bool, domain_type: &str) -> Result<String, String> {
    match (present, domain_type) {
        (true, t) => Ok(format!("/dev/kvm present; domain type {t}")),
        (false, "qemu") => Ok("/dev/kvm missing; using TCG emulation as configured".into()),
        (false, _) => Err("/dev/kvm missing; set libvirt.domain_type = \"qemu\" for TCG".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_status_lines() {
        assert_eq!(
            status_line(&Status::Ok, "kvm", "present"),
            "  ok    kvm: present"
        );
        assert_eq!(
            status_line(&Status::Fail, "kvm", "gone"),
            "  FAIL  kvm: gone"
        );
        assert_eq!(
            status_line(&Status::Warn("apt install nft".into()), "nft", "missing"),
            "  warn  nft: missing\n        apt install nft"
        );
    }

    #[test]
    fn kvm_needs_device_unless_emulating() {
        assert!(kvm_check(true, "kvm").is_ok());
        assert!(kvm_check(false, "qemu").is_ok());
        assert!(kvm_check(false, "kvm").is_err());
    }
}
