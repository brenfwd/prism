# Networking

A Prism network is a virtual switch. Each node that has the network keeps a Linux bridge named `ps<vni>` and a VXLAN device named `vx<vni>`, and the VXLAN traffic between nodes travels inside the WireGuard mesh. A VM on a Prism network therefore shares one layer 2 segment with the other VMs on it, wherever they run. The VNI is a number between 100000 and 999999 that Prism picks when it creates the network. Prism embeds the DHCP server, the DNS server and the NAT rules, so it needs no dnsmasq and no libvirt network.

A VM NIC can also join a host bridge that you manage yourself, or use user-mode networking. Those two are plain QEMU attachments, and the rest of this page is about Prism networks except where it says otherwise.

## Create a network

A fresh install gets a network called `default` a few seconds after the first start. To make another:

1. Open the Networks page and press New network.
2. Name it, or leave the name empty. The first network is called `default` and later ones `net1`, `net2` and so on. A name is a DNS label of 1 to 32 lowercase letters, digits and hyphens that does not start or end with a hyphen.
3. Choose Access. Internet access is a NAT network. Isolated lets machines talk to each other and nothing else.
4. Leave the subnet alone to let Prism pick a free one, or press Choose the subnet myself and type a range. Prism suggests `10.88.0.0/24` first and moves on when something uses it. A custom range must be between `/16` and `/29`, with no host bits set, and must not overlap another network, a route the host already has, or `100.64.0.0/10`, which Tailscale uses. The form checks the name and subnet as you type and offers a free subnet when yours clashes.
5. Set IPv6 and the allowed destinations if you need them, then save.

The API call is `POST /networks`. Every field is optional: `name`, `kind` (`nat` or `isolated`), `subnet`, `egress`, `dhcp`, `allow_prefixes`, `ipv6` and `nat66`.

A VM joins a network from the New machine form, from Add adapter on the Hardware tab, or through the `network` field of `POST /vms` and `POST /vms/{id}/nics`. Deleting a network needs every NIC off it first: `network <name> is used by <vms>; remove their NICs first`.

## Edit a network

Press the pencil on a network card. The name, the access kind, IPv6, the allowed destinations and the subnet can change. A new subnet applies to stopped machines only. Prism keeps each address's host offset when the new range has the same size and picks free addresses otherwise. It refuses with `stop <vms> before changing the subnet: running guests would keep their old address` while any VM on the network runs. Switching to Isolated clears the egress node, the allowed destinations and NAT66.

The card also has a Turn DHCP off or Turn DHCP on button. Without DHCP the machines do not get an address from Prism, so they need a static configuration. The API is `PATCH /networks/{id}`, which takes the same fields as `POST /networks`.

## Addresses, DHCP and DNS

Prism gives each NIC on a network an address when the VM is stored, and keeps it for as long as the NIC stays on that network. The DHCP server hands that address to the NIC's MAC address, together with the gateway, the DNS server, the MTU and the host name. Each node answers DHCP only for the VMs that run on it, so a migrated VM keeps its address and nothing has to hand a lease over.

Every node that has the network also holds a gateway address on the bridge, one per node. That address is the DHCP and DNS server for the VMs running there. Clones and restores get new addresses.

The DNS server answers for `<vm>.<network>.prism`, for example `web1.default.prism`, with an A record and, on IPv6 networks, an AAAA record. Everything else goes to the host's resolvers. Names under `ts.net` are never forwarded. The `GET /networks` response lists the zone as `domain` and the bridge as `bridge`.

Guests on a Prism network cannot reach the node itself except for DHCP, DNS (UDP and TCP port 53), ping and IPv6 neighbour discovery. They cannot reach the API, the cluster ports or the WireGuard interface.

## NAT and egress

On an Internet access network, outbound traffic leaves through a node that masquerades it behind its own address. The node's gateway address is the guests' default route. By default a VM uses the gateway of the node it runs on. Set `egress` on the network to a node ID to send all of the network's traffic out of that one node:

```sh
curl -X PATCH http://127.0.0.1:8080/api/v1/networks/default \
  -H 'Content-Type: application/json' -d '{"egress": "<node id>"}'
```

`{"egress": null}` returns the network to one egress per node. A NIC can carry its own `egress` node, which wins over the network's.

Guests never reach these destinations through the host: the tailnet range `100.64.0.0/10`, `fd7a:115c:a1e0::/48`, the cluster's WireGuard mesh and the addresses the cluster members advertise. The Allowed destinations field of the network (`allow_prefixes`) opens chosen IPv4 or IPv6 prefixes inside those ranges, such as `100.64.2.0/24` or `fd7a:115c:a1e0:ab12::/64`. A network takes at most 16 prefixes, each with no host bits set, and `/0` is refused. Isolated networks take none.

## IPv6

A network gets an IPv6 prefix by default: a random `/64` inside `fc00::/7` that no other network, the cluster mesh or a host route uses. The network card shows it next to the IPv4 subnet.

- Guests configure their address themselves (SLAAC) from router advertisements that each node sends every 120 seconds. The address comes from the NIC's MAC address in the modified EUI-64 form, which Linux uses by default. `GET /vms` lists it as `nic_ipv6`, a map from NIC ID to address.
- A guest may use other addresses in the prefix, such as the stable-privacy and temporary addresses NetworkManager and most desktop systems form. The node learns them from the IPv6 neighbour table of the bridge (every address the guest sent neighbour discovery from) and, when the guest agent runs, from the addresses the agent reports. A learned address is used for the AAAA records and for IPv6 port forwards in place of the EUI-64 one. The bridge accepts as source the learned addresses, the EUI-64 address, a link-local address and `::`. A guest's first packets from a new address can be dropped for up to 15 seconds, until the next reconcile pass. A NIC keeps at most 8 learned addresses, and an address stays known for 30 minutes after it was last seen. An address that is another NIC's EUI-64 address, an address the guest agent reports for another NIC, or a node's address on the bridge is never learned. A NIC that already has 8 seen addresses ignores new ones until old ones expire. Learned addresses live on the node that runs the VM. Other nodes answer with the EUI-64 address unless their own neighbour table has seen the guest.
- Guests cannot send router advertisements or DHCPv6 server replies (UDP source port 547) into the network, and neighbour messages that name another MAC address in their link-layer option are dropped, as are those whose first option is not that link-layer option. Neighbour messages sent from a node's addresses, and advertisements that claim them, are dropped too. Duplicate address detection from `::` passes. Trunk NICs get the same IPv6 checks on VLAN-tagged frames as on untagged ones, apart from learning, which uses untagged frames only. The checks read the first header after the IPv6 header, so a frame with extension headers in front of the ICMPv6 or UDP header is not matched.
- NAT66 is on for Internet access networks. It rewrites IPv6 traffic from the private prefix to the host's address, so guests reach IPv6 sites. The host needs an IPv6 default route. The IPv6 internet access switch in the network form turns it off.
- The DNS zone gets AAAA records, and the node answers DNS on its IPv6 address as well.

Turn IPv6 on or off with the IPv6 switch or `PATCH /networks/{id}` with `{"ipv6": true}`. Turning it on picks a prefix for a network that has none, and `false` removes it. Networks created before IPv6 support have no prefix until someone turns it on. Isolated networks cannot use NAT66 (`an isolated network has no way out; NAT66 needs a NAT network`).

## VLANs and trunks

A network carries 802.1Q VLANs inside its VNI. A VLAN ID runs from 2 to 4094, and each tag costs 4 bytes of MTU.

Each NIC on a Prism network has one of three modes. Open the Hardware tab and press the VLANs button on the adapter:

- Default puts the NIC on the untagged segment, which has the network's gateway and DHCP.
- One VLAN makes the NIC an access port on that VLAN. It sees only NICs on the same VLAN.
- Trunk delivers the listed VLANs tagged and keeps an optional native VLAN untagged. Tagged VLANs are comma-separated, and ranges work, as in `10, 20, 30-32`.

The change applies to a running machine at once. The rules are that IDs are between 2 and 4094 (`VLAN <n> is out of range 2 to 4094`), a VLAN cannot be both native and tagged, and the tagged list has no repeats. VLANs need a NIC on a Prism network (`VLANs need a NIC on a Prism network`). The API is `PUT /vms/{id}/nics/{nic}/vlans` with `{"vlan": 10, "trunk": [20, 30]}`.

A VLAN has no DHCP of its own unless you give it a subnet. Without one, the machines on it use static addresses.

### VLAN subnets

A VLAN subnet gives a VLAN its own address range, a gateway on every node, a DHCP scope, DNS and, on an Internet access network, NAT. Each VLAN interface may forward only out of the network's uplinks, so traffic does not cross between a VLAN and another segment.

1. On the Networks page, press Add VLAN on the network card.
2. Enter the VLAN ID. Prism picks a free `/24` and shows it.
3. Press Add subnet.

NICs already on that VLAN move into the new subnet and get new addresses, which their guests pick up when the DHCP client renews. `POST /networks/{id}/vlans` takes `{"vlan": 10, "subnet": "10.88.10.0/24"}`, and `subnet` is optional. `GET /networks/{id}/vlan-suggestion` returns the subnet the next one would use, and `DELETE /networks/{id}/vlans/{vlan}` removes one.

The errors are `VLAN <n> already has a subnet on <network>` and `<subnet> overlaps <other>, which a network or this host already uses`. Removing a subnet answers 409 while a NIC is on the VLAN. IPv6 stays on the untagged segment. A NIC changed to another VLAN with a subnet gets an address from that subnet.

## Port forwards

A port forward sends a port on the host to a port on a guest. It applies to NICs on a Prism network and to user-mode NICs. Open the Hardware tab and press the link icon on the adapter, then Add forward. The form starts with an unused host port from 2222 and the text "Nothing is forwarded. Add SSH to get started."

A row has a protocol (TCP or UDP), a host port, a guest port and the options below. Ports run from 1 to 65535. The same protocol and host port cannot appear twice (`tcp/2222 is forwarded twice`), and a host port serves one guest per node (`tcp/2222 is already forwarded to <vm>`).

On a Prism network the node that runs the VM installs destination NAT rules, and the change applies at once. The NIC needs an address first (`the NIC has no address lease to forward to yet`). The options are these:

- Host address limits the forward to one host address. Left empty, it answers on every address of the node (the destination must be one of the node's own addresses), but only for connections that arrive on the node's uplink. Connections from the mesh or from other networks are not forwarded.
- From this host (`local`, on by default) makes the forward work for connections that start on the node itself. With no host address it listens on `127.0.0.1`, since the node's other addresses reach the guest through the forward already.
- From other guests (`hairpin`, on by default) lets other machines on the same network reach the guest through the host address.

A forward whose host address is IPv6 targets the NIC's learned IPv6 address, or its EUI-64 address when none was learned. A forward without an address, or with an IPv4 one, targets the IPv4 lease. IPv6 forwards need IPv6 on the network (`IPv6 port forwards need an IPv6 network: turn on IPv6 for the network`). There is no loopback forward for IPv6.

On a user-mode NIC the forward is implemented by `passt`, which must be installed, and the change applies at the next start of the VM. The form shows All interfaces instead of a host address. Without the option the forward listens on loopback. When `passt` is missing the error is `passt port forwarding is unavailable: <detail>. <hint> Forwards on a Prism network need no passt.`

The API call is `PUT /vms/{id}/nics/{nic}/forwards` with the full list:

```sh
curl -X PUT http://127.0.0.1:8080/api/v1/vms/$VM/nics/$NIC/forwards \
  -H 'Content-Type: application/json' \
  -d '[{"proto": "tcp", "host_port": 2222, "guest_port": 22}]'
```

Host bridge NICs take no forwards (`port forwarding needs a Prism network or user-mode NIC`).
