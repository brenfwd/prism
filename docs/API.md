# REST API

The API lives under `/api/v1`. The OpenAPI 3.1 description is served by every
node at `GET /api/v1/openapi.json`; it is built from the operation table in
`crates/prism/src/api/openapi.rs`, which must be updated when routes change.
Errors are RFC 9457 problem details (`application/problem+json`).

## Deleting a VM

`DELETE /vms/{id}` powers the VM off if it is running, undefines it (libvirt
drops its snapshot metadata, managed-save image and NVRAM) and removes its
cloud-init seed ISO. It also deletes the VM's disk files, including snapshot
overlays, backing layers and saved memory images. Pass `delete_disks=false` to
keep them. A file that another VM uses is kept in both cases. ISOs, archive
files and volumes the VM never used are never touched.

`GET /vms/{id}/delete-plan` lists those files as `{items: [{pool, volume,
kind, size_bytes, shared_with}]}`. `kind` is `disk` or `snapshot`, `size_bytes`
is the space used on disk, and a non-empty `shared_with` means the delete keeps
that file.

## Rolling upgrades

Each node advertises `{version, feature_level}`; `GET /api/v1/cluster` reports
`supported_feature_level` beside the cluster's level. Join and accept are
refused when levels differ by more than one (`version_skew`). New `Command`
variants are gated with `feature_enabled(cluster_level, required)` and appear
only after every node supports them and the level is raised. Upgrade learners
and spokes first, then voters, then the hub.
