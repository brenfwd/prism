//! The shared netlink connection handle.

use crate::{Error, Result};

/// Netlink connections (rtnetlink and generic netlink) for one network namespace.
///
/// The sockets belong to the namespace of the thread that called [`Netlink::connect`], so a
/// caller can manage another namespace by connecting from a thread that has `setns`ed into it.
/// Cloning is cheap and shares the connections.
#[derive(Clone, Debug)]
pub struct Netlink {
    pub(crate) route: rtnetlink::Handle,
    pub(crate) genl: genetlink::GenetlinkHandle,
}

impl Netlink {
    /// Opens the connections and spawns their driver tasks on the current tokio runtime.
    ///
    /// # Panics
    /// Panics when called outside a tokio runtime.
    pub fn connect() -> Result<Self> {
        let (conn, route, _) =
            rtnetlink::new_connection().map_err(|e| Error::io("open rtnetlink socket", e))?;
        tokio::spawn(conn);
        let (conn, genl, _) =
            genetlink::new_connection().map_err(|e| Error::io("open generic netlink socket", e))?;
        tokio::spawn(conn);
        Ok(Self { route, genl })
    }

    /// The underlying rtnetlink handle, for requests this crate does not wrap.
    pub fn route(&self) -> &rtnetlink::Handle {
        &self.route
    }
}
