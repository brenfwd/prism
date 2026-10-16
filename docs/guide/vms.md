# Virtual machines

Each VM has a page with six tabs: Overview, Console, Serial, Hardware, Snapshots and Settings. The Machines page lists every VM in the cluster, and the keys `g` then `m` jump to it from anywhere. `?` opens the keyboard shortcut list and `Ctrl+K` (or `/`) opens the command palette.

Long operations run as tasks. Snapshots, clone, move, live migration, backups and downloads return at once with a task, and the task tray and the Tasks page show their progress. `POST /tasks/{id}/cancel` stops a running task, and finished tasks are kept for 7 days.

## Create a VM

The New machine form is described in [Getting started](getting-started.md). The same request through the API:

```sh
curl -X POST http://127.0.0.1:8080/api/v1/vms \
  -H 'Content-Type: application/json' \
  -d '{"name": "web1", "vcpus": 2, "memory_mib": 4096, "pool": "default", "disk_gib": 32,
       "iso": "debian-12.iso"}'
```

`name`, `vcpus`, `memory_mib` and `pool` are required. These fields are optional:

- `disk_gib` creates a boot disk of that size. Leave it out for a VM without a disk.
- `iso` attaches a volume from the pool as a CD-ROM. `image` copies a volume from the pool as the boot disk and grows it to `disk_gib`. A request cannot give both, and the error is `give either image or iso, not both`.
- `network` is one of `{"type": "network", "network": "<id>"}`, `{"type": "bridge", "bridge": "br0"}` and `{"type": "user"}`. Without it the VM joins the network named `default`, or user-mode networking when there is none.
- `cloud_init` takes `user_data` and an optional `network_config`. Prism writes them to a small seed ISO next to the first disk.
- `firmware` is `bios` (default) or `uefi`. `cpu_model` is `host_model` (default) or `host_passthrough`. Passthrough exposes the exact host CPU, so the VM can only move to hosts with an identical CPU.
- `balloon` is `manual` (default), `auto` or `off`, and `min_memory_mib` is the balloon floor.
- `autostart` starts the VM when the host boots. Prism does this itself and never uses libvirt autostart.

The VM names match `[A-Za-z0-9._-]`, at most 64 characters, with no leading dot or dash. `vcpus` runs from 1 to 512 and memory is at least 64 MiB. The API hides cloud-init user data, because it can hold secrets, and returns a `blake3:` hash in its place.

## Power

The power controls on the VM page map to `POST /vms/{id}/actions/{action}`, where the action is one of these:

- `start` redefines the domain from the stored spec, so pending changes take effect, and then boots it.
- `shutdown` asks the guest to shut down, and the guest stops itself.
- `stop` cuts the power. The UI asks for confirmation first.
- `reboot` asks the guest to restart. `reset` is a hard reset, and the UI asks for confirmation.
- `pause` and `resume` freeze and thaw the guest CPUs.

`start` fails with `host memory pressure is critical; refusing to start more VMs` while the host is in the red band of auto-ballooning.

## Console

The Console tab shows the guest display over VNC, which Prism decodes in the browser. Click the display to send keystrokes to the guest. The clipboard button types the clipboard text as keystrokes with a US layout. The `Ctrl+K` shortcut stays with the guest while the console has focus, and `Cmd+K` still opens the palette on macOS.

The Serial tab opens the serial console. A new serial connection takes the console over from any other client.

Consoles work for VMs on other nodes, because the node you are connected to tunnels the stream to the owner. A VM that is off or still starting has no display, and the API answers `VM has no VNC socket; is it running?`.

## Settings

The Settings tab changes these fields. The API equivalent is `PATCH /vms/{id}`.

- Description and Start when the host boots take effect at once.
- vCPUs, memory size and CPU model change the stored spec and apply at the next power cycle. A running VM shows the banner "Some changes apply after a power cycle".
- Memory ballooning mode applies at once. The Live memory slider on a running VM moves the balloon target without a reboot, between the floor and the memory size.
- The cloud-init card replaces or removes the user data. The guest reads it again only if its cloud-init runs again.

Ballooning has three modes. In `off` the guest has no balloon device and keeps all of its memory. In `manual` you set the target. In `auto` Prism checks every 10 seconds. When the host is under pressure it shrinks the balloon toward what the guest uses plus 20 percent, and when host memory is critical it shrinks toward the floor. When the guest runs short it grows the balloon again. Each step is at most 256 MiB, and the balloon never goes below the floor. The default floor is a quarter of the memory size, at least 128 MiB. The balloon is a virtio device, so the guest needs its driver.

Boot order is part of `PATCH /vms/{id}`: `boot_order` is a list of disk and NIC IDs, and devices left out of the list do not boot. The Hardware tab has a boot order editor. Send `generation` with a request to fail with `vm <id> is at generation <n>, not <m>` when someone else changed the VM first.

## Hardware and hotplug

The Hardware tab lists disks, CD/DVD drives and network adapters. A change is saved in the VM spec and then applied to the running guest when possible. The API response carries `hotplug: {applied, restart_required, error}`, and the VM shows "pending restart" when a change still waits.

1. Press Add disk, then choose New empty disk with a size and pool, or Existing volume. A running VM gets the disk hot-plugged. New disks are thin.
2. Press Add drive to add a CD/DVD drive. A drive added to a running VM appears at the next power cycle. Use Insert on a drive to choose an ISO from the drive's pool, and Eject to remove the disc. Both work while the VM runs. The API can also attach an ISO straight from an archive pool, see [Storage and the archive](storage-and-archive.md).
3. Use Grow on a disk to enlarge it. Disks cannot shrink (`disks can only grow`), and only disks resize, not CD-ROMs. The guest must grow its own partition and file system afterwards.
4. Use Detach on a disk to unplug it. Prism waits up to 10 seconds for the guest to release the device. The dialog can delete the volume as well, and the file is kept when another VM uses it. `DELETE /vms/{id}/disks/{disk}?delete_volume=true` does the same.
5. Press Add adapter to add a NIC, as a Prism network, a host bridge or user-mode networking. The MAC address is random unless you give one, and it must be a unicast address that no other VM uses (`invalid unicast MAC address`, `MAC <mac> is already in use`). The bridge name must be a valid Linux interface name of 1 to 15 characters (`invalid bridge name`). Remove adapter unplugs it.

Disks keep their guest device names (`vda`, `vdb`) across hotplug. Disks use the virtio bus and CD-ROMs use SATA. VLANs and port forwards are covered in [Networking](networking.md).

## Snapshots

A snapshot records the state of the disks, and optionally the memory, so the VM can return to it. Prism uses libvirt external snapshots: each disk continues in a new overlay file called `<name>.<snapshot>.qcow2` in the same pool, and the files below it become read-only.

1. Open the Snapshots tab and press Take snapshot.
2. Leave the name empty to get `snap-<seconds since the epoch>`, or give a name that matches `[A-Za-z0-9._-]` with at most 64 characters. Add a description if you like.
3. Tick Include memory on a running VM to save the RAM as well. Saving memory briefly pauses the guest, and a memory snapshot needs a disk to store it beside (`a memory snapshot needs a disk to store it beside`). When the guest agent is connected, Prism first asks it to freeze the file systems, and falls back to a crash-consistent snapshot if that fails.
4. Press Take snapshot. The snapshot appears in the tree under its parent.

To go back, choose Revert on a snapshot. A snapshot with memory resumes the running VM exactly where it was. A snapshot without memory restarts the VM from the saved disks. Reverting discards what happened since, so take a new snapshot first if you may want it. Delete merges the snapshot's overlays into their parents and removes the files libvirt no longer uses.

Revert and delete need libvirt 9.9 or newer on the host, and fail with `reverting and deleting external snapshots needs libvirt 9.9 or newer` otherwise. Only one task runs on a VM at a time (`another task is running on this VM`). After a VM has moved between nodes, reverting to a snapshot with memory needs `confirm_memory=true`, because that path has crashed libvirtd in testing. The UI asks for confirmation. Shutting the VM down first and reverting avoids it.

The API calls are `GET` and `POST /vms/{id}/snapshots`, `DELETE /vms/{id}/snapshots/{name}` and `POST /vms/{id}/snapshots/{name}/revert`.

## Clone

Clone makes an independent copy of a stopped VM.

1. Stop the VM. Cloning a running VM fails with `<name> is running; shut it down to clone it`.
2. Open the machine menu on the VM page and choose Clone.
3. Give the new name and press Clone. The name must be free (`a VM named <name> already exists`).

Prism copies every disk into a new qcow2 volume named `<name>-<device>.qcow2` in the disk's pool, which flattens snapshot history. The copy gets a new ID, new MAC addresses and new network leases, and it is owned by the node that ran the clone. Port forwards on user-mode NICs are not copied. API: `POST /vms/{id}/clone` with `{"name": "web2"}`.

## Move a stopped VM to another node

A cold move copies the disks and snapshot files to a pool on another cluster node, hands ownership to that node and deletes the source copy. It needs a cluster, and the VM must be stopped (`<name> is running; shut it down first (live migration moves running VMs)`).

```sh
curl -X POST http://127.0.0.1:8080/api/v1/vms/$VM/move \
  -H 'Content-Type: application/json' \
  -d '{"node": "<destination node id>", "pool": "default"}'
```

The destination must be an active member (`active member <id> not found`) and not the VM's current node (`the VM is already on that node`). Node IDs are in `GET /nodes` and `prism cluster status`.

## Live migration

Live migration moves a running VM to another node without stopping it. QEMU copies the memory and the disks to the destination, then the VM switches over with at most 300 ms of pause.

```sh
curl -X POST http://127.0.0.1:8080/api/v1/vms/$VM/migrate \
  -H 'Content-Type: application/json' \
  -d '{"node": "<destination node id>", "pool": "default", "postcopy": false}'
```

1. Eject every CD-ROM first (`eject CD-ROMs before migrating`).
2. Pick the destination node and the pool on that node that receives the disks.
3. Set `postcopy` to `true` to let Prism switch to post-copy when pre-copy does not converge. The destination then always wins.
4. Follow the `vm.migrate` task. The VM must be running (`<name> is not running; use move for stopped VMs`).

When a migration fails, Prism restores the source's snapshot metadata and clears the migration intent. `host_passthrough` VMs only migrate between hosts with identical CPUs.

## Delete a VM

1. Open the Settings tab and press Delete in the red card.
2. The dialog lists the files that go with the VM: disks, snapshot overlays, backing layers and saved memory images. A file that another VM uses is marked and kept.
3. Type the VM name to confirm and press Delete forever.

Prism powers the VM off if it runs, removes its definition and its cloud-init seed ISO, and deletes its files. `DELETE /vms/{id}?delete_disks=false` keeps the files. `GET /vms/{id}/delete-plan` returns the list of files. ISOs, archive files and volumes the VM never used are never touched.
