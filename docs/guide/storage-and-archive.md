# Storage and the archive

Prism keeps two kinds of storage. Disk pools hold the disks of VMs and sit on one node. Archive pools hold ISOs, backups and other files for the whole cluster, and keep copies of each file on one to three nodes.

## Disk pools

A disk pool is a directory on a node. Each file in it is a volume: a disk image, an ISO or a cloud image. The Storage page shows the pools of the node you are connected to, with free space and the volumes in each. The filter has All, Images, ISOs and Machine disks.

A pool has a disk format, `qcow2` or `raw`, which new volumes use. qcow2 is thin and supports snapshots. raw files are fully allocated and a little faster. Pools are defined in two ways:

- In `prism.toml`, in a `[pools.<name>]` table with `type = "dir"`, an absolute `path` and a `format`. `prism init` writes one called `default`. These pools can only be removed by editing the file.
- Through the UI or API, under one of the directories in `storage.pool_roots`. The pool lives at `<root>/<name>`.

To create a pool in the UI:

1. Add the parent directory to `pool_roots` in `prism.toml` and restart `prism serve`. Without it the request fails with `no storage.pool_roots configured`, and a `root` outside the list fails with `<path> is not in storage.pool_roots`.
2. Open the Storage page and press New pool.
3. Enter a name and choose the disk format.

The API is `POST /nodes/{node}/pools` with `{"name": "fast", "format": "qcow2"}` and an optional `root`. `{node}` is the node ID or `local`. Pool names follow the volume rules: letters, digits, dots, dashes and underscores, not starting with a dot or dash.

Remove a pool from the menu on its card. This forgets the pool and leaves the files alone. The removal fails while a VM uses the pool (`used by <vms>`).

A pool on a file system that does not support `O_DIRECT`, such as tmpfs, still works. Prism logs `no O_DIRECT support; disks will use cache=writeback` for it.

### Add files to a pool

1. Drop an ISO, a cloud image or a raw disk anywhere on the Storage page, or press Choose files. The file picker offers `.iso`, `.img`, `.qcow2` and `.raw`. Uploads of any size stream to disk.
2. Or press From URL. Enter an `http://` or `https://` link, a name (the last part of the link is the default) and optionally a SHA-256 checksum of 64 hex digits. Prism downloads in the background as a `volume.download` task, follows up to 5 redirects and rejects URLs with credentials. With a checksum, a file that does not match is discarded (`checksum mismatch: expected <a>, got <b>`). The file only appears in the pool when the download finishes.
3. To make an empty disk, press New disk, give a name and a size in GiB.

The API calls under `/nodes/{node}/pools/{pool}` are `PUT volumes/{name}` with the raw file as the body and `Content-Type: application/octet-stream`, `POST volumes` with `{"name": "data.qcow2", "size_gib": 50}`, `POST downloads` with `{"url": "...", "name": "...", "sha256": "..."}`, `GET volumes` and `GET volumes/{name}`. An upload or download to a name that exists fails with `<name> already exists`. Download errors you may see are `the URL must be http or https`, `the checksum must be 64 hex digits (SHA-256)`, `name the volume; the URL has no file name` and `invalid volume name`.

A volume that a VM uses cannot be deleted. The delete button is disabled with the label `In use by <vms>`, and `GET .../volumes/{name}/users` lists them. The Boot from this ISO button on an ISO starts the New machine form with that ISO chosen.

## Archive pools

An archive pool is a namespace of files, such as `isos/debian-12.iso`, spread over directories that nodes contribute. Each file is stored whole on a member directory, never split. The Archive page manages pools and members.

### Add disks to an archive pool

A member is a directory on one node, declared in that node's `prism.toml`:

```toml
[archive_members.bulk1]
pool = "library"
path = "/srv/prism/library"
weight = 1.0
reserve = "50GiB"
```

`pool` names the archive pool, and the pool is created on first use with one copy per file. `path` is absolute. `weight` is the relative share of new files and defaults to 1. `reserve` is the free space Prism keeps on that file system, as a number of bytes or a size like `50GiB`, and the member takes no new files once it would drop below it. Restart `prism serve` after editing the file. Member names are unique on a node.

1. Open the Archive page and press Create pool, or Add another pool when one exists.
2. Enter a name and choose the number of copies, 1 to 3. The page suggests 2 copies so that any one node can fail. The copies sit on different nodes, so a pool needs members on that many nodes before it has all of them.
3. Add `[archive_members.<name>]` sections as above on the nodes that should store the files.

Change the number of copies later from the pool card, or with `PUT /archive/pools/{pool}` and `{"replicas": 2}`. The extra copies fill in the background as an `archive.repair` task. The API refuses a value outside 1 to 3 with `replicas must be 1-3`.

### Upload, download and delete files

The Archive page does not handle files. Use the API, on any node:

```sh
curl -T debian-12.iso -H 'Content-Type: application/octet-stream' \
  http://127.0.0.1:8080/api/v1/archive/pools/library/files/isos/debian-12.iso
curl -O http://127.0.0.1:8080/api/v1/archive/pools/library/files/isos/debian-12.iso
curl -X DELETE http://127.0.0.1:8080/api/v1/archive/pools/library/files/isos/debian-12.iso
curl 'http://127.0.0.1:8080/api/v1/archive/pools/library/files?prefix=isos/'
```

Paths are `/`-separated names of letters, digits, dots, dashes and underscores, at most 16 levels deep. A bad path is refused with `paths are /-separated names of [A-Za-z0-9._-]`. Prism chooses the member for a new file by picking two active members at random, weighted by weight times free space, and taking the one with more room. It hashes the file with BLAKE3 while writing, records the hash in the cluster state and then copies it to the other members that the copy count asks for. Downloads support `Range` requests. A file's kind (`iso`, `disk`, `backup` or `other`) comes from its extension.

An upload fails with `archive pool <pool> has no reachable member directories; add [archive_members] to a node's prism.toml` when no member is online, with `no member of <pool> has room` when every member is full or under its reserve, and with `<path> already exists` for a repeat.

### Scrub, repair and rebalance

The three buttons on a pool card start tasks, one at a time per pool. A second request answers `another archive task is running on this pool`.

- Repair copies files that have fewer copies than the pool asks for onto other members. Prism also does this by itself 24 hours after a node has been unreachable.
- Scrub reads every copy on every node and compares its BLAKE3 hash with the recorded one. It replaces a bad or missing copy from a good one and reports counts of `checked`, `corrupt`, `missing`, `repaired` and `unrecoverable`. Prism scrubs every pool once a week without being asked.
- Rebalance moves files from members that are 10 percentage points fuller than the pool average toward emptier ones, at most 20 files per pass. Prism runs it daily.

Prism checks archive health every 5 minutes.

### Drain, pause and remove a member

Each member row on the Archive page shows its node, its state and its free space.

- Pause sets the member to `disabled`. It takes no new files and keeps what it has.
- Resume sets it back to `active`.
- Drain copies every file off the member to other members, deletes the source copies and then disables the member. A running drain is shown as `draining`. A second drain request answers `this member is already draining`.

To retire a node, drain each of its members first. `prism cluster remove` refuses while the node holds copies: `<node> still holds archive replicas; drain its archive members first`. The API calls are `PUT /archive/members/{node}/{name}` with `{"state": "disabled"}` and `POST /archive/members/{node}/{name}/drain`.

A download is served by the node you ask when it holds a copy, and otherwise by the first node that does. When that node is unreachable the request answers 502 with code `replica_unavailable`, and a file with no copy at all answers 503 with the same code. Repair counts a copy on a node that has been offline for more than 24 hours as missing.

## Attach an archive ISO to a VM

An archive ISO can be inserted into a CD-ROM drive without copying it by hand. This is an API operation. Add a drive or change the media of an existing one with an `archive` reference:

```sh
curl -X PATCH http://127.0.0.1:8080/api/v1/vms/$VM/disks/$DISK \
  -H 'Content-Type: application/json' \
  -d '{"archive": {"pool": "library", "path": "isos/debian-12.iso"}}'
```

`POST /vms/{id}/disks` takes the same `archive` field together with `"device": "cdrom"` and the local `pool` that receives the file. Archive files attach only as CD-ROMs, without a `volume` (`archive files attach only as CD-ROMs, without a volume`).

Prism fetches the file once into a hidden cache volume in that pool, named `archive-<hash>.iso`, and checks its BLAKE3 hash before the first use. The drive then reads the local copy, so the VM does not depend on the node that stores the archive file. Each pool keeps at most 32 GiB of cached ISOs. When room is needed, Prism removes the least recently used cached ISOs that no VM uses and that were not touched in the last 10 minutes. The sweep runs hourly.
