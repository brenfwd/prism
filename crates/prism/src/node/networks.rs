//! Network operations: the user-facing side of `crate::network`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use ipnet::Ipv4Net;
use serde::Serialize;
use uuid::Uuid;

use super::backups::unix_now;
use super::{Node, NodeError, Result};
use crate::network::{
    CheckNetwork, CreateNetwork, DEFAULT_NETWORK, NetworkDoc, UpdateNetwork, host, ipam,
    valid_network_name, valid_subnet,
};
use crate::spec::NicNetwork;
use crate::state::Command;

/// A network as the API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkView {
    #[serde(flatten)]
    pub network: NetworkDoc,
    pub bridge: String,
    pub domain: String,
    /// Names of the VMs with a NIC on it.
    pub vms: Vec<String>,
}

/// The verdict of a dry-run network check, for live form validation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkCheck {
    pub name_error: Option<String>,
    pub subnet_error: Option<String>,
    /// A free subnet to offer when the one asked for does not work.
    pub suggestion: Option<Ipv4Net>,
}

impl Node {
    /// Checks a name and subnet without creating anything. `id` is the
    /// network being edited, which is not its own conflict.
    pub fn check_network(&self, req: &CheckNetwork) -> Result<NetworkCheck> {
        let nets: Vec<_> = self
            .state
            .networks()?
            .into_iter()
            .filter(|n| Some(n.id) != req.id)
            .collect();
        let mut out = NetworkCheck::default();
        if let Some(name) = req.name.as_deref() {
            if !valid_network_name(name) {
                out.name_error = Some(
                    "Use 1 to 32 lowercase letters, digits or hyphens, not starting or ending with a hyphen.".into(),
                );
            } else if nets.iter().any(|n| n.name == name) {
                out.name_error = Some(format!("A network named {name} already exists."));
            }
        }
        let mut taken: Vec<_> = nets.iter().map(|n| n.subnet).collect();
        let others = taken.len();
        taken.extend(host::foreign_subnets(&host::routes()));
        if let Some(s) = req.subnet {
            out.subnet_error = valid_subnet(s).err().or_else(|| {
                let hit = ipam::conflicts(s, &taken)?;
                let who = if taken[..others].contains(&hit) {
                    "another Prism network"
                } else {
                    "a network this host already routes to"
                };
                Some(format!("{s} overlaps {hit}, used by {who}."))
            });
            if out.subnet_error.is_some() {
                taken.push(ipam::CGNAT);
                out.suggestion = ipam::pick_subnet(&taken);
            }
        }
        Ok(out)
    }

    /// The bridge of each network, for rendering NICs.
    pub(crate) fn network_bridges(&self) -> HashMap<Uuid, String> {
        self.state
            .networks()
            .unwrap_or_default()
            .into_iter()
            .map(|n| (n.id, n.bridge()))
            .collect()
    }

    fn network_view(&self, network: NetworkDoc) -> Result<NetworkView> {
        let vms = self
            .state
            .vms()?
            .into_iter()
            .filter(|v| {
                v.nics
                    .iter()
                    .any(|n| n.network.network_id() == Some(network.id))
            })
            .map(|v| v.name)
            .collect();
        Ok(NetworkView {
            bridge: network.bridge(),
            domain: network.domain(),
            network,
            vms,
        })
    }

    pub fn networks(&self) -> Result<Vec<NetworkView>> {
        self.state
            .networks()?
            .into_iter()
            .map(|n| self.network_view(n))
            .collect()
    }

    /// Finds a network by ID or name.
    pub fn network(&self, id_or_name: &str) -> Result<NetworkView> {
        let nets = self.state.networks()?;
        let net = Uuid::parse_str(id_or_name)
            .ok()
            .and_then(|id| nets.iter().find(|n| n.id == id))
            .or_else(|| nets.iter().find(|n| n.name == id_or_name))
            .cloned()
            .ok_or_else(|| NodeError::NotFound(format!("network {id_or_name}")))?;
        self.network_view(net)
    }

    pub async fn create_network(&self, req: CreateNetwork) -> Result<NetworkView> {
        let nets = self.state.networks()?;
        let name = req.name.unwrap_or_else(|| {
            if nets.is_empty() {
                DEFAULT_NETWORK.into()
            } else {
                (1..=nets.len() + 1)
                    .map(|i| format!("net{i}"))
                    .find(|n| nets.iter().all(|x| x.name != *n))
                    .unwrap_or_default()
            }
        });
        let subnet = if let Some(s) = req.subnet {
            let check = self.check_network(&CheckNetwork {
                id: None,
                name: None,
                subnet: Some(s),
            })?;
            if let Some(e) = check.subnet_error {
                return Err(NodeError::Conflict(e));
            }
            s
        } else {
            let mut taken: Vec<_> = nets.iter().map(|n| n.subnet).collect();
            taken.extend(host::foreign_subnets(&host::routes()));
            taken.push(ipam::CGNAT);
            ipam::pick_subnet(&taken)
                .ok_or_else(|| NodeError::Conflict("no free subnet left".into()))?
        };
        let doc = NetworkDoc {
            id: Uuid::new_v4(),
            name,
            kind: req.kind,
            vni: 0,
            subnet,
            gateways: BTreeMap::default(),
            egress: req.egress,
            dhcp: req.dhcp.unwrap_or(true),
            created: unix_now(),
        };
        let id = doc.id;
        self.state
            .apply(Command::PutNetwork {
                network: Box::new(doc),
                now: unix_now(),
            })
            .await?;
        self.network(&id.to_string())
    }

    pub async fn update_network(&self, id: &str, req: UpdateNetwork) -> Result<NetworkView> {
        let mut doc = self.network(id)?.network;
        if let Some(n) = req.name {
            doc.name = n;
        }
        if let Some(e) = req.egress {
            doc.egress = e;
        }
        if let Some(d) = req.dhcp {
            doc.dhcp = d;
        }
        let id = doc.id;
        self.state
            .apply(Command::PutNetwork {
                network: Box::new(doc),
                now: unix_now(),
            })
            .await?;
        self.network(&id.to_string())
    }

    pub async fn delete_network(&self, id: &str) -> Result<()> {
        let id = self.network(id)?.network.id;
        self.state
            .apply(Command::DeleteNetwork {
                id,
                now: unix_now(),
            })
            .await?;
        Ok(())
    }

    /// The network a new VM's first NIC joins when the request names none:
    /// the default network, or QEMU user networking without one.
    pub(crate) fn default_nic_network(&self, requested: Option<NicNetwork>) -> NicNetwork {
        requested.unwrap_or_else(|| {
            self.state
                .networks()
                .unwrap_or_default()
                .into_iter()
                .find(|n| n.name == DEFAULT_NETWORK)
                .map_or_else(NicNetwork::user, |n| NicNetwork::Network {
                    network: n.id,
                    ipv4: None,
                    egress: None,
                })
        })
    }

    /// Creates the default network the first time this cluster runs, so a
    /// new VM has internet out of the box.
    pub(super) async fn seed_default_network(self: Arc<Self>) {
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            match self.state.networks_used() {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!("default network: {e:#}");
                    continue;
                }
            }
            match self.create_network(CreateNetwork::default()).await {
                Ok(n) => {
                    tracing::info!(subnet = %n.network.subnet, "created the default network");
                    return;
                }
                Err(NodeError::Conflict(e)) => {
                    tracing::debug!("default network: {e}");
                    return;
                }
                Err(e) => tracing::warn!("creating the default network: {e}"),
            }
        }
    }
}
