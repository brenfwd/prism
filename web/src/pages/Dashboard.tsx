import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api } from "../api";
import { bytes, kib } from "../format";
import { Card, Meter, Stat, StateBadge } from "../components/ui";

export function DashboardPage() {
  const nodes = useQuery({ queryKey: ["nodes"], queryFn: api.nodes });
  const vms = useQuery({ queryKey: ["vms"], queryFn: api.vms });
  const pools = useQuery({ queryKey: ["pools"], queryFn: api.pools });
  const node = nodes.data?.[0];
  const running = vms.data?.filter((v) => v.status.state === "running").length ?? 0;
  const allocated = vms.data?.filter((v) => v.status.state === "running").reduce((a, v) => a + v.status.memory_kib, 0) ?? 0;
  return (
    <div className="space-y-4">
      <h1 className="text-xl font-semibold">Overview</h1>
      {node && (
        <Card
          title={
            <span className="flex items-center gap-2">
              {node.name}
              <span className={`size-2 rounded-full ${node.libvirt_connected ? "bg-emerald-500" : "bg-red-500"}`} title={node.libvirt_connected ? "libvirt connected" : "libvirt disconnected"} />
            </span>
          }
        >
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <Stat label="CPUs" value={node.host.cpus} sub={node.host.cpu_model} />
            <Stat
              label="Memory free"
              value={bytes(node.host.free_memory_bytes)}
              sub={`of ${kib(node.host.memory_kib)}`}
            />
            <Stat label="VMs running" value={`${running} / ${vms.data?.length ?? 0}`} sub={`${kib(allocated)} ballooned in`} />
            <Stat label="libvirt" value={node.host.libvirt_version || "—"} sub={`${node.domain_type === "kvm" ? "KVM" : "TCG (no KVM)"} · Prism ${node.version}`} />
          </div>
          <Meter className="mt-4" value={node.host.memory_kib * 1024 - node.host.free_memory_bytes} max={node.host.memory_kib * 1024} />
        </Card>
      )}
      <div className="grid gap-4 lg:grid-cols-2">
        <Card title="Virtual machines" actions={<Link to="/vms" className="text-xs text-indigo-600 hover:underline dark:text-indigo-400">All</Link>}>
          <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
            {vms.data?.slice(0, 8).map((vm) => (
              <li key={vm.id} className="flex items-center justify-between py-2 text-sm">
                <Link to="/vms/$id" params={{ id: vm.id }} className="hover:underline">{vm.name}</Link>
                <StateBadge state={vm.status.state} />
              </li>
            ))}
            {vms.data?.length === 0 && <li className="py-2 text-sm text-zinc-500">None yet.</li>}
          </ul>
        </Card>
        <Card title="Storage pools" actions={<Link to="/storage" className="text-xs text-indigo-600 hover:underline dark:text-indigo-400">Manage</Link>}>
          <ul className="space-y-3">
            {pools.data?.map((p) => (
              <li key={p.name} className="text-sm">
                <div className="mb-1 flex justify-between">
                  <span>{p.name}</span>
                  <span className="text-xs text-zinc-500 tabular-nums">{bytes(p.available_bytes)} free of {bytes(p.capacity_bytes)}</span>
                </div>
                <Meter value={p.capacity_bytes - p.available_bytes} max={p.capacity_bytes} />
              </li>
            ))}
          </ul>
        </Card>
      </div>
    </div>
  );
}
