import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, uploadVolume } from "../api";
import { bytes } from "../format";
import { Button, Card, ErrorText, Field, Input, Meter, Modal } from "../components/ui";

export function StoragePage() {
  const pools = useQuery({ queryKey: ["pools"], queryFn: api.pools });
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const pool = selected ?? pools.data?.[0]?.name ?? null;
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold">Storage</h1>
        <Button onClick={() => setCreating(true)}>New pool</Button>
      </div>
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <Card title="Pools">
          <ul className="-m-2 space-y-1">
            {pools.data?.map((p) => (
              <li key={p.name}>
                <button
                  onClick={() => setSelected(p.name)}
                  className={`w-full rounded-md p-2 text-left text-sm ${pool === p.name ? "bg-indigo-50 dark:bg-indigo-950/40" : "hover:bg-zinc-100 dark:hover:bg-zinc-800"}`}
                >
                  <div className="mb-1 flex justify-between">
                    <span className="font-medium">{p.name}</span>
                    <span className="text-xs text-zinc-500">{p.type} · {p.format}</span>
                  </div>
                  <Meter value={p.capacity_bytes - p.available_bytes} max={p.capacity_bytes} />
                  <div className="mt-1 text-xs text-zinc-500 tabular-nums">{bytes(p.available_bytes)} free</div>
                </button>
              </li>
            ))}
          </ul>
        </Card>
        {pool && <PoolVolumes pool={pool} onDeleted={() => setSelected(null)} />}
      </div>
      {creating && <CreatePool onClose={() => setCreating(false)} />}
    </div>
  );
}

function PoolVolumes({ pool, onDeleted }: { pool: string; onDeleted: () => void }) {
  const qc = useQueryClient();
  const info = useQuery({ queryKey: ["pools"], queryFn: api.pools, select: (ps) => ps.find((p) => p.name === pool) });
  const volumes = useQuery({ queryKey: ["volumes", pool], queryFn: () => api.volumes(pool) });
  const file = useRef<HTMLInputElement>(null);
  const [upload, setUpload] = useState<{ name: string; progress: number; abort: () => void } | null>(null);
  const [uploadError, setUploadError] = useState<unknown>(null);
  const [newDisk, setNewDisk] = useState(false);
  const del = useMutation({
    mutationFn: (name: string) => api.deleteVolume(pool, name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["volumes", pool] }),
  });
  const delPool = useMutation({ mutationFn: () => api.deletePool(pool), onSuccess: onDeleted });

  const startUpload = (f: File) => {
    setUploadError(null);
    const u = uploadVolume(pool, f, (p) => setUpload((cur) => (cur ? { ...cur, progress: p } : cur)));
    setUpload({ name: f.name, progress: 0, abort: u.abort });
    u.promise
      .then(() => qc.invalidateQueries({ queryKey: ["volumes", pool] }))
      .catch(setUploadError)
      .finally(() => setUpload(null));
  };

  return (
    <Card
      title={<span>{pool} <span className="font-normal text-zinc-500">{info.data?.path}</span></span>}
      actions={
        <>
          <input ref={file} type="file" className="hidden" onChange={(e) => e.target.files?.[0] && startUpload(e.target.files[0])} />
          <Button className="px-2 py-1 text-xs" disabled={!!upload} onClick={() => file.current?.click()}>Upload</Button>
          <Button className="px-2 py-1 text-xs" onClick={() => setNewDisk(true)}>New disk</Button>
          {info.data && !info.data.static_config && (
            <Button className="px-2 py-1 text-xs" variant="ghost" onClick={() => confirm(`Remove pool ${pool}? Files stay on disk.`) && delPool.mutate()}>
              Remove
            </Button>
          )}
        </>
      }
    >
      {upload && (
        <div className="mb-4 rounded-md bg-zinc-100 p-3 text-sm dark:bg-zinc-800">
          <div className="mb-2 flex justify-between">
            <span>Uploading {upload.name}</span>
            <span className="tabular-nums">{Math.round(upload.progress * 100)}%</span>
          </div>
          <Meter value={upload.progress} max={1} />
          <Button className="mt-2 px-2 py-1 text-xs" variant="ghost" onClick={upload.abort}>Cancel</Button>
        </div>
      )}
      <ErrorText error={uploadError || del.error || delPool.error} />
      <div className="-mx-4 -mb-4 overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="text-left text-xs text-zinc-500">
            <tr className="border-b border-zinc-200 dark:border-zinc-800">
              <th className="px-4 py-2 font-medium">Volume</th>
              <th className="px-4 py-2 font-medium">Kind</th>
              <th className="px-4 py-2 text-right font-medium">Size</th>
              <th className="px-4 py-2 text-right font-medium">On disk</th>
              <th className="px-4 py-2" />
            </tr>
          </thead>
          <tbody>
            {volumes.data?.map((v) => (
              <tr key={v.name} className="border-b border-zinc-100 last:border-0 dark:border-zinc-800/60">
                <td className="px-4 py-2 font-mono text-xs">{v.name}</td>
                <td className="px-4 py-2 text-xs">{v.kind}</td>
                <td className="px-4 py-2 text-right tabular-nums">{bytes(v.size_bytes)}</td>
                <td className="px-4 py-2 text-right tabular-nums text-zinc-500">{bytes(v.allocated_bytes)}</td>
                <td className="px-4 py-2 text-right">
                  <Button className="px-2 py-1 text-xs" variant="ghost" onClick={() => confirm(`Delete ${v.name}?`) && del.mutate(v.name)}>
                    Delete
                  </Button>
                </td>
              </tr>
            ))}
            {volumes.data?.length === 0 && (
              <tr><td colSpan={5} className="px-4 py-6 text-center text-zinc-500">Empty. Upload an ISO to install from it.</td></tr>
            )}
          </tbody>
        </table>
      </div>
      {newDisk && <NewDisk pool={pool} onClose={() => setNewDisk(false)} />}
    </Card>
  );
}

function NewDisk({ pool, onClose }: { pool: string; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [size, setSize] = useState(10);
  const create = useMutation({
    mutationFn: () => api.createVolume(pool, name, size),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["volumes", pool] });
      onClose();
    },
  });
  return (
    <Modal title={`New disk in ${pool}`} onClose={onClose}>
      <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); create.mutate(); }}>
        <Field label="Name"><Input autoFocus required value={name} onChange={(e) => setName(e.target.value)} placeholder="data.qcow2" /></Field>
        <Field label="Size (GiB)"><Input type="number" min={1} value={size} onChange={(e) => setSize(+e.target.value)} /></Field>
        <ErrorText error={create.error} />
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={create.isPending}>Create</Button>
        </div>
      </form>
    </Modal>
  );
}

function CreatePool({ onClose }: { onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [format, setFormat] = useState("qcow2");
  const create = useMutation({
    mutationFn: () => api.createPool(name, format),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["pools"] });
      onClose();
    },
  });
  return (
    <Modal title="New directory pool" onClose={onClose}>
      <form className="space-y-4" onSubmit={(e) => { e.preventDefault(); create.mutate(); }}>
        <Field label="Name" hint="Created under the first directory in storage.pool_roots">
          <Input autoFocus required value={name} onChange={(e) => setName(e.target.value)} placeholder="fast" />
        </Field>
        <Field label="Default disk format">
          <select className="block w-full rounded-md bg-white px-2.5 py-1.5 text-sm ring-1 ring-zinc-300 dark:bg-zinc-950 dark:ring-zinc-700" value={format} onChange={(e) => setFormat(e.target.value)}>
            <option>qcow2</option>
            <option>raw</option>
          </select>
        </Field>
        <ErrorText error={create.error} />
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={create.isPending}>Create</Button>
        </div>
      </form>
    </Modal>
  );
}
