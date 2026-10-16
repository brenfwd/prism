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
- Guests on IPv6 networks may use stable-privacy, temporary or static addresses. The node learns them from the bridge's neighbour table and the guest agent, uses them for AAAA records and IPv6 port forwards, and falls back to the EUI-64 address.
- `allow_prefixes` takes IPv6 prefixes next to IPv4 ones.
- Trunk NICs get IPv6 source, neighbour advertisement and redirect checks on VLAN-tagged frames, and the bridge drops DHCPv6 server replies and, on trunks, tagged router advertisements.
- `prism --version` prints the short git hash and build date.
- `cargo xtask` runs a testbed of nested VMs for integration testing.

### Changed

- Crate metadata is filled in. `prism-libvirt`, `prism-iso` and `prism-net` can be published; the other crates have `publish = false`. The minimum Rust version is 1.89.

- Port forwards on Prism networks match the incoming interface. External connections are forwarded only when they arrive on the node's uplink for the guest's address family, host-originated ones go through the output chain, and hairpin connections are accepted only from the guest's own bridge. Traffic from `wg-prism` and from other networks' bridges is not forwarded.
### Behaviour notes

- Calls to libvirt time out after 600 seconds, except snapshot create, snapshot revert and the transfer step of a live migration. Those have no time limit. Cancelling the task aborts the libvirt job, and so does a job that shows no progress for `libvirt.job_stall_timeout_secs` (300 by default, 0 turns it off).
- A live migration that fails, stalls or is cancelled aborts the job on the source, cleans up the destination and resumes the source domain if it was running before. When the transfer finished but the destination cannot be reached, the source stays untouched and the migration intent stays until it is resolved.
- A port forward on a host port that Prism listens on (the `http.listen` ports, the WireGuard port, and cluster ports 7443 and 7444) is refused. URL downloads are capped at 256 GiB and refuse redirects from https to http.
- `prism serve` starts VMs with autostart only after it reads a boot ID that differs from the stored one. If `/proc/sys/kernel/random/boot_id` cannot be read, autostart is skipped and a message is logged.
- `prism serve` fails to start when an archive member directory cannot be prepared, and the raft log store fails to open when SQLite does not enter WAL mode.
- A cluster write that reaches a node without raft answers 503 `cluster_degraded` instead of 500.
- Clones and restores start without port forwards, a fixed IPv6 address or an IPv4 lease, because those belong to the original VM.
- Reverting a snapshot taken without memory restarts a running VM from the saved disks. A stopped VM stays stopped.
