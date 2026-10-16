//! Choosing which learned IPv6 addresses a NIC may use.

use std::collections::BTreeSet;
use std::net::Ipv6Addr;

use prism_net::MacAddr;
use uuid::Uuid;

use super::{NetworkDoc, ipam};
use crate::cluster::model::MemberDoc;
use crate::spec::{NicNetwork, VmSpec};

/// Addresses no NIC may claim by being seen with them: the SLAAC address of every NIC on the
/// network and the address each node holds on the bridge. A guest that sends neighbour
/// discovery from one of these would otherwise take it over.
pub(super) fn reserved(
    net: &NetworkDoc,
    node: Uuid,
    vms: &[VmSpec],
    members: &[MemberDoc],
) -> BTreeSet<Ipv6Addr> {
    let Some(prefix) = net.subnet6 else {
        return BTreeSet::new();
    };
    let mut out: BTreeSet<Ipv6Addr> = vms
        .iter()
        .flat_map(|v| &v.nics)
        .filter(|n| matches!(&n.network, NicNetwork::Network { network, .. } if *network == net.id))
        .filter_map(|n| n.mac.parse::<MacAddr>().ok())
        .map(|mac| ipam::slaac(prefix, mac))
        .collect();
    out.extend(
        members
            .iter()
            .map(|m| m.node_id)
            .chain([node])
            .map(|n| ipam::node_addr6(prefix, n)),
    );
    out
}

/// The addresses of the network's routers: each node's address on the bridge and its
/// link-local address. Guests may neither send neighbour messages from them nor claim them.
pub(super) fn gateways(
    net: &NetworkDoc,
    node: Uuid,
    members: &[MemberDoc],
    link_local: impl Fn(Uuid) -> Ipv6Addr,
) -> Vec<Ipv6Addr> {
    let Some(prefix) = net.subnet6 else {
        return Vec::new();
    };
    let nodes: BTreeSet<Uuid> = members.iter().map(|m| m.node_id).chain([node]).collect();
    nodes
        .into_iter()
        .flat_map(|n| [ipam::node_addr6(prefix, n), link_local(n)])
        .collect()
}

/// The learned addresses of one NIC that it may use: inside the network's prefix and not
/// reserved for someone else. The NIC's own SLAAC address `eui` is always acceptable.
pub(super) fn for_nic(
    net: &NetworkDoc,
    eui: Option<Ipv6Addr>,
    learned: Option<&Vec<Ipv6Addr>>,
    reserved: &BTreeSet<Ipv6Addr>,
) -> Vec<Ipv6Addr> {
    let Some(prefix) = net.subnet6 else {
        return Vec::new();
    };
    let mut out: Vec<Ipv6Addr> = Vec::new();
    for a in learned.into_iter().flatten() {
        if prefix.contains(a) && (Some(*a) == eui || !reserved.contains(a)) && !out.contains(a) {
            out.push(*a);
        }
    }
    out
}

/// What a NIC may send from: its addresses, and its SLAAC address as the fallback should the
/// guest go back to it.
pub(super) fn allowed(addrs: &[Ipv6Addr], eui: Option<Ipv6Addr>) -> Vec<Ipv6Addr> {
    let mut out = addrs.to_vec();
    if let Some(e) = eui
        && !out.contains(&e)
    {
        out.push(e);
    }
    out
}
