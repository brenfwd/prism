import { useMemo, useState } from "react";
import { Link, useNavigate } from "@tanstack/react-router";
import type { Vm, VmState } from "../api";
import { useVms } from "../lib/hooks";
import { SAMPLE_CAPACITY, guestUsedKib, useSamples } from "../lib/stats";
import { kib, memLabel, pct } from "../lib/format";
import { Icon } from "../components/icons";
import { Badge, Button, Card, EmptyState, Input, Segmented, Sparkline, StateBadge, StateDot, cx } from "../components/ui";
import { PowerControls } from "./vm/VmPage";
import { usePendingAction, actionMeta } from "../lib/hooks";
import { FirstRun } from "./Overview";

type Filter = "all" | "running" | "stopped";

const group = (s: VmState): Filter => (s === "running" || s === "paused" || s === "shutting_down" || s === "suspended" ? "running" : "stopped");

export function MachinesPage() {
  const vms = useVms();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const navigate = useNavigate();
  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (vms.data ?? []).filter((v) => (filter === "all" || group(v.status.state) === filter) && (!q || v.name.toLowerCase().includes(q) || v.description.toLowerCase().includes(q)));
  }, [vms.data, query, filter]);
  const counts = useMemo(() => {
    const all = vms.data ?? [];
    return { all: all.length, running: all.filter((v) => group(v.status.state) === "running").length, stopped: all.filter((v) => group(v.status.state) === "stopped").length };
  }, [vms.data]);

  if (vms.isLoading) return <ListSkeleton />;
  if (!vms.data?.length) return <FirstRun />;

  return (
    <div className="animate-in">
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Machines</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">{counts.running} running · {counts.stopped} off</p>
        </div>
        <Link to="/vms/new"><Button variant="primary" icon="plus">New machine</Button></Link>
      </div>
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <div className="relative min-w-0 flex-1 sm:max-w-xs">
          <Icon name="search" className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-zinc-400" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Filter by name"
            aria-label="Filter machines"
            className="pl-9"
            onKeyDown={(e) => {
              if (e.key === "Enter" && list.length === 1) navigate({ to: "/vms/$id", params: { id: list[0].id } });
              if (e.key === "Escape") setQuery("");
            }}
          />
        </div>
        <Segmented
          label="Show"
          size="sm"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: `All ${counts.all}` },
            { value: "running", label: `Running ${counts.running}` },
            { value: "stopped", label: `Off ${counts.stopped}` },
          ]}
        />
      </div>
      {list.length === 0 ? (
        <Card>
          <EmptyState icon="search" title="Nothing matches" actions={<Button size="sm" onClick={() => (setQuery(""), setFilter("all"))}>Clear filters</Button>}>
            No machine matches “{query}”{filter !== "all" ? ` among ${filter === "running" ? "running" : "stopped"} machines` : ""}.
          </EmptyState>
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {list.map((vm) => <VmCard key={vm.id} vm={vm} />)}
        </div>
      )}
    </div>
  );
}

function ListSkeleton() {
  return (
    <div className="grid animate-pulse gap-4 sm:grid-cols-2 xl:grid-cols-3">
      {[0, 1, 2].map((i) => <div key={i} className="h-44 rounded-xl bg-zinc-100 dark:bg-zinc-900" />)}
    </div>
  );
}

export function VmCard({ vm }: { vm: Vm }) {
  const samples = useSamples(vm.id);
  const s = vm.status;
  const on = group(s.state) === "running";
  const pending = usePendingAction(vm.id);
  const cpu = [...samples].reverse().find((x) => x.cpu != null)?.cpu;
  const used = guestUsedKib(s);
  const disk = vm.disks.find((d) => d.device === "disk");
  return (
    <Card className="group relative flex flex-col transition-shadow hover:shadow-md hover:shadow-zinc-900/5" padded={false}>
      <div className="flex items-start gap-3 px-4 pt-4">
        <span className={cx("grid size-10 shrink-0 place-items-center rounded-xl", on ? "bg-emerald-50 text-emerald-600 dark:bg-emerald-500/10 dark:text-emerald-400" : "bg-zinc-100 text-zinc-400 dark:bg-zinc-800")}>
          <Icon name="server" className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <Link to="/vms/$id" params={{ id: vm.id }} className="block truncate font-semibold tracking-tight after:absolute after:inset-0 after:rounded-xl focus-visible:outline-none">
            {vm.name}
          </Link>
          <div className="mt-0.5 flex items-center gap-2 text-xs text-zinc-500">
            {pending ? <StateBadge state={s.state} pending={actionMeta[pending].pending} /> : (
              <span className="inline-flex items-center gap-1.5"><StateDot state={s.state} className="size-2 [&>span]:size-2" />{s.state.replace("_", " ")}</span>
            )}
            {s.pending_restart && <Badge tone="amber" icon="rotate" title="Changes apply after a power cycle">restart</Badge>}
          </div>
        </div>
      </div>
      <div className="px-4 pt-3">
        {on ? (
          <div className="grid grid-cols-2 gap-3">
            <div>
              <div className="flex justify-between text-[11px] text-zinc-500"><span>CPU</span><span className="tabular-nums">{cpu != null ? pct(cpu) : "…"}</span></div>
              <Sparkline className="!h-8" label={`CPU of ${vm.name}`} values={samples.map((x) => x.cpu)} max={1} capacity={SAMPLE_CAPACITY / 2} />
            </div>
            <div>
              <div className="flex justify-between text-[11px] text-zinc-500"><span>Memory</span><span className="tabular-nums">{used != null ? kib(used) : memLabel(vm.memory.max_mib)}</span></div>
              <Sparkline className="!h-8" color="emerald" label={`Memory of ${vm.name}`} values={samples.map((x) => x.usedKib ?? x.rssKib)} max={vm.memory.max_mib * 1024} capacity={SAMPLE_CAPACITY / 2} />
            </div>
          </div>
        ) : (
          <p className="flex h-12 items-center text-xs text-zinc-400">{vm.description || "Powered off"}</p>
        )}
      </div>
      <div className="mt-auto flex items-center gap-2 border-t border-zinc-100 px-4 py-2.5 text-xs text-zinc-500 dark:border-zinc-800">
        <span className="min-w-0 flex-1 truncate">
          {vm.cpu.vcpus} vCPU · {memLabel(vm.memory.max_mib)}{disk?.volume ? ` · ${disk.pool}` : ""}
        </span>
        <div className="relative z-10">
          <PowerControls vm={vm} compact />
        </div>
      </div>
    </Card>
  );
}
