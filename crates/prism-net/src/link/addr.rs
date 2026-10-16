//! Addresses and host routes on a link.

use std::net::{IpAddr, Ipv6Addr};

use futures::TryStreamExt;
use ipnet::IpNet;

use crate::{Error, Netlink, Result};

impl Netlink {
    /// Adds an address to a link unless already present (replacing its flags otherwise).
    pub async fn ensure_address(&self, name: &str, addr: IpNet) -> Result<()> {
        let index = self.link_index(name).await?;
        self.route
            .address()
            .add(index, addr.addr(), addr.prefix_len())
            .replace()
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("add address {addr} to {name}"), e))
    }

    /// Routes the host address `addr` (a /128) directly out of `name`, which
    /// lets the host forward to a guest whose network has no IPv6 prefix.
    pub async fn ensure_host_route6(&self, name: &str, addr: Ipv6Addr) -> Result<()> {
        use rtnetlink::RouteMessageBuilder;
        let index = self.link_index(name).await?;
        let msg = RouteMessageBuilder::<Ipv6Addr>::new()
            .destination_prefix(addr, 128)
            .output_interface(index)
            .build();
        self.route
            .route()
            .add(msg)
            .replace()
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("route {addr} via {name}"), e))
    }

    /// Removes every IPv4 address on a link except `keep`, so a re-addressed bridge drops its
    /// old gateway.
    pub async fn prune_v4_addresses(&self, name: &str, keep: Option<IpNet>) -> Result<()> {
        use netlink_packet_route::address::AddressAttribute;
        let index = self.link_index(name).await?;
        let msgs: Vec<_> = self
            .route
            .address()
            .get()
            .set_link_index_filter(index)
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::rtnl(format!("list addresses of {name}"), e))?;
        for msg in msgs {
            let addr = msg.attributes.iter().find_map(|a| match a {
                AddressAttribute::Address(ip) if ip.is_ipv4() => Some(*ip),
                _ => None,
            });
            let Some(ip) = addr else { continue };
            let wanted =
                keep.is_some_and(|k| k.addr() == ip && k.prefix_len() == msg.header.prefix_len);
            if wanted {
                continue;
            }
            self.route
                .address()
                .del(msg)
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("delete address {ip} from {name}"), e))?;
        }
        Ok(())
    }

    /// Removes every ULA address (`fc00::/7`) on a link except `keep`, so a bridge whose
    /// network lost or changed its IPv6 prefix drops the old address.
    pub async fn prune_ula_addresses(&self, name: &str, keep: Option<Ipv6Addr>) -> Result<()> {
        use netlink_packet_route::address::AddressAttribute;
        let index = self.link_index(name).await?;
        let msgs: Vec<_> = self
            .route
            .address()
            .get()
            .set_link_index_filter(index)
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::rtnl(format!("list addresses of {name}"), e))?;
        for msg in msgs {
            let addr = msg.attributes.iter().find_map(|a| match a {
                AddressAttribute::Address(IpAddr::V6(ip)) => Some(*ip),
                _ => None,
            });
            let Some(ip) = addr.filter(|ip| ip.octets()[0] & 0xfe == 0xfc && Some(*ip) != keep)
            else {
                continue;
            };
            self.route
                .address()
                .del(msg)
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("delete address {ip} from {name}"), e))?;
        }
        Ok(())
    }
}
