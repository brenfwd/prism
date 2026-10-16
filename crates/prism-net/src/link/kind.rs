//! Link kinds Prism creates and interface name validation.

use crate::{Error, Result};

/// The longest interface name the kernel accepts (`IFNAMSIZ` of 16 minus the NUL byte).
const IFNAME_MAX: usize = 15;

/// A kernel link kind (`IFLA_INFO_KIND`) that this crate creates or checks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkKind {
    Bridge,
    Vxlan,
    Vlan,
}

impl LinkKind {
    /// The kind string the kernel reports.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Vxlan => "vxlan",
            Self::Vlan => "vlan",
        }
    }
}

/// Rejects names the kernel would refuse: empty, longer than 15 bytes, `.` or `..`, or
/// containing `/`, whitespace or a NUL byte. The kernel's own error for these is a bare
/// `EINVAL`.
pub(super) fn check_ifname(name: &str) -> Result<()> {
    let bad_char = |c: char| c == '/' || c == '\0' || c.is_whitespace();
    if name.is_empty()
        || name.len() > IFNAME_MAX
        || name == "."
        || name == ".."
        || name.contains(bad_char)
    {
        return Err(Error::Invalid(format!(
            "interface name {name:?} must be 1-{IFNAME_MAX} bytes without '/' or whitespace"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_names() {
        assert!(check_ifname("ps100000").is_ok());
        assert!(check_ifname("exactly15bytes!").is_ok());
        assert!(check_ifname("sixteen-bytes-xx").is_err());
        assert!(check_ifname("").is_err());
        assert!(check_ifname("a/b").is_err());
        assert!(check_ifname("a b").is_err());
        assert!(check_ifname("..").is_err());
    }
}
