//! VLAN interfaces on a switch bridge: the layer 3 side of one VLAN.

use rtnetlink::{LinkUnspec, LinkVlan};

use super::{LinkKind, check_ifname};
use crate::{Error, Netlink, Result};

impl Netlink {
    /// Creates `name` as an 802.1Q interface for `vid` on `parent`, or corrects its MTU on an
    /// existing one, and brings it up. On a bridge with VLAN filtering the kernel adds `vid` to
    /// the bridge's own VLAN table when the interface appears, so tagged frames reach it.
    /// Returns the interface index.
    pub async fn ensure_vlan_interface(
        &self,
        name: &str,
        parent: &str,
        vid: u16,
        mtu: u32,
    ) -> Result<u32> {
        check_ifname(name)?;
        if !(1..=4094).contains(&vid) {
            return Err(Error::Invalid(format!("VLAN id {vid} out of range 1-4094")));
        }
        if self.existing_of_kind(name, LinkKind::Vlan).await?.is_none() {
            let base = self.link_index(parent).await?;
            self.route
                .link()
                .add(LinkVlan::new(name, base, vid).mtu(mtu).up().build())
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("create vlan interface {name}"), e))?;
        }
        let index = self.link_index(name).await?;
        self.route
            .link()
            .change(LinkUnspec::new_with_index(index).mtu(mtu).up().build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("configure vlan interface {name}"), e))?;
        Ok(index)
    }
}
