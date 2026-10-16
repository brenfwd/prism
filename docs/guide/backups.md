# Backups

A backup copies the disks of a VM into an archive pool, together with the VM's settings, so the VM can be rebuilt on any node. Backups are available through the API only. The UI has no backup page yet.

## Before the first backup

A backup needs an archive pool with at least one member, as described in [Storage and the archive](storage-and-archive.md). Prism stages each disk image as a temporary file in the pool of the VM's first disk, so that pool needs free space for one copy of the disk. The staged file is deleted after it has been stored.

Backups cover disks only. CD-ROM drives, snapshot history and saved memory are left out, and a restored VM has empty CD-ROM drives.

## Back up a VM

```sh
curl -X POST http://127.0.0.1:8080/api/v1/vms/$VM/backups \
  -H 'Content-Type: application/json' \
  -d '{"pool": "library"}'
```

The response is a task of kind `vm.backup`. `pool` names the archive pool, and `full` set to `true` forces a full backup. An unknown pool fails with `archive pool <name> not found`, and a VM that has another task running fails with `another task is running on this VM`. A VM with no disk makes the task fail with `the VM has no disks`.

How the backup runs depends on the VM:

- A running VM is backed up with libvirt's push mode. The guest keeps running. The first backup is full. Each later one is incremental and copies only the blocks that changed since the newest earlier backup whose libvirt checkpoint still exists. The task shows "full backup" or "incremental backup" and then "copying changed blocks".
- A stopped VM is copied by flattening each disk chain into one image, and the result is always a full backup.

Each disk is stored in the archive pool as `backups/<vm name>/<backup id>/<device>.qcow2`, where the device is the guest name such as `vda`. The archive records the BLAKE3 hash of every file. The task takes up to 24 hours before it gives up. `POST /tasks/{id}/cancel` stops it, and Prism removes the staged files and the new checkpoint.

Backups run on the node that owns the VM. Send the request to any node and the cluster forwards it.

## List and inspect backups

- `GET /vms/{id}/backups` lists the backups of one VM, oldest first.
- `GET /backups` lists the backups of every VM.
- `GET /backups/{id}` returns one.

Each entry has `id`, `vm`, `vm_name`, `created` (Unix seconds), `kind` (`full` or `incremental`), `parent` (the backup an incremental one builds on), `checkpoint`, `pool`, `disks` (each with `target`, `path`, `size` and `blake3`) and `spec`, the VM as it was.

Backups outlive the VM they came from, so deleting a VM does not delete its backups.

## Restore a backup

A restore always creates a new VM.

```sh
curl -X POST http://127.0.0.1:8080/api/v1/backups/$BACKUP/restore \
  -H 'Content-Type: application/json' \
  -d '{"name": "web1-restored", "pool": "default"}'
```

1. Pick the backup. For an incremental backup Prism restores the whole chain, from the full backup up to the one you picked.
2. Give the new VM a name that is free (`a VM named <name> already exists`) and a local disk pool on the node that handles the request, which becomes the node that owns the new VM.
3. Follow the `backup.restore` task. For each disk Prism fetches every layer from the archive, verifies the hash, links the layers and flattens them into `<name>-<device>.<format>` in the pool. The disk format is the pool's format.

The new VM has a new ID and new MAC addresses, and its network leases are assigned again. Its settings are the ones recorded in the backup. It is defined but not started. A restore fails with `<path> is missing from archive pool <pool>` when an archive file is gone, which a scrub can reveal.

## Delete a backup

`DELETE /backups/{id}` deletes the archive files and the record. It refuses while a later incremental backup depends on the backup (`later incremental backups depend on this one; delete them first`), so delete a chain from the newest backup backwards. A file that cannot be removed is logged and the backup record is deleted anyway.
