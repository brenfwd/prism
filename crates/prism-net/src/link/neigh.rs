//! The IPv6 neighbour table of a link: which addresses the kernel has seen behind which MAC.

use std::net::Ipv6Addr;

use futures::TryStreamExt;
use netlink_packet_route::AddressFamily;
use netlink_packet_route::neighbour::{
    NeighbourAddress, NeighbourAttribute, NeighbourMessage, NeighbourState,
};

use crate::{Error, MacAddr, Netlink, Result};

/// Whether an entry in `state` was learned from a message the neighbour sent, not just
/// requested.
fn is_resolved(state: NeighbourState) -> bool {
    matches!(
        state,
        NeighbourState::Reachable
            | NeighbourState::Stale
            | NeighbourState::Delay
            | NeighbourState::Probe
    )
}

/// The address and MAC of a resolved IPv6 neighbour entry.
fn entry(msg: &NeighbourMessage) -> Option<(Ipv6Addr, MacAddr)> {
    if !is_resolved(msg.header.state) {
        return None;
    }
    let (mut mac, mut dst) = (None, None);
    for attr in &msg.attributes {
        match attr {
            NeighbourAttribute::LinkLayerAddress(a) => {
                mac = <[u8; 6]>::try_from(a.as_slice()).ok().map(MacAddr);
            }
            NeighbourAttribute::Destination(NeighbourAddress::Inet6(a)) => dst = Some(*a),
            _ => {}
        }
    }
    Some((dst?, mac?))
}

impl Netlink {
    /// The resolved IPv6 neighbours on `dev` (a switch bridge): each address with the MAC it
    /// was seen behind.
    ///
    /// # Errors
    /// [`Error::NoSuchLink`] when `dev` does not exist, or a netlink failure.
    pub async fn neighbours6(&self, dev: &str) -> Result<Vec<(Ipv6Addr, MacAddr)>> {
        let index = self.link_index(dev).await?;
        let msgs: Vec<NeighbourMessage> = self
            .route
            .neighbours()
            .get()
            .set_address_family(AddressFamily::Inet6)
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::rtnl("dump neighbours", e))?;
        Ok(msgs
            .iter()
            .filter(|m| m.header.ifindex == index)
            .filter_map(entry)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(state: NeighbourState, mac: [u8; 6], dst: &str) -> NeighbourMessage {
        let mut m = NeighbourMessage::default();
        m.header.state = state;
        m.attributes
            .push(NeighbourAttribute::LinkLayerAddress(mac.to_vec()));
        m.attributes
            .push(NeighbourAttribute::Destination(NeighbourAddress::Inet6(
                dst.parse().unwrap(),
            )));
        m
    }

    #[test]
    fn resolved_entries_are_read() {
        let mac = [2, 0, 0, 0, 0, 1];
        let m = msg(NeighbourState::Stale, mac, "fd00::5");
        assert_eq!(entry(&m), Some(("fd00::5".parse().unwrap(), MacAddr(mac))));
    }

    #[test]
    fn unresolved_entries_are_skipped() {
        for s in [
            NeighbourState::Incomplete,
            NeighbourState::Failed,
            NeighbourState::None,
        ] {
            assert_eq!(entry(&msg(s, [2, 0, 0, 0, 0, 1], "fd00::5")), None);
        }
    }
}
