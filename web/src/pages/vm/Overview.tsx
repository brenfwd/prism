import { Link } from "@tanstack/react-router";
import type { Vm } from "../../api";
import { SAMPLE_CAPACITY, guestUsedKib, useSamples } from "../../lib/stats";
import { duration, kib, memLabel, pct } from "../../lib/format";
import { Icon, type IconName } from "../../components/icons";
import { Badge, Card, CopyButton, Meter, Sparkline, cx } from "../../components/ui";
import { isOn } from "./VmPage";

export function OverviewTab({ vm }: { vm: Vm }) {
  const samples = useSamples(vm.id);
  const s = vm.status;
  const on = isOn(vm);
  const lastCpu = [...samples].reverse().find((x) => x.cpu != null)?.cpu ?? null;
  const used = on ? guestUsedKib(s) : null;
  const actual = s.memory_stats.actual_kib ?? s.memory_kib;
  const maxKib = vm.memory.max_mib * 1024;
  const balloon = s.balloon;

  return (
    <div className="space-y-5">
      <div className="grid gap-4 md:grid-cols-3">
        <MetricCard
          icon="cpu"
          title="CPU"
          value={on && lastCpu != null ? pct(lastCpu) : "—"}
          sub={on ? `${vm.cpu.vcpus} vCPU · ${duration(s.cpu_time_ns)} CPU time` : `${vm.cpu.vcpus} vCPU`}
        >
          {on ? (
            <Sparkline label="CPU usage over the last minutes" values={samples.map((x) => x.cpu)} max={1} capacity={SAMPLE_CAPACITY} />
          ) : (
            <Idle />
          )}
        </MetricCard>
        <MetricCard
          icon="memory"
          title="Memory in use"
          value={used != null ? kib(used) : on ? kib(actual) : "—"}
          sub={
            used != null
              ? `of ${kib(s.memory_stats.available_kib ?? actual)} the guest sees`
              : on
                ? "Guest doesn't report usage yet (needs the balloon driver)"
                : `${memLabel(vm.memory.max_mib)} when running`
          }
        >
          {on ? (
            <Sparkline
              label="Guest memory in use"
              color="emerald"
              values={samples.map((x) => x.usedKib ?? x.rssKib)}
              max={samples.reduce((m, x) => Math.max(m, x.actualKib), maxKib)}
              capacity={SAMPLE_CAPACITY}
            />
          ) : (
            <Idle />
          )}
        </MetricCard>
        <MetricCard
          icon="layers"
          title="Balloon"
          value={on ? kib(actual) : memLabel(vm.memory.max_mib)}
          sub={
            vm.memory.balloon === "off"
              ? "Off: the guest keeps all its memory"
              : vm.memory.balloon === "auto"
                ? balloon
                  ? `Automatic · ${balloon.reason}`
                  : "Automatic"
                : "Manual · resize it in Settings"
          }
        >
          <div className="mt-4">
            <Meter value={on ? actual : maxKib} max={maxKib} tone="indigo" />
            <div className="mt-1.5 flex justify-between text-[11px] text-zinc-400 tabular-nums">
              <span>floor {memLabel(vm.memory.min_mib)}</span>
              <span>max {memLabel(vm.memory.max_mib)}</span>
            </div>
            {balloon?.unresponsive && (
              <p className="mt-2 flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400">
                <Icon name="alert" className="size-3.5" />
                The guest isn't responding to balloon requests.
              </p>
            )}
          </div>
        </MetricCard>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <ConnectCard vm={vm} />
        <Card title="Devices" actions={<Link to="/vms/$id" params={{ id: vm.id }} search={{ tab: "hardware" }} className="text-[13px] font-medium text-indigo-600 hover:underline dark:text-indigo-400">Edit hardware</Link>}>
          <ul className="-my-1 divide-y divide-zinc-100 text-sm dark:divide-zinc-800">
            {vm.disks.map((d) => (
              <li key={d.id} className="flex items-center gap-3 py-2">
                <Icon name={d.device === "cdrom" ? "disc" : "disk"} className="size-4 text-zinc-400" />
                <span className="w-10 font-mono text-xs text-zinc-500">{d.target}</span>
                <span className={cx("min-w-0 flex-1 truncate font-mono text-[13px]", !d.volume && "text-zinc-400 italic")}>{d.volume ?? "empty drive"}</span>
                {d.boot_order != null && <Badge>boot {d.boot_order}</Badge>}
              </li>
            ))}
            {vm.nics.map((n) => (
              <li key={n.id} className="flex items-center gap-3 py-2">
                <Icon name="network" className="size-4 text-zinc-400" />
                <span className="w-10 font-mono text-xs text-zinc-500">nic</span>
                <span className="min-w-0 flex-1 truncate">{n.network.type === "bridge" ? `Bridge ${n.network.bridge}` : "Private NAT"}</span>
                <span className="font-mono text-xs text-zinc-500">{n.mac}</span>
              </li>
            ))}
            {!vm.disks.length && !vm.nics.length && <li className="py-2 text-zinc-500">No devices.</li>}
          </ul>
        </Card>
      </div>

      <Card title="Details">
        <dl className="grid gap-x-8 gap-y-3 text-sm sm:grid-cols-2 lg:grid-cols-3">
          <Detail k="Guest agent" v={s.agent ? <span className="text-emerald-600 dark:text-emerald-400">Connected</span> : on ? "Not connected" : "—"} hint={!s.agent && on ? "Install qemu-guest-agent in the guest for clean shutdowns and richer stats." : undefined} />
          <Detail k="Firmware" v={vm.firmware.toUpperCase()} />
          <Detail k="CPU model" v={vm.cpu.model === "host_passthrough" ? "Host passthrough" : "Host model"} />
          <Detail k="Machine type" v={<span className="font-mono text-xs">{vm.machine}</span>} />
          <Detail k="cloud-init" v={vm.cloud_init ? <span className="font-mono text-xs" title={vm.cloud_init.user_data}>{vm.cloud_init.user_data.slice(0, 19)}…</span> : "None"} />
          <Detail k="ID" v={<span className="flex items-center gap-1 font-mono text-xs">{vm.id.slice(0, 13)}… <CopyButton text={vm.id} label="Copy ID" /></span>} />
        </dl>
      </Card>
    </div>
  );
}

function Idle() {
  return (
    <div className="grid h-14 place-items-center rounded-lg border border-dashed border-zinc-200 text-xs text-zinc-400 dark:border-zinc-800">
      Live chart while running
    </div>
  );
}

function MetricCard({ icon, title, value, sub, children }: { icon: IconName; title: string; value: React.ReactNode; sub: React.ReactNode; children: React.ReactNode }) {
  return (
    <Card>
      <div className="mb-3 flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-xs font-medium text-zinc-500 dark:text-zinc-400">
            <Icon name={icon} className="size-3.5" />
            {title}
          </div>
          <div className="mt-1 text-2xl font-semibold tracking-tight tabular-nums">{value}</div>
          <div className="mt-0.5 line-clamp-1 text-xs text-zinc-500 dark:text-zinc-400" title={typeof sub === "string" ? sub : undefined}>{sub}</div>
        </div>
      </div>
      {children}
    </Card>
  );
}

function Detail({ k, v, hint }: { k: string; v: React.ReactNode; hint?: string }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-zinc-500">{k}</dt>
      <dd className="mt-0.5 truncate">{v}</dd>
      {hint && <dd className="mt-0.5 text-xs text-zinc-400">{hint}</dd>}
    </div>
  );
}

function ConnectCard({ vm }: { vm: Vm }) {
  const nat = vm.nics.length > 0 && vm.nics.every((n) => n.network.type === "user");
  const bridge = vm.nics.find((n) => n.network.type === "bridge");
  return (
    <Card title="Connect">
      <div className="space-y-3 text-sm">
        <div className="grid grid-cols-2 gap-2">
          <ConnectLink vm={vm} tab="console" icon="monitor" title="Screen" body="Graphical console" />
          <ConnectLink vm={vm} tab="serial" icon="terminal" title="Serial" body="Text console, great for cloud images" />
        </div>
        {nat && (
          <p className="text-[13px] leading-relaxed text-zinc-500 dark:text-zinc-400">
            This machine uses private NAT: it can reach out to the internet, but nothing on your network can reach it. Use the consoles above, or add a bridged network adapter in Hardware to SSH in.
          </p>
        )}
        {bridge && bridge.network.type === "bridge" && (
          <p className="text-[13px] leading-relaxed text-zinc-500 dark:text-zinc-400">
            It's on bridge <span className="font-mono">{bridge.network.bridge}</span> with MAC <span className="font-mono">{bridge.mac}</span>. Look for that MAC in your DHCP server's leases to find its address.
          </p>
        )}
        {!vm.nics.length && <p className="text-[13px] text-zinc-500">No network adapter. Add one in Hardware.</p>}
      </div>
    </Card>
  );
}

function ConnectLink({ vm, tab, icon, title, body }: { vm: Vm; tab: "console" | "serial"; icon: IconName; title: string; body: string }) {
  return (
    <Link
      to="/vms/$id"
      params={{ id: vm.id }}
      search={{ tab }}
      className="group flex items-start gap-3 rounded-lg p-3 ring-1 ring-zinc-200 transition-colors ring-inset hover:bg-zinc-50 hover:ring-zinc-300 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]"
    >
      <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-zinc-900 text-white dark:bg-white/10">
        <Icon name={icon} className="size-4" />
      </span>
      <span className="min-w-0">
        <span className="flex items-center gap-1 text-sm font-semibold">
          {title}
          <Icon name="arrowRight" className="size-3.5 opacity-0 transition-opacity group-hover:opacity-60" />
        </span>
        <span className="block text-xs text-zinc-500 dark:text-zinc-400">{body}</span>
      </span>
    </Link>
  );
}
