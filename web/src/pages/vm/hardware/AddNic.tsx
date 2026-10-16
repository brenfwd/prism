import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Vm } from "../../../api";
import { useNetworks } from "../../../lib/hooks";
import { Button, Field, Input, Modal, Segmented, Select } from "../../../components/ui";
import { VlanSubnetOffer } from "../../networks/VlanSubnets";
import { deviceChange } from "./deviceChange";

/** Dialog that adds a network adapter: Prism network (optionally on a VLAN), host bridge or NAT, with an optional MAC. */
export function AddNic({ vm, onClose }: { vm: Vm; onClose: () => void }) {
  const qc = useQueryClient();
  const nets = useNetworks().data ?? [];
  const [type, setType] = useState<"network" | "user" | "bridge">("network");
  const [netId, setNetId] = useState<string | undefined>();
  const chosen = netId ?? nets[0]?.id;
  const bridges = useQuery({ queryKey: ["bridges"], queryFn: api.bridges }).data ?? [];
  const [bridgeSel, setBridgeSel] = useState<string | undefined>();
  const bridge = bridgeSel ?? bridges.find((b) => !b.prism)?.name ?? bridges[0]?.name ?? "br0";
  const setBridge = setBridgeSel;
  const [mac, setMac] = useState("");
  const [vlanText, setVlanText] = useState("");
  const vlan = vlanText ? Number(vlanText) : undefined;
  const vlanOk = vlan === undefined || (Number.isInteger(vlan) && vlan >= 2 && vlan <= 4094);
  const macOk = !mac || /^([0-9a-f]{2}:){5}[0-9a-f]{2}$/i.test(mac);
  const m = useMutation({
    mutationFn: () =>
      deviceChange(qc, "Network adapter added", () =>
        api.addNic(vm.id, { network: type === "network" && chosen ? { type: "network", network: chosen, vlan } : type === "bridge" ? { type: "bridge", bridge } : { type: "user" }, mac: mac || undefined }),
      ),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title="Add a network adapter"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={m.isPending} disabled={!macOk || !vlanOk || (type === "bridge" && !bridge)} onClick={() => m.mutate()}>Add adapter</Button>
        </>
      }
    >
      <div className="space-y-4">
        <Segmented label="Network type" value={type} onChange={setType} className="w-full" options={[...(nets.length ? [{ value: "network" as const, label: "Prism network" }] : []), { value: "bridge" as const, label: "Host bridge" }, { value: "user" as const, label: "Private NAT" }]} />
        {type === "network" && nets.length > 0 && (
          <Field label="Network" htmlFor="nic-net" hint="The machine gets an address automatically.">
            <select id="nic-net" value={chosen} onChange={(e) => setNetId(e.target.value)} className="w-full rounded-lg bg-white px-3 py-2 text-sm ring-1 ring-zinc-300 dark:bg-zinc-900 dark:ring-zinc-700">
              {nets.map((n) => <option key={n.id} value={n.id}>{n.name} ({n.subnet})</option>)}
            </select>
          </Field>
        )}
        <p className="text-[13px] text-zinc-500">
          {type === "network" ? "Joins a Prism network: addresses, DHCP and DNS are handled for you." : type === "bridge" ? "Joins a bridge on the host, so the machine gets an address on that network and is reachable like any other computer." : "Outbound-only internet through the host. Nothing can connect in."}
        </p>
        {type === "bridge" && (
          <Field label="Host bridge" htmlFor="nic-br">
            {bridges.length > 0 ? (
              <Select id="nic-br" value={bridge} onChange={(e) => setBridge(e.target.value)}>
                {bridges.map((b) => <option key={b.name} value={b.name}>{b.name}{b.prism ? " (Prism network)" : ""}{b.up ? "" : " (down)"}{b.ports.length ? `, ${b.ports.length} port${b.ports.length === 1 ? "" : "s"}` : ""}</option>)}
              </Select>
            ) : (
              <Input id="nic-br" value={bridge} onChange={(e) => setBridge(e.target.value)} className="font-mono" autoFocus placeholder="No bridges found; type a name" />
            )}
          </Field>
        )}
        {type === "network" && nets.length > 0 && (
          <Field label="VLAN" htmlFor="nic-vlan" hint="Optional (2 to 4094). Puts this adapter on its own segment. Trunks and later changes: use the VLANs button on the adapter." error={vlanOk ? undefined : "Use a number from 2 to 4094."}>
            <Input id="nic-vlan" inputMode="numeric" value={vlanText} onChange={(e) => setVlanText(e.target.value.replace(/\D/g, ""))} placeholder="none" className="font-mono" aria-invalid={!vlanOk} />
          </Field>
        )}
        {type === "network" && nets.length > 0 && vlan !== undefined && (
          <VlanSubnetOffer net={nets.find((n) => n.id === chosen)} vlan={vlan} />
        )}
        <Field label="MAC address" htmlFor="nic-mac" hint="Leave empty to pick a random one." error={macOk ? undefined : "Format: 52:54:00:12:34:56"}>
          <Input id="nic-mac" value={mac} onChange={(e) => setMac(e.target.value.trim())} placeholder="random" className="font-mono" aria-invalid={!macOk} />
        </Field>
      </div>
    </Modal>
  );
}
