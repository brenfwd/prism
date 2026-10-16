import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, type Vm } from "../../../api";
import { bytes } from "../../../lib/format";
import { useAllVolumes, usePools, useVms, volumeUsers } from "../../../lib/hooks";
import { Button, Field, Input, Modal, Segmented, Select } from "../../../components/ui";
import { isOn } from "../VmPage";
import { deviceChange } from "./deviceChange";

/** Dialog that adds a new empty disk, or attaches an unused volume, to the VM. */
export function AddDisk({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const qc = useQueryClient();
  const pools = usePools().data ?? [];
  const vms = useVms().data;
  const { data: volumes } = useAllVolumes();
  const users = volumeUsers(vms);
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [pool, setPool] = useState(vm.disks[0]?.pool ?? pools[0]?.name ?? "");
  const [sizeGib, setSizeGib] = useState(10);
  const free = volumes.filter((v) => v.kind === "disk" && !users.has(`${v.pool}/${v.name}`));
  const [pick, setPick] = useState<string>("");
  const chosen = free.find((v) => `${v.pool}/${v.name}` === pick) ?? free[0];
  const poolInfo = pools.find((p) => p.name === pool);
  const m = useMutation({
    mutationFn: () =>
      deviceChange(qc, mode === "new" ? `${sizeGib} GiB disk added` : `${chosen?.name} attached`, () =>
        mode === "new"
          ? api.addDisk(vm.id, { pool, size_gib: sizeGib, device: "disk" })
          : api.addDisk(vm.id, { pool: chosen!.pool, volume: chosen!.name, device: "disk" }),
      ),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title="Add a disk"
      description={isOn(vm) ? "It's hot-plugged: the running guest sees it immediately." : undefined}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={m.isPending} disabled={mode === "existing" && !chosen} onClick={() => m.mutate()}>
            {mode === "new" ? "Create and attach" : "Attach"}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <Segmented label="Disk source" value={mode} onChange={setMode} className="w-full" options={[{ value: "new", label: "New empty disk" }, { value: "existing", label: "Existing volume" }]} />
        {mode === "new" ? (
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Size (GiB)" htmlFor="nd-size" hint="Thin: only uses space as the guest writes.">
              <Input id="nd-size" type="number" min={1} value={sizeGib} onChange={(e) => setSizeGib(Math.max(1, +e.target.value))} autoFocus />
            </Field>
            <Field label="Pool" hint={poolInfo ? `${bytes(poolInfo.available_bytes)} free` : undefined}>
              <Select value={pool} onChange={(e) => setPool(e.target.value)}>
                {pools.map((p) => <option key={p.name} value={p.name}>{p.name}</option>)}
              </Select>
            </Field>
          </div>
        ) : free.length === 0 ? (
          <p className="text-sm text-zinc-500">No unattached disk volumes. <Link to="/storage" className="text-indigo-600 hover:underline dark:text-indigo-400">Upload one in Storage</Link>.</p>
        ) : (
          <Field label="Volume">
            <Select value={chosen ? `${chosen.pool}/${chosen.name}` : ""} onChange={(e) => setPick(e.target.value)}>
              {free.map((v) => (
                <option key={`${v.pool}/${v.name}`} value={`${v.pool}/${v.name}`}>
                  {v.name} · {bytes(v.size_bytes)} · {v.pool}
                </option>
              ))}
            </Select>
          </Field>
        )}
      </div>
    </Modal>
  );
}
