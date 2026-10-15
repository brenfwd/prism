import type { NetworkInfo, NicNetwork } from "../api";

/** Human label for a NIC's network, with its leased address when it has one. */
export function nicLabel(n: NicNetwork, nets: NetworkInfo[] | undefined): string {
  if (n.type === "bridge") return `Bridge ${n.bridge}`;
  if (n.type === "user") return "Private NAT";
  const name = nets?.find((x) => x.id === n.network)?.name ?? "network";
  return n.ipv4 ? `${name} (${n.ipv4})` : name;
}
