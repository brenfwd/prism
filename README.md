# Prism

A libvirt/KVM manager for Linux hosts, shipped as one binary with an embedded web UI.
See [docs/DESIGN.md](docs/DESIGN.md) for the full design.

**Status:** single-node MVP (part of milestones M0–M2). Clustering, networking switches, snapshots,
backups, migration and auto-ballooning are not implemented yet.

## What works

- Native libvirt RPC client in Rust (`crates/prism-libvirt`): XDR over the daemon's UNIX
  socket, call multiplexing, lifecycle events, keepalive and data streams. libvirt's C
  library is never linked.
- VMs: create, edit, delete, and start / shut down / power off / reboot / reset / pause / resume.
  The machine type is pinned at creation (for example `pc-q35-10.2`). Each VM gets a virtio disk,
  an optional install ISO, and user-mode or host-bridge networking. Firmware is BIOS or UEFI.
- Memory ballooning (manual): set a live balloon target between a floor and the maximum.
  Guest memory stats come from the virtio balloon driver.
- Storage: directory pools from `prism.toml` or created through the API under allowlisted
  roots. You can create qcow2/raw volumes, upload ISOs (streamed and renamed atomically) and
  delete volumes. Every file operation is confined with `openat2(RESOLVE_BENEATH | NO_SYMLINKS …)`.
- REST API under `/api/v1`, with RFC 9457 problem+json errors.
- SSE (`/api/v1/events?snapshot=1`): a snapshot followed by live `vm.state`, `vm.stats`,
  `vm.config`, `node.status` and `pool` events, with replay via `Last-Event-ID`.
- Consoles over WebSocket: VNC (`/vms/{id}/console`, raw RFB to QEMU's UNIX socket) and
  serial (`/vms/{id}/serial`, a libvirt console stream).
- Request hygiene: the `Host` allowlist (421) and Origin / `Sec-Fetch-Site` checks on writes and
  WebSockets. The server listens on loopback by default.
- Web UI (React, Tailwind, TanStack): overview, VM list, a create dialog, VM detail with a
  balloon slider, VNC console, serial console, settings, and storage with upload.
  The VNC console uses noVNC for now; the WASM/WebGL2 decoder comes later.

## Build

Requires Rust (stable or nightly) and [Bun](https://bun.sh).

```sh
cargo build --release
```

The UI is the `prism-web` crate (`web/`). Its `build.rs` runs `bun install --frozen-lockfile`
and `bun run build` into `OUT_DIR`, and the output is embedded into the binary. Cargo reruns it
only when files in `web/` change.

- `BUN=/path/to/bun` picks a specific Bun. Otherwise `bun` on `PATH`, then `~/.bun/bin/bun`.
- `PRISM_SKIP_WEB=1` skips the UI and embeds a placeholder page. Useful for Rust-only checks
  on machines without Bun.

### Static binary

A plain `cargo build` links glibc dynamically. For a fully static binary, build for a musl
target. The bundled SQLite is C code and needs a musl C compiler; `cargo-zigbuild` uses Zig
to provide one. musl targets link `crt-static` by default, so no `RUSTFLAGS` are needed.

One-time setup:

```sh
rustup target add x86_64-unknown-linux-musl     # aarch64-unknown-linux-musl for ARM
cargo install cargo-zigbuild
# plus Zig on PATH: https://ziglang.org/download/ (or your package manager)
```

Build:

```sh
cargo zigbuild --release --target x86_64-unknown-linux-musl -p prism
# → target/x86_64-unknown-linux-musl/release/prism
```

Verify that it is static:

```sh
B=target/x86_64-unknown-linux-musl/release/prism
file "$B"                       # "statically linked"
readelf -d "$B" | grep NEEDED   # must print nothing
ldd "$B"                        # "not a dynamic executable"
```

Notes:

- The UI is embedded as usual, so the build machine still needs Bun.
- Plain `cargo build --target x86_64-unknown-linux-musl` (without zigbuild) usually fails while
  compiling SQLite unless a musl C toolchain such as `musl-gcc` is installed.
- Zig may print `linker stderr: ignoring deprecated linker optimization setting "1"`. It is harmless.
- Any new dependency that links a system library (OpenSSL, libz, …) breaks the static
  build. The `readelf` check above catches it. See the dependency rules in
  [docs/DESIGN.md](docs/DESIGN.md).

## Run

Prism runs as root on a KVM host with libvirt ≥ 8.0 (`virtqemud` or `libvirtd`) and `qemu-img`.

```sh
prism init --config /etc/prism          # writes prism.toml; review it
prism doctor --config /etc/prism        # checks libvirt, /dev/kvm, qemu-img, pools
prism serve --config /etc/prism
```

Open <http://127.0.0.1:8080>. To reach it from elsewhere, use `tailscale serve`, an SSH
tunnel, or a management address in `[http] listen` (plus `allowed_hosts`). Never use 0.0.0.0.
Prism has no authentication of its own.

### Unprivileged development

`prism.toml` can point at a session-mode daemon and TCG:

```toml
[libvirt]
socket = "/run/user/1000/libvirt/libvirt-sock"
uri = "qemu:///session"
domain_type = "qemu"
```

Pool directories must support `O_DIRECT` (disks use `cache=none,io=native`), so tmpfs won't work.

For UI work, run `bun run dev` in `web/` for hot reload. It proxies `/api` to `127.0.0.1:8080`.

## Testbed

`cargo xtask testbed` runs Prism as root inside Ubuntu 26.04 VMs, for testing clustering,
WireGuard, bridges, VXLAN and nftables. You need membership in the `libvirt` group (it uses
`qemu:///system`), nested KVM, `ssh`, `ssh-keygen`, `curl` and `sha256sum`. You don't need sudo.

```sh
cargo xtask testbed up --nodes 3     # pool, networks, image, VMs; waits for cloud-init
cargo xtask testbed status           # state, IPs, cloud-init and prism.service per node
cargo xtask testbed ssh 1 -- sudo virsh list
cargo zigbuild --release --target x86_64-unknown-linux-musl -p prism
cargo xtask testbed deploy           # install the binary + prism.service on every node
curl http://<mgmt-ip>:8080/api/v1/nodes
cargo xtask testbed down             # remove the nodes (--all also removes image, pool and network)
```

- Each node `prism-tb-N` (hostname `tb-N`) has 4 GiB of RAM, 4 vCPUs with host-passthrough
  (nested KVM), and a 20 GiB qcow2 overlay on the cloud image. Disks live in the libvirt pool
  `prism-testbed` (`/var/lib/libvirt/images/prism-testbed`).
- NIC 1 is on libvirt's `default` NAT network and gets its address from DHCP.
  NIC 2 is on the isolated network `prism-tb` (no forwarding, no DHCP) with a static
  address `10.77.0.N/24`, to act as an extra L2 segment.
- cloud-init comes from a NoCloud seed ISO built by `crates/prism-iso`. It installs
  QEMU, libvirt, OVMF, wireguard-tools, nftables, iproute2 and bridge-utils, and gives user
  `ubuntu` passwordless sudo. The password `ubuntu` works for `virsh console` only.
- The SSH key, the cached image (checked against `SHA256SUMS`) and the generated XML and
  seeds are kept in `target/testbed/`.
- `deploy` adds the node's NAT address to `http.listen` and `allowed_hosts` in
  `/etc/prism/prism.toml`, so the host can reach each node's API on port 8080.

Every subcommand is idempotent. Running `up` again starts stopped nodes and adds missing ones.

## Checks

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
(cd web && bun run typecheck)
```
