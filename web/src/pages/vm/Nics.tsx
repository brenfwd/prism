import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type Nic, type PortForward, type Vm } from "../../api";
import { putVm } from "../../events";
import { toast } from "../../lib/toast";
import { Button, CopyButton, ErrorText, Input, Modal, Select } from "../../components/ui";
import { isOn } from "./VmPage";

/** MAC, the addresses the guest agent reports, and any port forwards. */
export function NicSub({ vm, nic }: { vm: Vm; nic: Nic }) {
  const live = isOn(vm) && !!vm.status.agent;
  const guest = useQuery({
    queryKey: ["guest-network", vm.id],
    queryFn: () => api.guestNetwork(vm.id),
    enabled: live,
    refetchInterval: 10_000,
  }).data;
  const mine = live ? guest?.interfaces.find((i) => i.mac?.toLowerCase() === nic.mac.toLowerCase()) : undefined;
  const ips = (mine?.addrs ?? []).filter((a) => !a.addr.startsWith("fe80:"));
  const forwards = nic.network.type === "user" || nic.network.type === "network" ? (nic.network.forwards ?? []) : [];
  const anyHost = nic.network.type === "network" ? "*" : "127.0.0.1";
  return (
    <span className="flex flex-wrap items-center gap-x-3 gap-y-0.5">
      <span className="font-mono">{nic.mac}</span>
      {ips.map((a) => (
        <span key={a.addr} className="inline-flex items-center gap-1 font-mono text-indigo-600 dark:text-indigo-400" title="Reported by the guest agent">
          {a.addr}
          <CopyButton text={a.addr} />
        </span>
      ))}
      {nic.network.type === "network" && (nic.network.vlan || nic.network.trunk?.length) ? (
        <span className="text-zinc-500">
          {nic.network.trunk?.length ? `trunk ${nic.network.vlan ? `native ${nic.network.vlan}, ` : ""}tagged ${nic.network.trunk.join(",")}` : `VLAN ${nic.network.vlan}`}
        </span>
      ) : null}
      {forwards.length > 0 && (
        <span className="text-zinc-500">
          {forwards.map((f) => `${f.host_addr ?? anyHost}:${f.host_port} → ${f.guest_port}/${f.proto}`).join(", ")}
        </span>
      )}
    </span>
  );
}

const blank = (): PortForward => ({ proto: "tcp", host_port: 2222, guest_port: 22 });

/** Edits the host-to-guest port forwards of a Prism network or private-NAT adapter. */
export function Forwards({ vm, nic, onClose }: { vm: Vm; nic: Nic; onClose: () => void }) {
  const qc = useQueryClient();
  const [rows, setRows] = useState<PortForward[]>(nic.network.type === "user" || nic.network.type === "network" ? (nic.network.forwards ?? []) : []);
  const viaNetwork = nic.network.type === "network";
  const set = (i: number, patch: Partial<PortForward>) => setRows((r) => r.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const passt = useQuery({ queryKey: ["prereqs"], queryFn: api.prereqs }).data?.find((p) => p.id === "passt");
  const missing = !viaNetwork && passt && !passt.ok;
  const valid = rows.every((r) => r.host_port > 0 && r.host_port < 65536 && r.guest_port > 0 && r.guest_port < 65536);
  const save = useMutation({
    mutationFn: () => api.setForwards(vm.id, nic.id, rows),
    onSuccess: (view) => {
      putVm(qc, view);
      toast({
        tone: isOn(view) && !viaNetwork ? "warning" : "success",
        title: "Port forwards saved",
        body: isOn(view) && !viaNetwork ? "They take effect the next time the machine starts." : undefined,
      });
      onClose();
    },
    onError: (e) => toast({ tone: "error", title: "Couldn't save port forwards", body: humanError(e) }),
  });
  return (
    <Modal
      title="Port forwards"
      description={viaNetwork ? "Reach a service in this machine through this host's addresses. Applied by the network, on whichever node runs the machine, and it follows the machine when it moves." : "Reach a service in this machine from the host. By default the host port listens on loopback only."}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={save.isPending} disabled={!valid || (!!missing && rows.length > 0)} onClick={() => save.mutate()}>Save</Button>
        </>
      }
    >
      <div className="space-y-3">
        {missing && (
          <p className="rounded-md bg-amber-50 p-2.5 text-[13px] text-amber-900 dark:bg-amber-950/40 dark:text-amber-200">
            Port forwarding needs <span className="font-mono">passt</span> on this host, and it isn't installed. {passt?.hint}
          </p>
        )}
        {isOn(vm) && !missing && !viaNetwork && (
          <p className="text-[13px] text-zinc-500">This machine is running, so changes apply the next time it starts.</p>
        )}
        {rows.length === 0 && <p className="text-[13px] text-zinc-500">Nothing is forwarded. Add SSH to get started.</p>}
        {rows.map((r, i) => (
          <div key={i} className="flex flex-wrap items-center gap-2">
            <Select aria-label="Protocol" value={r.proto} onChange={(e) => set(i, { proto: e.target.value as "tcp" | "udp" })} className="w-20">
              <option value="tcp">TCP</option>
              <option value="udp">UDP</option>
            </Select>
            <Input aria-label="Host port" type="number" min={1} max={65535} value={r.host_port} onChange={(e) => set(i, { host_port: +e.target.value })} className="w-24 font-mono" />
            <span className="text-zinc-400">→</span>
            <Input aria-label="Guest port" type="number" min={1} max={65535} value={r.guest_port} onChange={(e) => set(i, { guest_port: +e.target.value })} className="w-24 font-mono" />
            {viaNetwork ? (
              <Input aria-label="Host address" placeholder="any address" value={r.host_addr ?? ""} onChange={(e) => set(i, { host_addr: e.target.value.trim() || null })} className="w-36 font-mono" />
            ) : (
              <label className="flex items-center gap-1.5 text-xs text-zinc-500">
                <input type="checkbox" checked={!!r.host_addr} onChange={(e) => set(i, { host_addr: e.target.checked ? "0.0.0.0" : null })} />
                all interfaces
              </label>
            )}
            <Button size="xs" variant="ghost" onClick={() => setRows((x) => x.filter((_, j) => j !== i))}>Remove</Button>
          </div>
        ))}
        <Button size="sm" icon="plus" onClick={() => setRows((r) => [...r, { ...blank(), host_port: 2222 + r.length }])}>Add forward</Button>
        <ErrorText error={save.error} />
      </div>
    </Modal>
  );
}
