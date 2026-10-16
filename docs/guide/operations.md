# Operations

This page covers running Prism as a service, checking a host, the changes Prism makes to AppArmor, reading logs, and the messages Prism shows when something fails.

## Run Prism under systemd

Prism runs in the foreground and logs to standard error, so a unit file is enough. Save this as `/etc/systemd/system/prism.service`:

```ini
[Unit]
Description=Prism libvirt/KVM manager
Wants=network-online.target
After=network-online.target libvirtd.service

[Service]
ExecStart=/usr/local/bin/prism serve --config /etc/prism
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

On a host that uses the modular libvirt daemons, add `virtqemud.service` to `After=`. Then run `systemctl daemon-reload` and `systemctl enable --now prism`.

`prism serve` stops cleanly on SIGTERM and SIGINT. A lock file, `run/prism.lock` in the config directory, keeps a second instance from starting on the same directory (`another prism is already serving <dir>`). Prism starts VMs that have autostart set when the host boots, once libvirt is connected. It does not use libvirt autostart.

## The config directory

The config directory holds `prism.toml`, `data/` with the node identity and its databases, `wg/` with the WireGuard keys, and `run/prism.lock`. Directories have mode 0700 and files 0600. To move a node to new hardware, copy the directory and start `prism serve --adopt --config <dir>` on the new machine. Without `--adopt`, the start fails with `this config directory belongs to another machine (machine-id <id>); pass --adopt if it was moved here deliberately`.

Prism reads `prism.toml` once at start. Unknown keys are an error, so a typo stops the start. These checks run on load, and each failure names the setting:

- `http.listen is empty`.
- `invalid pool name "<name>"`.
- `pool <name>: only type = "dir" is supported`, `pool <name>: path must be absolute` and `pool <name>: format must be qcow2 or raw`.

## prism doctor

`prism doctor --config /etc/prism` checks the host without starting the node. It prints one line per check:

```text
  ok    libvirt: 10.0.0 via /run/libvirt/virtqemud-sock
  ok    kvm: /dev/kvm present; domain type kvm
  ok    qemu-img: qemu-img version 8.2.2
  warn  passt (opt-in user-mode networking): not installed; user-mode port forwards are unavailable
        Only needed for user-mode NICs; ports forwarded on Prism networks use nftables. Install the "passt" package (apt install passt) to opt in.
  ok    ip (iproute2): /usr/sbin/ip
  ok    pool default: /var/lib/prism/pools/default
```

- libvirt connects to the daemon socket and reads its version.
- kvm passes when `/dev/kvm` exists. Without it, the check passes with `using TCG emulation as configured` when `domain_type` is `qemu`, and fails with `/dev/kvm missing; set libvirt.domain_type = "qemu" for TCG` when it is `kvm`. The default `auto` picks `kvm` when `/dev/kvm` exists and `qemu` when it does not, so it never fails.
- qemu-img runs `qemu-img --version`.
- passt and ip are optional. A missing one shows `warn` with a hint and does not fail the run.
- Each pool in `prism.toml` is opened, which also creates its directory.

A `FAIL` line makes the command exit with `some checks failed`. The pools made through the API are not checked.

## AppArmor

On hosts where libvirt confines QEMU with AppArmor (Ubuntu does), `prism serve` writes two small rules at start. It needs root for this and logs a warning when it cannot write them.

- `/etc/apparmor.d/abstractions/libvirt-qemu.d/prism` contains `/run/prism/mig/** rw,`. It lets QEMU use the sockets of a live migration. Without it, a migration fails with a permission error on the migration or NBD socket. Prism writes the file only when `/etc/apparmor.d/abstractions/libvirt-qemu` exists.
- A block between `# BEGIN prism pools` and `# END prism pools` in `/etc/apparmor.d/local/usr.lib.libvirt.virt-aa-helper` lets libvirt's profile generator read each pool directory. A VM whose disk has a backing file or a snapshot layer fails to start with `Permission denied` on the base image without it. Prism rewrites only its own block, reloads the profile with `apparmor_parser -r` and updates the block when pools are added or removed. Pool paths that contain `{ } * ? [ ] "` or a newline are skipped.

On some distributions libvirt's AppArmor profile for `passt` is broken. `prism doctor` then reports that passt is installed but user-mode port forwards are unavailable, and the API refuses them with the same text. Forward ports on a Prism network instead, which uses nftables. Setting the environment variable `PRISM_ALLOW_PASST_APPARMOR` skips the check.

## Logs

Prism logs through the standard `tracing` filter and writes to standard error, which systemd keeps in the journal:

```sh
journalctl -u prism -f
```

The default filter is `info,tower_http=warn`. Set `RUST_LOG` to change it, for example `Environment=RUST_LOG=prism=debug,info` in the unit. Useful lines at the default level are `listening on http://<addr>`, `connected to libvirt at <socket>`, `libvirt connection lost`, `cannot connect to libvirt at <socket>: <error>`, `host memory pressure is critical` and `no O_DIRECT support; disks will use cache=writeback`. Task failures are in the Tasks page and in `GET /tasks`, with the error in the task's `error` field.

## Troubleshooting

An API error is a JSON problem document with `status`, `code` and `detail`. The `detail` text is what the UI shows. Responses with status 502 or 503 are marked `retryable`.

### The UI or API does not answer

- `Host "<name>" is not in http.allowed_hosts` (HTTP 421, code `host_not_allowed`): the browser used a host name that Prism does not know. Prism accepts IP addresses, `localhost` and the node's host name. Add other names to `http.allowed_hosts`, or `"*"` to turn the check off.
- `cross-site request refused` and `WebSocket Origin does not match Host` (HTTP 403, code `cross_origin`): a page from another origin sent a write or opened a console. Add the origin to `http.allowed_origins`.
- `writes require application/json or application/octet-stream` (HTTP 415): a write request with a body had another content type. Send `Content-Type: application/json`, or `application/octet-stream` for uploads.
- A malformed JSON body returns code `invalid_body` with the parser's message. Unknown fields are refused, so a misspelled field name shows up here.
- `binding <addr>`: the address in `http.listen` is taken or does not exist on this host.

### The command line cannot reach the node

- `connecting to prism at <addr>; is prism serve running?` and `timed out connecting to prism at <addr>`: the `prism cluster`, `join` and `leave` commands talk to the daemon at the loopback address in `http.listen`, or the first address if none is loopback.
- `prism at <addr> did not answer in time`: the call waited 120 seconds.
- `no cluster member named "<name>"`: `promote` and `remove` take a member name or ID from `prism cluster status`.
- `prism is still serving <dir>; stop it first`: `prism leave` waits 30 seconds for the daemon to stop after a graceful leave, and `prism leave --force` needs the daemon stopped before it starts.

### libvirt

- `libvirt is not connected` (HTTP 503, code `libvirt_unavailable`): Prism cannot reach the daemon at `libvirt.socket`. It retries by itself. `prism doctor` shows whether the socket answers.
- Errors that libvirt reports itself come back with code `libvirt_error` and libvirt's own message, with HTTP 409.
- `host memory pressure is critical; refusing to start more VMs`: Prism refuses `start` while host memory pressure is in the red band.

### VMs

- `VM has no VNC socket; is it running?`: the console needs a running VM.
- `vm <name> is owned by node <id>`: the node that handled the request does not own the VM. Cluster nodes forward VM requests to the owner, so this appears only while ownership is changing.
- `another task is running on this VM`: wait for the current task, or cancel it from the Tasks page.
- `a VM named <name> already exists` and the name rule `[A-Za-z0-9._-]{1,64}` for create, clone and restore.
- `vcpus must be 1..=512`, `memory must be at least 64 MiB`, `min_mib exceeds max_mib`, `target_mib must be between min_mib and max_mib`.
- `vm <id> is at generation <n>, not <m>`: the VM changed since you read it.
- `cloud-init needs a storage pool`: a VM with cloud-init and no disks has no pool for the seed ISO.
- `<name> is running; shut it down to clone it`, `<name> is running; shut it down first (live migration moves running VMs)`, `<name> is not running; use move for stopped VMs`, `eject CD-ROMs before migrating`, `moving VMs needs a cluster` and `live migration needs a cluster`.

### Storage

- `pool <name> not found` and `volume <name> not found`.
- `<name> already exists`, `invalid name "<name>"`, `unsupported format <format>`.
- `used by <vms>` (HTTP 409, code `in_use`) when deleting a pool or volume that VMs use.
- `pools from prism.toml are removed by editing prism.toml` (code `static_pool`).
- `qemu-img failed: <output>` and `I/O error: <error>` (HTTP 500, code `storage_error`).
- `<name> has a backing file outside its pool: <file>`, `<name>: backing chain is too deep` and `<name> has a backing file; flatten it first`: a disk image refers to a base file that Prism does not manage.
- `disks can only grow`, and `only disks can be resized`.

### Networks

- `no free subnet left` and `no free IPv6 prefix left`.
- `<subnet> overlaps <other>, used by another Prism network` and `<subnet> overlaps <other>, used by a network this host already routes to`.
- `subnet prefix must be /16 to /29`, `<subnet> has host bits set; use <subnet>` and the `100.64.0.0/10` overlap message.
- `<subnet> is too small for a gateway on every node` and `<subnet> is too small for the addresses in use`.
- `network <name> is used by <vms>; remove their NICs first`.
- `a network named <name> already exists`, `egress node <id> is not a member`.

### Cluster and archive

The cluster messages, with the steps that produce them, are in [Clustering](clustering.md), and the archive messages are in [Storage and the archive](storage-and-archive.md). Two more:

- `clustering is not available on this node` (HTTP 503, code `cluster_unavailable`).
- `that member is on another node` (HTTP 502, code `not_clustered`) and `the member's node is not in the cluster` (code `owner_unknown`): an archive request needs a node that is not reachable.
