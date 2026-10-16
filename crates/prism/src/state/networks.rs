//! How network commands change the documents, and the IPAM side effects of
//! VM and membership changes. Deterministic, like the rest of `apply`.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use uuid::Uuid;

use super::{Outcome, VMS};
use crate::cluster::model::{MEMBERS, MemberDoc};
use crate::db::Docs;
use crate::network::{
    NETWORKS, NetworkDoc, RELEASED_VNIS, ReleasedVni, VNI_COOLDOWN_SECS, ipam,
    valid_allow_prefixes, valid_network_name, valid_subnet,
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

/// Every address in `net` that a gateway or a NIC holds, except NIC `skip`.
fn used_addresses(
    d: &Docs<'_>,
    net: &NetworkDoc,
    skip: Option<Uuid>,
) -> anyhow::Result<BTreeSet<Ipv4Addr>> {
    let mut used: BTreeSet<Ipv4Addr> = net.gateways.values().copied().collect();
    for vm in d.list::<VmSpec>(VMS)? {
        for nic in vm.nics.iter().filter(|n| Some(n.id) != skip) {
            if let NicNetwork::Network {
                network,
                ipv4: Some(ip),
                ..
            } = &nic.network
                && *network == net.id
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
    let subnets: Vec<_> = others.iter().map(|n| n.subnet).collect();
    if let Some(o) = ipam::conflicts(new.subnet, &subnets) {
        return Ok(Outcome::Conflict(format!(
            "{} overlaps {o}, which another network uses",
            new.subnet
        )));
    }
    if let Err(e) = valid_allow_prefixes(new.kind, &new.allow_prefixes) {
        return Ok(Outcome::Conflict(e));
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
        if old.subnet != new.subnet
            && let Err(e) = renumber(d, &old, &mut doc)?
        {
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
        for node in nodes {
            let used = doc.gateways.values().copied().collect();
            let Some(ip) = ipam::first_free(doc.subnet, &used) else {
                return Ok(Outcome::Conflict(format!(
                    "{} is too small for a gateway on every node",
                    doc.subnet
                )));
            };
            doc.gateways.insert(node, ip);
        }
    }
    d.put(NETWORKS, &doc.id.to_string(), &doc)?;
    Ok(Outcome::Ok)
}

/// Moves a network to `doc.subnet`: every gateway and NIC lease keeps its
/// host offset when the new subnet is the same size, and otherwise gets the
/// lowest free address. Nothing is written unless everything fits.
fn renumber(
    d: &Docs<'_>,
    old: &NetworkDoc,
    doc: &mut NetworkDoc,
) -> anyhow::Result<Result<(), String>> {
    let mut used = BTreeSet::new();
    let mut pick = |ip: Ipv4Addr| -> Option<Ipv4Addr> {
        let new = ipam::translate(old.subnet, doc.subnet, ip)
            .filter(|n| !used.contains(n))
            .or_else(|| ipam::first_free(doc.subnet, &used))?;
        used.insert(new);
        Some(new)
    };
    let too_small = || format!("{} is too small for the addresses in use", doc.subnet);
    let mut gateways = old.gateways.clone();
    for ip in gateways.values_mut() {
        let Some(n) = pick(*ip) else {
            return Ok(Err(too_small()));
        };
        *ip = n;
    }
    let mut vms = Vec::new();
    for mut vm in d.list::<VmSpec>(VMS)? {
        let mut changed = false;
        for nic in &mut vm.nics {
            if let NicNetwork::Network {
                network,
                ipv4: Some(ip),
                ..
            } = &mut nic.network
                && *network == old.id
            {
                let Some(n) = pick(*ip) else {
                    return Ok(Err(too_small()));
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
    doc.gateways = gateways;
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
/// IPAM lease. `Err` carries a conflict message.
pub(super) fn assign_leases(d: &Docs<'_>, vm: &mut VmSpec) -> anyhow::Result<Result<(), String>> {
    for i in 0..vm.nics.len() {
        let nic_id = vm.nics[i].id;
        let NicNetwork::Network { network, ipv4, .. } = &vm.nics[i].network else {
            continue;
        };
        let Some(net) = d.get::<NetworkDoc>(NETWORKS, &network.to_string())? else {
            return Ok(Err(format!("network {network} does not exist")));
        };
        if let Err(e) = vm.nics[i].network.validate_vlans() {
            return Ok(Err(e));
        }
        let mut used = used_addresses(d, &net, Some(nic_id))?;
        // Earlier NICs of this VM, as they will be stored.
        for other in &vm.nics[..i] {
            if let NicNetwork::Network {
                network: n,
                ipv4: Some(ip),
                ..
            } = &other.network
                && *n == net.id
            {
                used.insert(*ip);
            }
        }
        let ip = match ipv4 {
            Some(ip) if !ipam::is_host(net.subnet, *ip) => {
                return Ok(Err(format!("{ip} is not a host address in {}", net.subnet)));
            }
            Some(ip) if used.contains(ip) => {
                return Ok(Err(format!("{ip} is already in use on {}", net.name)));
            }
            Some(ip) => *ip,
            None => match ipam::first_free(net.subnet, &used) {
                Some(ip) => ip,
                None => return Ok(Err(format!("network {} has no free addresses", net.name))),
            },
        };
        if let NicNetwork::Network { ipv4, .. } = &mut vm.nics[i].network {
            *ipv4 = Some(ip);
        }
    }
    Ok(Ok(()))
}

/// Gives a newly admitted member a gateway on every network.
pub(super) fn add_gateways(d: &Docs<'_>, node: Uuid) -> anyhow::Result<()> {
    for mut net in d.list::<NetworkDoc>(NETWORKS)? {
        if net.gateways.contains_key(&node) {
            continue;
        }
        let used = used_addresses(d, &net, None)?;
        if let Some(ip) = ipam::first_free(net.subnet, &used) {
            net.gateways.insert(node, ip);
            d.put(NETWORKS, &net.id.to_string(), &net)?;
        }
    }
    Ok(())
}

/// Frees a removed member's gateways and egress roles.
pub(super) fn remove_gateways(d: &Docs<'_>, node: Uuid) -> anyhow::Result<()> {
    for mut net in d.list::<NetworkDoc>(NETWORKS)? {
        let had = net.gateways.remove(&node).is_some();
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
