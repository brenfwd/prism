import type { QueryClient } from "@tanstack/react-query";
import { uploadVolume, humanError, type Task, type Vm, type Volume } from "../api";
import { createStore } from "./store";
import { toast } from "./toast";

/** What a task kind is doing, as a sentence fragment. */
export function taskLabel(t: Pick<Task, "kind" | "result">): { running: string; done: string; failed: string } {
  const name = (t.result as { name?: string } | null)?.name;
  switch (t.kind) {
    case "snapshot.create":
      return { running: "Taking snapshot", done: name ? `Snapshot ${name} taken` : "Snapshot taken", failed: "Snapshot failed" };
    case "snapshot.revert":
      return { running: "Reverting to snapshot", done: "Reverted to snapshot", failed: "Revert failed" };
    case "snapshot.delete":
      return { running: "Deleting snapshot", done: "Snapshot deleted", failed: "Couldn't delete snapshot" };
    default: {
      const k = t.kind.replace(/[._]/g, " ");
      return { running: k[0].toUpperCase() + k.slice(1), done: `${k} finished`, failed: `${k} failed` };
    }
  }
}

/** VM ID from a task resource like `/vms/<id>` or `/vms/<id>/snapshots/x`. */
export function taskVmId(t: Pick<Task, "resource">): string | null {
  const m = /^\/vms\/([0-9a-f-]{36})/.exec(t.resource);
  return m ? m[1] : null;
}

const seenRunning = new Set<string>();

/** Folds a `task.progress` event into the cache and toasts transitions. */
export function applyTask(qc: QueryClient, t: Task) {
  qc.setQueryData<Task[]>(["tasks"], (all) => {
    if (!all) return [t];
    const i = all.findIndex((x) => x.id === t.id);
    if (i < 0) return [t, ...all].slice(0, 200);
    const next = all.slice();
    next[i] = t;
    return next;
  });
  const vmId = taskVmId(t);
  if (t.state === "running") {
    seenRunning.add(t.id);
    return;
  }
  if (vmId && t.kind.startsWith("snapshot.")) qc.invalidateQueries({ queryKey: ["snapshots", vmId] });
  if (!seenRunning.delete(t.id)) return;
  const vm = vmId ? qc.getQueryData<Vm[]>(["vms"])?.find((v) => v.id === vmId) : undefined;
  const label = taskLabel(t);
  const where = vm ? ` on ${vm.name}` : "";
  const link = vmId ? { id: vmId, label: "View VM", tab: t.kind.startsWith("snapshot.") ? "snapshots" : undefined } : undefined;
  if (t.state === "succeeded") toast({ tone: "success", title: label.done + where, vm: link });
  else if (t.state === "cancelled") toast({ tone: "info", title: `${label.running}${where} was cancelled` });
  else toast({ tone: "error", title: label.failed + where, body: t.error ?? undefined, vm: link });
}

// ---- Uploads: tracked here so they survive navigation. ----

export interface Upload {
  id: number;
  pool: string;
  name: string;
  loaded: number;
  total: number;
  /** Bytes/second, smoothed. */
  rate: number;
  state: "uploading" | "done" | "failed";
  error?: string;
  abort: () => void;
}

export const uploads = createStore<Upload[]>([]);
let useq = 0;

export function startUpload(qc: QueryClient, pool: string, file: File, name = file.name): Promise<Volume | null> {
  const id = ++useq;
  let lastT = performance.now();
  let lastLoaded = 0;
  const patch = (p: Partial<Upload>) => uploads.set((all) => all.map((u) => (u.id === id ? { ...u, ...p } : u)));
  const u = uploadVolume(pool, file, name, (loaded, total) => {
    const now = performance.now();
    const dt = (now - lastT) / 1000;
    if (dt < 0.25) return patch({ loaded, total });
    const inst = (loaded - lastLoaded) / dt;
    lastT = now;
    lastLoaded = loaded;
    uploads.set((all) =>
      all.map((x) => (x.id === id ? { ...x, loaded, total, rate: x.rate ? x.rate * 0.7 + inst * 0.3 : inst } : x)),
    );
  });
  uploads.set((all) => [
    ...all,
    { id, pool, name, loaded: 0, total: file.size, rate: 0, state: "uploading", abort: u.abort },
  ]);
  return u.promise.then(
    (vol) => {
      patch({ state: "done", loaded: file.size });
      qc.invalidateQueries({ queryKey: ["volumes", pool] });
      qc.invalidateQueries({ queryKey: ["pools"] });
      setTimeout(() => uploads.set((all) => all.filter((x) => x.id !== id)), 4000);
      toast({ tone: "success", title: `${name} uploaded`, body: `Ready in pool ${pool}.` });
      return vol;
    },
    (e) => {
      const msg = humanError(e);
      const aborted = (e as { code?: string }).code === "aborted";
      uploads.set((all) => (aborted ? all.filter((x) => x.id !== id) : all.map((x) => (x.id === id ? { ...x, state: "failed", error: msg } : x))));
      if (!aborted) toast({ tone: "error", title: `Upload of ${name} failed`, body: msg });
      else toast({ tone: "info", title: `Upload of ${name} cancelled` });
      return null;
    },
  );
}

export function dismissUpload(id: number) {
  uploads.set((all) => all.filter((u) => u.id !== id));
}
