//! `prism.toml`: the settings a node reads at startup.
//!
//! The file lives in the config directory given with `--config`. Unknown keys
//! are rejected so a typo fails at startup instead of being ignored. Every
//! section is optional and falls back to the defaults below. Settings that
//! must be the same across a cluster live in the replicated state, not here.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// File name of the config inside the config directory.
pub const CONFIG_FILE: &str = "prism.toml";

/// The parsed `prism.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `[node]`: how this node presents itself.
    pub node: NodeConfig,
    /// `[http]`: listen addresses and request filtering.
    pub http: HttpConfig,
    /// `[libvirt]`: how to reach libvirt and which emulator to use.
    pub libvirt: LibvirtConfig,
    /// `[storage]`: where pools may be created.
    pub storage: StorageConfig,
    /// `[pools.<name>]`: disk pools fixed by the config file.
    pub pools: BTreeMap<String, PoolConfig>,
    /// `[cluster]`: how other members reach this node.
    pub cluster: ClusterSection,
    /// `[download]`: which addresses URL downloads may reach.
    pub download: DownloadConfig,
    /// Directories this node contributes to archive pools.
    pub archive_members: BTreeMap<String, ArchiveMemberConfig>,
}

/// `[archive_members.<name>]`: one directory on this node that holds archive
/// files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveMemberConfig {
    /// The archive pool this directory belongs to (created on first use).
    pub pool: String,
    /// Absolute path of the directory.
    pub path: PathBuf,
    /// Relative share of new files.
    #[serde(default = "one")]
    pub weight: f64,
    /// Free space to keep; the member takes no new files below it.
    #[serde(default, with = "size_str")]
    pub reserve: u64,
}

fn one() -> f64 {
    1.0
}

/// Sizes as bytes or strings like `50GiB`.
mod size_str {
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `with` signature
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(*v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum V {
            N(u64),
            S(String),
        }
        match V::deserialize(d)? {
            V::N(n) => Ok(n),
            V::S(s) => super::parse_size(&s).map_err(serde::de::Error::custom),
        }
    }
}

/// Parses a size such as `123`, `10k`, `50GiB` or `1.5T` into bytes. Units
/// are binary (1 k is 1024 bytes) and case-insensitive; fractions round down.
/// Fails on a missing number or an unknown unit.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("bad size {s:?}"))?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        u => return Err(format!("unknown size unit {u:?}")),
    };
    Ok((n * mult as f64) as u64)
}

/// `[cluster]` settings.
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

/// `[download]` settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DownloadConfig {
    /// Let URL downloads reach private ranges (RFC 1918, unique local and
    /// carrier-grade NAT). Loopback, link-local, the mesh and this node's own
    /// addresses stay blocked.
    pub allow_private: bool,
    /// Networks that downloads may always reach, even when blocked above.
    pub allow: Vec<ipnet::IpNet>,
}

/// `[node]` settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// Display name; defaults to the host name.
    pub name: Option<String>,
}

/// `[http]` settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Addresses to serve on; default `127.0.0.1:8080`. Must not be empty.
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

/// `[libvirt]` settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibvirtConfig {
    /// Daemon socket path, or `"auto"`.
    pub socket: String,
    /// Libvirt connection URI, default `qemu:///system`.
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
    /// Seconds a snapshot, a revert or a live migration may go without
    /// progress before Prism aborts it. 0 turns the check off. There is no
    /// limit on the total time.
    pub job_stall_timeout_secs: u64,
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
            job_stall_timeout_secs: 300,
        }
    }
}

/// `[storage]` settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Base directories under which pools may be created through the API.
    pub pool_roots: Vec<PathBuf>,
}

/// `[pools.<name>]`: a disk pool defined in the config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Pool type; only `dir` exists.
    #[serde(rename = "type", default = "default_pool_type")]
    pub kind: String,
    /// Absolute path of the pool directory.
    pub path: PathBuf,
    /// Disk image format for new volumes: `qcow2` (default) or `raw`.
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
    /// Reads and validates `prism.toml` in `dir`.
    ///
    /// Fails if the file is unreadable, is not valid TOML for this schema, or
    /// breaks a rule: pool and member names must be valid, paths absolute,
    /// pool type `dir`, pool format `qcow2` or `raw`, member weight positive
    /// and `http.listen` non-empty.
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
        for (name, m) in &self.archive_members {
            if !crate::storage::valid_name(name) || !crate::storage::valid_name(&m.pool) {
                bail!("archive member {name}: invalid member or pool name");
            }
            if !m.path.is_absolute() {
                bail!("archive member {name}: path must be absolute");
            }
            if m.weight.is_nan() || m.weight <= 0.0 {
                bail!("archive member {name}: weight must be positive");
            }
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

    /// How long a libvirt job may go without progress before it is aborted;
    /// zero means never.
    pub fn job_stall_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.libvirt.job_stall_timeout_secs)
    }

    /// The libvirt domain type to use, resolving `auto` to `kvm` when
    /// `/dev/kvm` exists and `qemu` (software emulation) otherwise.
    pub fn domain_type(&self) -> &str {
        match self.libvirt.domain_type.as_str() {
            "auto" if Path::new("/dev/kvm").exists() => "kvm",
            "auto" => "qemu",
            t => t,
        }
    }
}

/// The `prism.toml` text written by `prism init`, with the default pool at
/// `pool_path`.
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

[download]
allow_private = false    # URL downloads refuse private, loopback and mesh addresses
allow = []               # CIDRs downloads may always reach

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

    /// `contrib/prism.toml.example` with every commented-out setting enabled.
    #[test]
    fn example_toml_parses() {
        let text = include_str!("../../../contrib/prism.toml.example");
        let enabled: String = text
            .lines()
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if rest.starts_with('[') || rest.contains(" = ") => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let cfg: Config = toml::from_str(&enabled).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            cfg.pools["default"].path,
            Path::new("/var/lib/prism/pools/default")
        );
        assert_eq!(cfg.archive_members["disk1"].reserve, 50 << 30);
        toml::from_str::<Config>(text).unwrap().validate().unwrap();
    }

    #[test]
    fn default_toml_parses() {
        let cfg: Config =
            toml::from_str(&default_toml(Path::new("/var/lib/prism/pools/default"))).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.pools["default"].format, "qcow2");
        assert_eq!(cfg.http.listen[0].to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("50GiB").unwrap(), 50 << 30);
        assert_eq!(parse_size("1.5t").unwrap(), 3 << 39);
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert!(parse_size("5 parsecs").is_err());
        let cfg: Config = toml::from_str(
            "[archive_members.a]\npool = \"archive\"\npath = \"/srv/a\"\nreserve = \"10GiB\"\n",
        )
        .unwrap();
        assert_eq!(cfg.archive_members["a"].reserve, 10 << 30);
        assert!((cfg.archive_members["a"].weight - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_bad_settings() {
        for toml in [
            "[http]\nlisten = []\n",
            "[pools.a]\npath = \"/x\"\nformat = \"vmdk\"\n",
            "[pools.a]\npath = \"/x\"\ntype = \"lvm\"\n",
            "[archive_members.a]\npool = \"p\"\npath = \"/x\"\nweight = 0\n",
            "[nope]\n",
        ] {
            let cfg: Result<Config, _> = toml::from_str(toml);
            assert!(cfg.is_err() || cfg.unwrap().validate().is_err(), "{toml}");
        }
    }

    #[test]
    fn rejects_relative_pool() {
        let cfg: Config = toml::from_str("[pools.a]\npath = \"rel\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }
}
