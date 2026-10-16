import { Icon } from "../../components/icons";
import { Card, Field, Input } from "../../components/ui";
import { Choice, Step } from "./parts";
import type { NewVmForm } from "./useNewVmForm";

/** The last numbered step: Prism network, private NAT or a host bridge. */
export function NetworkStep({ form, n }: { form: NewVmForm; n: number }) {
  const { f, set, networks, netKind, netId } = form;
  return (
    <Card title={<Step n={n}>Network</Step>}>
      <div role="radiogroup" aria-label="Network" className="grid gap-2.5 sm:grid-cols-2">
        {networks.length > 0 && <Choice active={netKind === "network"} onClick={() => set({ net: "network" })} icon="network" title="Prism network" body="Addresses, DHCP and DNS handled for you. Machines on it can talk to each other." />}
        <Choice active={netKind === "user"} onClick={() => set({ net: "user" })} icon="cloud" title="Private (NAT)" body="Reaches the internet through the host. Works everywhere; not reachable from your LAN." />
        <Choice active={netKind === "bridge"} onClick={() => set({ net: "bridge" })} icon="network" title="Bridged to a LAN" body="Joins a host bridge and gets an address from that network, like a physical box." />
      </div>
      {netKind === "network" && networks.length > 1 && (
        <Field className="mt-4 max-w-xs" label="Network" htmlFor="pnet">
          <select id="pnet" value={netId} onChange={(e) => set({ netChoice: e.target.value })} className="w-full rounded-lg bg-white px-3 py-2 text-sm ring-1 ring-zinc-300 dark:bg-zinc-900 dark:ring-zinc-700">
            {networks.map((n) => <option key={n.id} value={n.id}>{n.name} ({n.subnet})</option>)}
          </select>
        </Field>
      )}
      {netKind === "user" && f.source === "image" && (
        <p className="mt-3 flex items-start gap-1.5 text-xs leading-relaxed text-zinc-500">
          <Icon name="info" className="mt-0.5 size-3.5 shrink-0" />
          With private NAT you sign in through the Serial or Console tab. Pick Bridged if you want to SSH in from your network.
        </p>
      )}
      {netKind === "bridge" && (
        <Field className="mt-4 max-w-xs" label="Host bridge" htmlFor="br" hint="The bridge interface on this host, e.g. br0.">
          <Input id="br" value={f.bridge} onChange={(e) => set({ bridge: e.target.value })} spellCheck={false} className="font-mono" />
        </Field>
      )}
    </Card>
  );
}
