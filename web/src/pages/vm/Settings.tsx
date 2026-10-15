import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { api, humanError, type BalloonMode, type CpuModel, type UpdateVm, type Vm } from "../../api";
import { putVm } from "../../events";
import { memLabel } from "../../lib/format";
import { useNode } from "../../lib/hooks";
import { toast } from "../../lib/toast";
import { Icon } from "../../components/icons";
import { Badge, Button, Card, Checkbox, ErrorText, Field, Input, Modal, Segmented, Switch, Textarea } from "../../components/ui";
import { isOn } from "./VmPage";

export function SettingsTab({ vm }: { vm: Vm }) {
  return (
    <div className="mx-auto max-w-3xl space-y-5">
      <General vm={vm} />
      <Resources vm={vm} />
      {vm.memory.balloon === "manual" && isOn(vm) && <LiveMemory vm={vm} />}
      <CloudInit vm={vm} />
      <Danger vm={vm} />
    </div>
  );
}

function useSave(vm: Vm, what: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (req: UpdateVm) => api.updateVm(vm.id, { ...req, generation: vm.generation }),
    onSuccess: (view) => {
      putVm(qc, view);
      if (isOn(view) && view.status.pending_restart && !vm.status.pending_restart)
        toast({ tone: "warning", title: `${what} saved`, body: "Applies the next time the machine powers off and starts." });
      else toast({ tone: "success", title: `${what} saved` });
    },
  });
}

function General({ vm }: { vm: Vm }) {
  const [description, setDescription] = useState(vm.description);
  useEffect(() => setDescription(vm.description), [vm.description]);
  const save = useSave(vm, "Description");
  const auto = useSave(vm, "Autostart");
  return (
    <Card title="General">
      <div className="space-y-5">
        <Field label="Description" htmlFor="desc">
          <div className="flex gap-2">
            <Input id="desc" value={description} onChange={(e) => setDescription(e.target.value)} placeholder="What's this machine for?" />
            {description !== vm.description && (
              <Button variant="primary" loading={save.isPending} onClick={() => save.mutate({ description })}>Save</Button>
            )}
          </div>
        </Field>
        <Switch
          checked={!!vm.autostart}
          disabled={auto.isPending}
          onChange={(v) => auto.mutate({ autostart: v })}
          label="Start when the host boots"
          description="Prism starts this machine automatically after the host restarts."
        />
        <ErrorText error={save.error || auto.error} />
      </div>
    </Card>
  );
}

const balloonHelp: Record<BalloonMode, string> = {
  auto: "Prism watches host memory pressure and gently takes back memory the guest isn't using, never going below the floor. When the guest needs it again, it gets it back.",
  manual: "The guest's memory stays where you set it. You can resize it live while it runs.",
  off: "The guest always has its full memory. Simplest and most predictable, but nothing is shared back with the host.",
};

function Resources({ vm }: { vm: Vm }) {
  const node = useNode();
  const initial = { vcpus: vm.cpu.vcpus, max: vm.memory.max_mib, min: vm.memory.min_mib, balloon: vm.memory.balloon, model: vm.cpu.model ?? "host_model" as CpuModel };
  const [f, setF] = useState(initial);
  useEffect(() => setF(initial), [vm.generation]);
  const dirty = JSON.stringify(f) !== JSON.stringify(initial);
  const save = useSave(vm, "CPU and memory");
  const minOk = f.min <= f.max && f.min >= 64;
  const on = isOn(vm);
  const needsRestart = on && (f.vcpus !== initial.vcpus || f.max !== initial.max || f.model !== initial.model);
  return (
    <Card title="CPU and memory" description={on ? "CPU count, memory size and CPU model change at the next power cycle; ballooning applies right away." : undefined}>
      <form
        className="space-y-5"
        onSubmit={(e) => {
          e.preventDefault();
          const req: UpdateVm = {};
          if (f.vcpus !== initial.vcpus) req.vcpus = f.vcpus;
          if (f.model !== initial.model) req.cpu_model = f.model;
          const mem: UpdateVm["memory"] = {};
          if (f.max !== initial.max) {
            mem.max_mib = f.max;
            // Keep the target inside the new bounds.
            mem.target_mib = Math.min(Math.max(vm.memory.target_mib, f.min), f.max);
          }
          if (f.min !== initial.min) mem.min_mib = f.min;
          if (f.balloon !== initial.balloon) mem.balloon = f.balloon;
          if (Object.keys(mem).length) req.memory = mem;
          save.mutate(req);
        }}
      >
        <div className="grid gap-5 sm:grid-cols-2">
          <Field label="vCPUs" htmlFor="vcpus" hint={node ? `The host has ${node.host.cpus}.` : undefined}>
            <Input id="vcpus" type="number" min={1} max={512} value={f.vcpus} onChange={(e) => setF({ ...f, vcpus: Math.max(1, +e.target.value) })} />
          </Field>
          <Field label="Memory (MiB)" htmlFor="maxmem" hint={`${memLabel(f.max)} at boot.`}>
            <Input id="maxmem" type="number" min={128} step={256} value={f.max} onChange={(e) => setF({ ...f, max: Math.max(64, +e.target.value) })} />
          </Field>
        </div>
        <Field label="CPU model" hint={f.model === "host_model" ? "The host's CPU features; can move between similar hosts." : "The exact host CPU: fastest, but can only move to identical hosts."}>
          <Segmented label="CPU model" value={f.model} onChange={(model) => setF({ ...f, model })} options={[{ value: "host_model", label: "Host model" }, { value: "host_passthrough", label: "Passthrough" }]} />
        </Field>
        <div className="rounded-lg bg-zinc-50 p-4 ring-1 ring-zinc-200/70 dark:bg-white/[0.02] dark:ring-zinc-800">
          <Field label="Memory ballooning" hint={balloonHelp[f.balloon]}>
            <Segmented label="Memory ballooning" value={f.balloon} onChange={(balloon) => setF({ ...f, balloon })} options={[{ value: "auto", label: "Automatic" }, { value: "manual", label: "Manual" }, { value: "off", label: "Off" }]} />
          </Field>
          {f.balloon !== "off" && (
            <Field className="mt-4 max-w-xs" label="Floor (MiB)" htmlFor="minmem" hint="Never shrink the guest below this." error={minOk ? undefined : "Must be between 64 MiB and the memory size."}>
              <Input id="minmem" type="number" min={64} step={64} value={f.min} onChange={(e) => setF({ ...f, min: +e.target.value })} aria-invalid={!minOk} />
            </Field>
          )}
        </div>
        <ErrorText error={save.error} />
        <div className="flex items-center justify-end gap-3">
          {needsRestart && <Badge tone="amber" icon="rotate">applies after power off</Badge>}
          {dirty && <Button variant="ghost" onClick={() => setF(initial)}>Reset</Button>}
          <Button type="submit" variant="primary" disabled={!dirty || !minOk} loading={save.isPending}>Save changes</Button>
        </div>
      </form>
    </Card>
  );
}

function LiveMemory({ vm }: { vm: Vm }) {
  const qc = useQueryClient();
  const [target, setTarget] = useState<number | null>(null);
  const save = useMutation({
    mutationFn: (t: number) => api.updateVm(vm.id, { memory: { target_mib: t } }),
    onSuccess: (v) => {
      putVm(qc, v);
      setTarget(null);
    },
    onError: (e) => {
      setTarget(null);
      toast({ tone: "error", title: "Couldn't resize memory", body: humanError(e) });
    },
  });
  const value = target ?? vm.memory.target_mib;
  return (
    <Card title="Live memory" description="Drag to give the running guest more or less memory, without a reboot.">
      <div className="mb-2 flex items-baseline justify-between">
        <span className="text-2xl font-semibold tracking-tight tabular-nums">{memLabel(value)}</span>
        <span className="text-xs text-zinc-500">{save.isPending ? "Applying…" : "of " + memLabel(vm.memory.max_mib)}</span>
      </div>
      <input
        type="range"
        aria-label="Memory target"
        className="w-full accent-indigo-600"
        min={vm.memory.min_mib}
        max={vm.memory.max_mib}
        step={64}
        value={value}
        onChange={(e) => setTarget(+e.target.value)}
        onPointerUp={() => target != null && save.mutate(target)}
        onKeyUp={() => target != null && save.mutate(target)}
      />
      <div className="flex justify-between text-xs text-zinc-500 tabular-nums">
        <span>floor {memLabel(vm.memory.min_mib)}</span>
        <span>max {memLabel(vm.memory.max_mib)}</span>
      </div>
    </Card>
  );
}

function CloudInit({ vm }: { vm: Vm }) {
  const [edit, setEdit] = useState(false);
  const [text, setText] = useState("#cloud-config\n");
  const save = useSave(vm, "cloud-init");
  return (
    <Card
      title="cloud-init"
      description="First-boot setup data, delivered to the guest on a small virtual CD."
      actions={
        vm.cloud_init ? (
          <>
            <Button size="sm" onClick={() => setEdit(true)}>Replace</Button>
            <Button
              size="sm"
              variant="ghost"
              loading={save.isPending}
              onClick={() => save.mutate({ cloud_init: null })}
            >
              Remove
            </Button>
          </>
        ) : (
          <Button size="sm" icon="plus" onClick={() => setEdit(true)}>Add user-data</Button>
        )
      }
    >
      {vm.cloud_init ? (
        <div className="flex items-center gap-2 text-sm">
          <Icon name="check" className="size-4 text-emerald-500" />
          <span>User-data attached</span>
          <span className="truncate font-mono text-xs text-zinc-500" title={vm.cloud_init.user_data}>{vm.cloud_init.user_data.slice(0, 26)}…</span>
        </div>
      ) : (
        <p className="text-sm text-zinc-500">None. Cloud images need user-data to create a login.</p>
      )}
      <p className="mt-2 text-xs text-zinc-500">For your security, Prism never shows the stored user-data (it may contain passwords), only its fingerprint. cloud-init normally runs once per machine.</p>
      <ErrorText className="mt-3" error={save.error} />
      {edit && (
        <Modal
          title="Replace user-data"
          size="lg"
          onClose={() => setEdit(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setEdit(false)}>Cancel</Button>
              <Button variant="primary" loading={save.isPending} onClick={() => save.mutate({ cloud_init: { user_data: text } }, { onSuccess: () => setEdit(false) })}>Save</Button>
            </>
          }
        >
          <Textarea rows={16} value={text} onChange={(e) => setText(e.target.value)} spellCheck={false} aria-label="user-data" autoFocus />
        </Modal>
      )}
    </Card>
  );
}

function Danger({ vm }: { vm: Vm }) {
  const [open, setOpen] = useState(false);
  return (
    <Card title={<span className="text-red-600 dark:text-red-400">Delete machine</span>} className="ring-red-200/80 dark:ring-red-500/20">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <p className="max-w-md text-sm text-zinc-600 dark:text-zinc-400">Powers it off and removes it for good. You choose whether its disks go too.</p>
        <Button variant="danger" icon="trash" onClick={() => setOpen(true)}>Delete {vm.name}…</Button>
      </div>
      {open && <DeleteDialog vm={vm} onClose={() => setOpen(false)} />}
    </Card>
  );
}

export function DeleteDialog({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [typed, setTyped] = useState("");
  const [disks, setDisks] = useState(true);
  const del = useMutation({
    mutationFn: () => api.deleteVm(vm.id, disks),
    onSuccess: () => {
      qc.setQueryData<Vm[]>(["vms"], (vms) => vms?.filter((v) => v.id !== vm.id));
      qc.invalidateQueries({ queryKey: ["volumes"] });
      toast({ tone: "success", title: `${vm.name} deleted` });
      navigate({ to: "/vms" });
    },
  });
  const diskCount = vm.disks.filter((d) => d.device === "disk").length;
  return (
    <Modal
      title={`Delete ${vm.name}?`}
      size="sm"
      onClose={onClose}
      icon={<div className="grid size-9 shrink-0 place-items-center rounded-full bg-red-50 text-red-600 dark:bg-red-500/10 dark:text-red-400"><Icon name="alert" className="size-[18px]" /></div>}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={typed.trim() !== vm.name} loading={del.isPending} onClick={() => del.mutate()}>Delete forever</Button>
        </>
      }
    >
      <form
        className="space-y-4 text-sm"
        onSubmit={(e) => {
          e.preventDefault();
          if (typed.trim() === vm.name) del.mutate();
        }}
      >
        <p className="text-zinc-600 dark:text-zinc-300">{isOn(vm) ? "It's running and will be powered off first. " : ""}This can't be undone.</p>
        <Checkbox checked={disks} onChange={setDisks}>
          Also delete its {diskCount === 1 ? "disk" : `${diskCount} disks`} and snapshots <span className="text-zinc-500">(ISOs are kept)</span>
        </Checkbox>
        <label className="block">
          <span className="mb-1.5 block text-[13px] text-zinc-600 dark:text-zinc-300">
            Type <span className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-[12px] font-semibold text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100">{vm.name}</span> to confirm
          </span>
          <Input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus data-autofocus autoComplete="off" spellCheck={false} aria-label="Machine name" />
        </label>
        <ErrorText error={del.error} />
      </form>
    </Modal>
  );
}
