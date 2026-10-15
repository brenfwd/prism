import { useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { api, humanError, type Pool, type Vm, type Volume } from "../api";
import { bytes } from "../lib/format";
import { looksLikeVmDisk, usePools, useVms, useVolumes, volumeUsers } from "../lib/hooks";
import { pickFiles } from "../lib/overlays";
import { startUpload, uploads } from "../lib/tasks";
import { useStore } from "../lib/store";
import { toast } from "../lib/toast";
import { applyTask } from "../lib/tasks";
import { confirm } from "../components/Confirm";
import { WindowDrop } from "../components/DropZone";
import { Icon, type IconName } from "../components/icons";
import { UploadRow } from "../components/TaskTray";
import { Badge, Button, Card, EmptyState, ErrorText, Field, IconButton, Input, Menu, Meter, Modal, Segmented, cx } from "../components/ui";

type Kind = "image" | "iso" | "vmdisk" | "loose" | "layer" | "file";
type Filter = "all" | "image" | "iso" | "vmdisk";

const kindMeta: Record<Kind, { label: string; icon: IconName; tone: "indigo" | "sky" | "neutral" | "amber" }> = {
  image: { label: "Disk image", icon: "cloud", tone: "indigo" },
  iso: { label: "ISO", icon: "disc", tone: "sky" },
  vmdisk: { label: "VM disk", icon: "disk", tone: "neutral" },
  loose: { label: "Unattached disk", icon: "disk", tone: "amber" },
  layer: { label: "Snapshot layer", icon: "layers", tone: "neutral" },
  file: { label: "File", icon: "file", tone: "neutral" },
};

function classify(v: Volume, pool: string, users: Map<string, Vm[]>): Kind {
  if (v.kind === "iso") return "iso";
  if (v.kind === "file") return "file";
  const used = users.get(`${pool}/${v.name}`);
  if (!used) return looksLikeVmDisk(v.name) ? "loose" : "image";
  const asLayer = used.every((vm) => vm.disks.every((d) => d.pool !== pool || d.volume !== v.name));
  return asLayer ? "layer" : "vmdisk";
}

export function StoragePage({ pool: wanted }: { pool?: string }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const pools = usePools();
  const [creating, setCreating] = useState(false);
  const [fromUrl, setFromUrl] = useState(false);
  const list = pools.data ?? [];
  const pool = list.find((p) => p.name === wanted) ?? list[0];
  const upload = (files: File[]) => {
    if (!pool) return;
    for (const f of files) void startUpload(qc, pool.name, f);
  };

  if (pools.isLoading) return <div className="h-40 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />;

  return (
    <div className="animate-in">
      {pool && <WindowDrop onFiles={upload} title={`Drop to upload into ${pool.name}`} hint="ISOs, cloud images (.img, .qcow2) and raw disks" />}
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Storage</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Images, ISOs and machine disks. Drop files anywhere on this page to upload.</p>
        </div>
        <div className="flex gap-2">
          <Button icon="plus" onClick={() => setCreating(true)}>New pool</Button>
          {pool && <Button icon="cloud" onClick={() => setFromUrl(true)}>From URL</Button>}
          {pool && <Button variant="primary" icon="upload" onClick={async () => upload(await pickFiles(".iso,.img,.qcow2,.raw"))}>Upload</Button>}
        </div>
      </div>

      {list.length === 0 ? (
        <Card>
          <EmptyState icon="database" title="No storage pools" actions={<Button variant="primary" icon="plus" onClick={() => setCreating(true)}>Create a pool</Button>}>
            A pool is a directory on this host where Prism keeps disks and images.
          </EmptyState>
        </Card>
      ) : (
        <>
          {list.length > 1 && (
            <div role="tablist" aria-label="Pools" className="scrollbar-thin -mx-1 mb-5 flex gap-3 overflow-x-auto px-1 pb-1">
              {list.map((p) => (
                <PoolTab key={p.name} pool={p} active={p.name === pool?.name} onClick={() => navigate({ to: "/storage", search: { pool: p.name }, replace: true })} />
              ))}
            </div>
          )}
          {pool && <PoolView pool={pool} single={list.length === 1} onUpload={upload} />}
        </>
      )}
      {fromUrl && pool && <FromUrl pool={pool} onClose={() => setFromUrl(false)} />}
      {creating && <CreatePool onClose={() => setCreating(false)} />}
    </div>
  );
}

function PoolTab({ pool, active, onClick }: { pool: Pool; active: boolean; onClick: () => void }) {
  return (
    <button
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={cx(
        "w-56 shrink-0 rounded-xl p-3.5 text-left ring-1 transition-all ring-inset",
        active ? "bg-white ring-2 ring-indigo-500 dark:bg-zinc-900 dark:ring-indigo-400" : "bg-white/50 ring-zinc-200 hover:bg-white dark:bg-zinc-900/40 dark:ring-zinc-800 dark:hover:bg-zinc-900",
      )}
    >
      <div className="mb-2 flex items-center justify-between">
        <span className="font-semibold">{pool.name}</span>
        <span className="text-[11px] text-zinc-500">{pool.format}</span>
      </div>
      <Meter value={pool.capacity_bytes - pool.available_bytes} max={pool.capacity_bytes} />
      <div className="mt-1.5 text-xs text-zinc-500 tabular-nums">{bytes(pool.available_bytes)} free of {bytes(pool.capacity_bytes)}</div>
    </button>
  );
}

function PoolView({ pool, single, onUpload }: { pool: Pool; single: boolean; onUpload: (f: File[]) => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const volumes = useVolumes(pool.name);
  const vms = useVms().data;
  const users = useMemo(() => volumeUsers(vms), [vms]);
  const ups = useStore(uploads).filter((u) => u.pool === pool.name);
  const [filter, setFilter] = useState<Filter>("all");
  const [newDisk, setNewDisk] = useState(false);

  const rows = useMemo(
    () =>
      (volumes.data ?? [])
        .map((v) => ({ v, kind: classify(v, pool.name, users), users: users.get(`${pool.name}/${v.name}`) ?? [] }))
        .sort((a, b) => order(a.kind) - order(b.kind) || a.v.name.localeCompare(b.v.name)),
    [volumes.data, vms, users, pool.name],
  );
  const shown = rows.filter((r) => filter === "all" || r.kind === filter || (filter === "vmdisk" && (r.kind === "layer" || r.kind === "loose")));
  const counts = { image: rows.filter((r) => r.kind === "image").length, iso: rows.filter((r) => r.kind === "iso").length, vmdisk: rows.filter((r) => r.kind === "vmdisk" || r.kind === "layer" || r.kind === "loose").length };

  const del = async (v: Volume) => {
    if (!(await confirm({ title: `Delete ${v.name}?`, body: `${bytes(v.allocated_bytes)} is freed in ${pool.name}. This can't be undone.`, confirmLabel: "Delete", danger: true }))) return;
    try {
      await api.deleteVolume(pool.name, v.name);
      qc.setQueryData<Volume[]>(["volumes", pool.name], (vs) => vs?.filter((x) => x.name !== v.name));
      toast({ tone: "success", title: `${v.name} deleted` });
    } catch (e) {
      toast({ tone: "error", title: `Couldn't delete ${v.name}`, body: humanError(e) });
    }
  };
  const removePool = async () => {
    if (!(await confirm({ title: `Remove pool ${pool.name}?`, body: "Prism forgets the pool. Files stay on disk in " + pool.path + ".", confirmLabel: "Remove pool", danger: true }))) return;
    try {
      await api.deletePool(pool.name);
      qc.invalidateQueries({ queryKey: ["pools"] });
      navigate({ to: "/storage", search: {} });
    } catch (e) {
      toast({ tone: "error", title: "Couldn't remove the pool", body: humanError(e) });
    }
  };

  return (
    <Card
      padded={false}
      title={
        <span className="flex items-center gap-2">
          {pool.name}
          <span className="font-mono text-xs font-normal text-zinc-400">{pool.path}</span>
        </span>
      }
      description={single ? <span className="tabular-nums">{bytes(pool.available_bytes)} free of {bytes(pool.capacity_bytes)} · new disks are {pool.format}</span> : undefined}
      actions={
        <>
          <Button size="sm" icon="plus" onClick={() => setNewDisk(true)}>New disk</Button>
          {!pool.static_config && (
            <Menu label="Pool actions" items={[{ label: "Remove pool…", icon: "trash", danger: true, onSelect: removePool }]} trigger={(p) => <IconButton icon="more" label="Pool actions" {...p} />} />
          )}
        </>
      }
    >
      {single && <div className="px-5 pt-3"><Meter value={pool.capacity_bytes - pool.available_bytes} max={pool.capacity_bytes} /></div>}
      <div className="flex flex-wrap items-center gap-3 px-5 pt-4 pb-3">
        <Segmented
          label="Show"
          size="sm"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: `All ${rows.length}` },
            { value: "image", label: `Images ${counts.image}` },
            { value: "iso", label: `ISOs ${counts.iso}` },
            { value: "vmdisk", label: `Machine disks ${counts.vmdisk}` },
          ]}
        />
      </div>
      {ups.length > 0 && <div className="divide-y divide-zinc-100 border-t border-zinc-100 px-5 dark:divide-zinc-800 dark:border-zinc-800">{ups.map((u) => <UploadRow key={u.id} u={u} />)}</div>}
      {volumes.isLoading ? (
        <div className="space-y-2 border-t border-zinc-100 p-5 dark:border-zinc-800">{[0, 1, 2].map((i) => <div key={i} className="h-10 animate-pulse rounded bg-zinc-100 dark:bg-zinc-800/60" />)}</div>
      ) : rows.length === 0 ? (
        <div className="border-t border-zinc-100 dark:border-zinc-800">
          <EmptyState icon="upload" title="This pool is empty" actions={<Button variant="primary" icon="upload" onClick={async () => onUpload(await pickFiles(".iso,.img,.qcow2,.raw"))}>Choose files</Button>}>
            Drop an installer ISO or a cloud image anywhere on this page. Cloud images boot in seconds and are set up for you.
          </EmptyState>
        </div>
      ) : shown.length === 0 ? (
        <p className="border-t border-zinc-100 px-5 py-8 text-center text-sm text-zinc-500 dark:border-zinc-800">Nothing of this kind here.</p>
      ) : (
        <ul className="divide-y divide-zinc-100 border-t border-zinc-100 dark:divide-zinc-800 dark:border-zinc-800">
          {shown.map(({ v, kind, users: us }) => {
            const m = kindMeta[kind];
            return (
              <li key={v.name} className="flex flex-wrap items-center gap-x-4 gap-y-2 px-5 py-3 sm:flex-nowrap">
                <span className={cx("grid size-9 shrink-0 place-items-center rounded-lg", kind === "image" ? "bg-indigo-50 text-indigo-600 dark:bg-indigo-500/10 dark:text-indigo-300" : kind === "iso" ? "bg-sky-50 text-sky-600 dark:bg-sky-500/10 dark:text-sky-300" : "bg-zinc-100 text-zinc-500 dark:bg-zinc-800")}>
                  <Icon name={m.icon} className="size-[18px]" />
                </span>
                <div className="min-w-0 flex-1 basis-48">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-mono text-[13px] font-medium" title={v.name}>{v.name}</span>
                    <Badge tone={m.tone}>{m.label}</Badge>
                  </div>
                  <div className="mt-0.5 truncate text-xs text-zinc-500 tabular-nums">
                    {bytes(v.size_bytes)}
                    {v.allocated_bytes !== v.size_bytes && <> · {bytes(v.allocated_bytes)} on disk</>}
                    {us.length > 0 && (
                      <>
                        {" · "}
                        {kind === "layer" ? "history of " : "used by "}
                        {us.map((vm, i) => (
                          <span key={vm.id}>
                            {i > 0 && ", "}
                            <Link to="/vms/$id" params={{ id: vm.id }} className="text-indigo-600 hover:underline dark:text-indigo-400">{vm.name}</Link>
                          </span>
                        ))}
                      </>
                    )}
                  </div>
                </div>
                <div className="ml-auto flex items-center gap-1.5">
                  {kind === "image" && (
                    <Link to="/vms/new" search={{ source: "image", pool: pool.name, volume: v.name }}>
                      <Button size="xs" variant="secondary" icon="cloud">Use as cloud image</Button>
                    </Link>
                  )}
                  {kind === "iso" && (
                    <Link to="/vms/new" search={{ source: "iso", pool: pool.name, volume: v.name }}>
                      <Button size="xs" variant="secondary" icon="play">Boot from this ISO</Button>
                    </Link>
                  )}
                  <IconButton
                    icon="trash"
                    size="xs"
                    label={us.length ? `In use by ${us.map((x) => x.name).join(", ")}` : `Delete ${v.name}`}
                    disabled={us.length > 0}
                    onClick={() => del(v)}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {newDisk && <NewDisk pool={pool} onClose={() => setNewDisk(false)} />}
    </Card>
  );
}

const order = (k: Kind) => ({ image: 0, iso: 1, vmdisk: 2, loose: 3, layer: 4, file: 5 })[k];

function NewDisk({ pool, onClose }: { pool: Pool; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [size, setSize] = useState(10);
  const file = name && !/\.(qcow2|raw|img)$/.test(name) ? `${name}.${pool.format}` : name;
  const create = useMutation({
    mutationFn: () => api.createVolume(pool.name, file, size, pool.format),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["volumes", pool.name] });
      toast({ tone: "success", title: `${file} created` });
      onClose();
    },
  });
  return (
    <Modal
      title={`New empty disk in ${pool.name}`}
      description="Attach it to a machine from its Hardware tab."
      onClose={onClose}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={create.isPending} disabled={!name} onClick={() => create.mutate()}>Create</Button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={(e) => (e.preventDefault(), name && create.mutate())}>
        <Field label="Name" htmlFor="nd-name" hint={name ? <>Saved as <span className="font-mono">{file}</span></> : undefined}>
          <Input id="nd-name" autoFocus value={name} onChange={(e) => setName(e.target.value.replace(/\s/g, "-"))} placeholder="data" />
        </Field>
        <Field label="Size (GiB)" htmlFor="nd-size" hint={`Thin: takes space only as it fills. ${bytes(pool.available_bytes)} free.`}>
          <Input id="nd-size" type="number" min={1} value={size} onChange={(e) => setSize(Math.max(1, +e.target.value))} />
        </Field>
        <ErrorText error={create.error} />
      </form>
    </Modal>
  );
}

function CreatePool({ onClose }: { onClose: () => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [format, setFormat] = useState<"qcow2" | "raw">("qcow2");
  const create = useMutation({
    mutationFn: () => api.createPool(name, format),
    onSuccess: (p) => {
      qc.invalidateQueries({ queryKey: ["pools"] });
      toast({ tone: "success", title: `Pool ${p.name} created`, body: p.path });
      navigate({ to: "/storage", search: { pool: p.name } });
      onClose();
    },
  });
  return (
    <Modal
      title="New storage pool"
      description="A directory on this host for disks and images, created under the configured pool root."
      onClose={onClose}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={create.isPending} disabled={!name} onClick={() => create.mutate()}>Create pool</Button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={(e) => (e.preventDefault(), name && create.mutate())}>
        <Field label="Name" htmlFor="pool-name">
          <Input id="pool-name" autoFocus value={name} onChange={(e) => setName(e.target.value.replace(/[^A-Za-z0-9._-]/g, ""))} placeholder="fast" />
        </Field>
        <Field label="Disk format" hint={format === "qcow2" ? "Thin, supports snapshots. The right choice almost always." : "Fully allocated files; a little faster, no snapshots of disk layers."}>
          <Segmented label="Disk format" value={format} onChange={setFormat} options={[{ value: "qcow2", label: "qcow2" }, { value: "raw", label: "raw" }]} />
        </Field>
        <ErrorText error={create.error} />
      </form>
    </Modal>
  );
}

function FromUrl({ pool, onClose }: { pool: Pool; onClose: () => void }) {
  const qc = useQueryClient();
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [sha, setSha] = useState("");
  const guess = url.split(/[?#]/)[0].split("/").pop() ?? "";
  const start = useMutation({
    mutationFn: () => api.downloadVolume(pool.name, url.trim(), name || undefined, sha.trim()),
    onSuccess: (t) => {
      applyTask(qc, t);
      toast({ tone: "info", title: "Download started", body: "It continues in the background; progress is in the task tray." });
      onClose();
    },
  });
  const ok = /^https?:\/\/\S+$/.test(url.trim()) && (sha.trim() === "" || /^[0-9a-fA-F]{64}$/.test(sha.trim()));
  return (
    <Modal
      title={`Download into ${pool.name}`}
      description="Prism fetches the file itself, so nothing passes through your browser."
      onClose={onClose}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" icon="cloud" loading={start.isPending} disabled={!ok} onClick={() => start.mutate()}>Download</Button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={(e) => (e.preventDefault(), ok && start.mutate())}>
        <Field label="Link to an ISO or cloud image" htmlFor="dl-url">
          <Input id="dl-url" autoFocus value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://cloud-images.ubuntu.com/…/server-cloudimg-amd64.img" />
        </Field>
        <Field label="Save as" htmlFor="dl-name" hint={guess ? "Leave empty to use the name from the link." : "The link has no file name, so choose one."}>
          <Input id="dl-name" value={name} onChange={(e) => setName(e.target.value.replace(/\s/g, "-"))} placeholder={guess || "image.iso"} />
        </Field>
        <Field label="SHA-256 checksum (optional)" htmlFor="dl-sha" hint="Paste it from the download page and Prism rejects a corrupted or tampered file.">
          <Input id="dl-sha" value={sha} onChange={(e) => setSha(e.target.value.trim())} placeholder="64 hex digits" className="font-mono" />
        </Field>
        <ErrorText error={start.error} />
      </form>
    </Modal>
  );
}
