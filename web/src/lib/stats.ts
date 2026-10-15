import { useSyncExternalStore } from "react";
import type { VmRuntime } from "../api";

/** One stats sample per SSE `vm.stats` event (every ~5 s). */
export interface Sample {
  t: number;
  /** Fraction of the VM's vCPUs busy since the previous sample, 0..1. */
  cpu: number | null;
  /** Guest memory in use (KiB), when the balloon driver reports it. */
  usedKib: number | null;
  /** Memory the guest currently has (balloon size, KiB). */
  actualKib: number;
  /** Host RSS of the QEMU process (KiB). */
  rssKib: number | null;
}

const LEN = 72; // six minutes at 5 s
const rings = new Map<string, Sample[]>();
const last = new Map<string, { t: number; cpu: number }>();
const subs = new Set<() => void>();
let version = 0;

export function guestUsedKib(s: VmRuntime): number | null {
  const m = s.memory_stats;
  if (m.available_kib != null && m.usable_kib != null) return Math.max(0, m.available_kib - m.usable_kib);
  if (m.available_kib != null && m.unused_kib != null) return Math.max(0, m.available_kib - m.unused_kib);
  return null;
}

/** Records a runtime snapshot for `id`; called by the event stream. */
export function record(id: string, s: VmRuntime) {
  const now = performance.now();
  if (s.state !== "running") {
    last.delete(id);
    if (s.state === "stopped" || s.state === "crashed") rings.delete(id);
    bump();
    return;
  }
  const prev = last.get(id);
  last.set(id, { t: now, cpu: s.cpu_time_ns });
  let cpu: number | null = null;
  if (prev && now - prev.t > 500 && s.cpu_time_ns >= prev.cpu) {
    cpu = Math.min(1, (s.cpu_time_ns - prev.cpu) / ((now - prev.t) * 1e6) / Math.max(1, s.vcpus));
  }
  const ring = rings.get(id) ?? [];
  // vm.state and vm.stats can arrive back to back; keep one sample per tick.
  if (ring.length && now - ring[ring.length - 1].t < 1000) {
    const dropped = ring.pop()!;
    cpu ??= dropped.cpu;
  }
  ring.push({
    t: now,
    cpu: cpu ?? (ring.length ? ring[ring.length - 1].cpu : null),
    usedKib: guestUsedKib(s),
    actualKib: s.memory_stats.actual_kib ?? s.memory_kib,
    rssKib: s.memory_stats.rss_kib,
  });
  if (ring.length > LEN) ring.shift();
  rings.set(id, ring);
  bump();
}

function bump() {
  version++;
  for (const s of subs) s();
}

function subscribe(fn: () => void) {
  subs.add(fn);
  return () => subs.delete(fn);
}

const empty: Sample[] = [];
const cache = new Map<string, { v: number; data: Sample[] }>();

export function useSamples(id: string): Sample[] {
  return useSyncExternalStore(subscribe, () => {
    const c = cache.get(id);
    if (c && c.v === version) return c.data;
    const data = rings.get(id)?.slice() ?? empty;
    cache.set(id, { v: version, data });
    return data;
  });
}

export const SAMPLE_CAPACITY = LEN;
