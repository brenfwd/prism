import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { hostAvailableBytes, looksLikeVmDisk, useAllVolumes, useNode, usePools, useTasks, useVms, volumeUsers } from "../lib/hooks";
import { startUpload, uploads } from "../lib/tasks";
import { useStore } from "../lib/store";
import { bytes } from "../lib/format";
import { DropBox, WindowDrop } from "../components/DropZone";
import { Icon, type IconName } from "../components/icons";
import { TaskRow, UploadRow } from "../components/TaskTray";
import { Badge, Button, Card, Meter, Stat, cx } from "../components/ui";
import { VmCard } from "./Machines";

export function OverviewPage() {
  const vms = useVms();
  const node = useNode();
  const pools = usePools().data ?? [];
  const tasks = useTasks().data ?? [];

  if (vms.isLoading) return <div className="h-40 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />;
  if (!vms.data?.length) return <FirstRun />;

  const list = vms.data;
  const running = list.filter((v) => v.status.state === "running");
  const total = (node?.host.memory_kib ?? 0) * 1024;
  const avail = hostAvailableBytes(node) ?? 0;
  const committed = running.reduce((a, v) => a + v.memory.max_mib * 1024 * 1024, 0);
  const capacity = pools.reduce((a, p) => a + p.capacity_bytes, 0);
  const free = pools.reduce((a, p) => a + p.available_bytes, 0);
  const level = node?.memory_pressure?.level;

  return (
    <div className="space-y-6 animate-in">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{node ? node.name : "Overview"}</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
            {node ? <>{node.host.cpu_model !== "x86_64" ? node.host.cpu_model : "x86-64"} · {node.host.cpus} CPUs · libvirt {node.host.libvirt_version} · Prism {node.version}</> : "Loading host…"}
          </p>
        </div>
        <Link to="/vms/new"><Button variant="primary" icon="plus">New machine</Button></Link>
      </div>

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Card>
          <Stat icon="server" label="Machines" value={<>{running.length}<span className="text-base font-normal text-zinc-400"> / {list.length}</span></>} sub="running" />
        </Card>
        <Card>
          <Stat icon="memory" label="Host memory" value={bytes(total - avail)} sub={`of ${bytes(total)} · ${bytes(avail)} free`} />
          <Meter className="mt-3" value={total - avail} max={total} />
        </Card>
        <Card>
          <Stat
            icon="gauge"
            label="Memory pressure"
            value={<span className={cx(level === "red" ? "text-red-600 dark:text-red-400" : level === "yellow" ? "text-amber-600 dark:text-amber-400" : "text-emerald-600 dark:text-emerald-400")}>{level === "red" ? "High" : level === "yellow" ? "Some" : "None"}</span>}
            sub={`${bytes(committed)} promised to running machines`}
          />
        </Card>
        <Card>
          <Stat icon="database" label="Storage" value={bytes(free)} sub={`free of ${bytes(capacity)} in ${pools.length} pool${pools.length === 1 ? "" : "s"}`} />
          <Meter className="mt-3" value={capacity - free} max={capacity} />
        </Card>
      </div>

      <div>
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-[15px] font-semibold tracking-tight">Machines</h2>
          <Link to="/vms" className="text-[13px] font-medium text-indigo-600 hover:underline dark:text-indigo-400">See all</Link>
        </div>
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {[...list].sort((a, b) => Number(b.status.state === "running") - Number(a.status.state === "running")).slice(0, 6).map((vm) => <VmCard key={vm.id} vm={vm} />)}
        </div>
      </div>

      {tasks.length > 0 && (
        <Card title="Recent activity" actions={<Link to="/tasks" className="text-[13px] font-medium text-indigo-600 hover:underline dark:text-indigo-400">All tasks</Link>}>
          <div className="-my-2 divide-y divide-zinc-100 dark:divide-zinc-800">
            {tasks.slice(0, 5).map((t) => <TaskRow key={t.id} task={t} vms={list} compact />)}
          </div>
        </Card>
      )}
    </div>
  );
}

/** The guided first run: get an image, then make a machine. */
export function FirstRun() {
  const qc = useQueryClient();
  const pools = usePools().data ?? [];
  const vms = useVms().data;
  const { data: volumes } = useAllVolumes();
  const users = volumeUsers(vms);
  const ups = useStore(uploads);
  const pool = pools[0]?.name;
  const images = volumes.filter((v) => v.kind === "disk" && !users.has(`${v.pool}/${v.name}`) && !looksLikeVmDisk(v.name));
  const isos = volumes.filter((v) => v.kind === "iso");
  const hasMedia = images.length + isos.length > 0;
  const upload = (files: File[]) => pool && files.forEach((f) => void startUpload(qc, pool, f));

  return (
    <div className="mx-auto max-w-3xl animate-in">
      {pool && <WindowDrop onFiles={upload} title="Drop to upload" hint={`Into pool ${pool}`} />}
      <div className="pt-6 pb-8 text-center sm:pt-10">
        <div className="mx-auto mb-5 grid size-16 place-items-center rounded-2xl bg-gradient-to-b from-indigo-500 to-indigo-700 shadow-lg shadow-indigo-600/25">
          <svg viewBox="0 0 32 32" className="size-9" aria-hidden="true">
            <path d="M16 5 27 25H5z" fill="#fff" opacity=".95" />
            <path d="M16 5 21 25H11z" fill="#c7d2fe" />
          </svg>
        </div>
        <h1 className="text-3xl font-semibold tracking-tight">Welcome to Prism</h1>
        <p className="mx-auto mt-2 max-w-md text-[15px] text-zinc-500 dark:text-zinc-400">
          Two steps to your first virtual machine. It takes about a minute.
        </p>
      </div>

      <ol className="space-y-4">
        <StepCard
          n={1}
          done={hasMedia}
          title="Bring an operating system"
          body={
            hasMedia ? (
              <>You have {images.length > 0 && <>{images.length} cloud image{images.length > 1 ? "s" : ""}</>}{images.length > 0 && isos.length > 0 && " and "}{isos.length > 0 && <>{isos.length} ISO{isos.length > 1 ? "s" : ""}</>} ready.</>
            ) : (
              <>Upload a <b>cloud image</b> (<span className="font-mono text-[13px]">.img</span>/<span className="font-mono text-[13px]">.qcow2</span>, boots in seconds) or an <b>installer ISO</b>. Drop it anywhere on this page.</>
            )
          }
        >
          {ups.length > 0 && <div className="mt-3 divide-y divide-zinc-100 dark:divide-zinc-800">{ups.map((u) => <UploadRow key={u.id} u={u} />)}</div>}
          {!hasMedia && !ups.some((u) => u.state === "uploading") && (
            <DropBox className="mt-4" onFiles={upload} accept=".img,.qcow2,.raw,.iso">
              <Icon name="upload" className="mx-auto mb-2 size-6 text-zinc-400" />
              <p className="text-sm font-medium">Drop an image or ISO here</p>
              <p className="mt-1 text-xs text-zinc-500">
                Tip: Ubuntu's cloud images live at <span className="font-mono">cloud-images.ubuntu.com</span>; Debian's at <span className="font-mono">cloud.debian.org/images/cloud</span>.
              </p>
            </DropBox>
          )}
          {hasMedia && (
            <div className="mt-3 flex flex-wrap gap-2">
              {[...images, ...isos].slice(0, 4).map((v) => (
                <Badge key={`${v.pool}/${v.name}`} icon={v.kind === "iso" ? "disc" : "cloud"}>{v.name} · {bytes(v.size_bytes)}</Badge>
              ))}
            </div>
          )}
        </StepCard>
        <StepCard
          n={2}
          done={false}
          active={hasMedia}
          title="Create your first machine"
          body="Pick the image, paste your SSH key, choose a size. Prism sets up the rest."
        >
          <div className="mt-4 flex flex-wrap gap-2">
            {images.length > 0 && (
              <Link to="/vms/new" search={{ source: "image", pool: images[0].pool, volume: images[0].name }}>
                <Button variant="primary" icon="cloud">From {images[0].name}</Button>
              </Link>
            )}
            {isos.length > 0 && (
              <Link to="/vms/new" search={{ source: "iso", pool: isos[0].pool, volume: isos[0].name }}>
                <Button variant={images.length ? "secondary" : "primary"} icon="disc">Install from {isos[0].name}</Button>
              </Link>
            )}
            <Link to="/vms/new" search={{}}>
              <Button variant={hasMedia ? "ghost" : "secondary"} icon="plus">{hasMedia ? "Other options" : "Create a blank machine"}</Button>
            </Link>
          </div>
        </StepCard>
      </ol>

      <div className="mt-10 grid gap-3 sm:grid-cols-3">
        <Hint icon="search" title="Jump anywhere">Press <b>/</b> or <b>Ctrl K</b> to search and run commands.</Hint>
        <Hint icon="camera" title="Undo with snapshots">Take one before a risky change; revert in seconds.</Hint>
        <Hint icon="layers" title="Memory that flexes">Automatic ballooning shares idle guest memory with the host.</Hint>
      </div>
      <p className="mt-8 text-center text-xs text-zinc-400">{pool ? <>Uploads go to pool <span className="font-mono">{pool}</span>. Manage pools in <Link to="/storage" className="underline">Storage</Link>.</> : "No storage pool yet. Create one in Storage."}</p>
    </div>
  );
}

function StepCard({ n, title, body, done, active = true, children }: { n: number; title: string; body: React.ReactNode; done: boolean; active?: boolean; children?: React.ReactNode }) {
  return (
    <li className={cx("rounded-2xl bg-white p-5 ring-1 ring-zinc-200/90 transition-opacity dark:bg-zinc-900/70 dark:ring-zinc-800", !active && "opacity-60")}>
      <div className="flex gap-4">
        <span className={cx("grid size-8 shrink-0 place-items-center rounded-full text-sm font-semibold", done ? "bg-emerald-500 text-white" : active ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-200 text-zinc-500 dark:bg-zinc-800")}>
          {done ? <Icon name="check" className="size-4" strokeWidth={3} /> : n}
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-base font-semibold tracking-tight">{title}</h2>
          <p className="mt-1 text-sm leading-relaxed text-zinc-500 dark:text-zinc-400">{body}</p>
          {children}
        </div>
      </div>
    </li>
  );
}

function Hint({ icon, title, children }: { icon: IconName; title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-xl p-4 ring-1 ring-zinc-200/80 dark:ring-zinc-800">
      <Icon name={icon} className="mb-2 size-4 text-indigo-500" />
      <p className="text-[13px] font-semibold">{title}</p>
      <p className="mt-0.5 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">{children}</p>
    </div>
  );
}
