//! Lets AppArmor-confined QEMU reach Prism's live-migration sockets, and
//! lets `virt-aa-helper` read the storage pools.
//!
//! On Ubuntu, libvirt confines each QEMU with a profile built from the
//! `libvirt-qemu` abstraction. Sockets under `/run/prism/mig` are not in it,
//! so `nbd-server-start` and the migration stream fail with `EACCES` (and
//! the kernel's rate limit often hides the denial). The abstraction includes
//! `libvirt-qemu.d/`, so a drop-in there grants access; libvirt re-parses it
//! whenever it (re)loads a domain profile.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const ABSTRACTION: &str = "/etc/apparmor.d/abstractions/libvirt-qemu";
const DROP_IN_DIR: &str = "/etc/apparmor.d/abstractions/libvirt-qemu.d";
const DROP_IN: &str = "prism";
const RULES: &str = "# Managed by Prism: live-migration sockets.\n/run/prism/mig/** rw,\n";

/// Installs the drop-in when the host uses libvirt's `AppArmor` profiles.
pub(super) fn allow_migration_sockets() {
    if !Path::new(ABSTRACTION).exists() {
        return;
    }
    if let Err(e) = install(Path::new(DROP_IN_DIR)) {
        tracing::warn!("cannot install the AppArmor rule for migration sockets: {e}");
    }
}

fn install(dir: &Path) -> std::io::Result<()> {
    let path = dir.join(DROP_IN);
    if std::fs::read_to_string(&path).is_ok_and(|cur| cur == RULES) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, RULES)?;
    // Prism runs with umask 077; apparmor_parser runs as root either way,
    // but keep it readable like the rest of /etc/apparmor.d.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    tracing::info!("installed AppArmor rule {}", path.display());
    Ok(())
}

const HELPER_PROFILE: &str = "/etc/apparmor.d/usr.lib.libvirt.virt-aa-helper";
const HELPER_LOCAL: &str = "/etc/apparmor.d/local/usr.lib.libvirt.virt-aa-helper";
const BEGIN: &str = "# BEGIN prism pools\n";
const END: &str = "# END prism pools\n";

/// Lets libvirt's profile generator read the storage pools.
///
/// `virt-aa-helper` opens each disk to find its backing chain and grants
/// QEMU the chain's files. Outside its own allowlist (`/var/lib/libvirt/images`
/// and `$HOME`) the open is denied, so a VM with snapshot layers or a
/// backing image under a pool cannot start ("Permission denied" on the base).
pub(super) fn allow_pool_dirs(dirs: &[std::path::PathBuf]) {
    if !Path::new(HELPER_PROFILE).exists() {
        return;
    }
    match install_pools(Path::new(HELPER_LOCAL), dirs) {
        Ok(true) => reload(HELPER_PROFILE),
        Ok(false) => {}
        Err(e) => tracing::warn!("cannot install the AppArmor rule for storage pools: {e}"),
    }
}

/// Whether `path` can go into a profile rule unquoted. Spaces, commas, `#`
/// and `@` would change how the root-run parser reads the rule.
fn safe_rule_path(path: &str) -> bool {
    path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '/'))
}

/// Rewrites Prism's block of `local`; whether the file changed.
fn install_pools(local: &Path, dirs: &[std::path::PathBuf]) -> std::io::Result<bool> {
    let cur = std::fs::read_to_string(local).unwrap_or_default();
    let mut block = String::from(BEGIN);
    let mut dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    dirs.sort();
    for d in &dirs {
        if !safe_rule_path(d) {
            tracing::warn!(
                "no AppArmor rule for pool directory {d:?}: it has characters outside A-Z, a-z, 0-9 and . _ + - /"
            );
            continue;
        }
        let d = d.trim_end_matches('/');
        let _ = writeln!(block, "{d}/ r,\n{d}/** r,");
    }
    block.push_str(END);
    let rest = match (cur.find(BEGIN), cur.find(END)) {
        (Some(b), Some(e)) if b < e => format!("{}{}", &cur[..b], &cur[e + END.len()..]),
        _ => cur.clone(),
    };
    let new = if rest.is_empty() || rest.ends_with('\n') {
        format!("{rest}{block}")
    } else {
        format!("{rest}\n{block}")
    };
    if new == cur {
        return Ok(false);
    }
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(local, new)?;
    std::fs::set_permissions(local, std::fs::Permissions::from_mode(0o644))?;
    tracing::info!("updated AppArmor pool rules in {}", local.display());
    Ok(true)
}

fn reload(profile: &str) {
    let parser = ["/usr/sbin/apparmor_parser", "/sbin/apparmor_parser"]
        .into_iter()
        .find(|p| Path::new(p).exists());
    let Some(parser) = parser else { return };
    match std::process::Command::new(parser)
        .args(["-r", profile])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => tracing::warn!(
            "apparmor_parser -r {profile}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("running apparmor_parser: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_once() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("libvirt-qemu.d");
        install(&sub).unwrap();
        assert_eq!(std::fs::read_to_string(sub.join(DROP_IN)).unwrap(), RULES);
        install(&sub).unwrap();
    }

    #[test]
    fn pool_block_is_replaced_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("local");
        std::fs::write(&f, "# mine\n").unwrap();
        assert!(install_pools(&f, &["/a".into()]).unwrap());
        assert!(!install_pools(&f, &["/a".into()]).unwrap());
        assert!(install_pools(&f, &["/b/".into()]).unwrap());
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.starts_with("# mine\n"));
        assert!(s.contains("/b/** r,") && !s.contains("/a/"));
    }
}
