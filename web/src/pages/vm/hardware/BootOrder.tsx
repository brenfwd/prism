import { useEffect, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, humanError, type Vm } from "../../../api";
import { putVm } from "../../../events";
import { toast } from "../../../lib/toast";
import { Icon } from "../../../components/icons";
import { Card, IconButton, cx } from "../../../components/ui";
import { isOn } from "../VmPage";
import { bootIds, bootItems, dropBootItem, moveBootItem, normalize, reorder, type BootItem } from "./bootItems";

/** The boot-order card: drag, arrow buttons and checkboxes all save through `updateVm`. */
export function BootOrder({ vm }: { vm: Vm }) {
  const qc = useQueryClient();
  const server = useMemo(() => bootItems(vm), [vm]);
  const [items, setItems] = useState(server);
  const [drag, setDrag] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  useEffect(() => setItems(server), [server]);

  const save = async (next: BootItem[]) => {
    setItems(next);
    setSaving(true);
    try {
      const view = await api.updateVm(vm.id, { boot_order: bootIds(next) });
      putVm(qc, view);
    } catch (e) {
      setItems(server);
      toast({ tone: "error", title: "Couldn't change the boot order", body: humanError(e) });
    } finally {
      setSaving(false);
    }
  };
  const move = (from: number, to: number) => {
    const next = moveBootItem(items, from, to);
    if (next) void save(next);
  };

  return (
    <Card title="Boot order" description="Drag to reorder. The machine tries each enabled device in turn." className="self-start">
      <ol className="-mx-1 space-y-1.5" aria-label="Boot order">
        {items.map((it, i) => (
          <li
            key={it.id}
            draggable
            onDragStart={(e) => {
              setDrag(it.id);
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", it.id);
            }}
            onDragEnd={() => setDrag(null)}
            onDragOver={(e) => {
              if (!drag || drag === it.id) return;
              e.preventDefault();
              const from = items.findIndex((x) => x.id === drag);
              if (from < 0) return;
              setItems(reorder(items, from, i));
            }}
            onDrop={(e) => {
              e.preventDefault();
              const idx = items.findIndex((x) => x.id === drag);
              setDrag(null);
              void save(dropBootItem(items, idx));
            }}
            className={cx(
              "group flex items-center gap-2 rounded-lg px-2 py-2 ring-1 transition-all ring-inset",
              drag === it.id ? "opacity-50 ring-indigo-400" : "ring-zinc-200 dark:ring-zinc-700/80",
              it.enabled ? "bg-white dark:bg-zinc-900" : "bg-zinc-50 dark:bg-zinc-900/40",
            )}
          >
            <Icon name="grip" className="size-4 cursor-grab text-zinc-300 active:cursor-grabbing dark:text-zinc-600" />
            <span className={cx("grid size-5 shrink-0 place-items-center rounded-full text-[11px] font-semibold tabular-nums", it.enabled ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-200 text-zinc-500 dark:bg-zinc-800")}>
              {it.enabled ? items.filter((x) => x.enabled).indexOf(it) + 1 : "n/a"}
            </span>
            <Icon name={it.icon} className={cx("size-4", it.enabled ? "text-zinc-500" : "text-zinc-300 dark:text-zinc-600")} />
            <div className="min-w-0 flex-1">
              <div className={cx("truncate text-[13px] font-medium", !it.enabled && "text-zinc-400")}>{it.label}</div>
              <div className="truncate font-mono text-[11px] text-zinc-400">{it.sub}</div>
            </div>
            <div className="flex opacity-60 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
              <IconButton icon="arrowUp" label={`Move ${it.label} up`} size="xs" disabled={i === 0 || saving} onClick={() => move(i, i - 1)} />
              <IconButton icon="arrowDown" label={`Move ${it.label} down`} size="xs" disabled={i === items.length - 1 || saving} onClick={() => move(i, i + 1)} />
            </div>
            <input
              type="checkbox"
              aria-label={`Boot from ${it.label}`}
              title={it.enabled ? "Don't boot from this" : "Boot from this"}
              className="size-4 accent-indigo-600"
              checked={it.enabled}
              disabled={saving}
              onChange={(e) => void save(normalize(items.map((x) => (x.id === it.id ? { ...x, enabled: e.target.checked } : x))))}
            />
          </li>
        ))}
        {!items.length && <li className="px-1 text-[13px] text-zinc-500">No bootable devices.</li>}
      </ol>
      <p className="mt-3 flex items-center gap-1.5 text-xs text-zinc-500">
        {saving ? <>Saving…</> : isOn(vm) ? <><Icon name="info" className="size-3.5" />Takes effect at the next boot.</> : null}
      </p>
    </Card>
  );
}
