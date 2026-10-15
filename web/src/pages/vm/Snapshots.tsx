import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type Snapshot, type Task, type Vm } from "../../api";
import { ago, dateTime } from "../../lib/format";
import { useTasks } from "../../lib/hooks";
import { applyTask, taskLabel, taskVmId } from "../../lib/tasks";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Icon, Spinner } from "../../components/icons";
import { Badge, Button, Card, EmptyState, Field, Input, Menu, IconButton, Progress, Switch, cx } from "../../components/ui";
import { isOn } from "./VmPage";

interface Node extends Snapshot {
  depth: number;
  children: Node[];
}

/** Orders snapshots as a tree, oldest first, siblings by creation. */
function tree(list: Snapshot[]): Node[] {
  const by = new Map<string, Node>(list.map((s) => [s.name, { ...s, depth: 0, children: [] }]));
  const roots: Node[] = [];
  for (const n of by.values()) {
    const p = n.parent ? by.get(n.parent) : undefined;
    if (p) p.children.push(n);
    else roots.push(n);
  }
  // The path from the root to the current snapshot is the main line.
  const onPath = new Set<string>();
  for (let c = list.find((s) => s.current); c; c = c.parent ? by.get(c.parent) : undefined) onPath.add(c.name);
  const out: Node[] = [];
  const walk = (n: Node, d: number) => {
    n.depth = d;
    out.push(n);
    const kids = n.children.sort((a, b) => (a.created ?? 0) - (b.created ?? 0));
    const main = kids.find((k) => onPath.has(k.name)) ?? (onPath.size ? undefined : kids[kids.length - 1]);
    // Side branches first (indented), then the main line continues.
    for (const k of kids) if (k !== main) walk(k, d + 1);
    if (main) walk(main, d);
  };
  roots.sort((a, b) => (a.created ?? 0) - (b.created ?? 0)).forEach((r) => walk(r, 0));
  return out;
}

export function SnapshotsTab({ vm, openForm }: { vm: Vm; openForm: boolean }) {
  const qc = useQueryClient();
  const snaps = useQuery({ queryKey: ["snapshots", vm.id], queryFn: () => api.snapshots(vm.id) });
  const tasks = useTasks().data ?? [];
  const running = tasks.filter((t) => t.state === "running" && taskVmId(t) === vm.id);
  const [form, setForm] = useState(openForm);
  useEffect(() => setForm((f) => f || openForm), [openForm]);
  const nodes = useMemo(() => tree(snaps.data ?? []), [snaps.data]);
  const busy = running.length > 0;

  const track = (t: Task) => {
    applyTask(qc, t);
  };

  const revert = async (s: Snapshot) => {
    const on = isOn(vm);
    const ok = await confirm({
      title: `Revert ${vm.name} to “${s.name}”?`,
      body: (
        <div className="space-y-2">
          <p>Disks go back to how they were {s.created ? ago(s.created * 1000) : "when it was taken"}. Anything written since is lost, unless you take a snapshot first.</p>
          {s.confirm_required && (
            <p className="font-medium text-amber-700 dark:text-amber-300">
              This machine has moved between nodes, and restoring memory afterwards has crashed the hypervisor in testing. The safe route is to shut the machine down first, then revert; the disks are restored either way. Continue only if you accept the risk.
            </p>
          )}
          {on && <p>{s.memory ? "The machine resumes exactly where it was, with its memory." : "This snapshot has no memory, so the running machine restarts from the saved disks."}</p>}
        </div>
      ),
      confirmLabel: s.confirm_required ? "Revert anyway" : "Revert",
      danger: true,
      typeToConfirm: on ? vm.name : undefined,
    });
    if (!ok) return;
    try {
      track(await api.revertSnapshot(vm.id, s.name, !!s.confirm_required));
    } catch (e) {
      toast({ tone: "error", title: "Couldn't revert", body: humanError(e) });
    }
  };
  const remove = async (s: Snapshot) => {
    const ok = await confirm({
      title: `Delete snapshot “${s.name}”?`,
      body: "The machine keeps its current state; only this restore point goes away. Its data is merged into the neighbouring layer, which can take a moment for large disks.",
      confirmLabel: "Delete snapshot",
      danger: true,
    });
    if (!ok) return;
    try {
      track(await api.deleteSnapshot(vm.id, s.name));
    } catch (e) {
      toast({ tone: "error", title: "Couldn't delete the snapshot", body: humanError(e) });
    }
  };

  return (
    <div className="grid items-start gap-5 lg:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="space-y-4">
        {running.map((t) => (
          <div key={t.id} className="flex items-center gap-3 rounded-xl bg-indigo-50 px-4 py-3 ring-1 ring-indigo-200/70 animate-in dark:bg-indigo-500/10 dark:ring-indigo-400/20">
            <Spinner className="size-4 text-indigo-600 dark:text-indigo-300" />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium">{taskLabel(t).running}…</p>
              {t.message && <p className="truncate text-xs text-zinc-500 dark:text-zinc-400">{t.message}</p>}
              <Progress value={t.progress} className="mt-2" />
            </div>
            <Button size="xs" variant="ghost" onClick={() => api.cancelTask(t.id).catch((e) => toast({ tone: "error", title: "Couldn't cancel", body: humanError(e) }))}>
              Cancel
            </Button>
          </div>
        ))}

        <Card
          title="Restore points"
          description="Snapshots capture the disks (and optionally the memory) so you can roll back in seconds."
          actions={!form && <Button size="sm" variant="primary" icon="camera" disabled={busy} onClick={() => setForm(true)}>Take snapshot</Button>}
          padded={false}
        >
          {snaps.isLoading ? (
            <div className="space-y-3 p-5">
              {[0, 1].map((i) => <div key={i} className="h-12 animate-pulse rounded-lg bg-zinc-100 dark:bg-zinc-800/60" />)}
            </div>
          ) : nodes.length === 0 ? (
            <EmptyState
              icon="camera"
              title="No snapshots yet"
              actions={!form && <Button variant="primary" icon="camera" onClick={() => setForm(true)}>Take the first one</Button>}
            >
              Take one before upgrades or risky changes. It's instant, and the machine keeps running.
            </EmptyState>
          ) : (
            <ol className="relative border-t border-zinc-100 px-5 py-4 dark:border-zinc-800">
              {nodes.map((n, i) => (
                <li key={n.name} className="relative flex gap-4 pb-5 last:pb-0" style={{ marginLeft: n.depth * 28 }}>
                  {/* rail */}
                  {i < nodes.length - 1 && <span className="absolute top-6 bottom-0 left-[11px] w-px bg-zinc-200 dark:bg-zinc-700" aria-hidden="true" />}
                  <span
                    className={cx(
                      "relative z-10 mt-0.5 grid size-6 shrink-0 place-items-center rounded-full ring-4 ring-white dark:ring-zinc-900",
                      n.current ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-200 text-zinc-500 dark:bg-zinc-700 dark:text-zinc-300",
                    )}
                  >
                    <Icon name={n.memory ? "zap" : "camera"} className="size-3" />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-[13px] font-semibold">{n.name}</span>
                      {n.current && <Badge tone="indigo">you are here</Badge>}
                      {n.memory && <Badge tone="sky" title="Includes memory: reverting resumes the running machine">with memory</Badge>}
                      <span className="text-xs text-zinc-500" title={n.created ? dateTime(n.created * 1000) : undefined}>
                        {n.created ? ago(n.created * 1000) : ""}
                        {n.state && ` · ${n.state === "shutoff" ? "taken while off" : n.state === "running" || n.state === "paused" ? "taken while running" : n.state === "disk-snapshot" ? "disks only" : n.state}`}
                      </span>
                    </div>
                    {n.description && <p className="mt-0.5 text-[13px] text-zinc-600 dark:text-zinc-400">{n.description}</p>}
                  </div>
                  <div className="flex shrink-0 items-start gap-1">
                    <Button size="xs" variant="secondary" icon="history" disabled={busy} onClick={() => revert(n)}>Revert</Button>
                    <Menu
                      label={`Actions for ${n.name}`}
                      items={[{ label: "Delete snapshot", icon: "trash", danger: true, disabled: busy, onSelect: () => remove(n) }]}
                      trigger={(p) => <IconButton icon="more" label={`Actions for ${n.name}`} size="xs" {...p} />}
                    />
                  </div>
                </li>
              ))}
              <li className="relative flex gap-4 pt-1" style={{ marginLeft: (nodes.find((n) => n.current)?.depth ?? 0) * 28 }}>
                <span className="grid size-6 shrink-0 place-items-center">
                  <span className={cx("size-2.5 rounded-full", isOn(vm) ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600")} />
                </span>
                <span className="text-xs text-zinc-500">Now · {isOn(vm) ? "running" : "off"}</span>
              </li>
            </ol>
          )}
        </Card>
      </div>
      {form ? (
        <TakeForm vm={vm} busy={busy} onDone={(t) => (t && track(t), setForm(false))} onCancel={() => setForm(false)} />
      ) : (
        <Card title="How snapshots work">
          <ul className="space-y-2.5 text-[13px] leading-relaxed text-zinc-600 dark:text-zinc-400">
            <li className="flex gap-2"><Icon name="camera" className="mt-0.5 size-4 text-zinc-400" />Disk-only snapshots are instant and don't pause the machine.</li>
            <li className="flex gap-2"><Icon name="zap" className="mt-0.5 size-4 text-zinc-400" />With memory, reverting resumes the machine exactly where it was. Saving the memory briefly pauses it.</li>
            <li className="flex gap-2"><Icon name="history" className="mt-0.5 size-4 text-zinc-400" />Reverting discards everything since. Take a fresh snapshot first if you might want it back.</li>
          </ul>
        </Card>
      )}
    </div>
  );
}

function TakeForm({ vm, busy, onDone, onCancel }: { vm: Vm; busy: boolean; onDone: (t: Task | null) => void; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [memory, setMemory] = useState(false);
  const [pending, setPending] = useState(false);
  const on = vm.status.state === "running" || vm.status.state === "paused";
  const nameOk = !name || /^[A-Za-z0-9_][A-Za-z0-9._-]{0,63}$/.test(name);
  const submit = async () => {
    setPending(true);
    try {
      const t = await api.createSnapshot(vm.id, { name: name || undefined, description, memory: on && memory });
      onDone(t);
    } catch (e) {
      toast({ tone: "error", title: "Couldn't take the snapshot", body: humanError(e) });
      setPending(false);
    }
  };
  return (
    <Card title="Take a snapshot" className="animate-in">
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (nameOk) void submit();
        }}
      >
        <Field label="Name" htmlFor="snap-name" hint="Optional. Leave empty for a timestamp." error={nameOk ? undefined : "Letters, digits, “.”, “_” or “-”."}>
          <Input id="snap-name" value={name} onChange={(e) => setName(e.target.value.replace(/\s/g, "-"))} placeholder="before-upgrade" autoFocus aria-invalid={!nameOk} />
        </Field>
        <Field label="Note" htmlFor="snap-desc">
          <Input id="snap-desc" value={description} onChange={(e) => setDescription(e.target.value)} placeholder="What's about to change?" />
        </Field>
        <Switch
          checked={on && memory}
          onChange={setMemory}
          disabled={!on}
          label="Include memory"
          description={on ? "Reverting then resumes right where it left off. Briefly pauses the machine." : "Only possible while the machine runs."}
        />
        <div className="flex justify-end gap-2 pt-1">
          <Button variant="ghost" onClick={onCancel}>Cancel</Button>
          <Button type="submit" variant="primary" icon="camera" loading={pending} disabled={busy || !nameOk}>Take snapshot</Button>
        </div>
      </form>
    </Card>
  );
}
