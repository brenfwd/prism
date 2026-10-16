# Prism user guide

This guide covers running Prism on one host or on a cluster of hosts. Each page is about one task area, and each step in it was checked against the code of this release.

- [Getting started](getting-started.md) takes a fresh host to a running VM.
- [Virtual machines](vms.md) covers creating, consoles, power, snapshots, hotplug, clone, move, live migration and delete.
- [Networking](networking.md) covers Prism networks, DHCP and DNS, NAT, IPv6, VLANs and port forwards.
- [Storage and the archive](storage-and-archive.md) covers disk pools, downloads, archive pools and ISO attach.
- [Backups](backups.md) covers full and incremental backups and restore.
- [Clustering](clustering.md) covers creating a cluster, joining nodes, roles, the hub, removal and upgrades.
- [Operations](operations.md) covers the systemd unit, `prism doctor`, AppArmor, logs and error messages.

The [README](../../README.md) has the install and build instructions, the command table and the `prism.toml` reference. [API.md](../API.md) describes the HTTP API, and every node serves its OpenAPI description at `/api/v1/openapi.json`.

The examples that call the API use `curl` against `http://127.0.0.1:8080/api/v1`. Write requests need a `Content-Type: application/json` header. Any cluster node accepts a request for any VM and forwards it to the node that owns the VM.
