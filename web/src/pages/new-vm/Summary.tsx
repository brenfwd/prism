import { Button, Card, Checkbox, ErrorText, Kbd, modKey } from "../../components/ui";
import { memLabel } from "../../lib/format";
import { sshKeyLines } from "../../lib/cloudinit";
import { CapacityBar } from "./CapacityBar";
import { SumRow } from "./parts";
import type { NewVmForm } from "./useNewVmForm";

/** The sticky side card: what will be created, host and pool capacity, and the create button. */
export function Summary({ form }: { form: NewVmForm }) {
  const { f, set, size, cap, node, avail, poolInfo, blockers, create, sourceVol } = form;
  const networkName = form.netKind === "network" ? (form.networks.find((n) => n.id === form.netId)?.name ?? "Prism network") : form.netKind === "user" ? "Private NAT" : f.bridge;
  const signIn = f.raw != null ? "Custom" : `${f.username || "?"}${sshKeyLines(f.sshKeys).length ? " · key" : ""}${f.password ? " · password" : ""}`;
  return (
    <Card className="overflow-hidden" padded={false}>
      <div className="border-b border-zinc-100 bg-gradient-to-b from-indigo-50/80 to-transparent px-5 pt-5 pb-4 dark:border-zinc-800 dark:from-indigo-500/10">
        <div className="text-xs font-medium text-zinc-500">Summary</div>
        <div className="mt-1 truncate text-lg font-semibold tracking-tight">{f.name || "Unnamed"}</div>
        <div className="mt-0.5 truncate text-[13px] text-zinc-500">
          {f.source === "blank" ? "Blank disk" : sourceVol ? sourceVol.name : f.source === "image" ? "No image picked" : "No ISO picked"}
        </div>
      </div>
      <dl className="space-y-2.5 px-5 py-4 text-[13px]">
        <SumRow icon="cpu" k="vCPUs" v={size.vcpus} />
        <SumRow icon="memory" k="Memory" v={memLabel(size.mem)} warn={cap.memOver} />
        <SumRow icon="disk" k="Disk" v={`${size.disk} GiB`} warn={cap.diskOver} />
        <SumRow icon="network" k="Network" v={networkName} />
        {f.source === "image" && <SumRow icon="key" k="Sign-in" v={signIn} warn={form.noLogin} />}
      </dl>
      {avail != null && node && (
        <div className="px-5 pb-4">
          <CapacityBar label="Host memory" used={node.host.memory_kib * 1024 - avail} adding={cap.memBytes} total={node.host.memory_kib * 1024} />
          {poolInfo && <CapacityBar className="mt-3" label={`Pool ${poolInfo.name}`} used={poolInfo.capacity_bytes - poolInfo.available_bytes} adding={cap.diskBytes} total={poolInfo.capacity_bytes} note="max, thin" />}
        </div>
      )}
      <div className="space-y-3 border-t border-zinc-100 px-5 py-4 dark:border-zinc-800">
        <Checkbox checked={f.startNow} onChange={(startNow) => set({ startNow })}>Start it right away</Checkbox>
        <ErrorText error={create.error} />
        <Button type="submit" variant="primary" size="lg" className="w-full" loading={create.isPending} disabled={blockers.length > 0}>
          {create.isPending ? (f.source === "image" ? "Copying image…" : "Creating…") : "Create machine"}
        </Button>
        {blockers.length > 0 ? (
          <p className="text-center text-xs text-zinc-500">{blockers[0]}</p>
        ) : (
          <p className="hidden items-center justify-center gap-1 text-xs text-zinc-400 sm:flex">
            or press <Kbd>{modKey}</Kbd><Kbd>↵</Kbd>
          </p>
        )}
      </div>
    </Card>
  );
}
