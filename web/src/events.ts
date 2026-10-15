import { useEffect, useState } from "react";
import type { QueryClient } from "@tanstack/react-query";
import type { NodeView, Pool, Vm, VmRuntime } from "./api";

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

export type StreamState = "connecting" | "live" | "down";

/** Subscribes to the SSE stream and keeps the query cache current. */
export function useEventStream(qc: QueryClient): StreamState {
  const [state, setState] = useState<StreamState>("connecting");
  useEffect(() => {
    const es = new EventSource("/api/v1/events?snapshot=1");
    es.onopen = () => setState("live");
    es.onerror = () => setState(es.readyState === EventSource.CLOSED ? "down" : "connecting");

    es.addEventListener("snapshot", (e) => {
      const s: Snapshot = JSON.parse((e as MessageEvent).data);
      qc.setQueryData(["nodes"], s.nodes);
      qc.setQueryData(["vms"], s.vms);
      qc.setQueryData(["pools"], s.pools);
      for (const vm of s.vms) qc.setQueryData(["vm", vm.id], vm);
      setState("live");
    });
    const onStatus = (e: Event) => {
      const ev: Envelope<VmRuntime> = JSON.parse((e as MessageEvent).data);
      patchVm(qc, vmId(ev.resource), (vm) => ({ ...vm, status: ev.payload }));
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
      const vm = ev.payload;
      qc.setQueryData<Vm[]>(["vms"], (vms) => {
        if (!vms) return [vm];
        const i = vms.findIndex((v) => v.id === id);
        return i < 0 ? [...vms, vm].sort((a, b) => a.name.localeCompare(b.name)) : vms.map((v) => (v.id === id ? vm : v));
      });
      qc.setQueryData(["vm", id], vm);
    });
    es.addEventListener("node.status", (e) => {
      const ev: Envelope<NodeView> = JSON.parse((e as MessageEvent).data);
      qc.setQueryData<NodeView[]>(["nodes"], (ns) =>
        ns?.some((n) => n.id === ev.payload.id) ? ns.map((n) => (n.id === ev.payload.id ? ev.payload : n)) : [ev.payload],
      );
    });
    es.addEventListener("pool", (e) => {
      const ev: Envelope = JSON.parse((e as MessageEvent).data);
      const name = decodeURIComponent(ev.resource.split("/").pop() ?? "");
      qc.invalidateQueries({ queryKey: ["pools"] });
      qc.invalidateQueries({ queryKey: ["volumes", name] });
    });
    return () => es.close();
  }, [qc]);
  return state;
}
