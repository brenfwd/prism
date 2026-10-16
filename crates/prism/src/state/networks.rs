//! How network commands change the documents, and the IPAM side effects of
//! VM and membership changes. Deterministic, like the rest of `apply`.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use uuid::Uuid;

use super::{CLUSTER_ID, Outcome, VMS};
use crate::cluster::model::{CLUSTER, ClusterDoc, MEMBERS, MemberDoc};
use crate::db::Docs;
use crate::network::{
    NETWORKS, NetworkDoc, RELEASED_VNIS, ReleasedVni, VNI_COOLDOWN_SECS, ipam,
    valid_allow_prefixes, valid_network_name, valid_subnet, valid_subnet6, valid_vlan_subnets,
};
use crate::spec::{NicNetwork, VmSpec};

/// The nodes that hold a gateway on every network: the members, or this
/// node while standalone.
fn nodes(d: &Docs<'_>, local: Uuid) -> anyhow::Result<Vec<Uuid>> {
    let mut members: Vec<MemberDoc> = d.list(MEMBERS)?;
    if members.is_empty() {
        return Ok(vec![local]);
    }
    members.sort_by_key(|m| m.raft_id);
    Ok(members.into_iter().map(|m| m.node_id).collect())
}

/// Every address in `segment` of `net` (a VLAN, or the network's own subnet for `None`) that a
/// gateway or a NIC holds, except NIC `skip`.
fn used_addresses(
    d: &Docs<'_>,
    net: &NetworkDoc,
    segment: Option<u16>,
    skip: Option<Uuid>,
) -> anyhow::Result<BTreeSet<Ipv4Addr>> {
    let mut used: BTreeSet<Ipv4Addr> = net.gateways_of(segment).values().copied().collect();
    for vm in d.list::<VmSpec>(VMS)? {
        for nic in vm.nics.iter().filter(|n| Some(n.id) != skip) {
            if let NicNetwork::Network {
                network,
                ipv4: Some(ip),
                vlan,
                ..
            } = &nic.network
                && *network == net.id
                && net.segment(*vlan) == segment
            {
                used.insert(*ip);
            }
        }
    }
    Ok(used)
}

pub(super) fn put_network(
    d: &Docs<'_>,
    local: Uuid,
    new: &NetworkDoc,
    now: i64,
) -> anyhow::Result<Outcome> {
    if !valid_network_name(&new.name) {
        return Ok(Outcome::Conflict(format!(
            "invalid network name {:?}",
            new.name
        )));
    }
    if let Err(e) = valid_subnet(new.subnet) {
        return Ok(Outcome::Conflict(e));
    }
    let others: Vec<NetworkDoc> = d
        .list::<NetworkDoc>(NETWORKS)?
        .into_iter()
        .filter(|n| n.id != new.id)
        .collect();
    if others.iter().any(|n| n.name == new.name) {
        return Ok(Outcome::Conflict(format!(
            "a network named {} already exists",
            new.name
        )));
    }
    if let Err(e) = valid_vlan_subnets(new) {
        return Ok(Outcome::Conflict(e));
    }
    let subnets: Vec<_> = others.iter().flat_map(NetworkDoc::subnets).collect();
    for subnet in new.subnets() {
        if let Some(o) = ipam::conflicts(subnet, &subnets) {
            return Ok(Outcome::Conflict(format!(
                "{subnet} overlaps {o}, which another network uses"
            )));
        }
    }
    if let Err(e) = valid_allow_prefixes(new.kind, &new.allow_prefixes) {
        return Ok(Outcome::Conflict(e));
    }
    if let Some(p6) = new.subnet6 {
        let mut taken: Vec<_> = others.iter().filter_map(|n| n.subnet6).collect();
        if let Some(c) = d.get::<ClusterDoc>(CLUSTER, CLUSTER_ID)? {
            taken.push(ipnet::Ipv6Net::new(c.mesh_prefix, 48)?.trunc());
        }
        if let Err(e) = valid_subnet6(p6) {
            return Ok(Outcome::Conflict(e));
        }
        if let Some(o) = ipam::conflicts6(p6, &taken) {
            return Ok(Outcome::Conflict(format!(
                "{p6} overlaps {o}, which another network or the mesh uses"
            )));
        }
    }
    let nodes = nodes(d, local)?;
    if let Some(e) = new.egress
        && !nodes.contains(&e)
    {
        return Ok(Outcome::Conflict(format!(
            "egress node {e} is not a member"
        )));
    }
    let mut doc = new.clone();
    if let Some(old) = d.get::<NetworkDoc>(NETWORKS, &new.id.to_string())? {
        doc.vni = old.vni;
        doc.gateways = old.gateways.clone();
        doc.created = old.created;
        if let Err(e) = settle_vlans(d, Some(&old), &mut doc, &nodes)? {
            return Ok(Outcome::Conflict(e));
        }
        if let Err(e) = readdress(d, &old, &mut doc)? {
            return Ok(Outcome::Conflict(e));
        }
    } else {
        let mut taken: BTreeSet<u32> = others.iter().map(|n| n.vni).collect();
        for r in d.list::<ReleasedVni>(RELEASED_VNIS)? {
            if now - r.released < VNI_COOLDOWN_SECS {
                taken.insert(r.vni);
            } else {
                d.delete(RELEASED_VNIS, &r.vni.to_string())?;
            }
        }
        let Some(vni) = ipam::alloc_vni(&taken) else {
            return Ok(Outcome::Conflict("no free VNI".into()));
        };
        doc.vni = vni;
        doc.gateways.clear();
        for &node in &nodes {
            let used = doc.gateways.values().copied().collect();
            let Some(ip) = ipam::first_free(doc.subnet, &used) else {
                return Ok(Outcome::Conflict(format!(
                    "{} is too small for a gateway on every node",
                    doc.subnet
                )));
            };
            doc.gateways.insert(node, ip);
        }
        if let Err(e) = settle_vlans(d, None, &mut doc, &nodes)? {
            return Ok(Outcome::Conflict(e));
        }
    }
    d.put(NETWORKS, &doc.id.to_string(), &doc)?;
    Ok(Outcome::Ok)
}

/// Gives every VLAN subnet of `doc` its gateways: VLANs `old` already had keep theirs, new ones
/// get the lowest free address on each node. A VLAN cannot change subnet or disappear while a NIC
/// is on it. Nothing is written.
fn settle_vlans(
    d: &Docs<'_>,
    old: Option<&NetworkDoc>,
    doc: &mut NetworkDoc,
    nodes: &[Uuid],
) -> anyhow::Result<Result<(), String>> {
    for (vid, vs) in &mut doc.vlan_subnets {
        match old.and_then(|o| o.vlan_subnets.get(vid)) {
            Some(o) if o.subnet == vs.subnet => vs.gateways.clone_from(&o.gateways),
            Some(o) => {
                return Ok(Err(format!(
                    "VLAN {vid} already uses {}; remove it before choosing another subnet",
                    o.subnet
                )));
            }
            None => {
                vs.gateways.clear();
                for &node in nodes {
                    let used = vs.gateways.values().copied().collect();
                    let Some(ip) = ipam::first_free(vs.subnet, &used) else {
                        return Ok(Err(format!(
                            "{} is too small for a gateway on every node",
                            vs.subnet
                        )));
                    };
                    vs.gateways.insert(node, ip);
                }
            }
        }
    }
    let Some(old) = old else { return Ok(Ok(())) };
    for vid in old
        .vlan_subnets
        .keys()
        .filter(|v| !doc.vlan_subnets.contains_key(v))
    {
        let users: Vec<String> = d
            .list::<VmSpec>(VMS)?
            .into_iter()
            .filter(|vm| {
                vm.nics.iter().any(|n| {
                    matches!(&n.network, NicNetwork::Network { network, vlan, .. }
                        if *network == doc.id && *vlan == Some(*vid))
                })
            })
            .map(|vm| vm.name)
            .collect();
        if !users.is_empty() {
            return Ok(Err(format!(
                "VLAN {vid} is used by {}; move their NICs to another VLAN first",
                users.join(", ")
            )));
        }
    }
    Ok(Ok(()))
}

/// Moves addresses after `doc` differs from `old` in its subnets. When the network's own subnet
/// changed, every gateway and NIC lease in it keeps its host offset if the new subnet is the same
/// size, and otherwise gets the lowest free address. NICs on a VLAN that just got a subnet move
/// into it. Nothing is written unless everything fits.
fn readdress(
    d: &Docs<'_>,
    old: &NetworkDoc,
    doc: &mut NetworkDoc,
) -> anyhow::Result<Result<(), String>> {
    let moved = old.subnet != doc.subnet;
    let added: Vec<u16> = doc
        .vlan_subnets
        .keys()
        .filter(|v| !old.vlan_subnets.contains_key(v))
        .copied()
        .collect();
    if !moved && added.is_empty() {
        return Ok(Ok(()));
    }
    let mut used: BTreeMap<Option<u16>, BTreeSet<Ipv4Addr>> = BTreeMap::new();
    for v in &added {
        used.insert(
            Some(*v),
            doc.gateways_of(Some(*v)).values().copied().collect(),
        );
    }
    let mut pick = |doc: &NetworkDoc, segment: Option<u16>, ip: Option<Ipv4Addr>| {
        let taken = used.entry(segment).or_default();
        let subnet = doc.subnet_of(segment);
        let new = ip
            .and_then(|ip| ipam::translate(old.subnet, subnet, ip))
            .filter(|n| !taken.contains(n))
            .or_else(|| ipam::first_free(subnet, taken))?;
        taken.insert(new);
        Some(new)
    };
    let too_small = |subnet| format!("{subnet} is too small for the addresses in use");
    let mut gateways = old.gateways.clone();
    if moved {
        for ip in gateways.values_mut() {
            let Some(n) = pick(doc, None, Some(*ip)) else {
                return Ok(Err(too_small(doc.subnet)));
            };
            *ip = n;
        }
    }
    let mut vms = Vec::new();
    for mut vm in d.list::<VmSpec>(VMS)? {
        let mut changed = false;
        for nic in &mut vm.nics {
            let NicNetwork::Network {
                network,
                ipv4: Some(ip),
                vlan,
                ..
            } = &mut nic.network
            else {
                continue;
            };
            if *network != old.id {
                continue;
            }
            let target = doc.segment(*vlan);
            let wanted = if target == old.segment(*vlan) {
                (moved && target.is_none()).then_some(None)
            } else {
                Some(target)
            };
            if let Some(segment) = wanted {
                let Some(n) = pick(doc, segment, segment.is_none().then_some(*ip)) else {
                    return Ok(Err(too_small(doc.subnet_of(segment))));
                };
                *ip = n;
                changed = true;
            }
        }
        if changed {
            vm.generation += 1;
            vms.push(vm);
        }
    }
    if moved {
        doc.gateways = gateways;
    }
    for vm in vms {
        d.put(VMS, &vm.id.to_string(), &vm)?;
    }
    Ok(Ok(()))
}

pub(super) fn delete_network(d: &Docs<'_>, id: Uuid, now: i64) -> anyhow::Result<Outcome> {
    let Some(net) = d.get::<NetworkDoc>(NETWORKS, &id.to_string())? else {
        return Ok(Outcome::NotFound(format!("network {id}")));
    };
    let users: Vec<String> = d
        .list::<VmSpec>(VMS)?
        .into_iter()
        .filter(|vm| vm.nics.iter().any(|n| n.network.network_id() == Some(id)))
        .map(|vm| vm.name)
        .collect();
    if !users.is_empty() {
        return Ok(Outcome::Conflict(format!(
            "network {} is used by {}; remove their NICs first",
            net.name,
            users.join(", ")
        )));
    }
    d.delete(NETWORKS, &id.to_string())?;
    d.put(
        RELEASED_VNIS,
        &net.vni.to_string(),
        &ReleasedVni {
            vni: net.vni,
            released: now,
        },
    )?;
    Ok(Outcome::Ok)
}

/// Checks the network NICs of `vm` and gives each without an address an
/// IPAM lease from the subnet of its VLAN. `Err` carries a conflict message.
pub(super) fn assign_leases(d: &Docs<'_>, vm: &mut VmSpec) -> anyhow::Result<Result<(), String>> {
    for i in 0..vm.nics.len() {
        let nic_id = vm.nics[i].id;
        let NicNetwork::Network {
            network,
            ipv4,
            vlan,
            ..
        } = &vm.nics[i].network
        else {
            continue;
        };
        let Some(net) = d.get::<NetworkDoc>(NETWORKS, &network.to_string())? else {
            return Ok(Err(format!("network {network} does not exist")));
        };
        if let Err(e) = vm.nics[i].network.validate_vlans() {
            return Ok(Err(e));
        }
        let segment = net.segment(*vlan);
        let subnet = net.subnet_of(segment);
        let mut used = used_addresses(d, &net, segment, Some(nic_id))?;
        // Earlier NICs of this VM, as they will be stored.
        for other in &vm.nics[..i] {
            if let NicNetwork::Network {
                network: n,
                ipv4: Some(ip),
                vlan: v,
                ..
            } = &other.network
                && *n == net.id
                && net.segment(*v) == segment
            {
                used.insert(*ip);
            }
        }
        let ip = match ipv4 {
            Some(ip) if !ipam::is_host(subnet, *ip) => {
                return Ok(Err(format!("{ip} is not a host address in {subnet}")));
            }
            Some(ip) if used.contains(ip) => {
                return Ok(Err(format!("{ip} is already in use on {}", net.name)));
            }
            Some(ip) => *ip,
            None => match ipam::first_free(subnet, &used) {
                Some(ip) => ip,
                None => {
                    return Ok(Err(format!(
                        "{subnet} on network {} has no free addresses",
                        net.name
                    )));
                }
            },
        };
        if let NicNetwork::Network { ipv4, .. } = &mut vm.nics[i].network {
            *ipv4 = Some(ip);
        }
    }
    Ok(Ok(()))
}

/// Gives a newly admitted member a gateway on every network and VLAN subnet.
pub(super) fn add_gateways(d: &Docs<'_>, node: Uuid) -> anyhow::Result<()> {
    for mut net in d.list::<NetworkDoc>(NETWORKS)? {
        let mut changed = false;
        if !net.gateways.contains_key(&node) {
            let used = used_addresses(d, &net, None, None)?;
            if let Some(ip) = ipam::first_free(net.subnet, &used) {
                net.gateways.insert(node, ip);
                changed = true;
            }
        }
        let vlans: Vec<u16> = net
            .vlan_subnets
            .iter()
            .filter(|(_, v)| !v.gateways.contains_key(&node))
            .map(|(vid, _)| *vid)
            .collect();
        for vid in vlans {
            let used = used_addresses(d, &net, Some(vid), None)?;
            if let Some(ip) = ipam::first_free(net.subnet_of(Some(vid)), &used)
                && let Some(vs) = net.vlan_subnets.get_mut(&vid)
            {
                vs.gateways.insert(node, ip);
                changed = true;
            }
        }
        if changed {
            d.put(NETWORKS, &net.id.to_string(), &net)?;
        }
    }
    Ok(())
}

/// Frees a removed member's gateways and egress roles.
pub(super) fn remove_gateways(d: &Docs<'_>, node: Uuid) -> anyhow::Result<()> {
    for mut net in d.list::<NetworkDoc>(NETWORKS)? {
        let mut had = net.gateways.remove(&node).is_some();
        for vs in net.vlan_subnets.values_mut() {
            had |= vs.gateways.remove(&node).is_some();
        }
        let egress = net.egress == Some(node);
        if egress {
            net.egress = None;
        }
        if had || egress {
            d.put(NETWORKS, &net.id.to_string(), &net)?;
        }
    }
    Ok(())
}
