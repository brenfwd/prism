// Pure rules for the new-machine form: names, sizes, capacity checks and
// network selection. Nothing here touches React, so `bun test` covers it.
import type { CreateVm } from "../../api";
import { GiB } from "../../lib/format";

/** What the machine boots from. */
export type Source = "image" | "iso" | "blank";
/** A size card on the form. */
export type PresetId = "small" | "medium" | "large" | "custom";
/** How the machine reaches the network. */
export type NetKind = "network" | "user" | "bridge";

/** CPU, memory (MiB) and disk (GiB) for a machine. */
export interface Size {
  vcpus: number;
  mem: number;
  disk: number;
}

/** A fixed size card with the line shown under it. */
export interface Preset extends Size {
  label: string;
  blurb: string;
}

export const presets: Record<Exclude<PresetId, "custom">, Preset> = {
  small: { label: "Small", vcpus: 1, mem: 1024, disk: 10, blurb: "Services, tests, a quick sandbox" },
  medium: { label: "Medium", vcpus: 2, mem: 4096, disk: 32, blurb: "Web apps, build agents, desktops" },
  large: { label: "Large", vcpus: 4, mem: 8192, disk: 64, blurb: "Databases and heavier work" },
};

/** Starting values for the custom size card. */
export const defaultCustom: Size = { vcpus: 2, mem: 2048, disk: 20 };

const NAME_RE = /^[A-Za-z0-9_][A-Za-z0-9._-]{0,63}$/;

/** A short word taken from the source file name, or `vm` when none fits. */
export function baseName(volume: string | undefined): string {
  if (!volume) return "vm";
  const word = volume.toLowerCase().replace(/\.(img|qcow2|raw|iso)$/, "").match(/[a-z]+/)?.[0];
  return word && word.length > 1 ? word : "vm";
}

/** The first `base-N` (N from 1) that is not in `taken`. */
export function suggestName(base: string, taken: Set<string>): string {
  for (let i = 1; ; i++) if (!taken.has(`${base}-${i}`)) return `${base}-${i}`;
}

/** The name as a DNS label: lower case, dashes for other characters, at most 63 long. */
export const hostnameOf = (name: string) =>
  name.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/^-+|-+$/g, "").slice(0, 63) || "vm";

/** The reason `name` cannot be used, or `null` when it can. */
export function nameError(name: string, taken: Set<string>): string | null {
  if (!name) return "Give it a name.";
  if (!NAME_RE.test(name)) return "Use letters, digits, “.”, “_” or “-” (no spaces), up to 64.";
  if (taken.has(name)) return `There's already a machine called ${name}.`;
  return null;
}

/** The preset to select before the user picks one: Medium when the host has 10 GiB free. */
export const defaultPreset = (avail: number | null): PresetId => (avail != null && avail >= 10 * GiB ? "medium" : "small");

/** The smallest disk (GiB) that holds the source image; 1 for other sources. */
export function minDiskGib(source: Source, sourceBytes: number | undefined): number {
  return source === "image" && sourceBytes != null ? Math.ceil(sourceBytes / GiB) : 1;
}

/** The size that will be created: the chosen card, with the disk raised to `minDisk`. */
export function resolveSize(preset: PresetId, custom: Size, minDisk: number): Size {
  const base = preset === "custom" ? custom : presets[preset];
  return { vcpus: base.vcpus, mem: base.mem, disk: Math.max(base.disk, minDisk) };
}

/** Whether `memMib` is more than the `avail` bytes the host has free. */
export const memExceeds = (memMib: number, avail: number | null) => avail != null && memMib * 1024 * 1024 > avail;

/** How the chosen size compares with free host memory and free pool space. */
export interface Capacity {
  memBytes: number;
  memOver: boolean;
  memTight: boolean;
  diskBytes: number;
  diskOver: boolean;
}

/**
 * Checks a size against the host's free memory (`avail`, bytes) and the
 * pool's free space. Memory is tight above 80% of what is free.
 */
export function capacity(size: Size, avail: number | null, poolAvailable: number | undefined): Capacity {
  const memBytes = size.mem * 1024 * 1024;
  const memOver = avail != null && memBytes > avail;
  const diskBytes = size.disk * GiB;
  return {
    memBytes,
    memOver,
    memTight: avail != null && !memOver && memBytes > avail * 0.8,
    diskBytes,
    diskOver: poolAvailable != null && diskBytes > poolAvailable,
  };
}

/** Inputs to [`blockers`]. */
export interface BlockerInput {
  nameError: string | null;
  source: Source;
  hasSource: boolean;
  badKey: boolean;
  poolReady: boolean;
}

/** The reasons the form cannot be submitted yet, most urgent first. */
export function blockers(i: BlockerInput): string[] {
  const out: string[] = [];
  if (i.nameError) out.push(i.nameError);
  if (i.source !== "blank" && !i.hasSource) out.push(i.source === "image" ? "Pick a cloud image." : "Pick an ISO.");
  if (i.badKey) out.push("Fix the SSH key.");
  if (!i.poolReady) out.push("Create a storage pool first.");
  return out;
}

/** The network kind in effect: a Prism network falls back to NAT when none exists. */
export const effectiveNet = (net: NetKind, netId: string | undefined): NetKind => (net === "network" && !netId ? "user" : net);

/** The `network` field of a create request. */
export function networkRequest(kind: NetKind, netId: string | undefined, bridge: string): CreateVm["network"] {
  if (kind === "network" && netId) return { type: "network", network: netId };
  if (kind === "bridge") return { type: "bridge", bridge };
  return { type: "user" };
}
