import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type ArchiveMember, type ArchivePool } from "../api";
import { bytes, plural } from "../lib/format";
import { toast } from "../lib/toast";
import { confirm } from "../components/Confirm";
import { Badge, Button, Callout, Card, EmptyState, Field, Input, Meter, Segmented } from "../components/ui";

const tone = { active: "green", draining: "amber", disabled: "neutral" } as const;

export function ArchivePage() {
  const pools = useQuery({ queryKey: ["archive-pools"], queryFn: api.archivePools, refetchInterval: 8000 });
  if (pools.isLoading) return <div className="h-40 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />;
  const list = pools.data ?? [];
  return (
    <div className="animate-in space-y-5">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Archive</h1>
        <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Shared file storage for ISOs and backups. Files are spread over member disks and copied to survive a node loss.</p>
      </div>
      {pools.isError && <Callout tone="warning" title="Archive unavailable">{humanError(pools.error)}</Callout>}
      {list.length === 0 && !pools.isError ? (
        <Card><EmptyState icon="database" title="No archive pools yet">Create one below. Add disks to it by listing an archive member in each node's prism.toml.</EmptyState></Card>
      ) : (
        list.map((p) => <PoolCard key={p.name} pool={p} />)
      )}
      <NewPool existing={list.map((p) => p.name)} />
    </div>
  );
}

function NewPool({ existing }: { existing: string[] }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("archive");
  const taken = existing.includes(name.trim());
  const [replicas, setReplicas] = useState("2");
  const create = useMutation({
    mutationFn: () => api.createArchivePool(name.trim(), Number(replicas)),
    onSuccess: () => {
      setOpen(false);
      void qc.invalidateQueries({ queryKey: ["archive-pools"] });
    },
    onError: (e) => toast({ tone: "error", title: "Could not create pool", body: humanError(e) }),
  });
  if (existing.length > 0 && !open) {
    return <div><Button variant="secondary" icon="plus" onClick={() => setOpen(true)}>Add another pool</Button></div>;
  }
  return (
    <Card title="New archive pool" description="Two copies is a good default: any one node can fail.">
      <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); create.mutate(); }}>
        <Field label="Name" htmlFor="ap-name" error={taken ? `A pool named ${name.trim()} already exists` : undefined}><Input id="ap-name" value={name} onChange={(e) => setName(e.target.value)} /></Field>
        <Field label="Copies"><Segmented label="Copies" value={replicas} onChange={setReplicas} options={[{ value: "1", label: "1" }, { value: "2", label: "2" }, { value: "3", label: "3" }]} /></Field>
        <Button type="submit" variant="primary" icon="plus" loading={create.isPending} disabled={!name.trim() || taken}>Create pool</Button>
      </form>
    </Card>
  );
}

function PoolCard({ pool }: { pool: ArchivePool }) {
  const qc = useQueryClient();
  const refresh = () => qc.invalidateQueries({ queryKey: ["archive-pools"] });
  const err = (e: unknown) => toast({ tone: "error", title: "Archive change failed", body: humanError(e) });
  const replicas = useMutation({ mutationFn: (n: number) => api.setArchiveReplicas(pool.name, n), onSuccess: refresh, onError: err });
  const task = useMutation({
    mutationFn: (a: "repair" | "scrub" | "rebalance") => api.archiveAction(pool.name, a),
    onSuccess: (_t, a) => toast({ tone: "info", title: { repair: "Repair started", scrub: "Scrub started", rebalance: "Rebalance started" }[a], body: "Progress shows in the task tray." }),
    onError: err,
  });
  return (
    <Card
      title={pool.name}
      description={`${plural(pool.files, "file")} · ${bytes(pool.used_bytes)} · ${pool.replicas} ${pool.replicas === 1 ? "copy" : "copies"}`}
      actions={
        <div className="flex flex-wrap items-center gap-2">
          <Segmented label="Copies" size="sm" value={String(pool.replicas)} onChange={(v) => replicas.mutate(Number(v))} options={[{ value: "1", label: "1x" }, { value: "2", label: "2x" }, { value: "3", label: "3x" }]} />
          <Button size="sm" loading={task.isPending && task.variables === "repair"} onClick={() => task.mutate("repair")}>Repair</Button>
          <Button size="sm" loading={task.isPending && task.variables === "scrub"} onClick={() => task.mutate("scrub")}>Scrub</Button>
          <Button size="sm" loading={task.isPending && task.variables === "rebalance"} onClick={() => task.mutate("rebalance")}>Rebalance</Button>
        </div>
      }
      padded={false}
    >
      {pool.members.length === 0 ? (
        <p className="p-4 text-[13px] text-zinc-500">No disks contribute yet. Add an [archive_members.x] section with pool = "{pool.name}" to a node's prism.toml.</p>
      ) : (
        <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
          {pool.members.map((m) => <MemberRow key={`${m.node}/${m.name}`} m={m} />)}
        </ul>
      )}
    </Card>
  );
}

function MemberRow({ m }: { m: ArchiveMember }) {
  const qc = useQueryClient();
  const refresh = () => qc.invalidateQueries({ queryKey: ["archive-pools"] });
  const err = (e: unknown) => toast({ tone: "error", title: "Archive change failed", body: humanError(e) });
  const set = useMutation({ mutationFn: (s: "active" | "disabled") => api.setArchiveMember(m.node, m.name, s), onSuccess: refresh, onError: err });
  const drain = useMutation({
    mutationFn: () => api.drainArchiveMember(m.node, m.name),
    onSuccess: () => { toast({ tone: "info", title: "Draining", body: "Files are moving to other disks." }); void refresh(); },
    onError: err,
  });
  const used = m.capacity ? m.capacity.total - m.capacity.free : 0;
  return (
    <li className="flex flex-wrap items-center gap-3 px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-medium">{m.node_name || "offline node"} / {m.name}</span>
          <Badge tone={tone[m.state]}>{m.state}</Badge>
        </div>
        {m.capacity ? (
          <div className="mt-1.5 flex items-center gap-3">
            <Meter value={used} max={m.capacity.total} className="w-40" />
            <span className="text-xs text-zinc-500">{bytes(m.capacity.free)} free of {bytes(m.capacity.total)}</span>
          </div>
        ) : (
          <div className="mt-0.5 text-xs text-zinc-500">Capacity unknown while the node is unreachable</div>
        )}
      </div>
      {m.state === "active" && (
        <>
          <Button size="sm" variant="ghost" loading={set.isPending} onClick={() => set.mutate("disabled")}>Pause</Button>
          <Button size="sm" loading={drain.isPending} onClick={async () => {
            if (await confirm({ title: `Drain ${m.name}?`, body: "Its files are copied to other disks, then it stops receiving new ones.", confirmLabel: "Drain" })) drain.mutate();
          }}>Drain</Button>
        </>
      )}
      {m.state === "disabled" && <Button size="sm" loading={set.isPending} onClick={() => set.mutate("active")}>Resume</Button>}
    </li>
  );
}
