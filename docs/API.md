# REST API

The API lives under `/api/v1` and is served identically by every node. Request and response bodies are JSON unless a route says otherwise. The examples in the [user guide](guide/README.md) use `curl` against `http://127.0.0.1:8080/api/v1`.

## OpenAPI

Every node serves an OpenAPI 3.1 description at `GET /api/v1/openapi.json`. The description lists each route with its tag, summary and path parameters, and gives every operation one `default` response, the `Problem` schema. It has no request or response body schemas. It is built from the operation table in `crates/prism/src/api/openapi.rs`, and a unit test fails when a route in the router is missing from the table, so update the table when routes change. The `/internal/…` routes that nodes use among themselves are not listed.

## Access and request checks

The API has no authentication. Prism listens on `127.0.0.1:8080` unless `http.listen` says otherwise, and every request that reaches it is trusted. One middleware in front of every route, including SSE and WebSockets, stops websites and DNS rebinding from driving the API through an operator's browser:

1. A `Host` outside `http.allowed_hosts` gets 421 `host_not_allowed`. IP literals, `localhost` and the node's names always pass, and `*` turns the check off. `X-Forwarded-Host` replaces `Host` for this check when present.
2. `GET`, `HEAD` and `OPTIONS` pass.
3. A WebSocket upgrade with an `Origin` that does not match `Host` or `http.allowed_origins` gets 403 `cross_origin`.
4. Any other request passes when `Sec-Fetch-Site` is `same-origin` or `none`, or when it has no `Sec-Fetch-Site` and no `Origin` (curl and the CLI). Otherwise `Origin` must match `Host` or `http.allowed_origins`, or the answer is 403 `cross_origin`.
5. A write with a body must have `Content-Type: application/json` or `application/octet-stream`, or the answer is 415 `unsupported_media_type`.

Responses carry `X-Content-Type-Options: nosniff` and `Cross-Origin-Resource-Policy: same-origin`. Bodies over 1 MiB are refused, except on the volume and archive upload routes.

## Errors

Errors are RFC 9457 problem details with the content type `application/problem+json`:

```json
{
  "type": "about:blank",
  "title": "Conflict",
  "status": 409,
  "code": "conflict",
  "detail": "another task is running on this VM",
  "retryable": false,
  "degraded": false
}
```

`code` is a stable machine-readable string, `detail` is the text the UI shows, `retryable` is true for 502 and 503, and `degraded` is true for `cluster_degraded`. The codes the common layer produces are:

| Status | Code | Meaning |
| --- | --- | --- |
| 400 | `invalid` | A field or path is not valid |
| 400, 415, 422 | `invalid_body` | The body is not valid JSON for the route, or a field is unknown or has the wrong type |
| 415 | `unsupported_media_type` | A write with a body used another content type |
| 403 | `cross_origin` | The Origin check failed |
| 404 | `not_found`, `no_such_node` | The resource or the owning node does not exist |
| 409 | `conflict`, `exists`, `in_use`, `libvirt_error` | The request clashes with current state, or libvirt refused it |
| 421 | `host_not_allowed` | The Host check failed |
| 500 | `internal`, `storage_error` | A server fault |
| 502 | `owner_unreachable`, `replica_unavailable`, `owner_unknown`, `not_clustered` | The node that owns the resource did not answer |
| 503 | `libvirt_unavailable`, `cluster_degraded`, `cluster_unavailable`, `replica_unavailable` | libvirt is not connected, or the cluster cannot commit changes |

Other codes appear on single routes, for example the join errors listed in the [clustering guide](guide/clustering.md). Unknown JSON fields are refused with `invalid_body`.

## Cluster behaviour

Any node answers any request. A request for a VM, a node's pools or a task that another node owns is forwarded to that node over the mesh and runs through its handlers, and the response comes back unchanged. `POST /vms?node=<id>` creates a VM on another node. WebSocket consoles are tunnelled separately. Changes to replicated state are committed through the hub or raft leader.

While a node cannot reach the hub or a quorum, every response carries `X-Prism-Degraded: true`, reads work, and any change that must commit answers 503 `cluster_degraded`.

Long operations (snapshots, clone, move, migrate, backups, restores, downloads, archive repair, scrub, rebalance and drain) answer `202 Accepted` with the task as the body and `Location: /api/v1/tasks/{id}`. Creating a VM, network or pool answers `201 Created`, and deletes answer `204 No Content`. There are no `ETag` or `If-Match` headers. `PATCH /vms/{id}` takes an optional `generation` field in the body for optimistic concurrency. Lists are not paginated.

## Routes

`{node}` accepts a node ID or `local`. IDs of VMs, networks, tasks and backups are UUIDs.

### Nodes and host

| Method and path | Purpose |
| --- | --- |
| `GET /nodes`, `GET /nodes/{node}` | Nodes with capacity and state |
| `GET /nodes/{node}/bridges` | Host bridges a NIC can attach to |
| `GET /nodes/{node}/prereqs` | Optional host programs and whether they work |

### VMs

| Method and path | Purpose |
| --- | --- |
| `GET /vms`, `POST /vms` | List, create (`?node=` picks the node) |
| `GET /vms/{id}`, `PATCH /vms/{id}` | Show, update |
| `DELETE /vms/{id}` | Delete; `?delete_disks=false` keeps the disk files |
| `GET /vms/{id}/delete-plan` | Files a delete would remove |
| `POST /vms/{id}/actions/{action}` | `start`, `shutdown`, `stop`, `reboot`, `reset`, `pause`, `resume` |
| `POST /vms/{id}/disks`, `PATCH` and `DELETE /vms/{id}/disks/{disk}` | Disks and CD-ROMs; `?delete_volume=true` on delete removes the volume |
| `POST /vms/{id}/nics`, `DELETE /vms/{id}/nics/{nic}` | NICs |
| `PUT /vms/{id}/nics/{nic}/forwards` | Replace the NIC's port forwards |
| `PUT /vms/{id}/nics/{nic}/vlans` | Set the NIC's VLAN and trunk |
| `GET` and `POST /vms/{id}/snapshots`, `DELETE /vms/{id}/snapshots/{name}` | Snapshots |
| `POST /vms/{id}/snapshots/{name}/revert` | Revert; `?confirm_memory=true` for a memory snapshot of a moved VM |
| `GET /vms/{id}/guest/network` | Guest addresses from the guest agent |
| `POST /vms/{id}/clone`, `POST /vms/{id}/move`, `POST /vms/{id}/migrate` | Clone (stopped VM), cold move, live migration |
| `GET /vms/{id}/console`, `GET /vms/{id}/serial` | WebSocket: VNC (binary RFB) and serial console |

### Backups

| Method and path | Purpose |
| --- | --- |
| `GET /vms/{id}/backups`, `POST /vms/{id}/backups` | A VM's backups, create a backup |
| `GET /backups`, `GET /backups/{id}`, `DELETE /backups/{id}` | All backups, show, delete |
| `POST /backups/{id}/restore` | Restore as a new VM |

### Storage

| Method and path | Purpose |
| --- | --- |
| `GET` and `POST /nodes/{node}/pools` | Local pools |
| `GET` and `DELETE /nodes/{node}/pools/{pool}` | Show, forget a pool |
| `GET` and `POST /nodes/{node}/pools/{pool}/volumes` | List, create an empty volume |
| `GET`, `PUT` and `DELETE /nodes/{node}/pools/{pool}/volumes/{volume}` | Show, upload (raw body, `application/octet-stream`), delete |
| `GET /nodes/{node}/pools/{pool}/volumes/{volume}/users` | VMs that use the volume |
| `POST /nodes/{node}/pools/{pool}/downloads` | Download a URL into the pool |

### Archive

| Method and path | Purpose |
| --- | --- |
| `GET` and `POST /archive/pools` | Archive pools |
| `PUT /archive/pools/{pool}` | Set the replica count (1 to 3) |
| `POST /archive/pools/{pool}/repair`, `/scrub`, `/rebalance` | Start the task for the pool |
| `GET /archive/pools/{pool}/files` | List files; `?prefix=` filters |
| `GET`, `PUT` and `DELETE /archive/pools/{pool}/files/{path}` | Download (with `Range`), upload, delete |
| `PUT /archive/members/{node}/{name}` | Set a member's state, `active` or `disabled` |
| `POST /archive/members/{node}/{name}/drain` | Move all files off a member |

Downloads are attachments with `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`. The `?local` and `?member` query parameters on archive routes are for requests that nodes forward to each other.

### Networks

| Method and path | Purpose |
| --- | --- |
| `GET` and `POST /networks` | List, create |
| `POST /networks/check` | Check a name and subnet before creating |
| `GET`, `PATCH` and `DELETE /networks/{id}` | Show, update, delete |
| `POST /networks/{id}/vlans`, `DELETE /networks/{id}/vlans/{vlan}` | VLAN subnets |
| `GET /networks/{id}/vlan-suggestion` | The subnet the next VLAN would get |

### Tasks

| Method and path | Purpose |
| --- | --- |
| `GET /tasks` | Tasks of every node; `?limit=` (default 100, at most 1000) |
| `GET /tasks/{id}`, `POST /tasks/{id}/cancel` | Show, cancel |

### Cluster

| Method and path | Purpose |
| --- | --- |
| `GET /cluster`, `POST /cluster` | Status, create (`{name, mode}` with mode `hub_spoke` or `consensus`) |
| `GET` and `POST /cluster/invites`, `DELETE /cluster/invites/{id}` | Invites; create takes `{name?, ttl_secs?}` |
| `POST /cluster/join` | Join with an invite; answers the reply to give to the sponsor |
| `POST /cluster/accept` | Admit a node from its reply (`{reply, confirm}`; without `confirm` it previews) |
| `POST /cluster/leave` | Leave the cluster and wipe this node |
| `GET /cluster/removed` | Removed members and who still trusts them |
| `DELETE /cluster/members/{node}` | Remove a member |
| `POST /cluster/members/{node}/voter` | Make a member a voter or demote it (consensus mode) |
| `POST /cluster/hub` | Hand the hub role to another member |
| `GET /cluster/mesh` | WireGuard peer status |

### Events and OpenAPI

| Method and path | Purpose |
| --- | --- |
| `GET /events` | Server-sent events |
| `GET /openapi.json` | The OpenAPI document |

## Server-sent events

`GET /api/v1/events?snapshot=1` opens one SSE stream. With `snapshot=1` the first event is `snapshot`, carrying `{cursor, nodes, vms, pools}`. Without it the stream starts at the live edge. The server sends `retry: 3000` first and a `ping` comment every 15 seconds.

Each event has an `id` of the form `<node id>.<boot_epoch>.<seq>`, which is monotonic on the node that produced it, an `event` name equal to its type, and a JSON body (the payload here is shortened):

```text
id: 7f3c0e1a-5b2d-4c9e-8a17-0d4e6f1b2c3d.3.18842
event: vm.state
data: {"type":"vm.state","origin_node":"7f3c0e1a-5b2d-4c9e-8a17-0d4e6f1b2c3d","resource":"/vms/9b2e4c1d-7a3f-4e58-b0a6-2f9d8c7e1a45","ts":"2026-10-16T09:41:07Z","op":"upsert","payload":{"state":"running"}}
```

`op` is `upsert` or `delete`, and `resource` is the API path of what changed. The types are:

| Type | Payload |
| --- | --- |
| `snapshot` | Nodes, VMs and pools at the stream's cursor (first event, with `snapshot=1`) |
| `vm.state`, `vm.stats` | A VM's runtime state; `vm.stats` is sent when only counters changed |
| `vm.config` | The VM view, or a delete |
| `pool`, `volume` | A local pool or volume |
| `network` | A network |
| `backup` | A backup record |
| `cluster` | The cluster document |
| `archive.pool`, `archive.file` | An archive pool or file |
| `task.progress` | A task |
| `node.status` | A node |
| `reset` | Sent when the client must start over |

A client that reconnects with `Last-Event-ID` receives the events it missed when the ID is still in the node's ring (10,000 events or 10 minutes, kept in memory). An ID from another node or boot, or one that fell out of the ring, gets `reset` and a snapshot. A client that cannot keep up gets `reset` and a snapshot too. Events that originate on other nodes appear on every node's stream with their original `origin_node`.

## Deleting a VM

`DELETE /vms/{id}` powers the VM off if it is running, undefines it (libvirt drops its snapshot metadata, managed-save image and NVRAM) and removes its cloud-init seed ISO. It also deletes the VM's disk files, including snapshot overlays, backing layers and saved memory images. Pass `delete_disks=false` to keep them. A file that another VM uses is kept in both cases. ISOs, archive files and volumes the VM never used are never touched.

`GET /vms/{id}/delete-plan` lists those files as `{items: [{pool, volume, kind, size_bytes, shared_with}]}`. `kind` is `disk` or `snapshot`, `size_bytes` is the space used on disk, and a non-empty `shared_with` means the delete keeps that file.

## IPv6 on networks

`POST /networks` takes `ipv6` (default `true`) and `nat66` (default `true` on a NAT network). With IPv6 on, the node picks a free ULA /64 and the network shows it as `subnet6`. `PATCH /networks/{id}` takes the same two fields: `ipv6: true` picks a prefix for a network that has none, and `ipv6: false` removes it. Networks created before IPv6 support have no `subnet6` until someone turns it on.

Every VM in `GET /vms` and `GET /vms/{id}` has `nic_ipv6`, a map from NIC ID to the SLAAC address that NIC forms on its network. It is absent for NICs on networks without IPv6. A port forward with an IPv6 `host_addr` targets that address.

## VLAN subnets

A network can give a VLAN its own subnet, with a gateway on every node, a DHCP scope, DNS and (on a NAT network) internet access. `GET /networks` and `GET /networks/{id}` list them as `vlan_subnets`, an object keyed by VLAN ID whose values carry `subnet` and `gateways` (one address per node).

`POST /networks/{id}/vlans` takes `{vlan, subnet?}` and returns the network. The VLAN ID is 2 to 4094. Without `subnet` the node picks a free `/24`. NICs of the network that are already on that VLAN move into the new subnet and get new addresses. It answers 409 when the VLAN has a subnet already or the subnet overlaps another network or a host route.

`DELETE /networks/{id}/vlans/{vlan}` removes the subnet. It answers 409 while a NIC is on the VLAN. `GET /networks/{id}/vlan-suggestion` returns `{subnet}`, the subnet the next `POST` without a `subnet` would use.

A NIC takes its address from the subnet of its `vlan` (its native VLAN for a trunk), or from the network's subnet when that VLAN has none. Changing a NIC's VLAN with `PUT /vms/{id}/nics/{nic}/vlans` gives it an address from the new subnet. Guests keep the old address until their DHCP client renews.

## Rolling upgrades

Each node advertises `{version, feature_level}`; `GET /api/v1/cluster` reports `supported_feature_level` beside the cluster's level. Join and accept are refused when levels differ by more than one (`version_skew`). New `Command` variants are gated with `feature_enabled(cluster_level, required)` and appear only after every node supports them and the level is raised. Upgrade learners and spokes first, then voters, then the hub.

## Versioning

The `/api/v1` contract changes additively. A breaking change would ship as `/api/v2` beside v1.
