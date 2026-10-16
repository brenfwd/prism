import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, humanError, type NetworkInfo, type Vm } from "../api";
import { useNetworks, useVms } from "../lib/hooks";
import { toast } from "../lib/toast";
import { confirm } from "../components/Confirm";
import { NetworkModal } from "./networks/NetworkModal";
import { VlanSubnetList } from "./networks/VlanSubnets";
import { Badge, Button, Card, EmptyState, IconButton } from "../components/ui";

export function NetworksPage() {
  const nets = useNetworks();
  const vms = useVms().data ?? [];
  const list = nets.data ?? [];
  const [editing, setEditing] = useState<NetworkInfo | "new" | null>(null);

  if (nets.isLoading) return <div className="h-40 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />;
  const add = (
    <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>
      New network
    </Button>
  );
  return (
    <div className="animate-in">
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Networks</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Private networks with DHCP, DNS and internet access built in. Defaults pick a free subnet for you.</p>
        </div>
        {add}
      </div>
      {list.length === 0 ? (
        <Card>
          <EmptyState icon="network" title="No networks yet" actions={add}>
            Create one and new machines can join it with an address assigned automatically.
          </EmptyState>
        </Card>
      ) : (
        <div className="grid gap-4 lg:grid-cols-2">
          {list.map((n) => (
            <NetworkCard key={n.id} net={n} vms={vms} onRename={() => setEditing(n)} />
          ))}
        </div>
      )}
      {editing && <NetworkModal existing={editing === "new" ? undefined : editing} onClose={() => setEditing(null)} />}
    </div>
  );
}

function NetworkCard({ net, vms, onRename }: { net: NetworkInfo; vms: Vm[]; onRename: () => void }) {
  const qc = useQueryClient();
  const members = vms
    .map((vm) => ({
      vm,
      ips: vm.nics.flatMap((n) =>
        n.network.type === "network" && n.network.network === net.id ? [n.network.ipv4 ?? "pending", ...(vm.nic_ipv6?.[n.id] ? [vm.nic_ipv6[n.id]] : [])] : [],
      ),
    }))
    .filter((m) => m.ips.length);
  const refresh = () => qc.invalidateQueries({ queryKey: ["networks"] });
  const dhcp = useMutation({ mutationFn: () => api.updateNetwork(net.id, { dhcp: !net.dhcp }), onSuccess: refresh, onError: (e) => toast({ tone: "error", title: "Network change failed", body: humanError(e) }) });
  const del = useMutation({ mutationFn: () => api.deleteNetwork(net.id), onSuccess: refresh, onError: (e) => toast({ tone: "error", title: "Network change failed", body: humanError(e) }) });
  return (
    <Card
      title={net.name}
      description={<span className="font-mono">{[net.subnet, net.subnet6].filter(Boolean).join("  ")}</span>}
      actions={
        <>
        <IconButton icon="pencil" label="Edit network" onClick={onRename} />
        <IconButton
          icon="trash"
          label="Delete network"
          disabled={members.length > 0}
          onClick={async () => {
            if (await confirm({ title: `Delete ${net.name}?`, body: "Its subnet is released.", confirmLabel: "Delete", danger: true })) del.mutate();
          }}
        />
        </>
      }
    >
      <div className="mb-3 flex flex-wrap gap-1.5">
        <Badge tone={net.kind === "nat" ? "indigo" : "neutral"}>{net.kind === "nat" ? "Internet access" : "Isolated"}</Badge>
        <Badge tone={net.dhcp ? "green" : "neutral"}>{net.dhcp ? "DHCP on" : "DHCP off"}</Badge>
        <Badge tone={net.subnet6 ? "green" : "neutral"}>{net.subnet6 ? (net.nat66 ? "IPv6 with NAT" : "IPv6") : "IPv6 off"}</Badge>
        <Badge>{net.domain}</Badge>
      </div>
      {members.length === 0 ? (
        <p className="text-[13px] text-zinc-500">No machines yet. Pick this network when creating one.</p>
      ) : (
        <ul className="divide-y divide-zinc-100 text-sm dark:divide-zinc-800">
          {members.map(({ vm, ips }) => (
            <li key={vm.id} className="flex items-center justify-between py-1.5">
              <Link to="/vms/$id" params={{ id: vm.id }} className="hover:underline">{vm.name}</Link>
              <span className="font-mono text-xs text-zinc-500">{ips.join(", ")}</span>
            </li>
          ))}
        </ul>
      )}
      <VlanSubnetList net={net} />
      <div className="mt-3">
        <Button variant="ghost" loading={dhcp.isPending} onClick={() => dhcp.mutate()}>{net.dhcp ? "Turn DHCP off" : "Turn DHCP on"}</Button>
      </div>
    </Card>
  );
}
