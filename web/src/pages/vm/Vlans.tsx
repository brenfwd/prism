import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type Nic, type Vm } from "../../api";
import { putVm } from "../../events";
import { toast } from "../../lib/toast";
import { useNetworks } from "../../lib/hooks";
import { VlanSubnetOffer } from "../networks/VlanSubnets";
import { Button, ErrorText, Field, Input, Modal, Segmented } from "../../components/ui";

/** Parses "10, 20, 30-32" into VLAN ids; null when malformed. */
export function parseVlans(text: string): number[] | null {
  const out = new Set<number>();
  for (const part of text.split(/[\s,]+/).filter(Boolean)) {
    const m = /^(\d+)(?:-(\d+))?$/.exec(part);
    if (!m) return null;
    const lo = +m[1];
    const hi = m[2] ? +m[2] : lo;
    if (lo < 2 || hi > 4094 || hi < lo || hi - lo > 512) return null;
    for (let v = lo; v <= hi; v++) out.add(v);
  }
  return [...out].sort((a, b) => a - b);
}

/** Edits a Prism network adapter's VLANs; applied to a running machine at once. */
export function VlanEditor({ vm, nic, onClose }: { vm: Vm; nic: Nic; onClose: () => void }) {
  const qc = useQueryClient();
  const cur = nic.network.type === "network" ? nic.network : undefined;
  const net = useNetworks().data?.find((n) => n.id === cur?.network);
  const [mode, setMode] = useState<"none" | "access" | "trunk">(cur?.trunk?.length ? "trunk" : cur?.vlan ? "access" : "none");
  const [vlanText, setVlanText] = useState(cur?.vlan ? String(cur.vlan) : "");
  const [trunkText, setTrunkText] = useState((cur?.trunk ?? []).join(", "));
  const vlan = vlanText ? Number(vlanText) : undefined;
  const vlanOk = vlan === undefined || (vlan >= 2 && vlan <= 4094);
  const trunk = mode === "trunk" ? parseVlans(trunkText) : [];
  const valid = vlanOk && trunk !== null && (mode !== "access" || vlan !== undefined) && (mode !== "trunk" || trunk.length > 0) && !(vlan !== undefined && mode === "trunk" && trunk?.includes(vlan));
  const save = useMutation({
    mutationFn: () => api.setVlans(vm.id, nic.id, { vlan: mode === "none" ? null : vlan ?? null, trunk: trunk ?? [] }),
    onSuccess: (view) => {
      putVm(qc, view);
      toast({ tone: "success", title: "VLANs updated" });
      onClose();
    },
    onError: (e) => toast({ tone: "error", title: "Couldn't update VLANs", body: humanError(e) }),
  });
  return (
    <Modal
      title="VLANs"
      description="Split this network into separate segments. Changes apply to a running machine immediately."
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={save.isPending} disabled={!valid} onClick={() => save.mutate()}>Save</Button>
        </>
      }
    >
      <div className="space-y-4">
        <Segmented
          label="VLAN mode"
          value={mode}
          onChange={setMode}
          className="w-full"
          options={[
            { value: "none" as const, label: "Default" },
            { value: "access" as const, label: "One VLAN" },
            { value: "trunk" as const, label: "Trunk" },
          ]}
        />
        <p className="text-[13px] text-zinc-500">
          {mode === "none" && "On the network's default segment: the gateway, DHCP and DNS just work."}
          {mode === "access" && "Only sees adapters on the same VLAN. With a subnet for the VLAN, the guest gets its address, gateway and DNS by DHCP; without one it needs a static address."}
          {mode === "trunk" && "Receives several VLANs tagged (802.1Q), for a guest that runs its own VLAN interfaces, a router or a hypervisor. Untagged traffic stays on the native VLAN."}
        </p>
        {mode !== "none" && (
          <Field label={mode === "trunk" ? "Native VLAN" : "VLAN"} htmlFor="vlan-id" hint={mode === "trunk" ? "Optional. Leave empty to keep the default segment untagged, with DHCP." : "2 to 4094."} error={vlanOk ? undefined : "Use a number from 2 to 4094."}>
            <Input id="vlan-id" inputMode="numeric" value={vlanText} onChange={(e) => setVlanText(e.target.value.replace(/\D/g, ""))} placeholder={mode === "trunk" ? "default" : "e.g. 10"} className="font-mono" aria-invalid={!vlanOk} />
          </Field>
        )}
        {mode !== "none" && <VlanSubnetOffer net={net} vlan={vlan} />}
        {mode === "trunk" && (
          <Field label="Tagged VLANs" htmlFor="vlan-trunk" hint="Comma-separated, ranges allowed: 10, 20, 30-32." error={trunk === null ? "Use numbers 2 to 4094, like 10, 20, 30-32." : undefined}>
            <Input id="vlan-trunk" value={trunkText} onChange={(e) => setTrunkText(e.target.value)} placeholder="10, 20" className="font-mono" aria-invalid={trunk === null} />
          </Field>
        )}
        <ErrorText error={save.error} />
      </div>
    </Modal>
  );
}
