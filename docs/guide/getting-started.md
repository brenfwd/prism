# Getting started

This page takes a host with no Prism on it to a running VM. The commands run as root, because Prism creates bridges, WireGuard interfaces and nftables rules.

## Before you start

The host needs `/dev/kvm`, libvirt 8.0 or later with `virtqemud` or `libvirtd` running, QEMU and `qemu-img`. Without `/dev/kvm`, set `domain_type = "qemu"` in `prism.toml` to run guests under TCG emulation. `ip` from iproute2 is needed for networks and for `prism leave`, and `passt` is optional. The README lists the requirements and the install steps.

## Start a node

1. Create the config directory:

   ```sh
   prism init --config /etc/prism
   ```

   This writes `/etc/prism/prism.toml` with one directory pool, `default`, at `/var/lib/prism/pools/default`. Pass `--pool-path <dir>` to put the pool somewhere else. The command fails with `/etc/prism/prism.toml already exists` when the file is there already.

2. Check the host:

   ```sh
   prism doctor --config /etc/prism
   ```

   Each check prints one line. A failed check prints `FAIL` and makes the command exit with `some checks failed`. A missing optional program prints `warn` and a hint. See [Operations](operations.md) for the checks.

3. Run the node:

   ```sh
   prism serve --config /etc/prism
   ```

   The log shows `listening on http://127.0.0.1:8080`. To keep it running across reboots, use the systemd unit in [Operations](operations.md).

4. Open <http://127.0.0.1:8080>. Prism has no login, so it listens on loopback only. To reach it from another machine, use an SSH tunnel, or add the management address to `http.listen` and the host name you type in the browser to `http.allowed_hosts`.

A few seconds after the first start Prism creates a network called `default` with a free subnet, so new VMs have a network and internet access. See [Networking](networking.md).

## Create the first VM

The Machines page lists VMs, and the New machine button opens a form with one card per decision.

1. Put an installer ISO or a cloud image into the pool. On the Storage page, drop the file anywhere on the page, or press Choose files. The From URL button downloads a file by link and can check a SHA-256 checksum. The file picker offers `.iso`, `.img`, `.qcow2` and `.raw` files.
2. Open New machine and choose what to start from. Cloud image copies a ready disk and writes a cloud-init config, so the machine boots with your user name and SSH key. Install from ISO attaches the ISO to a fresh empty disk. Blank machine creates an empty disk and nothing else.
3. Pick the file, name the machine and choose a size. Small is 1 vCPU, 1 GiB of memory and a 10 GiB disk. Medium is 2 vCPUs, 4 GiB and 32 GiB. Large is 4 vCPUs, 8 GiB and 64 GiB. Custom starts at 2 vCPUs, 2 GiB and 20 GiB. Names use letters, digits, dots, dashes and underscores, up to 64 characters, and do not start with a dot or dash.
4. For a cloud image, give a user name, an optional password and one or more SSH public keys. Prism keeps the last key you entered.
5. Choose a network. Prism network is the default when one exists. Private (NAT) uses user-mode networking and needs `passt` only for port forwards. Bridged to a LAN joins an existing host bridge such as `br0`.
6. Open the Advanced section for firmware (BIOS or UEFI), CPU model, memory ballooning, storage pool, description and Start when the host boots. Defaults suit most machines.
7. Press Create machine. The Summary card shows how much host memory and pool space the machine takes, and warns when either is short.

## Open the console

1. Open the machine from the Machines page and start it with the power controls.
2. Open the Console tab for the graphical display, or the Serial tab for a text console. Keys 1 to 6 switch between the VM tabs: Overview, Console, Serial, Hardware, Snapshots, Settings.
3. Click inside the display to send keystrokes to the guest.

The next pages cover each area in turn, starting with [virtual machines](vms.md).
