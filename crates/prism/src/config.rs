//! `prism.toml`: node-local bootstrap settings.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "prism.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub node: NodeConfig,
    pub http: HttpConfig,
    pub libvirt: LibvirtConfig,
    pub storage: StorageConfig,
    pub pools: BTreeMap<String, PoolConfig>,
    pub cluster: ClusterSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClusterSection {
    /// Underlay hosts (IPs or names) other members reach this node at, in
    /// preference order. Empty behind NAT: peers learn it by roaming.
    pub advertise: Vec<String>,
    /// UDP port of `wg-prism`; the only port members need between them.
    pub wireguard_port: u16,
}

impl Default for ClusterSection {
    fn default() -> Self {
        Self {
            advertise: Vec::new(),
            wireguard_port: crate::cluster::mesh::DEFAULT_PORT,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// Display name; defaults to the host name.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub listen: Vec<SocketAddr>,
    /// Extra `Host` values accepted besides IP literals, `localhost` and
    /// the host name. `"*"` disables the check.
    pub allowed_hosts: Vec<String>,
    /// Cross-origin `Origin` values allowed to make writes.
    pub allowed_origins: Vec<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: vec![SocketAddr::from(([127, 0, 0, 1], 8080))],
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibvirtConfig {
    /// Daemon socket path, or `"auto"`.
    pub socket: String,
    pub uri: String,
    /// `"auto"` (kvm when `/dev/kvm` exists), `"kvm"` or `"qemu"`.
    pub domain_type: String,
    /// Explicit `<emulator>` path; libvirt picks one when unset.
    pub emulator: Option<PathBuf>,
    /// `qemu-img` binary.
    pub qemu_img: PathBuf,
    /// Explicit UEFI firmware image, used as a read-only ROM loader instead
    /// of libvirt's firmware auto-selection.
    pub uefi_loader: Option<PathBuf>,
}

impl Default for LibvirtConfig {
    fn default() -> Self {
        Self {
            socket: "auto".into(),
            uri: "qemu:///system".into(),
            domain_type: "auto".into(),
            emulator: None,
            qemu_img: "qemu-img".into(),
            uefi_loader: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Base directories under which pools may be created through the API.
    pub pool_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    #[serde(rename = "type", default = "default_pool_type")]
    pub kind: String,
    pub path: PathBuf,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_pool_type() -> String {
    "dir".into()
}

fn default_format() -> String {
    "qcow2".into()
}

impl Config {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        for (name, pool) in &self.pools {
            if !crate::storage::valid_name(name) {
                bail!("invalid pool name {name:?}");
            }
            if pool.kind != "dir" {
                bail!("pool {name}: only type = \"dir\" is supported");
            }
            if !pool.path.is_absolute() {
                bail!("pool {name}: path must be absolute");
            }
            if !matches!(pool.format.as_str(), "qcow2" | "raw") {
                bail!("pool {name}: format must be qcow2 or raw");
            }
        }
        if self.http.listen.is_empty() {
            bail!("http.listen is empty");
        }
        Ok(())
    }

    /// The libvirt socket to use, resolving `"auto"`.
    pub fn libvirt_socket(&self) -> PathBuf {
        if self.libvirt.socket != "auto" {
            return PathBuf::from(&self.libvirt.socket);
        }
        ["/run/libvirt/virtqemud-sock", "/run/libvirt/libvirt-sock"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/run/libvirt/virtqemud-sock"))
    }

    pub fn domain_type(&self) -> &str {
        match self.libvirt.domain_type.as_str() {
            "auto" if Path::new("/dev/kvm").exists() => "kvm",
            "auto" => "qemu",
            t => t,
        }
    }
}

/// The default `prism.toml` written by `prism init`.
pub fn default_toml(pool_path: &Path) -> String {
    format!(
        r#"[http]
# Loopback by default. Reach the UI with `tailscale serve`, an SSH tunnel or a
# management address; never 0.0.0.0.
listen = ["127.0.0.1:8080"]
allowed_hosts = []
allowed_origins = []

[libvirt]
socket = "auto"          # virtqemud-sock, then libvirt-sock
uri = "qemu:///system"
domain_type = "auto"     # kvm when /dev/kvm exists, else qemu (TCG)

[storage]
pool_roots = []          # directories under which the API may create pools

[cluster]
advertise = []           # underlay IPs/names other members reach this node at
wireguard_port = 51820   # the only port members need open between them

[pools.default]
type = "dir"
path = "{}"
format = "qcow2"
"#,
        pool_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_parses() {
        let cfg: Config =
            toml::from_str(&default_toml(Path::new("/var/lib/prism/pools/default"))).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.pools["default"].format, "qcow2");
        assert_eq!(cfg.http.listen[0].to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn rejects_relative_pool() {
        let cfg: Config = toml::from_str("[pools.a]\npath = \"rel\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }
}
