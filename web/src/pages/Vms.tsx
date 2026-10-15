import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api } from "../api";
import { mib } from "../format";
import { Button, Card, StateBadge } from "../components/ui";
import { CreateVmDialog } from "./CreateVm";
import { VmActions } from "./VmActions";

export function VmsPage() {
  const vms = useQuery({ queryKey: ["vms"], queryFn: api.vms });
  const [creating, setCreating] = useState(false);
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold">Virtual machines</h1>
        <Button variant="primary" onClick={() => setCreating(true)}>New VM</Button>
      </div>
      <Card>
        {vms.data?.length === 0 ? (
          <p className="py-8 text-center text-sm text-zinc-500">No VMs yet.</p>
        ) : (
          <div className="-m-4 overflow-x-auto">
            <table className="w-full text-sm">
              <thead className="text-left text-xs text-zinc-500">
                <tr className="border-b border-zinc-200 dark:border-zinc-800">
                  <th className="px-4 py-2 font-medium">Name</th>
                  <th className="px-4 py-2 font-medium">State</th>
                  <th className="px-4 py-2 font-medium">vCPU</th>
                  <th className="px-4 py-2 font-medium">Memory</th>
                  <th className="px-4 py-2" />
                </tr>
              </thead>
              <tbody>
                {vms.data?.map((vm) => (
                  <tr key={vm.id} className="border-b border-zinc-100 last:border-0 dark:border-zinc-800/60">
                    <td className="px-4 py-2.5">
                      <Link to="/vms/$id" params={{ id: vm.id }} className="font-medium text-indigo-600 hover:underline dark:text-indigo-400">
                        {vm.name}
                      </Link>
                      {vm.status.pending_restart && <span className="ml-2 text-xs text-amber-600">restart pending</span>}
                    </td>
                    <td className="px-4 py-2.5"><StateBadge state={vm.status.state} /></td>
                    <td className="px-4 py-2.5 tabular-nums">{vm.cpu.vcpus}</td>
                    <td className="px-4 py-2.5 tabular-nums">
                      {vm.status.state === "running" ? mib(vm.status.memory_kib / 1024) : mib(vm.memory.target_mib)}
                      <span className="text-zinc-400"> / {mib(vm.memory.max_mib)}</span>
                    </td>
                    <td className="px-4 py-2.5"><div className="flex justify-end"><VmActions vm={vm} compact /></div></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
      {creating && <CreateVmDialog onClose={() => setCreating(false)} />}
    </div>
  );
}
