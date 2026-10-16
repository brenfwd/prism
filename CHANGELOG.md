# Changelog

All notable changes to Prism are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-16

First release.

### Added

- A single static binary, `prism`, with an embedded web UI. It talks to libvirt through its own async Rust client for the remote RPC protocol (`prism-libvirt`), so the libvirt C library is not linked.
- VM lifecycle, snapshots (including saved memory), checkpoints, hotplug, cloning, auto-ballooning and cloud-init seed ISOs written by `prism-iso`.
- Browser console: an RFB decoder in Rust compiled to WebAssembly, with WebGL2 rendering in a worker.
- Disk pools, volume uploads and downloads from a URL with checksum verification (rustls), and image format reporting.
- Clustering with a replicated log (openraft), copy-paste invites, a join fingerprint check, hub transfer, planned leave and member removal with progress. Any node serves any request.
- A WireGuard mesh over IPv6 ULA addresses (`prism-net`), with VXLAN, bridges, per-NIC VLANs and trunks.
- Networks with DHCP, DNS, NAT, router advertisements, SLAAC, optional ULA /64 prefixes, NAT66, per-VLAN subnets and nftables firewall rules.
- Port forwards on Prism networks with nftables DNAT, for IPv4 and IPv6 guests, plus passt as an opt-in for user-mode NICs.
- Cold move and live migration between nodes, including VMs with snapshots and local disks.
- Archive pools: a cluster-wide namespace over member directories, with replicas, drain, rebalance and a weekly scrub.
- Backups to archive pools with incremental chains and restore.
- `prism init`, `serve`, `doctor`, `cluster`, `join` and `leave` subcommands.
- An OpenAPI document served by the API, with `docs/API.md`.
- A user guide in `docs/guide`, the design document in `docs/DESIGN.md`, a systemd unit and a commented `prism.toml` example in `contrib/`.
- `prism --version` prints the short git hash and build date.
- `cargo xtask` runs a testbed of nested VMs for integration testing.

### Changed

- Crate metadata is filled in. `prism-libvirt`, `prism-iso` and `prism-net` can be published; the other crates have `publish = false`. The minimum Rust version is 1.89.
