//! Host prerequisites that are external programs, shared by `prism doctor`
//! and the API so the UI can explain what is missing and why.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Prereq {
    /// Stable key, e.g. `passt`.
    pub id: &'static str,
    pub label: &'static str,
    pub ok: bool,
    /// Without it only the named feature is unavailable, not the node.
    pub required: bool,
    pub detail: String,
    /// What to do when it is missing.
    pub hint: Option<&'static str>,
}

/// Finds `name` on `PATH` and the usual sbin directories (a service's PATH
/// often leaves those out).
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    dirs.extend(["/usr/sbin", "/usr/bin", "/sbin", "/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(name)).find(|p| is_exec(p))
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether libvirt's `AppArmor` passt sub-profile makes passt unusable.
///
/// The sub-profile (`libvirt-<uuid>//passt`) lacks mmap of the binary and
/// the passt process then cannot `accept` QEMU's connection, so a VM with a
/// forward either dies with SIGSEGV or boots with a dead NIC. A drop-in
/// cannot reach the sub-profile (redefining it fails to load). Set
/// `PRISM_ALLOW_PASST_APPARMOR=1` once the distribution fixes its profile.
fn passt_blocked_by_apparmor() -> bool {
    if std::env::var_os("PRISM_ALLOW_PASST_APPARMOR").is_some() {
        return false;
    }
    let enabled = std::fs::read_to_string("/sys/module/apparmor/parameters/enabled")
        .is_ok_and(|s| s.trim() == "Y");
    let abstraction =
        std::fs::read_to_string("/etc/apparmor.d/abstractions/libvirt-qemu").unwrap_or_default();
    enabled && abstraction.contains("profile passt {")
}

/// Whether port forwards can work on this host.
pub fn passt() -> Prereq {
    let found = find_program("passt");
    let blocked = found.is_some() && passt_blocked_by_apparmor();
    Prereq {
        id: "passt",
        label: "passt (opt-in user-mode networking)",
        ok: found.is_some() && !blocked,
        required: false,
        detail: match (&found, blocked) {
            (None, _) => "not installed; user-mode port forwards are unavailable".into(),
            (Some(p), true) => format!(
                "{} is installed, but libvirt's AppArmor profile for passt breaks it \
                 on this host; user-mode port forwards are unavailable",
                p.display()
            ),
            (Some(p), false) => p.display().to_string(),
        },
        hint: Some(if blocked {
            "libvirt's AppArmor passt sub-profile is broken on this distribution. \
             Forward ports on a Prism network NIC instead (nftables, no passt needed), or update libvirt/apparmor."
        } else {
            "Only needed for user-mode NICs; ports forwarded on Prism networks use nftables. Install the \"passt\" package (apt install passt) to opt in."
        }),
    }
}

fn program(id: &'static str, label: &'static str, bin: &str, hint: &'static str) -> Prereq {
    let found = find_program(bin);
    Prereq {
        id,
        label,
        ok: found.is_some(),
        required: false,
        detail: found.map_or_else(|| "not installed".into(), |p| p.display().to_string()),
        hint: Some(hint),
    }
}

/// Every external program Prism may call, besides `qemu-img` which the
/// config names and `doctor` checks itself.
pub fn all() -> Vec<Prereq> {
    vec![
        passt(),
        program(
            "ip",
            "ip (iproute2)",
            "ip",
            "Install iproute2; networks and cluster leave use it.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_sh() {
        assert!(super::find_program("sh").is_some());
        assert!(super::find_program("definitely-not-a-program-xyz").is_none());
    }
}
