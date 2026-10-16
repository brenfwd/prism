# Prism

Prism manages KVM virtual machines through libvirt. It is one static binary with an embedded web UI, and it runs on each host. It talks to the local libvirt daemon with its own Rust implementation of libvirt's RPC protocol, so the libvirt C library is never linked.

Hosts can be joined into a cluster. Cluster traffic runs over a WireGuard mesh that Prism sets up between the members. A cluster supports live migration of VMs between nodes, archive pools that span several nodes, backups and virtual networks. The UI covers VM creation, consoles (VNC and serial), memory ballooning, storage, networks and a task history.

Prism has no authentication. It listens on loopback by default.

## Requirements

- A Linux host with KVM (`/dev/kvm`), run as root.
- libvirt 8.0 or later, with `virtqemud` or `libvirtd` running, and QEMU.
- `qemu-img`.
- `ip` from iproute2, which networks and cluster leave use.
- `passt` for user-mode guest networking (optional; `prism doctor` reports it).

## Install

From a release binary, copy it into place:

```sh
install -m 0755 prism /usr/local/bin/prism
```

To build from source you need Rust 1.88 or later with the `wasm32-unknown-unknown` target, and [Bun](https://bun.sh) for the UI:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release          # target/release/prism
```

The `prism-web` crate (`web/`) has a `build.rs` that builds the console decoder (`crates/prism-rfb-wasm`), runs `bun install --frozen-lockfile` and `bun run build`, and embeds the result in the binary. Set `BUN=/path/to/bun` to choose a Bun binary. Set `PRISM_SKIP_WEB=1` to embed a placeholder page, for Rust-only checks on a machine without Bun.

### Static build

A plain `cargo build` links glibc dynamically. For a static binary, build for musl. The bundled SQLite is C code and needs a musl C compiler, which `cargo-zigbuild` gets from Zig. musl targets link statically by default, so no `RUSTFLAGS` are needed.

```sh
rustup target add x86_64-unknown-linux-musl     # aarch64-unknown-linux-musl for ARM
cargo install cargo-zigbuild                    # and put Zig on PATH
cargo zigbuild --release --target x86_64-unknown-linux-musl -p prism
```

The binary is `target/x86_64-unknown-linux-musl/release/prism`. To check that it is static:

```sh
B=target/x86_64-unknown-linux-musl/release/prism
file "$B"                       # "statically linked"
readelf -d "$B" | grep NEEDED   # prints nothing
ldd "$B"                        # "not a dynamic executable"
```

The build machine still needs Bun, because the UI is embedded. Plain `cargo build --target x86_64-unknown-linux-musl` usually fails in SQLite unless `musl-gcc` is installed. Zig may print `ignoring deprecated linker optimization setting "1"`, which is harmless. A dependency that links a system library (OpenSSL, libz) breaks the static build, and the `readelf` check catches it.

## First run

1. Create the config directory:

   ```sh
   prism init --config /etc/prism
   ```

   This writes `/etc/prism/prism.toml` with one directory pool at `/var/lib/prism/pools/default`. Use `--pool-path` to put that pool elsewhere.

2. Check the host:

   ```sh
   prism doctor --config /etc/prism
   ```

   It checks libvirt, `/dev/kvm`, `qemu-img`, the optional programs and the configured pools.

3. Start the node:

   ```sh
   prism serve --config /etc/prism
   ```

4. Open <http://127.0.0.1:8080>. To reach the UI from another machine, use an SSH tunnel, `tailscale serve`, or a management address in `http.listen` together with `http.allowed_hosts`. Do not listen on 0.0.0.0.
5. Upload an installer ISO on the storage page, then create a VM from the VM list. Pick the ISO as the install media, start the VM and open its console.

## Command line

Every subcommand takes `--config <dir>`, the config directory. `--help` works on each one.

| Command | What it does |
| --- | --- |
| `prism init [--pool-path <dir>]` | Creates the config directory and a default `prism.toml`. |
| `prism serve [--adopt]` | Runs the node. `--adopt` accepts a config directory copied from another machine. |
| `prism doctor` | Checks host prerequisites. |
| `prism cluster init --name <name> [--consensus]` | Makes this node the hub of a new cluster. `--consensus` uses 3 or 5 voting members instead of a single hub. |
| `prism cluster status` | Shows cluster status and members. |
| `prism cluster invite [--name <name>] [--ttl <seconds>] [--revoke <id>]` | Creates an invite for a new node. The default lifetime is one hour. `--revoke` cancels an open invite. |
| `prism join <invite>` | Run on the new node. Prints a reply to give to the hub. |
| `prism cluster accept <reply> [-y]` | Run on the hub. Admits the node that printed the reply. |
| `prism cluster remove <node>` | Removes a member. Its VMs must be moved first. |
| `prism cluster promote <node>` | Hands the hub role to another member. Run it on the current hub. |
| `prism cluster removed` | Lists removed members, whose keys, IDs and addresses stay blocked. |
| `prism leave [--force]` | Leaves the cluster and wipes this node's state. `--force` wipes without telling the cluster. Stop `prism serve` first. |

To form a cluster, run `prism cluster init` on the first node, then `prism cluster invite` on it. Paste the invite into `prism join` on the new node, and paste the reply it prints into `prism cluster accept` on the hub. Members need UDP port 51820 (`cluster.wireguard_port`) open between them.

## Configuration

The config directory holds `prism.toml`, `data/` (the node's databases and identity), `wg/` (WireGuard keys) and `run/prism.lock`. Directories are created with mode 0700 and files with 0600. Unknown keys in `prism.toml` are an error. Every section is optional. Settings shared by a cluster live in the replicated state, not in this file.

A minimal file:

```toml
[http]
listen = ["127.0.0.1:8080"]

[libvirt]
uri = "qemu:///system"
domain_type = "auto"     # kvm when /dev/kvm exists, else qemu

[storage]
pool_roots = ["/var/lib/prism/pools"]   # where the API may create pools

[pools.default]
type = "dir"
path = "/var/lib/prism/pools/default"
format = "qcow2"         # or raw

[cluster]
advertise = ["192.0.2.10"]   # addresses other members reach this node at
wireguard_port = 51820
```

Other keys are `node.name`, `http.allowed_hosts`, `http.allowed_origins`, `libvirt.socket`, `libvirt.emulator`, `libvirt.qemu_img`, `libvirt.uefi_loader`, and `[archive_members.<name>]` tables (`pool`, `path`, `weight`, `reserve`) that add a directory on this node to an archive pool.

For development without root, point Prism at a session daemon and TCG:

```toml
[libvirt]
socket = "/run/user/1000/libvirt/libvirt-sock"
uri = "qemu:///session"
domain_type = "qemu"
```

Pool directories must support `O_DIRECT`, so tmpfs does not work.

## Development

Run `bun run wasm` once in `web/`, then `bun run dev` for hot reload. It proxies `/api` to `127.0.0.1:8080`. Checks:

```sh
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test
(cd web && bun run typecheck)
```

`cargo xtask testbed` runs Prism inside Ubuntu VMs for testing clusters, WireGuard, bridges and VXLAN. It needs the `libvirt` group, nested KVM, `ssh`, `ssh-keygen`, `curl` and `sha256sum`. See `cargo xtask testbed --help`.

## Documentation

- [docs/guide](docs/guide) is the user guide.
- [docs/DESIGN.md](docs/DESIGN.md) is the design, including the dependency rules.
- [docs/API.md](docs/API.md) describes the HTTP API.

Prism is licensed under Apache-2.0.
