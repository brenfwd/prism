//! Lets AppArmor-confined QEMU reach Prism's live-migration sockets.
//!
//! On Ubuntu, libvirt confines each QEMU with a profile built from the
//! `libvirt-qemu` abstraction. Sockets under `/run/prism/mig` are not in it,
//! so `nbd-server-start` and the migration stream fail with `EACCES` (and
//! the kernel's rate limit often hides the denial). The abstraction includes
//! `libvirt-qemu.d/`, so a drop-in there grants access; libvirt re-parses it
//! whenever it (re)loads a domain profile.

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
}
