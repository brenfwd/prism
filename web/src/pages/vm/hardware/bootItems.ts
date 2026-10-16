// Pure boot-order rules: which devices can boot, in what order, and how a
// move or drop changes the list. The UI in `BootOrder.tsx` only renders it.
import type { IconName } from "../../../components/icons";
import type { Nic, Vm } from "../../../api";

/** A device that can be in the boot list; `enabled` ones boot in list order. */
export interface BootItem {
  id: string;
  label: string;
  sub: string;
  icon: IconName;
  enabled: boolean;
}

/** The VM's disks, CD-ROMs and adapters: those with a boot order first, in order, then the rest. */
export function bootItems(vm: Vm): BootItem[] {
  const all = [
    ...vm.disks.map((d) => ({
      id: d.id,
      order: d.boot_order ?? null,
      label: d.device === "cdrom" ? `CD-ROM ${d.target ?? ""}` : `Disk ${d.target ?? ""}`,
      sub: d.volume ?? "empty",
      icon: (d.device === "cdrom" ? "disc" : "disk") as IconName,
    })),
    ...vm.nics.map((n: Nic) => ({
      id: n.id,
      order: n.boot_order ?? null,
      label: "Network (PXE)",
      sub: n.mac,
      icon: "network" as IconName,
    })),
  ];
  const on = all.filter((x) => x.order != null).sort((a, b) => a.order! - b.order!);
  const off = all.filter((x) => x.order == null);
  return [...on.map((x) => ({ ...x, enabled: true })), ...off.map((x) => ({ ...x, enabled: false }))];
}

/** Puts enabled items before disabled ones, keeping relative order. */
export const normalize = (list: BootItem[]): BootItem[] => [...list.filter((x) => x.enabled), ...list.filter((x) => !x.enabled)];

/** Enables the item at `idx` when an enabled item follows it: dropping a device above one that boots turns it on. */
function enableIfAboveEnabled(list: BootItem[], idx: number): BootItem[] {
  if (idx < 0 || list[idx].enabled || !list.slice(idx + 1).some((x) => x.enabled)) return list;
  return list.map((x, i) => (i === idx ? { ...x, enabled: true } : x));
}

/** Moves the item at `from` to `to` without any normalizing, for the live drag preview. */
export function reorder(list: BootItem[], from: number, to: number): BootItem[] {
  const next = list.slice();
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

/** The saved list after moving `from` to `to`, or `null` when the move does nothing. */
export function moveBootItem(list: BootItem[], from: number, to: number): BootItem[] | null {
  if (to < 0 || to >= list.length || from === to) return null;
  return normalize(enableIfAboveEnabled(reorder(list, from, to), to));
}

/** The saved list after the item at `idx` (already in its dropped position) is released. */
export const dropBootItem = (list: BootItem[], idx: number): BootItem[] => normalize(enableIfAboveEnabled(list, idx));

/** IDs of the enabled items, in boot order. */
export const bootIds = (list: BootItem[]): string[] => list.filter((x) => x.enabled).map((x) => x.id);
