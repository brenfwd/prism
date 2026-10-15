import { useEffect } from "react";
import type { QueryClient } from "@tanstack/react-query";
import type { NodeView, Pool, Task, Vm, VmRuntime } from "./api";
import { record } from "./lib/stats";
import { applyTask } from "./lib/tasks";
import { createStore, useStore } from "./lib/store";

interface Envelope<T = unknown> {
  type: string;
  resource: string;
  op: "upsert" | "delete";
  payload: T;
}

interface Snapshot {
  cursor: string;
  nodes: NodeView[];
  vms: Vm[];
  pools: Pool[];
}

function vmId(resource: string): string {
  return resource.split("/")[2] ?? "";
}

function patchVm(qc: QueryClient, id: string, f: (vm: Vm) => Vm) {
  qc.setQueryData<Vm[]>(["vms"], (vms) => vms?.map((v) => (v.id === id ? f(v) : v)));
  qc.setQueryData<Vm>(["vm", id], (vm) => (vm ? f(vm) : vm));
}

/** Writes a VM view (from an API response or event) into the cache. */
export function putVm(qc: QueryClient, vm: Vm) {
  qc.setQueryData<Vm[]>(["vms"], (vms) => {
    if (!vms) return [vm];
    const i = vms.findIndex((v) => v.id === vm.id);
    return i < 0 ? [...vms, vm].sort((a, b) => a.name.localeCompare(b.name)) : vms.map((v) => (v.id === vm.id ? vm : v));
  });
  qc.setQueryData(["vm", vm.id], vm);
}

export type StreamState = "connecting" | "live" | "down";
export const stream = createStore<StreamState>("connecting");
export const useStream = () => useStore(stream);

/** Subscribes to the SSE stream and keeps the query cache current. */
export function useEventStream(qc: QueryClient) {
  useEffect(() => {
    const es = new EventSource("/api/v1/events?snapshot=1");
    es.onopen = () => stream.set("live");
    es.onerror = () => stream.set(es.readyState === EventSource.CLOSED ? "down" : "connecting");

    es.addEventListener("snapshot", (e) => {
      const s: Snapshot = JSON.parse((e as MessageEvent).data);
      qc.setQueryData(["nodes"], s.nodes);
      qc.setQueryData(["vms"], s.vms);
      qc.setQueryData(["pools"], s.pools);
      for (const vm of s.vms) {
        qc.setQueryData(["vm", vm.id], vm);
        record(vm.id, vm.status);
      }
      // Tasks may have finished while we were away.
      qc.invalidateQueries({ queryKey: ["tasks"] });
      stream.set("live");
    });
    es.addEventListener("reset", () => {
      qc.invalidateQueries();
    });
    const onStatus = (e: Event) => {
      const ev: Envelope<VmRuntime> = JSON.parse((e as MessageEvent).data);
      const id = vmId(ev.resource);
      record(id, ev.payload);
      patchVm(qc, id, (vm) => ({ ...vm, status: ev.payload }));
    };
    es.addEventListener("vm.state", onStatus);
    es.addEventListener("vm.stats", onStatus);
    es.addEventListener("vm.config", (e) => {
      const ev: Envelope<Vm | null> = JSON.parse((e as MessageEvent).data);
      const id = vmId(ev.resource);
      if (ev.op === "delete" || !ev.payload) {
        qc.setQueryData<Vm[]>(["vms"], (vms) => vms?.filter((v) => v.id !== id));
        qc.removeQueries({ queryKey: ["vm", id] });
        return;
      }
      putVm(qc, ev.payload);
      // Disk changes move volumes around (snapshots, new disks).
      qc.invalidateQueries({ queryKey: ["volumes"] });
    });
    es.addEventListener("node.status", (e) => {
      const ev: Envelope<NodeView> = JSON.parse((e as MessageEvent).data);
      qc.setQueryData<NodeView[]>(["nodes"], (ns) =>
        ns?.some((n) => n.id === ev.payload.id) ? ns.map((n) => (n.id === ev.payload.id ? ev.payload : n)) : [ev.payload],
      );
    });
    es.addEventListener("pool", (e) => {
      const ev: Envelope<Pool | null> = JSON.parse((e as MessageEvent).data);
      const name = decodeURIComponent(ev.resource.split("/").pop() ?? "");
      if (ev.op === "delete") {
        qc.setQueryData<Pool[]>(["pools"], (ps) => ps?.filter((p) => p.name !== name));
      } else if (!ev.payload) {
        qc.invalidateQueries({ queryKey: ["pools"] });
      } else {
        const pool = ev.payload;
        qc.setQueryData<Pool[]>(["pools"], (ps) =>
          ps?.some((p) => p.name === name) ? ps.map((p) => (p.name === name ? pool : p)) : [...(ps ?? []), pool],
        );
      }
      qc.invalidateQueries({ queryKey: ["volumes", name] });
    });
    es.addEventListener("task.progress", (e) => {
      const ev: Envelope<Task> = JSON.parse((e as MessageEvent).data);
      applyTask(qc, ev.payload);
    });
    return () => es.close();
  }, [qc]);
}
