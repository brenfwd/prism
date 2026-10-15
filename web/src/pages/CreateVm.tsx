import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { api, type CreateVm } from "../api";
import { Button, ErrorText, Field, Input, Modal, inputClass } from "../components/ui";

export function CreateVmDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const pools = useQuery({ queryKey: ["pools"], queryFn: api.pools });
  const [form, setForm] = useState({
    name: "",
    vcpus: 2,
    memory_mib: 2048,
    disk_gib: 20,
    pool: "",
    iso: "",
    firmware: "bios" as "bios" | "uefi",
    network: "user",
    bridge: "br0",
  });
  const pool = form.pool || pools.data?.[0]?.name || "";
  const volumes = useQuery({ queryKey: ["volumes", pool], queryFn: () => api.volumes(pool), enabled: !!pool });
  const isos = volumes.data?.filter((v) => v.kind === "iso") ?? [];

  const create = useMutation({
    mutationFn: (req: CreateVm) => api.createVm(req),
    onSuccess: (vm) => {
      onClose();
      navigate({ to: "/vms/$id", params: { id: vm.id } });
    },
  });

  const set = <K extends keyof typeof form>(k: K, v: (typeof form)[K]) => setForm((f) => ({ ...f, [k]: v }));

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    create.mutate({
      name: form.name.trim(),
      vcpus: form.vcpus,
      memory_mib: form.memory_mib,
      firmware: form.firmware,
      pool,
      disk_gib: form.disk_gib > 0 ? form.disk_gib : undefined,
      iso: form.iso || undefined,
      network: form.network === "bridge" ? { type: "bridge", bridge: form.bridge } : { type: "user" },
    });
  };

  return (
    <Modal title="New virtual machine" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <Field label="Name">
          <Input autoFocus required pattern="[A-Za-z0-9_][A-Za-z0-9._\-]{0,63}" value={form.name} onChange={(e) => set("name", e.target.value)} placeholder="web-1" />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="vCPUs">
            <Input type="number" min={1} max={512} value={form.vcpus} onChange={(e) => set("vcpus", +e.target.value)} />
          </Field>
          <Field label="Memory (MiB)">
            <Input type="number" min={64} step={64} value={form.memory_mib} onChange={(e) => set("memory_mib", +e.target.value)} />
          </Field>
          <Field label="Pool">
            <select className={inputClass} value={pool} onChange={(e) => set("pool", e.target.value)}>
              {pools.data?.map((p) => <option key={p.name}>{p.name}</option>)}
            </select>
          </Field>
          <Field label="Disk (GiB)" hint="0 for no disk">
            <Input type="number" min={0} value={form.disk_gib} onChange={(e) => set("disk_gib", +e.target.value)} />
          </Field>
        </div>
        <Field label="Install ISO" hint={isos.length === 0 ? "Upload ISOs on the Storage page" : undefined}>
          <select className={inputClass} value={form.iso} onChange={(e) => set("iso", e.target.value)}>
            <option value="">None</option>
            {isos.map((v) => <option key={v.name}>{v.name}</option>)}
          </select>
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Network">
            <select className={inputClass} value={form.network} onChange={(e) => set("network", e.target.value)}>
              <option value="user">User (NAT, no setup)</option>
              <option value="bridge">Host bridge</option>
            </select>
          </Field>
          {form.network === "bridge" ? (
            <Field label="Bridge">
              <Input required value={form.bridge} onChange={(e) => set("bridge", e.target.value)} />
            </Field>
          ) : (
            <Field label="Firmware">
              <select className={inputClass} value={form.firmware} onChange={(e) => set("firmware", e.target.value as "bios" | "uefi")}>
                <option value="bios">BIOS</option>
                <option value="uefi">UEFI</option>
              </select>
            </Field>
          )}
        </div>
        {form.network === "bridge" && (
          <Field label="Firmware">
            <select className={inputClass} value={form.firmware} onChange={(e) => set("firmware", e.target.value as "bios" | "uefi")}>
              <option value="bios">BIOS</option>
              <option value="uefi">UEFI</option>
            </select>
          </Field>
        )}
        <ErrorText error={create.error} />
        <div className="flex justify-end gap-2 pt-2">
          <Button type="button" variant="ghost" onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={create.isPending || !pool}>
            {create.isPending ? "Creating…" : "Create"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
