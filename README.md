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

## Checks

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
(cd web && bun run typecheck)
```
