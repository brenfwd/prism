# Prism — libvirt/KVM Cluster Manager: Design Doc

Oct 14, 2026 · @Brenden Forward

## Summary, goals and non-goals

Prism is one static Rust binary, `prism`, that runs on every KVM host. It talks to the local libvirt daemon over libvirt's own RPC protocol, implemented natively in Rust, so the libvirt C library is never linked. Each node serves a REST + SSE API and an embedded React/Tailwind PWA, and nodes mesh over mutual-TLS gRPC.

**Non-goals** (as specified): auth, users, HTTP TLS termination, HTTP load balancing. Every HTTP request is trusted, including `X-Forwarded-*` headers.

**Feasibility corrections the research surfaced:**

- The `virt` crate links C libvirt (plus glib, gnutls, libxml2), so it is rejected in favour of a native RPC client.
- H.264/WebCodecs console streaming is impossible with QEMU's VNC server, which never sends H.264.
- A bridged-mode per-NIC egress override cannot stay on the same L2 segment as the rest of its switch.
- libvirt networks (dnsmasq) and libvirt storage drivers are avoided: they are host-local and depend on external binaries.

### Requirements traceability

| Requirement | How it is met | Section |
| --- | --- | --- |
| Static binary, almost any modern Linux | musl + cargo-zigbuild; only ring, SQLite and mimalloc as static C; kernel ≥ 5.4 | Dependencies |
| React + Tailwind SPA/PWA, accelerated displays | Rust→WASM RFB decoder in a worker, WebGL2 on OffscreenCanvas | Web UI |
| Full API with SSE, no polling | `/api/v1` REST, snapshot-then-stream SSE with replay | API |
| Optional clustering, any node serves any request | Ownership router forwards over gRPC; cluster-wide event fan-in | Clustering |
| Memory ballooning | virtio-balloon with free page reporting and a PSI-driven auto loop | VM lifecycle |
| File-level archive load balancing | Archive pools place whole files by weighted free space | Storage |
| gRPC + Protobuf over self-signed TLS | tonic + rustls(ring), rcgen cluster CA, pinned join tokens | Transport |
| Portable config dir as argument | `prism serve --config <dir>`; copy the dir to move a node | Config |
| Easily move VMs and files | Live migration, cold move, archive drain and transfers | Live ops, Storage |
| Storage and pools managed in UI | Local pools and archive pools screens | Web UI, Storage |
| Move and snapshot without downtime | Managed migration (≤ 300 ms pause), live external snapshots, push backups | Live ops |
| Host and bridge networking | NAT and bridged switch types, safe host-bridge wizard | Networking |
| Cluster virtual switch with remote egress | VXLAN overlay; per-NIC egress via per-node gateway VIPs | Networking |
| Internal VM-to-VM switches | Isolated switches, VLAN access/trunk ports | Networking |
| Idiomatic Rust, 0 clippy warnings | `clippy -D warnings` gate, workspace lints | Build |
| Few non-pure-Rust crates, no OpenSSL | cargo-deny bans and a static-ELF check in CI | Dependencies |
| Hub-spoke and consensus clusters, degraded spokes | Single-voter raft + learners, or 3/5-voter raft; frozen read-only cache | Clustering |
| Disks local to the VM's host | Ownership invariant; migration copies disks via NBD | Storage, Live ops |
| Backups on any node, for any VM | Backups stream into any archive pool | Storage, Live ops |

## System architecture

Prism is a single process on a tokio runtime, built from one Cargo workspace. Every node runs the same binary with the same features; cluster role is data, not a build option.

*Diagram: architecture · two nodes joined by the WireGuard mesh (see the [live design doc](https://claude.ai/code/artifact/c0a1e8f6-a56b-42bb-8a73-796e86f09cc7)).*

Node A accepts the request and forwards it to node B, which owns the VM. B's `prism-node` drives its local libvirt over the native RPC client. Events flow back so A's SSE stream reflects the change.

| Crate | Role |
| --- | --- |
| `prism-xdrgen` | Build-time parser and code generator for libvirt `.x` files (about 1.5k lines; existing XDR crates are stale) |
| `prism-libvirt` | Async libvirt RPC client: framing, serial dispatch, keepalive, events, streams |
| `prism-proto` | `.proto` files compiled with tonic-prost-build + protox (no protoc) |
| `prism-state` | Replicated `Command` log applied to SQLite; VM ownership and epochs; archive index |
| `prism-cluster` | Mode policy, membership, degraded-mode gate, request router, event fan-in |
| `prism-tls` | rcgen cluster CA, join tokens, pinned and revocation-aware verifiers |
| `prism-node` | VM reconciler, domain XML renderer, balloon loop, migration/snapshot/backup orchestration, tasks |
| `prism-storage` | Local disk pools, archive pools, BLAKE3 chunked transfer, ISO cache |
| `prism-net` | Bridges, VXLAN, FDB reconciler, embedded DHCP/DNS, nftables-netlink layer, WireGuard |
| `prism-api` | axum REST, SSE, WebSockets, OpenAPI (utoipa), embedded UI assets |
| `prism-rfb-wasm` | Rust→WASM VNC (RFB) decoder used by the browser console |
| `prism` (bin) | clap CLI: `serve --config <dir>`, `init`, `join`, `cluster promote`, `cluster recover`, `doctor` |
| `web/` | React + TypeScript + Vite + Tailwind SPA |

**Request flow.** The browser calls the HTTP API on any node. The router resolves which node owns the target resource. Local requests go through `prism-node` to `prism-libvirt` and the libvirt daemon; remote ones are forwarded over gRPC to the owner and run through the same handlers there.

**Event flow.** libvirt domain events, task progress and state changes enter a per-node event bus. Each node subscribes to every peer's bus, so any node's SSE stream covers the whole cluster.

## Dependency strategy and static build

Prism builds as a fully static musl binary with three statically compiled C components: ring, SQLite and mimalloc. Nothing links OpenSSL, aws-lc or libvirt, and the binary has no `NEEDED` entries.

### Reaching libvirt without linking it

Prism speaks libvirt's remote RPC protocol (XDR frames over the local UNIX socket) directly. This is the approach DigitalOcean's pure-Go `go-libvirt` uses in production.

- **Codegen.** `prism-xdrgen` parses vendored `remote_protocol.x`, `qemu_protocol.x` and `virkeepaliveprotocol.x` from a pinned libvirt tag. It emits structs, XDR encode/decode traits and typed async stubs for an allowlist of about 70 procedures, not all 450+.
- **Wire layer.** Frames carry a length and a header (`prog, vers, proc, type, serial, status`). One reader task dispatches replies by serial, so long calls like `Perform3` run while `GetJobStats` and `AbortJob` proceed.
- **Keepalive.** The reader answers server PINGs immediately and sends its own every 5 s. After 5 misses it reconnects and re-registers event callbacks.
- **Socket.** Prism tries `/run/libvirt/virtqemud-sock` first, then `/run/libvirt/libvirt-sock`. It uses only the QEMU driver, so it never needs virtnetworkd or virtstoraged.
- **Auth.** Prism runs as root; polkit and none are accepted. SASL-only setups fail fast with a remediation message.
- **Events.** `DOMAIN_EVENT_CALLBACK_REGISTER_ANY` covers lifecycle, reboot, balloon change, job completed, block job, migration iteration, device add/remove and agent lifecycle.

libvirt maintainers call this protocol private, but procedures are only ever appended. Prism pins libvirt ≥ 8.0, feature-detects newer calls and tests against a version matrix.

### Crates

| Crate | Purpose | C code? |
| --- | --- | --- |
| tokio, tokio-util, bytes | Runtime and codecs | No |
| axum, tower-http, rust-embed | REST, SSE, WebSocket, embedded UI | No |
| tonic 0.14 (`tls-ring`), tonic-prost, prost, protox | gRPC without protoc | No |
| rustls 0.23 (ring provider), tokio-rustls | TLS | ring: C/asm via `cc`, static |
| rcgen, x509-parser | Cluster CA and node certs | via ring |
| rusqlite (`bundled`) | Local and replicated state | SQLite amalgamation, static |
| openraft 0.9.x (pinned) | Consensus and hub-spoke replication | No |
| rtnetlink, netlink-packet-route, netlink-packet-wireguard | Bridges, VXLAN, FDB, VLANs, WireGuard | No |
| quick-xml + serde | Domain XML | No |
| blake3, lz4\_flex | Checksums, transfer compression | No (blake3 `pure` optional) |
| rustix | Sockets, `SEEK_DATA/HOLE`, statx | No |
| dhcproto, hickory-proto | Embedded DHCP and DNS | No |
| zbus | NetworkManager checkpoints | No |
| mimalloc | Global allocator | Vendored C, static |
| tracing, clap, thiserror, uuid, serde\_json | Basics | No |

ring is chosen over aws-lc-rs because it needs only `cc`, not cmake and clang. mimalloc replaces musl's malloc, whose global lock throttles TLS and migration throughput; it stays behind a default-on feature so it can be benchmarked.

### Build and enforcement

- `cargo-zigbuild` targets `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` with no Docker.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.
- cargo-deny bans `openssl*`, `native-tls`, `aws-lc*`, `libz-sys`, `zstd-sys`, `virt*`, `bindgen` and `pkg-config`. Only `ring`, `libsqlite3-sys` and `libmimalloc-sys` may be `-sys` crates.
- CI runs `readelf -d` on the binary and fails on any `NEEDED` entry.

### Host prerequisites

`prism doctor` checks each of these and prints a fix:

| Requirement | Standalone | Clustered |
| --- | --- | --- |
| libvirt ≥ 8.0, QEMU ≥ 6.2, `qemu-img` | Yes | Yes |
| Kernel | ≥ 5.4 | ≥ 5.6 (WireGuard) |
| Modules: bridge, vxlan, tun, vhost\_net, nf\_tables (+ nat, masq, ct) | Yes | Yes |
| Module: wireguard | No | Yes |
| PSI (`/proc/pressure/memory`) | Optional | Optional |
| Root | Yes | Yes |

## Config directory and persistence

The config directory, passed as `prism serve --config <dir>`, is the node's entire identity: copying it to new hardware moves the node. Only bootstrap settings are hand-edited; everything else is desired state changed through the API.

```
<config>/
  prism.toml          # node-local bootstrap only (operator-edited)
  tls/                # ca.crt, trust.pem, node.crt, node.key, [ca.key on CA holders]
  wg/                 # wg.key (WireGuard private key, 0600) — clustered nodes only
  data/
    identity          # node UUID + machine-id fingerprint
    state.db          # replicated state machine (SQLite, WAL)
    raft.db           # raft log, vote, snapshot metadata (absent when standalone)
    node.db           # node-local: tasks, upload sessions, ISO cache index, event ring
  cache/              # content-addressed ISO cache (path overridable)
  run/prism.lock      # flock; refuses a second instance
```

```toml
[node]
name = "kvm-a"

[http]
listen = ["0.0.0.0:8080"]

[cluster]               # topology lives in data/, not here
listen = "0.0.0.0:7443" # join + bootstrap; all other traffic rides WireGuard
advertise = ["203.0.113.11"]
wireguard_port = 51820

[libvirt]
socket = "auto"         # virtqemud-sock, then libvirt-sock

[pools.default]         # local VM disk pool
type = "dir"
path = "/var/lib/prism/pools/default"
format = "qcow2"

[archive_members.a1]    # contributes a directory to a cluster archive pool
pool = "archive"
path = "/srv/archive"
weight = 1.0
reserve = "50GiB"
```

### What lives where

| Data | Location | Changed by |
| --- | --- | --- |
| Listen addresses, libvirt socket, local pool and archive-member paths | `prism.toml` | Operator |
| Cluster mode and membership | `data/state.db` | `prism init / join / cluster …` |
| VMs, switches, archive pools, schedules, IPAM, revocations | `data/state.db` (replicated) | API |
| Tasks, uploads, ISO cache index, SSE replay ring | `data/node.db` | Prism |
| Raft log | `data/raft.db` | Prism |

Cluster mode is never in the toml, so the toml and the stored state cannot disagree. Schema changes run as forward-only SQLite migrations, keyed by `user_version`, at startup.

### Portability and safety

- `identity` records the machine-id. On a mismatch Prism refuses to start unless run with `--adopt`, so a cloned directory cannot run as two nodes.
- `prism backup-config <file>` takes a consistent SQLite online backup plus `tls/` and `wg/`, for disaster recovery of a node.

## Clustering

All three modes share one replicated-state engine; they differ only in who may vote. Every config write is a versioned protobuf `Command` applied deterministically to `state.db`, in the same transaction that records `applied_index`.

| Mode | Voters | Other nodes | Writes go to |
| --- | --- | --- | --- |
| Standalone | None; commands apply directly | — | Local node |
| Hub-spoke | The hub only | Spokes, as raft learners | Hub |
| Consensus | 3 or 5 peers | Extra nodes, as learners | Raft leader |

Hub-spoke is a single-voter openraft group. That gives spokes the required "pull all config from the hub" for free: a learner that falls behind log compaction receives `InstallSnapshot`, then the tail. Every node, including the hub, runs VMs. Clustering requires the WireGuard mesh described under Transport.

### Desired vs runtime state

- **Replicated (desired):** VM specs, VM ownership `(host_node, owner_epoch)`, switches and VNIs, IPAM, archive pools and file index, schedules, members, WireGuard public keys, join-token hashes, revocations.
- **Node-authoritative (runtime):** power state, stats, pool usage, free space, task progress. These never enter raft.

Each node pushes runtime deltas to every peer over `NodeStatus.Stream` and keeps an in-memory cluster view. The full mesh is supported up to 32 nodes.

Writes carry an `expected_revision`, which appears in HTTP as `ETag` / `If-Match`.

### Degraded mode

A node is degraded when a spoke cannot reach the hub, or when consensus quorum is lost. Config freezes to the last applied revision. Responses carry `X-Prism-Degraded: true` and `stale_since`.

| Allowed while degraded | Denied (503 `cluster_degraded`) |
| --- | --- |
| All reads, from cache, marked stale | Create, edit or delete anything |
| Power actions on existing VMs (start, shutdown, stop, reboot, reset, pause, resume) on reachable owners | Move or migrate |
| Console and serial | Snapshot and backup creation |
| Archive downloads, ISO cache use | Archive upload and delete |
| Task cancel, local event stream | Join, leave, pool changes |

Scheduled backups that fall in a degraded window are recorded as failed tasks.

### Changing the hub and recovering quorum

- **Planned:** `prism cluster promote <node>` adds the target as a voter, removes the old hub's vote and demotes the old hub to learner. These are two membership changes with no data loss.
- **Forced:** `prism cluster promote --force` (hub dead) or `prism cluster recover --force-single` (quorum lost) re-initialises membership from the most up-to-date survivor and bumps `cluster_generation`.
- **Fencing:** a returning node that sees a higher generation demotes itself. It writes any un-replicated entries to `data/lost-<gen>.jsonl`; they are reported, never merged.

### Any node serves any request

The router resolves the owner of each request:

| Target | Owner |
| --- | --- |
| VM operation | `host_node` |
| Local pool or volume | Its node |
| Archive file read | A healthy replica holder |
| Config write | Hub or raft leader |
| Task | The node running it |

REST calls are forwarded verbatim over `HttpTunnel.Forward` and run through the owner's own axum handlers. `X-Prism-Forwarded-By` enforces a one-hop limit. Console, serial and file streams are proxied byte-for-byte over bidirectional gRPC streams with backpressure.

### Cluster-wide events

Each node subscribes to every peer with `Events.Subscribe(since = node, boot_epoch, seq)` and merges those streams with its own. After a reconnect it resumes from the last sequence. If the peer's buffer has rolled over, it fetches that peer's runtime snapshot and emits `resync`.

### A VM never runs on two nodes

1. A node starts a VM only if committed state says it owns that VM at the current epoch. Ownership is frozen while degraded.
2. Moves and migrations commit `owner_epoch + 1` before the destination may start the domain.
3. A reconciler runs at boot and every 30 s. It force-stops and quarantines any Prism domain the node does not own, keeping its disks.
4. libvirt autostart is never used; Prism decides what starts after a host reboot.

## Inter-node transport and security

Clustering requires a kernel WireGuard mesh, `wg-prism`, and every byte between nodes travels inside it: gRPC, bulk data and the VXLAN overlay. gRPC additionally runs mutual TLS with the cluster's self-signed CA, as required. Standalone nodes need neither.

### Layers

| Layer | Carries | Port (on the mesh) | Protection |
| --- | --- | --- | --- |
| WireGuard `wg-prism` | Everything below | UDP 51820 (public) | Curve25519 peer keys, ChaCha20-Poly1305 |
| gRPC control plane | Raft, status, events, forwarding, console | TCP 7443 | mTLS (tonic + rustls/ring) inside WireGuard |
| Data plane | Migration RAM, NBD disk copy, blobs, cold-move extents | TCP 7444 | WireGuard, plus a one-time session token |
| VXLAN overlay | VM switch traffic | UDP 4789 | WireGuard |

**Firewall result:** between nodes only UDP 51820 must be open, plus TCP 7443 on the public address for joining. Every other service listens only on the node's mesh address.

**Why the data plane skips TLS:** WireGuard already encrypts and authenticates each peer by key, and HTTP/2 framing caps gRPC bulk throughput at roughly 1–3 Gb/s. The data plane is a plain TCP stream on the mesh: gRPC `Tunnel.Open` issues a one-time token, and the listener checks both the token and the peer's mesh source address.

### Mesh addressing and MTU

- Each cluster gets an IPv4 mesh prefix, default `100.96.0.0/16`, configurable at `prism init`. Each node is assigned one address from it in replicated state.
- Inner IPv4 keeps VXLAN overhead at 50 B. WireGuard adds 60 B over an IPv4 underlay, or 80 B over IPv6.
- On a 1500-byte underlay, guests get MTU 1390. A 9000-byte underlay keeps 1500-byte guests, and Prism recommends it.
- `PersistentKeepalive = 25` keeps peers behind NAT reachable.

### Certificates and joining

- `prism init` creates the cluster: an ECDSA P-256 CA (rcgen), the mesh prefix and the first node's WireGuard key.
- Node certificates last 1 year and auto-renew at two-thirds of their lifetime via `Cluster.RenewCert`. SANs are `<uuid>.node.prism` and `prism://node/<uuid>`, so verification works regardless of IP.
- `ca.key` lives only on the hub, or on consensus voters, with mode 0600. It is never sent to learners.

The join token is `prism1.<base64url(cbor{addrs, ca_sha256, secret, exp})>`. It is single-use and expires after 1 hour; only `sha256(secret)` is stored.

1. `prism join <addr> --token <t>` generates a TLS key, a CSR and a WireGuard keypair locally.
2. It connects to `<addr>:7443` on the public interface. A pinned verifier accepts the server only if the CA's SHA-256 matches the token.
3. `Join.Join{secret, csr, wg_pubkey, endpoint}` returns the signed cert, trust bundle, cluster ID, assigned mesh address and current peer list.
4. A committed `AddMember` command adds the new peer to every node's `wg-prism`. From then on all traffic uses mesh addresses.

### Removal and rotation

- **Removal:** `RemoveMember` deletes the node's WireGuard peer on every node, cutting it off immediately. Its cert serial also goes on a replicated deny-list checked by wrapper verifiers.
- **WireGuard key rotation:** a node generates a new key, commits `RotateWgKey`, and peers swap it in. Prism keeps the overlap brief because WireGuard allows one key per peer.
- **CA rotation:** add the new CA to `trust.pem`, re-issue all node certs, wait for every member to report, then drop the old CA.

### gRPC services (package `prism.v1`)

| Service | Purpose |
| --- | --- |
| `Join` | Token-authenticated cert issuance and mesh bootstrap (only service on the public port) |
| `Raft` | AppendEntries, Vote, chunked InstallSnapshot |
| `Cluster` | Status, RenewCert, RotateWgKey, Leave, Promote, ForwardWrite |
| `NodeStatus` | Bidirectional heartbeats and runtime-state deltas |
| `Events` | Subscribe(since) server stream |
| `HttpTunnel` | Forward one REST request as a bidirectional stream |
| `Console` | Bidirectional VNC and serial byte stream |
| `Blob` | Stat, Delete and metadata for archive files; bytes move on the data plane |
| `Tunnel` | Open a token-authorised data-plane session (migration, NBD, disk, blob) |

Bulk sessions use 4 MiB chunks, resumable offsets and a final whole-file BLAKE3.

### Rolling upgrades

- Proto changes are additive only and never reuse field numbers.
- Each node advertises `{version, feature_level}`. The cluster uses the minimum and raises it explicitly, so new `Command` variants appear only once every node supports them.
- Join is refused across more than one minor version. Upgrade learners and spokes first, then voters, then the hub or leader.

## VM lifecycle and memory ballooning

Prism stores a typed, versioned `VmSpec` as the source of truth and renders libvirt domain XML from it; users never edit XML. Ballooning is on by default, and an optional Auto mode returns idle guest memory to the host based on kernel pressure signals.

### VmSpec

- **Identity:** UUID, name, `generation` counter, owning node and `owner_epoch`.
- **CPU:** current and max vCPUs. The model is `HostModel` or `ClusterBaseline`, computed via `CONNECT_BASELINE_HYPERVISOR_CPU` across nodes so migration works on mixed hardware.
- **Memory:** `max_mib` (boot size), `min_mib` (balloon floor), `initial_mib`, balloon mode `Off | Manual | Auto`, `headroom_pct`.
- **Machine:** type pinned at creation (for example `pc-q35-8.2`, never the `q35` alias); BIOS or UEFI.
- **Disks:** virtio-blk or virtio-scsi; qcow2 or raw; `cache=none`, `io=native`, `discard=unmap`. Always in a pool on the owning node.
- **NICs:** switch, MAC, optional VLAN and egress override (see Networking).
- **Devices:** VNC on a UNIX socket, serial console, qemu-guest-agent channel, virtio-rng, `usb-tablet` for absolute pointer.
- **CD-ROM:** an archive file reference; the ISO is cached locally before attach (see Storage).
- **Cloud-init:** a NoCloud seed ISO built by Prism's own small ISO9660 writer.

### Rendering and reconciling

`render(VmSpec) -> DomainXml` is a pure function over serde structs serialised with quick-xml, tested against golden files. The XML carries `<metadata><prism:vm generation="…" xmlns:prism="urn:prism:1"/></metadata>`.

A per-host reconciler compares desired and actual state on every change and libvirt event:

- **Hot-applied:** balloon target, current vCPUs, NIC and disk hotplug (`ATTACH_DEVICE_FLAGS` / `DETACH_DEVICE_FLAGS`), CD-ROM media changes.
- **Restart required:** everything else is flagged `pending_restart` and shown in the UI.

### Lifecycle states

`Defined → Starting → Running ⇄ Paused`, `Running → ShuttingDown → Stopped`, plus `Migrating`, `Crashed` and `Quarantined`. Each transition comes from a libvirt lifecycle event and is published as an SSE `vm.state` event; nothing polls.

### Balloon device

```xml
<memballoon model='virtio' autodeflate='on' freePageReporting='on'>
  <stats period='5'/>
</memballoon>
```

- `autodeflate` deflates the balloon when the guest is close to OOM.
- `freePageReporting` (libvirt ≥ 6.9, QEMU ≥ 5.1) returns freed guest pages to the host automatically.
- Stats for all VMs come from one batched `CONNECT_GET_ALL_DOMAIN_STATS(BALLOON)` call every 5 s.
- Free page reporting and Auto mode are disabled for VMs with VFIO passthrough, locked memory or hugepages.

### Auto-balloon policy

A per-host loop runs every 10 s. It reads host PSI (`/proc/pressure/memory` `some` and `full` avg10), `MemAvailable` and per-VM cgroup `memory.pressure`. From each guest it reads `usable`, `available`, `disk_caches`, and deltas of `major_fault` and `swap_in`.

| Host state | Trigger | Action |
| --- | --- | --- |
| Green | `some` < 1% and MemAvailable > 15% | Only grow VMs under guest pressure, by 10% |
| Yellow | `some` 1–10%, or MemAvailable 5–15% | Reclaim from VMs above their target |
| Red | `full` > 2%, `some` > 10%, or MemAvailable < 5% | Reclaim toward `min_mib`; refuse new starts and inbound migrations; alert |

- **Per-VM target:** clamp(`min_mib`, in-use × (1 + headroom), `max_mib`), where in-use = available − usable.
- **Guardrails:** at most 256 MiB or 5% per VM per tick; 30 s hysteresis; skip VMs whose stats are more than 3 periods old; stop reclaiming from a VM whose major-fault rate spikes.
- **Apply:** `DOMAIN_SET_MEMORY_FLAGS(AFFECT_LIVE)`. The change appears as a `BALLOON_CHANGE` event, then an SSE update.
- **No PSI:** some kernels need `psi=1`. Without `/proc/pressure`, the loop falls back to MemAvailable only.

Manual mode exposes a target slider in the UI and `PATCH /api/v1/vms/{id}` with `memory.target_mib`.

## Storage

Prism has two storage kinds. **Local pools** hold live VM disks and are always on the VM's host. **Archive pools** hold everything else (ISOs, backups, templates, exports) and are load-balanced at the file level across directories on any nodes.

### Local VM disk pools

| Pool type | Formats | Provisioning | v1 |
| --- | --- | --- | --- |
| `dir` | qcow2 (default), raw | Sparse files | Yes |
| `lvm` | raw | Thin LV when a thin pool exists, else thick | Yes |
| ZFS dataset / zvol | raw | Thin | Deferred |

- Prism manages volumes itself with `qemu-img` and rustix, not through libvirt's storage driver. That removes any virtstoraged dependency and lets the same code run on modular and monolithic libvirt.
- Operations: create, resize (online via `BLOCK_RESIZE`), clone, import from an archive file, upload, download, delete.
- **Online move between local pools:** `blockcopy` to the new pool, wait for the READY event, then pivot. The guest keeps running.
- A disk can only belong to a VM owned by the same node; the API rejects anything else.

### Archive pools

An archive pool is a cluster-wide namespace, `archive://<pool>/<path>`, over member directories declared in each node's `prism.toml`. In standalone mode it is simply one or more local directories.

**Replicated index:**

- `files(id, pool, path, size, blake3, kind, created)`
- `replicas(file_id, member_id, state: pending | ok | missing | corrupt)`
- `members(id, node, path, weight, reserve, state: active | draining | disabled)`

Free space per member is runtime state, reported by each node.

**Placement.** Each file is stored whole on one member. Prism picks two members at random, weighted by `weight × free`, and takes the better one ("power of two choices"); members below their `reserve` are excluded. Optional replicas (default 1 copy) go on distinct nodes. Users can pin a file to a member or tag.

**Writing a file:**

1. Stream into `<member>/.prism-partial/<upload-id>` while hashing with BLAKE3.
2. fsync, then rename into place.
3. Commit `AddFile` to the index.
4. Fill extra replicas in the background over the data plane.

**Lifecycle and health:**

- **Member offline:** its files return 503 `replica_unavailable`. After a 24 h grace period, under-replicated files are copied elsewhere.
- **Draining:** a mover task copies files off, commits each replica swap, then deletes the source.
- **Rebalancing:** an optional task moves files from members above the pool's mean fill toward lighter ones.
- **Scrub:** weekly BLAKE3 re-hash; corrupt replicas are marked and repaired from a good copy.
- **Quotas:** optional per-pool byte limit; capacity is reported per member and per pool.

### Moving files between nodes

- **Uploads through any node:** tus-compatible resumable `POST /api/v1/uploads`, `PATCH` with `Upload-Offset`, `HEAD` for the current offset. The receiving node streams chunks to the chosen member's node over the data plane.
- **Downloads:** `GET` with `Range`, proxied from a healthy replica.
- **Move or copy** between pools or members runs as a task with resumable offsets and end-to-end BLAKE3.

### ISOs used by a VM on another node

The attach task fetches the ISO into the host's content-addressed cache, `cache/blake3/<hash>`, verifies it, then attaches it. The cache is refcounted by attached VMs and evicted LRU. If a replica already sits on the host, it is attached in place. Streaming over remote NBD was rejected: guest I/O would then depend on another node staying up.

### Backups

Backups are written locally by libvirt (see Live operations), then streamed into any archive pool chosen by the user. A backup directory can therefore hold any node's VMs. Each backup set records the VM spec, its chain (full + incrementals) and checksums, so it can be restored on any node.

## Live operations

Migration, snapshots and backups all run while the VM keeps running. The only pause is QEMU's final switchover, capped at 300 ms for migration. Each operation is a task with SSE progress and cancellation.

| Operation | Guest pause | Mechanism |
| --- | --- | --- |
| Live migration (local disks) | Typically 50–300 ms switchover | Managed V3 migration, NBD disk mirror, data-plane tunnel |
| Disk-only snapshot | Effectively none (ms with fsfreeze) | Atomic `blockdev-snapshot` transaction |
| Snapshot with memory | About 0.1–1 s, scales with dirty rate | External memory file written live, then a final pause |
| Snapshot delete | Milliseconds at pivot | `SNAPSHOT_DELETE` or block commit + pivot |
| Backup (full or incremental) | None; optional fsfreeze < 1 s | `BACKUP_BEGIN` push mode + checkpoints |
| Cold move | VM is off | Sparse extent copy |

### Live migration with local disks

Prism does not use libvirt peer-to-peer migration, which would need libvirt-to-libvirt connectivity. Instead it re-implements libvirt's client-side V3 managed migration, holding one local libvirt connection on each node and coordinating over gRPC.

1. **Pre-flight:** check the CPU baseline and machine type, destination memory (above the balloon floor) and disk space. Refuse VMs with VFIO devices.
2. **Prepare disks:** copy read-only backing layers to the destination, then pre-create the top images with `qemu-img create` at the same virtual size.
3. **Begin:** `src.Begin3Params` returns a cookie and the domain XML.
4. **Prepare:** `dst.Prepare3Params` starts the destination QEMU listening on `/run/prism/mig/<id>/mem.sock`, and libvirt's NBD server on `nbd.sock`.
5. **Tunnel:** the source Prism listens on the **same paths** locally and splices each connection over the WireGuard data plane to the real sockets. This is the contract libvirt documents for `disks_uri`.
6. **Perform:** `src.Perform3Params` with `LIVE | PERSIST_DEST | UNDEFINE_SOURCE | NON_SHARED_INC | AUTO_CONVERGE | ABORT_ON_ERROR`, `migrate_uri=unix://…/mem.sock`, `disks_uri=unix://…/nbd.sock` and `migrate_disks`. `MIGRATE_SET_MAX_DOWNTIME` is 300 ms.
7. **Finish and confirm:** `dst.Finish3Params`, then `src.Confirm3Params`, replicating libvirt's success and failure matrix exactly.
8. **Commit:** redefine snapshot and checkpoint metadata on the destination with `REDEFINE`, since libvirt does not migrate it. Commit `owner_epoch + 1`, push FDB updates, then send QMP `announce-self`.

Details:

- **Progress:** `GET_JOB_STATS` every second plus `MIGRATION_ITERATION` events give remaining RAM, dirty rate and a downtime estimate.
- **Postcopy:** opt-in per task. Prism calls `MIGRATE_START_POST_COPY` if the dirty rate outpaces bandwidth, and uses `POSTCOPY_RESUME` (libvirt ≥ 8.5) after a network failure.
- **Multifd:** for 25 GbE and faster, `parallel.connections` opens one data-plane stream per channel.
- **Crash safety:** the migration phase is persisted. A restarted Prism resumes the source or kills the destination, so the guest is never left paused.
- **SELinux / AppArmor:** sockets are labelled `svirt_socket_t`. If a host's policy still blocks them, Prism falls back to loopback TCP with the same port on both sides.
- **Pre-flight failure leaves nothing behind:** destination images and domains are removed on abort.

### Cold move

The VM is shut off. Prism copies the whole disk chain using `SEEK_DATA` / `SEEK_HOLE` sparse extents, lz4 framing and per-extent BLAKE3, over the data plane. A per-extent manifest in `node.db` makes the copy resumable. It then commits the ownership change, defines the VM on the destination and deletes the source.

### Snapshots

- Created with `SNAPSHOT_CREATE_XML` and `LIVE`, as external snapshots: disk overlays, plus an external memory file when memory is included.
- `QUIESCE` (guest-agent fsfreeze) is used for disk-only snapshots when the agent is up.
- **Delete:** native `SNAPSHOT_DELETE` on libvirt ≥ 9.0. On older libvirt: `BLOCK_COMMIT(ACTIVE)`, wait for `BLOCK_JOB_2` READY, `BLOCK_JOB_ABORT(PIVOT)`, then delete the metadata only.
- **Revert** of a running VM asks for typed confirmation in the UI.
- Snapshots form a tree, and the chain is shown as one in the UI.

### Backups

1. Optional `FSFREEZE` through the guest agent.
2. `BACKUP_BEGIN` in push mode to local staging, with a new checkpoint (a persistent dirty bitmap). Incremental backups reference the previous checkpoint (libvirt ≥ 7.6, QEMU ≥ 6.1).
3. `FSTHAW` as soon as the call returns.
4. On `JOB_COMPLETED`, stream the result into the chosen archive pool on any node, then clear staging.

After a migration, the next backup is a full one, because checkpoint bitmaps are not guaranteed to survive non-shared-disk migration. Restore creates a new VM on any node, or replaces the VM in place.

### Tasks

- Every long operation is a task with a UUIDv7 ID, stored in the executing node's `node.db`.
- States: `Pending → Running → Succeeded | Failed`, with `Cancelling` in between.
- Progress fields: phase, bytes done and total, rate, and for migrations dirty rate and downtime estimate. Published as SSE `task.progress`.
- Cancel maps to `ABORT_JOB` or `BLOCK_JOB_ABORT`. On restart, Prism recovers running tasks by inspecting libvirt's job state.

## Networking

Every VM network is a Prism **switch**: a cluster-wide L2 segment built from a Linux bridge on each node, joined by a VXLAN overlay that runs inside the WireGuard mesh. A switch's egress node is chosen per switch and can be overridden per NIC, so a VM on host-a can send all its traffic out host-b's network.

### Switch types

| Type | Use | Egress |
| --- | --- | --- |
| `isolated` | Internal VM-to-VM traffic only | None |
| `nat` | Host (NAT) networking | Masquerade out of the egress node's uplink |
| `bridged` | VMs directly on a physical LAN | The uplink node's physical NIC or host bridge |

A **physical network** is a node-local uplink (an existing bridge, NIC or VLAN sub-interface) that a `bridged` or `nat` switch uses on its egress node. Standalone nodes use the same model without VXLAN.

### Data plane on each node

For every switch, each node creates:

- a bridge `ps<id>` with `vlan_filtering=1`, STP off and multicast snooping off;
- a VXLAN device `vx<id>` with the switch's VNI, `dstport 4789`, `nolearning`, local address = the node's WireGuard mesh address.

Forwarding uses unicast FDB entries, no multicast: one all-zeros entry per remote node for broadcast replication, and one entry per remote VM MAC. Prism reconciles them from replicated state over rtnetlink. Devices are created on every node in advance, so live migration never waits for setup. Prism's own devices are not persisted; it rebuilds them at boot.

- **VNIs:** allocated by the leader from 100000–999999, with a 24 h cooldown before reuse.
- **MTU:** probed between nodes at join with DF pings. Switch MTU = underlay MTU − 110 B (60 WireGuard + 50 VXLAN): 1390 on a 1500 underlay, 1500 on a 9000 underlay. It is pushed through the XML `<mtu>` element and DHCP option 26.

### Egress per switch and per NIC

On a `nat` switch, each egress node in use holds a gateway VIP on `ps<id>` and masquerades through nftables. The DHCP reply for a NIC names the VIP of that NIC's effective egress node as its router.

VM1 on host-a with egress override host-b therefore sends its traffic over the overlay to host-b's VIP, and host-b NATs it out. Return traffic comes back over L2 to VM1. VM1 stays L2-adjacent to everything else on the switch, and no `ip rule` is needed.

*Diagram: per-NIC egress · one NAT switch across two nodes (see the [live design doc](https://claude.ai/code/artifact/c0a1e8f6-a56b-42bb-8a73-796e86f09cc7)).*

The highlighted path is VM1's outbound traffic. Replies are un-NATed on host-b and return over the same overlay, while VM2 and VM3 keep using host-a's gateway.

- **Bridged switches:** exactly one node may bridge a VNI to a given physical LAN, or the LAN gets an L2 loop. A per-NIC override on a bridged switch becomes a hidden child switch with its own VNI, and the UI warns that the NIC leaves the parent's L2 segment.
- **Static-IP guests** must set the matching gateway themselves; cloud-init does this automatically.
- **Failover:** the egress role is a lease with a 10 s TTL in replicated state. A holder that loses quorum for 5 s withdraws its VIP and detaches its uplink (self-fencing). The next node in the switch's ordered backup list takes the VIP, then sends gratuitous ARP, unsolicited NA and RARP for every VM MAC. NAT connections reset.

### Internal switches and VLANs

- An `isolated` switch has no VIP or uplink; it spans every node through the overlay.
- An optional `private` flag sets the bridge-port `isolated` flag so VMs reach only the gateway.
- **VLANs:** a switch carries 802.1Q tags inside its VNI. Each NIC is an access port (one VID) or a trunk (a list). libvirt's `<vlan>` element does not work on plain Linux bridges, so Prism programs bridge VLANs over netlink when the tap appears.
- **MACs and IPs:** MACs come from a Prism prefix, assigned centrally. IPAM gives each NIC a static lease on DHCP-managed switches.

### DHCP, DNS and NAT are embedded

libvirt networks are host-local and need the dnsmasq binary, which breaks the cluster-wide IPAM and the static-binary goal. Prism embeds:

- **DHCP** (dhcproto): each node answers only for MACs of VMs running on it, from IPAM leases. There are no duplicate offers over the overlay and no lease handover on migration. Options 1, 3, 6, 15, 26 and 121.
- **DNS** (hickory-proto): authoritative for `<vm>.<switch>.prism`, forwarding everything else to the host's resolvers.
- **NAT and filtering:** a minimal in-house nftables layer over netlink owns only the tables `inet prism` and `ip prism-nat`. It is validated in CI against `nft -j list ruleset`. `prism doctor` flags Docker or firewalld rules that drop forwarded traffic.

### VM attachment

```xml
<interface type='bridge'>
  <source bridge='ps1a2b'/>
  <target dev='pn<nicid>'/>
  <mac address='52:54:00:…'/>
  <model type='virtio'/>
  <driver name='vhost'/>
  <mtu size='1390'/>
</interface>
```

The deterministic tap name lets Prism match each tap to its NIC for VLAN programming.

### Through live migration

On the destination's resume event, the leader pushes `MAC → destination node` FDB updates to every node. Prism then issues QMP `announce-self` so physical switches and bridges learn the new location. Gateway VIP MACs do not move, so guest ARP caches stay valid.

### Safe host bridge creation

Moving a host IP from a NIC onto a bridge is the dangerous step, so it is always reversible.

- **NetworkManager hosts:** Prism uses an NM checkpoint with a rollback timeout over D-Bus (zbus), as Cockpit does.
- **systemd-networkd hosts:** Prism writes `.netdev` / `.network` files and keeps the previous ones.
- **Other hosts:** netlink changes plus a rollback snapshot persisted to disk.
- **Commit rule:** the browser must reconnect and confirm, and a peer must reach the node, within 60 s. Otherwise the change rolls back.
- Hosts that use ifupdown or netplan-only setups get manual instructions instead.

Prism installs an NM drop-in that marks `ps*`, `vx*` and `pn*` devices unmanaged.

### Kernel and privileges

- **Modules:** bridge, vxlan, tun, vhost\_net, wireguard, nf\_tables, nft\_nat, nft\_masq, nft\_ct, 8021q (VLAN sub-interfaces only).
- **Capabilities:** `CAP_NET_ADMIN`, `CAP_NET_BIND_SERVICE` (ports 53 and 67), `CAP_NET_RAW` (ARP/RARP announcements).
- **sysctls:** `ip_forward=1`, `rp_filter=2` on Prism bridges, IPv6 disabled on `vx*` and `pn*` ports, `br_netfilter` not loaded.

## HTTP API and SSE event stream

The API is JSON over REST under `/api/v1`, served identically by every node, with one SSE stream that delivers a snapshot and then live changes, so clients never poll. The UI uses only this public API.

### Conventions

- **OpenAPI** 3.1 generated by utoipa, served at `/api/v1/openapi.json`.
- **Errors:** RFC 9457 `application/problem+json` with a stable `code`, `retryable`, `degraded` and `request_id`. 409 = ownership or revision conflict, 412 = `If-Match` failed, 502 = owner node unreachable, 503 = degraded.
- **Concurrency:** resources carry a revision as `ETag`; writes accept `If-Match`.
- **Idempotency:** `POST`s accept an `Idempotency-Key`, remembered for 24 h.
- **Lists:** cursor pagination (`?cursor=&limit=`), field filters and `?node=`.
- **Long operations** return `202 Accepted` and `Location: /api/v1/tasks/{id}`.
- **Trust:** no auth. `X-Forwarded-For`, `-Proto` and `-Host` are honoured for logs and absolute URLs. CORS is same-origin by default and configurable.
- **Cross-node:** any node serves any path. Responses include `X-Prism-Served-By`; a forwarded call adds `X-Prism-Forwarded-By`.

### Resources

| Path | Methods |
| --- | --- |
| `/cluster`, `/cluster/members`, `/cluster/join-tokens` | GET, POST, DELETE |
| `/nodes`, `/nodes/{id}` | GET |
| `/nodes/{id}/pools`, `/nodes/{id}/pools/{p}/volumes` | CRUD |
| `/nodes/{id}/physical-networks`, `/nodes/{id}/bridge-wizard` | GET, POST |
| `/vms`, `/vms/{id}` | CRUD |
| `/vms/{id}/actions/{start,shutdown,stop,reboot,reset,pause,resume}` | POST |
| `/vms/{id}/migrate`, `/vms/{id}/move` | POST → task |
| `/vms/{id}/snapshots`, `/vms/{id}/snapshots/{s}/revert` | CRUD, POST |
| `/vms/{id}/backups`, `/backups/{b}/restore` | GET, POST → task |
| `/vms/{id}/disks`, `/vms/{id}/nics` | CRUD (hotplug where possible) |
| `/vms/{id}/console`, `/vms/{id}/serial` | WebSocket |
| `/archive/pools`, `/archive/pools/{p}/members` | CRUD |
| `/archive/pools/{p}/files`, `/archive/pools/{p}/files/{path}` | GET (Range), HEAD, DELETE, POST (move/copy) |
| `/uploads`, `/uploads/{id}` | POST, PATCH, HEAD, DELETE (tus-compatible) |
| `/switches`, `/switches/{id}/leases` | CRUD, GET |
| `/tasks`, `/tasks/{id}`, `/tasks/{id}/cancel` | GET, POST |
| `/events` | SSE |

### SSE stream

`GET /api/v1/events?topics=vms,tasks,nodes&snapshot=1` sends one `snapshot` event with the current state and cursor, then deltas.

```
id: 7f3c…a1.3.18842
event: vm.state
data: {"type":"vm.state","origin_node":"7f3c…a1","resource":"/vms/9b2e…",
       "rev":412,"ts":"2026-10-14T22:41:07Z","op":"upsert",
       "payload":{"state":"running","node":"kvm-b"}}
```

- **IDs:** `<node>.<boot_epoch>.<seq>`, monotonic per serving node.
- **Types:** `vm.state`, `vm.config`, `vm.stats`, `task.progress`, `node.status`, `archive.file`, `switch.*`, `cluster.membership`, `resync`, `reset`.
- **Ops:** `upsert`, `delete`, `invalidate`. The UI patches its cache on `upsert` and `delete`, and refetches on `invalidate`.
- **Replay:** each node keeps a ring of 10k events or 10 minutes, mirrored in `node.db`. A `Last-Event-ID` inside the ring replays. An ID from another node or boot, or older than the ring, gets `event: reset` and the client re-snapshots. This is acceptable because HTTP load balancing is the user's concern.
- **Liveness:** a `: ping` comment every 15 s and `retry: 3000`.
- **Backpressure:** each subscriber has a bounded queue. A slow client that overflows gets `reset` instead of unbounded memory growth.
- **Rate shaping:** `vm.stats` is sent at most every 5 s per VM, and only when subscribed.

### WebSockets

- `/api/v1/vms/{id}/console` carries binary RFB to and from QEMU's VNC UNIX socket, proxied over gRPC `Console` when the VM is on another node.
- `/api/v1/vms/{id}/serial` carries the serial console stream from `DOMAIN_OPEN_CONSOLE`.

### Versioning

The `/api/v1` contract changes additively. A breaking change ships as `/api/v2` beside v1 for at least one minor release.

## Web UI

The UI is a React + TypeScript + Tailwind SPA built with Vite, installable as a PWA and embedded in the binary. Its VM console decodes VNC in a Rust→WASM worker and draws with WebGL2 on an OffscreenCanvas, so rendering is GPU-accelerated and off the main thread.

### Stack and data flow

- **Routing and data:** TanStack Router and TanStack Query. `@xyflow/react` draws the network topology.
- **Live updates:** one SSE connection per tab. `upsert` and `delete` events patch the query cache with `setQueryData`; `invalidate`, `resync` and `reset` trigger a refetch. There are no polling intervals anywhere.
- **PWA:** vite-plugin-pwa precaches only the app shell. API, SSE and WebSocket traffic is never cached. Install and the Clipboard API need a secure context, which the user's TLS-terminating proxy provides.
- **Theme:** light and dark follow the OS, with a manual toggle. Keyboard navigation and ARIA labels throughout.

### Screens

| Screen | Contents |
| --- | --- |
| Dashboard | Cluster health, degraded banner, capacity bars, running tasks, alerts |
| Nodes | List; detail with hardware, load, memory pressure, pools, physical networks, host-bridge wizard, drain |
| VMs | Filterable table, bulk power actions, create wizard (template, ISO, cloud-init) |
| VM detail | Summary, Console, Hardware, Disks, NICs, Snapshots tree, Backups, Events |
| Local pools | Per node: volumes, usage, upload, import, move disk |
| Archive pools | Members with fill bars, file browser, resumable upload, download, drain, rebalance, restore |
| Switches | List and editor (type, VLANs, egress and backups), topology with egress VIPs highlighted, IPAM leases |
| Tasks | Live list with progress, per-task log, cancel |
| Cluster | Mode, members, join tokens, promote/recover guidance, certificate and WireGuard status |

### Operation flows

- **Migrate:** the target picker ranks nodes by fit (CPU compatibility, free memory after balloon floor, disk space, switch presence). It shows live progress: remaining RAM, dirty rate, downtime estimate, with postcopy and bandwidth cap as advanced options.
- **Move file:** drag a file between archive members, or pick a destination pool.
- **Snapshot:** toggles for memory and quiesce; revert of a running VM asks for typed confirmation.
- **Backup:** run now or on a schedule to any archive pool, with retention and an incremental marker; restore as a new VM or in place.
- **NIC egress:** a dropdown on each NIC lists nodes with an uplink on that switch.

Every long action returns a task and shows a toast linking to it.

### Hardware-accelerated console

```
QEMU VNC (UNIX socket) → owner node → [gRPC Console over WireGuard] → serving node
  → WebSocket /api/v1/vms/{id}/console → Web Worker
Worker: WebSocket + prism-rfb-wasm decoder + WebGL2 on OffscreenCanvas
Main thread → Worker: pointer, keys, clipboard, resize (postMessage)
```

- **Decoder:** `prism-rfb-wasm`, written in Rust, satisfies the all-Rust rule for non-UI code. It is written from the RFB spec, not copied from noVNC (MPL-2.0). Streaming zlib uses `miniz_oxide`.
- **Encodings, in preference order:** Tight (JPEG via `createImageBitmap` in the worker), TightPNG, ZRLE, Hextile, Raw.
- **Pseudo-encodings:** ExtendedDesktopSize, Cursor, QEMU ExtendedKeyEvent, LED state. Quality and compression levels adapt to measured bandwidth.
- **Rendering:**
  - The framebuffer lives in WASM memory.
  - Dirty rectangles upload with `texSubImage2D`, then one full-screen quad draws per animation frame.
  - The cursor is its own quad, so pointer motion is instant.
  - Scaling (fit, 1:1, remote resize) happens in the shader.
- **Fallbacks:** the worker feature-detects. Without OffscreenCanvas WebGL2 (some Safari builds) it renders WebGL2 on the main thread, then Canvas2D.
- **Not used:** WebGPU (texture upload is the bottleneck, not compute), H.264 (QEMU's VNC server cannot send it) and SPICE (spice-html5 is barely maintained).
- **Input:** `KeyboardEvent.code` maps to XT scancodes over QEMU ExtendedKeyEvent, so the guest's keyboard layout applies. Fullscreen uses `navigator.keyboard.lock()` where available. The toolbar has Ctrl-Alt-Del and "type clipboard".
- **Clipboard:** a `qemu-vdagent` channel with spice-vdagent in the guest; "type clipboard as keystrokes" always works.
- **Serial console:** xterm.js with its WebGL addon, as a tab beside VNC and the default for headless VMs.

### Embedding and caching

- Vite emits `assets/[name]-[hash].*`. The build pre-compresses them with brotli and gzip, and rust-embed bakes them into the binary.
- Hashed assets: `Cache-Control: public, max-age=31536000, immutable`. `index.html`, `sw.js` and the manifest: `no-cache` with an ETag.
- Unknown non-API paths serve `index.html`. A strict CSP allows `worker-src 'self'` and `'wasm-unsafe-eval'`; the decoder is served as `application/wasm`.

## Build, packaging, testing and quality

CI blocks any merge that has a clippy warning, a banned dependency, a dynamically linked binary or a failing test. The end-to-end suite runs real VMs in nested KVM across the supported distributions.

### Quality gates

- `cargo fmt --check`.
- `cargo clippy --workspace --all-targets -- -D warnings`, with `[workspace.lints]` enabling a curated `clippy::pedantic` subset. Generated XDR and protobuf code sit in their own modules with a scoped `#[allow]`, so handwritten code gets the full rules.
- `#![forbid(unsafe_code)]` everywhere except `prism-libvirt`'s fd-passing and the allocator, each documented.
- `cargo deny check` for licences, advisories and banned crates.
- A static-ELF check: `readelf -d` shows no `NEEDED` entries.
- Frontend: `tsc --noEmit`, ESLint and Prettier.

### Tests

| Level | What | Where |
| --- | --- | --- |
| Unit | Domain XML golden files, balloon policy, placement, IPAM, FDB diffing, XDR round-trips | `cargo test` |
| Protocol | Mock libvirt RPC server replaying captured traffic, including keepalive and event edge cases | `cargo test` |
| Cluster | 3- and 5-node in-process clusters with fault injection: partitions, hub kill, forced promote | `cargo test` (simulated network) |
| End-to-end | Real libvirt and QEMU in nested KVM: Ubuntu 22.04/24.04 (AppArmor), Debian 12, RHEL 9 (SELinux) | Nightly CI |
| UI | Playwright against a mock API and against the e2e cluster, including console rendering | CI |

End-to-end scenarios cover live migration under memory load (downtime measured), backup and restore across nodes, cross-node egress, hub loss with degraded spokes, and rolling upgrade.

### Release

- Static `prism-x86_64` and `prism-aarch64` binaries with SHA-256 checksums.
- A systemd unit and an NM unmanaged-devices drop-in, installed by `prism install`.
- `prism doctor` runs at startup and on demand.

## Implementation milestones

Prism ships in nine milestones, each ending at a testable gate; a standalone node is useful after M3, and clustering builds on it. Every milestone keeps the clippy, cargo-deny and static-ELF gates green.

*Diagram: implementation milestones · 9 phases, each with an exit gate (see the [live design doc](https://claude.ai/code/artifact/c0a1e8f6-a56b-42bb-8a73-796e86f09cc7)).*

| # | Milestone | Scope | Gate |
| --- | --- | --- | --- |
| M0 | Foundations | Workspace, CI gates, static build, xdrgen, libvirt RPC client and events | Lists domains and streams lifecycle events on 3 distributions |
| M1 | Standalone node | VmSpec and renderer, lifecycle, dir and LVM pools, REST + SSE, tasks | A VM is created and booted via the API, with state seen over SSE |
| M2 | UI and console | App shell, WASM VNC console on WebGL2, serial console | A guest OS is installed from an ISO entirely in the browser |
| M3 | Memory and data protection | Ballooning, live snapshots, backups to a local archive pool | Auto-balloon reclaims under induced pressure; an incremental backup restores |
| M4 | Hub-spoke cluster | CA and join, WireGuard mesh, raft learners, forwarding, event fan-in, degraded mode | Any node serves any VM; killing the hub leaves spokes read-only |
| M5 | Consensus | 3- and 5-voter raft, promote and recover with generation fencing | Survives losing 1 of 3 voters; a forced promotion is fenced |
| M6 | Networking | Switches, VXLAN, DHCP/DNS/NAT, per-NIC egress, VLANs, bridge wizard | A VM on node A egresses via node B; isolated VM-to-VM traffic works |
| M7 | Mobility and archive | Live migration, cold move, archive pools with placement, replicas and ISO cache | Live migration under load pauses under 300 ms; an archive member drains |
| M8 | Hardening | Cert and key rotation, rolling upgrades, full e2e matrix, UI polish | The full matrix is green; a rolling upgrade crosses a minor version |

M0 is highlighted because every later milestone depends on the native libvirt client. Networking (M6) and mobility (M7) need the cluster transport from M4.

## Risks and open questions

The largest risk is the native libvirt RPC client, since every feature sits on it, so it is built and proven first (M0). Each open question below has a recommended default; approving the doc accepts the defaults unless you mark otherwise.

### Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| libvirt's RPC protocol is formally private | A libvirt upgrade breaks Prism | Procedures are append-only in practice; pin libvirt ≥ 8.0, feature-detect, CI version matrix |
| XDR codegen has quirks (`%` passthrough, typed params) | M0 slips | Allowlisted generation; golden round-trips against captured traffic |
| AppArmor/SELinux block Prism-owned migration sockets | Live migration fails on some distributions | `svirt_socket_t` labelling, loopback TCP fallback, spike early in M7 |
| openraft is pre-1.0 | API churn, subtle bugs | Pin 0.9.x behind a `ConsensusLog` trait; fault-injection tests |
| Pure-Rust nftables support is immature | NAT bugs | Minimal in-house netlink layer validated against `nft -j` |
| Docker or firewalld drop forwarded traffic | VMs lose connectivity silently | `prism doctor` detects and explains |
| WireGuard + VXLAN cost 110 B of MTU | Guests get 1390 on a 1500 underlay | Probe at join, push MTU via XML and DHCP, recommend 9000 underlay |
| Forced hub promotion can lose recent writes | Config changes lost | Explicit `--force`, generation fencing, lost entries exported |
| Postcopy split-brain on network loss | VM state spread across hosts | Postcopy opt-in; `POSTCOPY_RESUME` |
| Full-mesh event and status fan-in | Overhead past \~32 nodes | Document 32 nodes as the supported maximum |

### Open questions (recommended default first)

1. **Minimum versions.** *Default:* libvirt ≥ 8.0, QEMU ≥ 6.2 (Ubuntu 22.04, Debian 12, RHEL 9 and newer). Standalone needs kernel ≥ 5.4; clusters need ≥ 5.6 for WireGuard.
2. **WireGuard mesh prefix.** *Default:* `100.96.0.0/16`, chosen at `prism init` and fixed thereafter.
3. **Per-NIC egress on bridged switches.** *Default:* allowed as a hidden child switch with a UI warning, since it cannot stay L2-adjacent.
4. **Static-IP guests and egress overrides.** *Default:* document that the gateway must match; add policy routing only if requested.
5. **NAT egress failover drops open connections.** *Default:* accept and document; conntrack sync would need C tooling.
6. **Archive replica count.** *Default:* 1 copy, configurable per pool.
7. **Backups during degraded mode.** *Default:* skipped and recorded as failed in v1; a local journal with replay later.
8. **ZFS pools.** *Default:* deferred until after v1.
