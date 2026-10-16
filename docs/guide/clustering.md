# Clustering

Several Prism nodes can form a cluster. Members share one replicated state: VM definitions, networks, pool definitions, archive indexes, backups and the member list. They reach each other over a WireGuard mesh that Prism builds, so an API request sent to any node is forwarded to the node that owns the VM, pool or file. VMs live on one node at a time. They move between nodes only when you move them, see [Virtual machines](vms.md).

The mesh is one interface, `wg-prism`, on each node, with addresses from a random `fdXX:XXXX:XXXX::/48` prefix that the first node picks. The only port members need open between them is the WireGuard UDP port, 51820 by default (`cluster.wireguard_port` in `prism.toml`). Every member is fully trusted: a member's WireGuard key gives it access to all cluster services, and VM guests can never reach them.

## Choose a topology

The topology is chosen when the cluster is created and does not change afterwards.

- Hub and spokes (`hub_spoke`) has one voter, the hub. Every other member is a spoke that follows it. Changes to cluster state go through the hub. A spoke whose hub is away keeps its VMs running and can still be read, but cannot change anything.
- Consensus (`consensus`) has three or five voting members. Changes need a majority of the voters online, and the cluster survives the loss of a minority.

## Create a cluster

1. Set `[cluster] advertise` in `prism.toml` to the addresses other nodes reach this node at, for example `advertise = ["192.0.2.10"]`, and start or restart `prism serve`. A node without an advertised address cannot create invites (`this node has no reachable endpoint: set [cluster] advertise in prism.toml, or invite from another member`).
2. On the first node, open the Cluster page, enter a cluster name of up to 64 characters, choose the topology and press Create cluster. The command line equivalent is:

   ```sh
   prism cluster init --config /etc/prism --name lab
   prism cluster init --config /etc/prism --name lab --consensus
   ```

3. The node becomes the hub, or the first voter in consensus mode, and the command prints its mesh address and the next step.

A node that already belongs to a cluster answers `this node is already in a cluster`.

## Add a node

Adding a node takes three steps on two machines. The node that creates the invite is called the sponsor. Any active member can be the sponsor.

1. On the sponsor, open the Cluster page, press Invite node, and optionally suggest a node name. The command line equivalent is:

   ```sh
   prism cluster invite --config /etc/prism --name rack2-host1
   ```

   The invite is a block of text that carries no secrets. It is valid for one hour. `--ttl <seconds>` changes that, up to 7 days (`invites last at most 7 days`). Open invites are listed on the Cluster page. `prism cluster invite --revoke <id>` or the Revoke button cancels one.

2. On the new node, which must be standalone and running, open the Cluster page, paste the invite into Join a cluster and press Join cluster. The command line equivalent is:

   ```sh
   prism join --config /etc/prism <invite>
   ```

   The node prints a reply and a key fingerprint. The fingerprint is the first 8 characters of the node's WireGuard public key.

3. Paste the reply on the sponsor, in the Cluster page's invite dialog under Reply from the new node, or on the command line:

   ```sh
   prism cluster accept --config /etc/prism <reply>
   ```

   Both show the new node's name, its endpoints and its key fingerprint. Compare that fingerprint with the one the new node printed. Press Fingerprint matches, admit, or answer `y` at the prompt. `-y` skips the prompt. The preview says `behind NAT` for a node with no advertised address.

The new node appears as Joining and turns Active when it has caught up with the cluster state. It finishes by itself, so the join command does not need to be run again. Errors from the steps:

- `cluster_mismatch: this reply belongs to a different cluster`.
- `invite_not_found: no such invite`, when the invite was revoked or never existed.
- `invite_expired: the invite expired; create a new one`.
- `invite_consumed: this invite was already used by another node`.
- `address_mismatch: the reply does not match the invite's reserved address`.
- `member_exists: this node or key is already a member`.
- `member_tombstoned: this node was removed; run prism leave --force on it, then join with a new invite`.
- `version_skew: the node runs prism <version> (feature level <n>); upgrade one side`, when the two sides differ by more than one feature level.
- `this node is already in a cluster`, on the join side, when the node must leave first.

The cluster checks the expiry against its own clock, not the joining node's. A node behind NAT needs no advertised address: peers learn its address when it contacts them.

## Look at the cluster

`prism cluster status` prints one line for the cluster and one per member:

```text
Cluster lab (hub_spoke), this node: leader
  hub1             active   fd3a:7c1:9e02::1           192.0.2.10  (voter)
  rack2-host1      active   fd3a:7c1:9e02::2           192.0.2.11
```

On a standalone node it prints `Standalone (not in a cluster).`. When the node cannot reach a leader, the first line ends with `- DEGRADED (read-only)`. The Cluster page shows the same members with a health badge, a Hub or Voter badge, the version and the join time. `GET /cluster` returns the full status, `GET /cluster/mesh` returns the WireGuard peer status and `GET /nodes` lists the nodes.

## Voters in consensus mode

In consensus mode a new member joins as a non-voting learner. Use Make voter and Demote on the Members card to change the voter set, or `POST /cluster/members/{node}/voter` with `{"voter": true}`. The number of voters must be 1, 3 or 5, and a change that would leave another number is refused, for example `that would leave 2 voters; use 3 or 5 (or 1 while setting up)`. Hub and spokes clusters answer `voters are chosen only in consensus mode; hub-spoke has one voter, the hub`.

## Move the hub

The hub cannot leave or be removed while it is the only voter. Hand its role to another member first.

1. Run the command on the current hub, naming an active member:

   ```sh
   prism cluster promote --config /etc/prism rack2-host1
   ```

   The Leave dialog on the hub's Cluster page offers the same action.
2. Prism makes the target a voter, then removes the old hub's vote. The old hub becomes a spoke. The command waits up to 30 seconds for the new hub to take over. After that it answers `the new hub has not taken over yet; it will finish in the background`.

The command refuses in these cases: `only hub-spoke clusters have a hub; consensus clusters use voters`, `choose a different node to become the hub`, `only the current hub can hand over the hub role`, `<name> has not finished joining yet`, `no such member`, and `the cluster is degraded; wait until it is healthy`.

The node name in `promote` and `remove` can also be a node ID.

## Remove a node and leave a cluster

To remove a member from another member, run `prism cluster remove <node>` or press the trash icon next to the member. Prism refuses while the node still owns work:

- `<name> still owns VMs; move or delete them first`.
- `<name> still holds archive replicas; drain its archive members first`. See [Storage and the archive](storage-and-archive.md).
- `<name> is the hub, the cluster's only voter; hand the hub role over with prism cluster promote first`.
- In consensus mode, `<name> is a voter; removing it would leave <n> voters. Make another member a voter first so 1, 3 or 5 remain`.

A node cannot remove itself that way (`remove this node from another member`). Removal ends the node's access to the cluster everywhere: its WireGuard key, node ID and mesh address are blocked for good, so it cannot rejoin with the same identity. `prism cluster removed` lists the blocked members. It also shows which members still trust a removed node, until they have applied the removal:

```text
  0f6c1a52-8e3d-4b1f-9a77-2c4d5e6f7a88  fd3a:7c1:9e02::3  key 7Hq2mPzL0wVx9sTnB4rY8cJdK1aFeUoGi3N5bXtE6yM=  (log index 41)
    still trusts it: hub1 (at index 40)
```

To leave from the node itself, keep `prism serve` running and run:

```sh
prism leave --config /etc/prism
```

The node removes itself from the cluster, the daemon stops, and Prism deletes `data/` and `wg/` under the config directory, the `wg-prism` interface, the `ps*` and `vx*` links and the `prism` nftables tables. The config file stays. The command waits up to 30 seconds for the daemon to stop, and prints `Wiped <dir>/data and <dir>/wg. To rejoin, get a new invite and run prism join.`. The same restrictions as for removal apply. The hub answers `this node is the hub; hand the hub role to another member first (prism cluster promote <node>), or run prism leave --force`.

When a node is dead or cannot reach the cluster, use `prism leave --force` on it with `prism serve` stopped. This wipes the node without telling the cluster, so run `prism cluster remove` for it on another member afterwards. The last node of a cluster can leave without removal.

## Degraded clusters

A node is degraded when it sees no leader: a spoke that cannot reach the hub, or a consensus cluster that lost its majority. The Cluster page shows a Degraded notice: "This node cannot reach the rest of the cluster. Reads work, changes are paused." Every API response carries the header `X-Prism-Degraded: true`. A change is refused with HTTP 503, code `cluster_degraded` and a detail like `cluster is degraded: no hub or leader is reachable`, `cluster is degraded: the leader <id> is unreachable` or `cluster is degraded: leadership kept changing`. The error is marked retryable. A degraded node keeps its VMs running.

## Upgrades and feature levels

Every node reports its Prism version and a feature level. This release supports level 3. Level 2 added networks and level 3 added the replicated archive timers. The cluster keeps its own level, which is the minimum that all members support. `GET /cluster` shows it in `cluster.feature_level` and shows what the node's binary supports as `supported_feature_level`.

A rolling upgrade replaces the binary and restarts `prism serve` on one node at a time.

1. Upgrade spokes (and consensus learners) first, then the other voters, then the hub, which is last.
2. Between steps, the nodes may run versions up to one feature level apart. Join and accept refuse a larger gap with `version_skew`.
3. When every member runs the new binary, the cluster raises its level by itself. Operations that depend on the new level answer `the cluster is at feature level <n> but this needs <m>: upgrade every node to the same prism version, and the level rises automatically` until then.
