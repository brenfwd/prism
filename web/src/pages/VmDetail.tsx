import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { api, type BalloonMode, type Vm } from "../api";
import { duration, kib, mib } from "../format";
import { Button, Card, ErrorText, Field, Input, Meter, Stat, StateBadge, inputClass } from "../components/ui";
import { VncConsole } from "../components/VncConsole";
import { SerialConsole } from "../components/SerialConsole";
import { VmActions } from "./VmActions";

const tabs = ["Summary", "Console", "Serial", "Settings"] as const;
type Tab = (typeof tabs)[number];

export function VmDetailPage({ id }: { id: string }) {
  const vm = useQuery({ queryKey: ["vm", id], queryFn: () => api.vm(id) });
  const [tab, setTab] = useState<Tab>("Summary");
  if (vm.error) return <ErrorText error={vm.error} />;
  if (!vm.data) return <p className="text-sm text-zinc-500">Loading…</p>;
  const v = vm.data;
  const running = v.status.state === "running" || v.status.state === "paused";
  return (
    <div className="flex h-full flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <Link to="/vms" className="text-sm text-zinc-500 hover:underline">VMs</Link>
        <span className="text-zinc-400">/</span>
        <h1 className="text-xl font-semibold">{v.name}</h1>
        <StateBadge state={v.status.state} />
        {v.status.pending_restart && (
          <span className="rounded bg-amber-100 px-2 py-0.5 text-xs text-amber-800 dark:bg-amber-900/40 dark:text-amber-300">
            changes apply after power off
          </span>
        )}
        <div className="flex-1" />
        <VmActions vm={v} />
      </div>
      <nav className="flex gap-1 border-b border-zinc-200 dark:border-zinc-800">
        {tabs.map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`-mb-px border-b-2 px-3 py-2 text-sm ${tab === t ? "border-indigo-500 font-medium" : "border-transparent text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"}`}
          >
            {t}
          </button>
        ))}
      </nav>
      <div className="min-h-0 flex-1">
        {tab === "Summary" && <Summary vm={v} />}
        {tab === "Console" && (running ? <VncConsole vmId={v.id} /> : <NotRunning />)}
        {tab === "Serial" && (running ? <SerialConsole vmId={v.id} /> : <NotRunning />)}
        {tab === "Settings" && <Settings vm={v} />}
      </div>
    </div>
  );
}

function NotRunning() {
  return <p className="py-12 text-center text-sm text-zinc-500">Start the VM to open its console.</p>;
}

function Summary({ vm }: { vm: Vm }) {
  const s = vm.status;
  const ms = s.memory_stats;
  const inUse = ms.available_kib != null && ms.usable_kib != null ? ms.available_kib - ms.usable_kib : null;
  return (
    <div className="grid gap-4 lg:grid-cols-3">
      <Card title="Resources" className="lg:col-span-2">
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Stat label="vCPUs" value={vm.cpu.vcpus} />
          <Stat label="Balloon" value={kib(s.memory_kib || vm.memory.target_mib * 1024)} sub={`of ${mib(vm.memory.max_mib)} max`} />
          <Stat label="Guest in use" value={inUse != null ? kib(inUse) : "—"} sub={ms.available_kib != null ? `of ${kib(ms.available_kib)}` : "needs guest balloon driver"} />
          <Stat label="CPU time" value={duration(s.cpu_time_ns)} />
        </div>
        {inUse != null && ms.available_kib != null && <Meter className="mt-4" value={inUse} max={ms.available_kib} />}
        <Balloon vm={vm} />
      </Card>
      <Card title="Hardware">
        <dl className="space-y-2 text-sm">
          <Row k="Machine" v={vm.machine} />
          <Row k="Firmware" v={vm.firmware.toUpperCase()} />
          {vm.disks.map((d) => (
            <Row key={d.id} k={d.device === "cdrom" ? "CD-ROM" : "Disk"} v={`${d.pool}/${d.volume}`} />
          ))}
          {vm.nics.map((n) => (
            <Row key={n.id} k="NIC" v={`${n.network.type === "bridge" ? n.network.bridge : "user"} · ${n.mac}`} />
          ))}
          <Row k="ID" v={vm.id} />
        </dl>
      </Card>
    </div>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex justify-between gap-4">
      <dt className="text-zinc-500">{k}</dt>
      <dd className="truncate font-mono text-xs leading-5" title={v}>{v}</dd>
    </div>
  );
}

function Balloon({ vm }: { vm: Vm }) {
  const [target, setTarget] = useState<number | null>(null);
  const save = useMutation({
    mutationFn: (t: number) => api.updateVm(vm.id, { memory: { target_mib: t } }),
    onSuccess: () => setTarget(null),
  });
  if (vm.memory.balloon === "off") return null;
  const value = target ?? vm.memory.target_mib;
  return (
    <div className="mt-6 border-t border-zinc-200 pt-4 dark:border-zinc-800">
      <div className="mb-2 flex items-center justify-between text-sm">
        <span className="font-medium">Balloon target</span>
        <span className="tabular-nums">{mib(value)}</span>
      </div>
      <input
        type="range"
        className="w-full accent-indigo-600"
        min={vm.memory.min_mib}
        max={vm.memory.max_mib}
        step={64}
        value={value}
        onChange={(e) => setTarget(+e.target.value)}
        onPointerUp={() => target != null && save.mutate(target)}
        onKeyUp={() => target != null && save.mutate(target)}
      />
      <div className="flex justify-between text-xs text-zinc-500">
        <span>floor {mib(vm.memory.min_mib)}</span>
        <span>max {mib(vm.memory.max_mib)}</span>
      </div>
      <ErrorText error={save.error} />
    </div>
  );
}

function Settings({ vm }: { vm: Vm }) {
  const navigate = useNavigate();
  const [form, setForm] = useState({
    vcpus: vm.cpu.vcpus,
    max_mib: vm.memory.max_mib,
    min_mib: vm.memory.min_mib,
    balloon: vm.memory.balloon,
    description: vm.description,
  });
  const save = useMutation({
    mutationFn: () =>
      api.updateVm(vm.id, {
        vcpus: form.vcpus,
        description: form.description,
        memory: { max_mib: form.max_mib, min_mib: form.min_mib, balloon: form.balloon },
      }),
  });
  const [deleteDisks, setDeleteDisks] = useState(true);
  const del = useMutation({
    mutationFn: () => api.deleteVm(vm.id, deleteDisks),
    onSuccess: () => navigate({ to: "/vms" }),
  });
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Card title="Configuration">
        <form
          className="space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <Field label="Description">
            <Input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} />
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label="vCPUs">
              <Input type="number" min={1} value={form.vcpus} onChange={(e) => setForm({ ...form, vcpus: +e.target.value })} />
            </Field>
            <Field label="Balloon mode">
              <select className={inputClass} value={form.balloon} onChange={(e) => setForm({ ...form, balloon: e.target.value as BalloonMode })}>
                <option value="manual">Manual</option>
                <option value="off">Off</option>
              </select>
            </Field>
            <Field label="Max memory (MiB)">
              <Input type="number" min={64} step={64} value={form.max_mib} onChange={(e) => setForm({ ...form, max_mib: +e.target.value })} />
            </Field>
            <Field label="Balloon floor (MiB)">
              <Input type="number" min={0} step={64} value={form.min_mib} onChange={(e) => setForm({ ...form, min_mib: +e.target.value })} />
            </Field>
          </div>
          <ErrorText error={save.error} />
          <div className="flex items-center justify-end gap-3">
            {save.isSuccess && <span className="text-xs text-emerald-600">Saved</span>}
            <Button type="submit" variant="primary" disabled={save.isPending}>Save</Button>
          </div>
        </form>
      </Card>
      <Card title="Delete">
        <p className="mb-3 text-sm text-zinc-600 dark:text-zinc-400">Powers off and removes the VM. This cannot be undone.</p>
        <label className="mb-4 flex items-center gap-2 text-sm">
          <input type="checkbox" checked={deleteDisks} onChange={(e) => setDeleteDisks(e.target.checked)} />
          Also delete its disks (ISOs are kept)
        </label>
        <ErrorText error={del.error} />
        <Button
          variant="danger"
          disabled={del.isPending}
          onClick={() => confirm(`Delete ${vm.name}?`) && del.mutate()}
        >
          Delete VM
        </Button>
      </Card>
    </div>
  );
}
