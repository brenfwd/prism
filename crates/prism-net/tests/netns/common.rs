//! Shell helpers, throwaway network namespaces and the two-namespace `WireGuard` mesh fixture.

use ipnet::Ipv6Net;
use prism_net::Netlink;
use prism_net::nft::{BoundaryConfig, Nft};
use prism_net::wireguard::{InterfaceConfig, KeyPair, PeerConfig};
use std::fs::File;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsFd;
use std::process::Command;

/// The uid QEMU runs as in the boundary rules.
pub const QEMU_UID: u32 = 64055;

pub fn require_root() {
    assert!(
        rustix::process::geteuid().is_root(),
        "these tests need root (run with sudo)"
    );
}

pub fn run(cmd: &str) -> std::process::Output {
    let go = || {
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .expect("spawn sh")
    };
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(go),
        Err(_) => go(),
    }
}

/// Runs a shell command and returns stdout; panics on failure.
pub fn sh(cmd: &str) -> String {
    let out = run(cmd);
    assert!(
        out.status.success(),
        "`{cmd}` failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn sh_ok(cmd: &str) -> bool {
    run(cmd).status.success()
}

/// A network namespace that is deleted on drop.
pub struct Ns(String);

impl Ns {
    pub fn new(name: &str) -> Self {
        let _ = sh_ok(&format!("ip netns del {name} 2>/dev/null"));
        sh(&format!("ip netns add {name}"));
        sh(&format!("ip -n {name} link set lo up"));
        // No duplicate address detection: addresses are usable as soon as links are up.
        sh(&format!(
            "ip netns exec {name} sysctl -qw net.ipv6.conf.all.accept_dad=0 net.ipv6.conf.default.accept_dad=0"
        ));
        Self(name.to_owned())
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn exec(&self, cmd: &str) -> String {
        sh(&format!("ip netns exec {} {cmd}", self.0))
    }

    pub fn exec_ok(&self, cmd: &str) -> bool {
        sh_ok(&format!("ip netns exec {} {cmd}", self.0))
    }

    /// Runs `f` on a thread inside the namespace (with the current tokio runtime entered), so
    /// sockets it creates belong to the namespace.
    pub fn enter<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let path = format!("/run/netns/{}", self.0);
        let rt = tokio::runtime::Handle::try_current().ok();
        let go = move || {
            std::thread::spawn(move || {
                let fd = File::open(&path).expect("open netns");
                rustix::thread::move_into_link_name_space(
                    fd.as_fd(),
                    Some(rustix::thread::LinkNameSpaceType::Network),
                )
                .expect("setns");
                let _guard = rt.as_ref().map(tokio::runtime::Handle::enter);
                f()
            })
            .join()
            .expect("netns thread panicked")
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(go),
            Err(_) => go(),
        }
    }

    pub fn netlink(&self) -> Netlink {
        self.enter(|| Netlink::connect().expect("netlink"))
    }

    pub fn nft(&self) -> Nft {
        self.enter(|| Nft::new().expect("nft socket"))
    }
}

impl Drop for Ns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
    }
}

pub fn mesh_prefix() -> Ipv6Net {
    "fd12:3456:789a::/48".parse().unwrap()
}

pub fn mesh_addr(n: u16) -> Ipv6Addr {
    Ipv6Addr::new(0xfd12, 0x3456, 0x789a, 1, 0, 0, 0, n)
}

/// Two namespaces joined by a veth "underlay" (`10.99.0.0/24`, `fd99::/64`) with `wg-prism`
/// configured on both through this crate.
pub struct Mesh {
    pub a: Ns,
    pub b: Ns,
    pub nla: Netlink,
    pub nlb: Netlink,
    pub keys: [KeyPair; 2],
}

impl Mesh {
    pub fn peers_of(&self, side: usize) -> Vec<PeerConfig> {
        let other = 1 - side;
        vec![PeerConfig {
            public_key: self.keys[other].public,
            endpoint: Some(SocketAddr::from((
                Ipv4Addr::new(10, 99, 0, other as u8 + 1),
                51820,
            ))),
            mesh_address: mesh_addr(other as u16 + 1),
            keepalive: Some(25),
        }]
    }

    pub fn wg_config(&self, side: usize) -> InterfaceConfig {
        InterfaceConfig {
            name: "wg-prism".into(),
            private_key: self.keys[side].private.clone(),
            listen_port: 51820,
            fwmark: None,
            mtu: 1420,
            address: Ipv6Net::new(mesh_addr(side as u16 + 1), 64).unwrap(),
        }
    }

    pub async fn new(prefix: &str) -> Self {
        let a = Ns::new(&format!("{prefix}-a"));
        let b = Ns::new(&format!("{prefix}-b"));
        sh(&format!(
            "ip link add va netns {} type veth peer name vb netns {}",
            a.name(),
            b.name()
        ));
        for (ns, dev, n) in [(&a, "va", 1), (&b, "vb", 2)] {
            ns.exec(&format!("ip addr add 10.99.0.{n}/24 dev {dev}"));
            ns.exec(&format!("ip -6 addr add fd99::{n}/64 dev {dev} nodad"));
            ns.exec(&format!("ip link set {dev} up"));
        }
        // The first neighbour resolution on a fresh veth can take over a second.
        for target in ["10.99.0.2", "fd99::2"] {
            assert!(
                (0..10).any(|_| a.exec_ok(&format!("ping -c 1 -W 1 {target}"))),
                "underlay {target} unreachable"
            );
        }
        let mesh = Self {
            nla: a.netlink(),
            nlb: b.netlink(),
            a,
            b,
            keys: [KeyPair::generate(), KeyPair::generate()],
        };
        for side in 0..2 {
            let nl = mesh.nl(side);
            nl.ensure_wireguard(&mesh.wg_config(side)).await.unwrap();
            let changes = nl
                .set_wireguard_peers("wg-prism", &mesh.peers_of(side))
                .await
                .unwrap();
            assert_eq!(changes.added, 1);
        }
        mesh
    }

    pub fn nl(&self, side: usize) -> &Netlink {
        if side == 0 { &self.nla } else { &self.nlb }
    }
}

pub fn boundary_cfg() -> BoundaryConfig {
    BoundaryConfig {
        mesh_prefix: mesh_prefix(),
        wg_interface: "wg-prism".into(),
        qemu_uid: Some(QEMU_UID),
    }
}
