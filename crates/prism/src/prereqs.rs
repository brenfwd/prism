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

/// Whether port forwards can work on this host.
pub fn passt() -> Prereq {
    let found = find_program("passt");
    Prereq {
        id: "passt",
        label: "passt (port forwarding)",
        ok: found.is_some(),
        required: false,
        detail: found.map_or_else(
            || "not installed; port forwards are unavailable".into(),
            |p| p.display().to_string(),
        ),
        hint: Some("Install the \"passt\" package (apt install passt), then forward ports again."),
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
