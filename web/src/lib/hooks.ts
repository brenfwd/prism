import { useQueries, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { api, humanError, type NodeView, type Vm, type VmAction, type VmState, type Volume } from "../api";
import { putVm } from "../events";
import { createStore, useStore } from "./store";
import { toast } from "./toast";

/** Returns a function that cancels a task and toasts if the request fails. */
export function useCancelTask(): (id: string) => void {
  return (id) => {
    api.cancelTask(id).catch((e) => toast({ tone: "error", title: "Couldn't cancel", body: humanError(e) }));
  };
}

export const useVms = () => useQuery({ queryKey: ["vms"], queryFn: api.vms });
export const useVm = (id: string) => useQuery({ queryKey: ["vm", id], queryFn: () => api.vm(id) });
export const useNodes = () => useQuery({ queryKey: ["nodes"], queryFn: api.nodes });
export const usePools = () => useQuery({ queryKey: ["pools"], queryFn: api.pools });
export const useNetworks = () => useQuery({ queryKey: ["networks"], queryFn: api.networks });
export const useTasks = () => useQuery({ queryKey: ["tasks"], queryFn: api.tasks });
export const useVolumes = (pool: string | undefined) =>
  useQuery({ queryKey: ["volumes", pool], queryFn: () => api.volumes(pool!), enabled: !!pool });

export function useNode(): NodeView | undefined {
  return useNodes().data?.[0];
}

/** Every volume in every pool, tagged with its pool. */
export function useAllVolumes(): { data: (Volume & { pool: string })[]; loading: boolean } {
  const pools = usePools().data ?? [];
  const results = useQueries({
    queries: pools.map((p) => ({ queryKey: ["volumes", p.name], queryFn: () => api.volumes(p.name) })),
  });
  return {
    data: results.flatMap((r, i) => (r.data ?? []).map((v) => ({ ...v, pool: pools[i].name }))),
    loading: results.some((r) => r.isLoading),
  };
}

/** A disk Prism created for a machine (`<vm>-disk0.qcow2`), even if detached. */
export const looksLikeVmDisk = (name: string) => /-disk\d+\.(qcow2|raw|img)$/.test(name);

/** Memory the host can still hand out, in bytes. */
export function hostAvailableBytes(node: NodeView | undefined): number | null {
  if (!node) return null;
  const total = node.host.memory_kib * 1024;
  if (node.memory_pressure) return Math.round((total * node.memory_pressure.mem_available_pct) / 100);
  return node.host.free_memory_bytes;
}

/** Which VMs use each `pool/volume` (as a disk, CD-ROM or snapshot layer). */
export function volumeUsers(vms: Vm[] | undefined): Map<string, Vm[]> {
  const m = new Map<string, Vm[]>();
  for (const vm of vms ?? []) {
    for (const d of vm.disks) {
      for (const v of [d.volume, ...(d.backing ?? [])]) {
        if (!v) continue;
        const k = `${d.pool}/${v}`;
        const list = m.get(k) ?? [];
        if (!list.includes(vm)) list.push(vm);
        m.set(k, list);
      }
    }
  }
  return m;
}

// ---------------------------------------------------------------- power actions

export const actionMeta: Record<VmAction, { label: string; pending: string; expect?: VmState[]; icon: "play" | "power" | "square" | "rotate" | "pause" | "zap" }> = {
  start: { label: "Start", pending: "Starting…", expect: ["running"], icon: "play" },
  shutdown: { label: "Shut down", pending: "Shutting down…", expect: ["stopped", "crashed"], icon: "power" },
  stop: { label: "Force stop", pending: "Stopping…", expect: ["stopped", "crashed"], icon: "square" },
  reboot: { label: "Reboot", pending: "Rebooting…", icon: "rotate" },
  reset: { label: "Reset", pending: "Resetting…", icon: "zap" },
  pause: { label: "Pause", pending: "Pausing…", expect: ["paused"], icon: "pause" },
  resume: { label: "Resume", pending: "Resuming…", expect: ["running"], icon: "play" },
};

/** Power actions in flight, by VM ID, so every view shows the same spinner. */
const inflight = createStore<Record<string, VmAction>>({});

export function usePendingAction(id: string): VmAction | undefined {
  return useStore(inflight)[id];
}

function clear(id: string) {
  inflight.set(({ [id]: _, ...rest }) => rest);
}

/** Actions that make sense for a VM in this state, most useful first. */
export function availableActions(state: VmState): VmAction[] {
  switch (state) {
    case "running":
      return ["shutdown", "reboot", "pause", "stop", "reset"];
    case "paused":
      return ["resume", "stop"];
    case "shutting_down":
      return ["stop"];
    case "suspended":
      return ["resume", "stop"];
    case "stopped":
    case "crashed":
      return ["start"];
    default:
      return ["start", "stop"];
  }
}

/**
 * Runs a power action. Resolves `true` once the VM reaches the state the
 * action aims for (as reported over SSE), `false` on error or timeout.
 */
export async function runAction(qc: QueryClient, vm: Pick<Vm, "id" | "name">, action: VmAction, quiet = false): Promise<boolean> {
  const meta = actionMeta[action];
  inflight.set((m) => ({ ...m, [vm.id]: action }));
  try {
    const view = await api.action(vm.id, action);
    putVm(qc, view);
    if (!meta.expect || meta.expect.includes(view.status.state)) {
      clear(vm.id);
      return true;
    }
  } catch (e) {
    clear(vm.id);
    toast({ tone: "error", title: `Couldn't ${meta.label.toLowerCase()} ${vm.name}`, body: humanError(e) });
    return false;
  }
  // Wait for the state change to arrive over SSE.
  return new Promise<boolean>((resolve) => {
    const unsub = qc.getQueryCache().subscribe(() => {
      const cur = qc.getQueryData<Vm>(["vm", vm.id]);
      if (cur && meta.expect!.includes(cur.status.state)) done(true);
    });
    const timer = setTimeout(() => {
      done(false);
      if (action === "shutdown" && !quiet) {
        toast({
          tone: "warning",
          title: `${vm.name} is still running`,
          body: "The guest didn't respond to the shutdown request. It may lack ACPI support, or still be booting.",
          action: { label: "Force stop", run: () => void runAction(qc, vm, "stop") },
          timeout: 0,
        });
      }
    }, action === "shutdown" ? 90_000 : 30_000);
    function done(ok: boolean) {
      unsub();
      clearTimeout(timer);
      clear(vm.id);
      resolve(ok);
    }
  });
}

/** Shuts down cleanly (falling back to a hard stop), then starts again. */
export async function powerCycle(qc: QueryClient, vm: Pick<Vm, "id" | "name">) {
  const cur = qc.getQueryData<Vm>(["vm", vm.id]);
  if (cur && (cur.status.state === "running" || cur.status.state === "paused")) {
    const ok = await runAction(qc, vm, cur.status.state === "paused" ? "stop" : "shutdown", true);
    if (!ok && !(await runAction(qc, vm, "stop"))) return;
  }
  if (await runAction(qc, vm, "start")) toast({ tone: "success", title: `${vm.name} restarted with the new hardware` });
}

export function useRunAction() {
  const qc = useQueryClient();
  return (vm: Pick<Vm, "id" | "name">, action: VmAction) => runAction(qc, vm, action);
}
