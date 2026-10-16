import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type NetworkInfo } from "../../api";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Badge, Button, ErrorText, Field, IconButton, Input, Modal } from "../../components/ui";

const validVlan = (v: number | undefined): v is number => v !== undefined && Number.isInteger(v) && v >= 2 && v <= 4094;

/** Adds a VLAN subnet and refreshes everything that shows it. */
function useAddVlanSubnet(netId: string | undefined, onDone?: () => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vlan: number) => api.addVlanSubnet(netId ?? "", { vlan }),
    onSuccess: (n, vlan) => {
      toast({ tone: "success", title: `VLAN ${vlan} has its own subnet`, body: `${n.vlan_subnets?.[vlan]?.subnet} with a gateway and DHCP on every node.` });
      void qc.invalidateQueries({ queryKey: ["networks"] });
      void qc.invalidateQueries({ queryKey: ["vms"] });
      void qc.invalidateQueries({ queryKey: ["vlan-suggestion"] });
      onDone?.();
    },
    onError: (e) => toast({ tone: "error", title: "Couldn't add the VLAN subnet", body: humanError(e) }),
  });
}

/** The subnet Prism would give the next VLAN of `net`. */
function useSuggestion(net: NetworkInfo | undefined) {
  return useQuery({
    queryKey: ["vlan-suggestion", net?.id, Object.keys(net?.vlan_subnets ?? {}).length],
    queryFn: () => api.vlanSuggestion(net!.id),
    enabled: !!net,
  }).data?.subnet;
}

/**
 * Shown next to a VLAN number: says where its guests get their addresses, and offers a subnet
 * with one click when the VLAN has none.
 */
export function VlanSubnetOffer({ net, vlan }: { net: NetworkInfo | undefined; vlan: number | undefined }) {
  const add = useAddVlanSubnet(net?.id);
  const suggestion = useSuggestion(net);
  if (!net || !validVlan(vlan)) return null;
  const own = net.vlan_subnets?.[vlan];
  if (own) {
    return <p className="rounded-lg bg-emerald-50 px-3 py-2 text-[13px] text-emerald-800 dark:bg-emerald-950/40 dark:text-emerald-300">VLAN {vlan} has its own subnet, <span className="font-mono">{own.subnet}</span>. Guests on it get an address, gateway and DNS by DHCP.</p>;
  }
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-indigo-50 px-3 py-2 text-[13px] text-indigo-900 dark:bg-indigo-950/40 dark:text-indigo-200">
      <span>
        VLAN {vlan} has no subnet on {net.name}, so it has no gateway or DHCP.
        {suggestion && <> Prism can use <span className="font-mono">{suggestion}</span> for it.</>}
      </span>
      <Button variant="primary" size="sm" loading={add.isPending} disabled={!suggestion} onClick={() => add.mutate(vlan)}>Add subnet</Button>
    </div>
  );
}

/** The VLAN subnets of a network, with a button to add one. */
export function VlanSubnetList({ net }: { net: NetworkInfo }) {
  const qc = useQueryClient();
  const [adding, setAdding] = useState(false);
  const entries = Object.entries(net.vlan_subnets ?? {}).sort(([a], [b]) => Number(a) - Number(b));
  const remove = useMutation({
    mutationFn: (vlan: number) => api.removeVlanSubnet(net.id, vlan),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["networks"] });
      void qc.invalidateQueries({ queryKey: ["vlan-suggestion"] });
    },
    onError: (e) => toast({ tone: "error", title: "Couldn't remove the VLAN subnet", body: humanError(e) }),
  });
  return (
    <div className="mt-3 border-t border-zinc-100 pt-3 dark:border-zinc-800">
      <div className="mb-1.5 flex items-center justify-between">
        <span className="text-[13px] font-medium text-zinc-700 dark:text-zinc-300">VLANs</span>
        <Button variant="ghost" size="sm" icon="plus" onClick={() => setAdding(true)}>Add VLAN</Button>
      </div>
      {entries.length === 0 ? (
        <p className="text-[13px] text-zinc-500">No VLANs have a subnet. An adapter on a VLAN shares the network's addresses until you add one.</p>
      ) : (
        <ul className="space-y-1">
          {entries.map(([vid, v]) => (
            <li key={vid} className="flex items-center justify-between gap-2 text-sm">
              <span className="flex items-center gap-2">
                <Badge tone="indigo">VLAN {vid}</Badge>
                <span className="font-mono text-xs text-zinc-500">{v.subnet}</span>
              </span>
              <IconButton
                icon="trash"
                label={`Remove the subnet of VLAN ${vid}`}
                size="xs"
                onClick={async () => {
                  if (await confirm({ title: `Remove the subnet of VLAN ${vid}?`, body: `Adapters on VLAN ${vid} must move first. ${v.subnet} is released.`, confirmLabel: "Remove", danger: true })) remove.mutate(Number(vid));
                }}
              />
            </li>
          ))}
        </ul>
      )}
      {adding && <AddVlanModal net={net} onClose={() => setAdding(false)} />}
    </div>
  );
}

function AddVlanModal({ net, onClose }: { net: NetworkInfo; onClose: () => void }) {
  const [text, setText] = useState("");
  const vlan = text ? Number(text) : undefined;
  const suggestion = useSuggestion(net);
  const taken = validVlan(vlan) && !!net.vlan_subnets?.[vlan];
  const add = useAddVlanSubnet(net.id, onClose);
  const ok = validVlan(vlan) && !taken;
  return (
    <Modal
      title={`Add a VLAN to ${net.name}`}
      description="Each VLAN gets its own subnet, a gateway on every node, DHCP, DNS and internet access. VLANs stay separate from each other and from the default segment."
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={add.isPending} disabled={!ok || !suggestion} onClick={() => ok && add.mutate(vlan)}>
            {ok && suggestion ? `Add VLAN ${vlan} on ${suggestion}` : "Add VLAN"}
          </Button>
        </>
      }
    >
      <Field label="VLAN ID" htmlFor="vlan-new" hint="2 to 4094. Prism picks a free subnet." error={text && !validVlan(vlan) ? "Use a number from 2 to 4094." : taken ? `VLAN ${vlan} already has a subnet.` : undefined}>
        <Input id="vlan-new" inputMode="numeric" autoFocus value={text} onChange={(e) => setText(e.target.value.replace(/\D/g, ""))} placeholder="e.g. 10" className="font-mono" />
      </Field>
      <ErrorText error={add.error} />
    </Modal>
  );
}
