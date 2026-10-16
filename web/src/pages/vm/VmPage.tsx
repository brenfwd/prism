import { lazy, Suspense, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, humanError, type Vm, type VmAction } from "../../api";
import { applyTask } from "../../lib/tasks";
import { toast } from "../../lib/toast";
import type { VmTab } from "../../main";
import { actionMeta, availableActions, powerCycle, usePendingAction, useRunAction, useVm } from "../../lib/hooks";
import { memLabel } from "../../lib/format";
import { Icon, type IconName } from "../../components/icons";
import { Badge, Button, Callout, EmptyState, IconButton, Input, Menu, Modal, StateBadge, cx } from "../../components/ui";
import { confirm } from "../../components/Confirm";

const VncConsole = lazy(() => import("../../components/VncConsole").then((m) => ({ default: m.VncConsole })));
const SerialConsole = lazy(() => import("../../components/SerialConsole").then((m) => ({ default: m.SerialConsole })));
import { OverviewTab } from "./Overview";
import { HardwareTab } from "./Hardware";
import { SnapshotsTab } from "./Snapshots";
import { SettingsTab } from "./Settings";

const tabs: { id: VmTab; label: string; icon: IconName }[] = [
  { id: "overview", label: "Overview", icon: "gauge" },
  { id: "console", label: "Console", icon: "monitor" },
  { id: "serial", label: "Serial", icon: "terminal" },
  { id: "hardware", label: "Hardware", icon: "cpu" },
  { id: "snapshots", label: "Snapshots", icon: "camera" },
  { id: "settings", label: "Settings", icon: "settings" },
];

export function VmPage({ id, tab, take }: { id: string; tab: VmTab; take: boolean }) {
  const q = useVm(id);
  if (q.error) {
    return (
      <EmptyState
        icon="alert"
        title="This machine doesn't exist"
        actions={<Link to="/vms"><Button>Back to machines</Button></Link>}
      >
        It may have been deleted. {humanError(q.error)}
      </EmptyState>
    );
  }
  if (!q.data) return <PageSkeleton />;
  const vm = q.data;
  return (
    <div className="animate-in">
      <Header vm={vm} />
      <nav aria-label="Machine sections" className="scrollbar-thin -mx-4 mb-6 flex gap-1 overflow-x-auto border-b border-zinc-200 px-4 sm:mx-0 sm:px-0 dark:border-zinc-800">
        {tabs.map((t, i) => (
          <Link
            key={t.id}
            to="/vms/$id"
            params={{ id }}
            search={{ tab: t.id }}
            replace
            aria-current={tab === t.id ? "page" : undefined}
            title={`${t.label} (${i + 1})`}
            className={cx(
              "-mb-px flex items-center gap-2 border-b-2 px-3 py-2.5 text-sm font-medium whitespace-nowrap transition-colors",
              tab === t.id
                ? "border-indigo-500 text-zinc-900 dark:text-white"
                : "border-transparent text-zinc-500 hover:border-zinc-300 hover:text-zinc-800 dark:hover:border-zinc-700 dark:hover:text-zinc-200",
            )}
          >
            <Icon name={t.icon} className="size-4 opacity-70" />
            {t.label}
          </Link>
        ))}
      </nav>
      {tab === "overview" && <OverviewTab vm={vm} />}
      <Suspense fallback={<div className="h-[calc(100dvh-17rem)] min-h-[360px] animate-pulse rounded-xl bg-zinc-900" />}>
        {tab === "console" && (isOn(vm) ? <VncConsole key={vm.id} vmId={vm.id} /> : <Off vm={vm} what="console" />)}
        {tab === "serial" && (isOn(vm) ? <SerialConsole key={vm.id} vmId={vm.id} /> : <Off vm={vm} what="serial console" />)}
      </Suspense>
      {tab === "hardware" && <HardwareTab vm={vm} />}
      {tab === "snapshots" && <SnapshotsTab vm={vm} openForm={take} />}
      {tab === "settings" && <SettingsTab vm={vm} />}
    </div>
  );
}

export const isOn = (vm: Vm) => ["running", "paused", "shutting_down", "suspended"].includes(vm.status.state);

function PageSkeleton() {
  return (
    <div className="animate-pulse space-y-4">
      <div className="h-8 w-56 rounded-lg bg-zinc-200 dark:bg-zinc-800" />
      <div className="h-4 w-80 rounded bg-zinc-200 dark:bg-zinc-800" />
      <div className="h-10 rounded bg-zinc-100 dark:bg-zinc-900" />
      <div className="grid gap-4 sm:grid-cols-3">
        {[0, 1, 2].map((i) => <div key={i} className="h-32 rounded-xl bg-zinc-100 dark:bg-zinc-900" />)}
      </div>
    </div>
  );
}

function Off({ vm, what }: { vm: Vm; what: string }) {
  const run = useRunAction();
  const pending = usePendingAction(vm.id);
  return (
    <div className="grid h-[calc(100dvh-17rem)] min-h-[360px] place-items-center rounded-xl bg-zinc-950 text-center text-zinc-300 ring-1 ring-zinc-900/10 dark:ring-white/10">
      <div className="px-6">
        <div className="mx-auto mb-4 grid size-14 place-items-center rounded-2xl bg-white/5 text-zinc-400 ring-1 ring-white/10">
          <Icon name="power" className="size-7" />
        </div>
        <p className="text-base font-semibold text-white">{vm.name} is {vm.status.state === "crashed" ? "crashed" : "off"}</p>
        <p className="mt-1 text-sm text-zinc-400">Start it to open the {what}. It connects as soon as it boots.</p>
        <Button className="mt-5" variant="primary" icon="play" loading={pending === "start"} onClick={() => run(vm, "start")}>
          Start {vm.name}
        </Button>
      </div>
    </div>
  );
}

/** The most useful action for the current state, then the rest. */
export function PowerControls({ vm, compact }: { vm: Vm; compact?: boolean }) {
  const run = useRunAction();
  const pending = usePendingAction(vm.id);
  const actions = availableActions(vm.status.state);
  const [primary, ...rest] = actions;
  const confirmable: VmAction[] = ["stop", "reset"];
  const doRun = async (a: VmAction) => {
    if (confirmable.includes(a)) {
      const ok = await confirm({
        title: a === "stop" ? `Force stop ${vm.name}?` : `Reset ${vm.name}?`,
        body: a === "stop" ? "This cuts the power immediately, like pulling the plug. Unsaved data in the guest is lost." : "This presses the reset button: an instant hard reboot without a clean shutdown.",
        confirmLabel: a === "stop" ? "Force stop" : "Reset",
        danger: true,
      });
      if (!ok) return;
    }
    void run(vm, a);
  };
  if (!primary) return null;
  const pm = actionMeta[primary];
  return (
    <div className="flex items-center gap-2">
      <Button
        variant={primary === "start" || primary === "resume" ? "primary" : "secondary"}
        icon={pm.icon}
        loading={pending === primary}
        disabled={!!pending && pending !== primary}
        onClick={() => doRun(primary)}
        size={compact ? "sm" : "md"}
      >
        {pending === primary ? pm.pending : pm.label}
      </Button>
      {rest.length > 0 && (
        <Menu
          label="More power actions"
          items={rest.map((a) => ({
            label: actionMeta[a].label,
            icon: actionMeta[a].icon,
            danger: a === "stop" || a === "reset",
            disabled: !!pending,
            onSelect: () => doRun(a),
          }))}
          trigger={(p) => (
            <IconButton icon="more" label="More power actions" variant="secondary" size={compact ? "sm" : "md"} {...p} />
          )}
        />
      )}
    </div>
  );
}

function Header({ vm }: { vm: Vm }) {
  const qc = useQueryClient();
  const pending = usePendingAction(vm.id);
  const s = vm.status;
  const nic = vm.nics[0];
  const [cloning, setCloning] = useState(false);
  const boot = vm.disks.find((d) => d.device === "disk");
  return (
    <div className="mb-5">
      <div className="flex flex-col gap-x-4 gap-y-3 sm:flex-row sm:items-start">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="truncate text-2xl font-semibold tracking-tight">{vm.name}</h1>
            <StateBadge state={s.state} pending={pending ? actionMeta[pending].pending : null} />
            {s.agent && <Badge tone="sky" icon="link" title="The QEMU guest agent is connected">agent</Badge>}
            {vm.autostart && <Badge icon="zap" title="Starts when the host boots">autostart</Badge>}
          </div>
          <p className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-zinc-500 dark:text-zinc-400">
            {vm.description && <span className="w-full text-zinc-700 sm:w-auto dark:text-zinc-300">{vm.description}</span>}
            <Meta icon="cpu">{vm.cpu.vcpus} vCPU</Meta>
            <Meta icon="memory">{memLabel(vm.memory.max_mib)}</Meta>
            {boot?.volume && <Meta icon="disk"><span className="font-mono text-xs">{boot.volume}</span></Meta>}
            {nic && <Meta icon="network">{nic.network.type === "bridge" ? nic.network.bridge : "NAT"}</Meta>}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {isOn(vm) && (
            <Link to="/vms/$id" params={{ id: vm.id }} search={{ tab: "console" }}>
              <Button icon="monitor" variant="ghost" className="hidden sm:inline-flex">Console</Button>
            </Link>
          )}
          <PowerControls vm={vm} />
          <Menu
            label="Machine actions"
            items={[{
              label: "Clone…",
              icon: "copy",
              disabled: isOn(vm),
              onSelect: () => setCloning(true),
            }]}
            trigger={(p) => <IconButton icon="more" label="Machine actions" variant="secondary" {...p} />}
          />
        </div>
      </div>
      {cloning && <CloneVm vm={vm} onClose={() => setCloning(false)} />}
      {s.pending_restart && (
        <Callout
          tone="warning"
          className="mt-4"
          icon="rotate"
          title="Some changes apply after a power cycle"
          action={
            isOn(vm) ? (
              <Button size="sm" variant="secondary" loading={!!pending} onClick={() => void powerCycle(qc, vm)}>
                Restart now
              </Button>
            ) : undefined
          }
        >
          A reboot from inside the guest isn't enough: the machine has to power off and start again.
        </Callout>
      )}
    </div>
  );
}

function Meta({ icon, children }: { icon: IconName; children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
      <Icon name={icon} className="size-3.5 opacity-70" />
      {children}
    </span>
  );
}

function CloneVm({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(`${vm.name}-clone`);
  const clone = useMutation({
    mutationFn: () => api.cloneVm(vm.id, name),
    onSuccess: (t) => {
      applyTask(qc, t);
      toast({ tone: "info", title: `Cloning ${vm.name}`, body: "Watch the progress in the task tray. The copy appears in Machines when it's ready." });
      onClose();
    },
    onError: (e) => toast({ tone: "error", title: "Couldn't start the clone", body: humanError(e) }),
  });
  return (
    <Modal
      title={`Clone ${vm.name}`}
      description="Copies every disk (snapshot history is flattened) into a new machine with its own identity and network addresses."
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" icon="copy" loading={clone.isPending} disabled={!name} onClick={() => clone.mutate()}>Clone</Button>
        </>
      }
    >
      <form onSubmit={(e) => (e.preventDefault(), name && clone.mutate())}>
        <Input autoFocus aria-label="Name of the clone" value={name} onChange={(e) => setName(e.target.value.replace(/[^A-Za-z0-9._-]/g, "-"))} />
      </form>
    </Modal>
  );
}
