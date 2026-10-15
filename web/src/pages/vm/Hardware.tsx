import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, humanError, type Disk, type Nic, type Vm } from "../../api";
import { putVm } from "../../events";
import { bytes, GiB } from "../../lib/format";
import { useAllVolumes, useNetworks, usePools, useVms, volumeUsers } from "../../lib/hooks";
import { nicLabel } from "../../lib/networks";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Icon, type IconName } from "../../components/icons";
import {
  Badge, Button, Card, Checkbox, EmptyState, ErrorText, Field, IconButton, Input, Menu, Modal, Segmented, Select, cx,
} from "../../components/ui";
import { isOn } from "./VmPage";
import { Forwards, NicSub } from "./Nics";

/** Applies a device change and explains what happened. */
async function deviceChange(qc: QueryClient, _vm: Vm, what: string, run: () => Promise<Vm>): Promise<boolean> {
  try {
    const view = await run();
    putVm(qc, view);
    const h = view.hotplug;
    if (!isOn(view) || !h) toast({ tone: "success", title: what });
    else if (h.applied) toast({ tone: "success", title: `${what} live`, body: "The running guest sees the change now." });
    else
      toast({
        tone: "warning",
        title: `${what}: saved for the next power cycle`,
        body: h.error ? `The running machine refused it live (${h.error}). It applies after it powers off and starts again.` : "The running machine couldn't take this change live. It applies after it powers off and starts again.",
      });
    return true;
  } catch (e) {
    toast({ tone: "error", title: `${what} failed`, body: humanError(e) });
    return false;
  }
}

export function HardwareTab({ vm }: { vm: Vm }) {
  const nets = useNetworks().data;
  const { data: volumes } = useAllVolumes();
  const size = (d: Disk) => volumes.find((v) => v.pool === d.pool && v.name === d.volume);
  const disks = vm.disks.filter((d) => d.device === "disk");
  const cdroms = vm.disks.filter((d) => d.device === "cdrom");
  const [modal, setModal] = useState<null | { kind: "addDisk" } | { kind: "addNic" } | { kind: "grow"; disk: Disk } | { kind: "insert"; disk: Disk } | { kind: "detach"; disk: Disk } | { kind: "forwards"; nic: Nic }>(null);
  const qc = useQueryClient();
  const close = () => setModal(null);

  return (
    <div className="space-y-5">
      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="space-y-5">
          <Card
            title="Disks"
            description={isOn(vm) ? "Disks hot-plug into the running machine." : undefined}
            actions={<Button size="sm" icon="plus" onClick={() => setModal({ kind: "addDisk" })}>Add disk</Button>}
            padded={false}
          >
            {disks.length === 0 ? (
              <EmptyState className="!py-8" icon="disk" title="No disks" actions={<Button size="sm" icon="plus" onClick={() => setModal({ kind: "addDisk" })}>Add a disk</Button>}>
                This machine can only boot from the network or a CD-ROM.
              </EmptyState>
            ) : (
              <ul className="divide-y divide-zinc-100 border-t border-zinc-100 dark:divide-zinc-800 dark:border-zinc-800">
                {disks.map((d) => {
                  const v = size(d);
                  return (
                    <DeviceRow
                      key={d.id}
                      icon="disk"
                      title={<span className="font-mono text-[13px]">{d.volume}</span>}
                      sub={
                        <>
                          <span className="font-mono">{d.target}</span> · {d.pool} · {d.format}
                          {v && <> · {bytes(v.size_bytes)}<span className="text-zinc-400"> ({bytes(v.allocated_bytes)} used)</span></>}
                          {d.backing && d.backing.length > 0 && <> · {d.backing.length} snapshot layer{d.backing.length > 1 ? "s" : ""}</>}
                        </>
                      }
                      badge={d.boot_order != null ? <Badge tone="indigo">boot #{d.boot_order}</Badge> : undefined}
                      actions={
                        <Menu
                          label={`Actions for ${d.target}`}
                          items={[
                            { label: "Grow…", icon: "maximize", onSelect: () => setModal({ kind: "grow", disk: d }) },
                            "separator",
                            { label: "Detach…", icon: "trash", danger: true, onSelect: () => setModal({ kind: "detach", disk: d }) },
                          ]}
                          trigger={(p) => <IconButton icon="more" label={`Actions for ${d.target}`} {...p} />}
                        />
                      }
                    />
                  );
                })}
              </ul>
            )}
          </Card>

          <Card
            title="CD / DVD drives"
            actions={
              <Button
                size="sm"
                icon="plus"
                onClick={() => deviceChange(qc, vm, "CD-ROM drive added", () => api.addDisk(vm.id, { pool: vm.disks[0]?.pool ?? "default", device: "cdrom" }))}
              >
                Add drive
              </Button>
            }
            padded={false}
          >
            {cdroms.length === 0 ? (
              <p className="border-t border-zinc-100 px-5 py-4 text-[13px] text-zinc-500 dark:border-zinc-800">
                No drives. Add one to insert an ISO{isOn(vm) ? "; drives are added at the next power cycle" : ""}.
              </p>
            ) : (
              <ul className="divide-y divide-zinc-100 border-t border-zinc-100 dark:divide-zinc-800 dark:border-zinc-800">
                {cdroms.map((d) => (
                  <DeviceRow
                    key={d.id}
                    icon="disc"
                    title={d.volume ? <span className="font-mono text-[13px]">{d.volume}</span> : <span className="text-zinc-400 italic">Empty</span>}
                    sub={<><span className="font-mono">{d.target}</span> · {d.pool}</>}
                    badge={d.boot_order != null ? <Badge tone="indigo">boot #{d.boot_order}</Badge> : undefined}
                    actions={
                      <div className="flex items-center gap-1">
                        <Button size="xs" variant="secondary" icon="disc" onClick={() => setModal({ kind: "insert", disk: d })}>
                          {d.volume ? "Change" : "Insert ISO"}
                        </Button>
                        {d.volume && (
                          <Button size="xs" variant="ghost" icon="eject" onClick={() => deviceChange(qc, vm, "Ejected", () => api.updateDisk(vm.id, d.id, { media: null }))}>
                            Eject
                          </Button>
                        )}
                        <IconButton
                          icon="trash"
                          label="Remove drive"
                          size="xs"
                          onClick={async () => {
                            if (await confirm({ title: `Remove drive ${d.target}?`, body: "The ISO file stays in storage.", confirmLabel: "Remove", danger: true }))
                              void deviceChange(qc, vm, "Drive removed", () => api.removeDisk(vm.id, d.id, false));
                          }}
                        />
                      </div>
                    }
                  />
                ))}
              </ul>
            )}
          </Card>

          <Card
            title="Network adapters"
            actions={<Button size="sm" icon="plus" onClick={() => setModal({ kind: "addNic" })}>Add adapter</Button>}
            padded={false}
          >
            {vm.nics.length === 0 ? (
              <p className="border-t border-zinc-100 px-5 py-4 text-[13px] text-zinc-500 dark:border-zinc-800">No network. The machine is fully offline.</p>
            ) : (
              <ul className="divide-y divide-zinc-100 border-t border-zinc-100 dark:divide-zinc-800 dark:border-zinc-800">
                {vm.nics.map((n) => (
                  <DeviceRow
                    key={n.id}
                    icon="network"
                    title={n.network.type === "bridge" ? <>Bridge <span className="font-mono">{n.network.bridge}</span></> : nicLabel(n.network, nets)}
                    sub={<NicSub vm={vm} nic={n} />}
                    badge={n.boot_order != null ? <Badge tone="indigo">boot #{n.boot_order}</Badge> : undefined}
                    actions={
                      <>
                      {(n.network.type === "user" || n.network.type === "network") && <IconButton icon="link" label="Port forwards" size="xs" onClick={() => setModal({ kind: "forwards", nic: n })} />}
                      <IconButton
                        icon="trash"
                        label="Remove adapter"
                        size="xs"
                        onClick={async () => {
                          if (await confirm({ title: "Remove this network adapter?", body: isOn(vm) ? "It's unplugged from the running machine right away." : undefined, confirmLabel: "Remove", danger: true }))
                            void deviceChange(qc, vm, "Network adapter removed", () => api.removeNic(vm.id, n.id));
                        }}
                      />
                      </>
                    }
                  />
                ))}
              </ul>
            )}
          </Card>
        </div>
        <BootOrder vm={vm} />
      </div>

      {modal?.kind === "addDisk" && <AddDisk vm={vm} onClose={close} />}
      {modal?.kind === "forwards" && <Forwards vm={vm} nic={modal.nic} onClose={close} />}
      {modal?.kind === "addNic" && <AddNic vm={vm} onClose={close} />}
      {modal?.kind === "grow" && <Grow vm={vm} disk={modal.disk} current={size(modal.disk)?.size_bytes} onClose={close} />}
      {modal?.kind === "insert" && <Insert vm={vm} disk={modal.disk} onClose={close} />}
      {modal?.kind === "detach" && <Detach vm={vm} disk={modal.disk} onClose={close} />}
    </div>
  );
}

function DeviceRow({ icon, title, sub, badge, actions }: { icon: IconName; title: ReactNode; sub: ReactNode; badge?: ReactNode; actions: ReactNode }) {
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-2 px-5 py-3">
      <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
        <Icon name={icon} className="size-[18px]" />
      </span>
      <div className="min-w-0 flex-1 basis-40">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium">{title}</span>
          {badge}
        </div>
        <div className="truncate text-xs text-zinc-500">{sub}</div>
      </div>
      <div className="ml-auto">{actions}</div>
    </li>
  );
}

// ---------------------------------------------------------------- boot order

interface BootItem {
  id: string;
  label: string;
  sub: string;
  icon: IconName;
  enabled: boolean;
}

function bootItems(vm: Vm): BootItem[] {
  const all = [
    ...vm.disks.map((d) => ({
      id: d.id,
      order: d.boot_order ?? null,
      label: d.device === "cdrom" ? `CD-ROM ${d.target ?? ""}` : `Disk ${d.target ?? ""}`,
      sub: d.volume ?? "empty",
      icon: (d.device === "cdrom" ? "disc" : "disk") as IconName,
    })),
    ...vm.nics.map((n: Nic) => ({
      id: n.id,
      order: n.boot_order ?? null,
      label: "Network (PXE)",
      sub: n.mac,
      icon: "network" as IconName,
    })),
  ];
  const on = all.filter((x) => x.order != null).sort((a, b) => a.order! - b.order!);
  const off = all.filter((x) => x.order == null);
  return [...on.map((x) => ({ ...x, enabled: true })), ...off.map((x) => ({ ...x, enabled: false }))];
}

function BootOrder({ vm }: { vm: Vm }) {
  const qc = useQueryClient();
  const server = useMemo(() => bootItems(vm), [vm]);
  const [items, setItems] = useState(server);
  const [drag, setDrag] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  useEffect(() => setItems(server), [server]);

  const save = async (next: BootItem[]) => {
    setItems(next);
    setSaving(true);
    try {
      const view = await api.updateVm(vm.id, { boot_order: next.filter((x) => x.enabled).map((x) => x.id) });
      putVm(qc, view);
    } catch (e) {
      setItems(server);
      toast({ tone: "error", title: "Couldn't change the boot order", body: humanError(e) });
    } finally {
      setSaving(false);
    }
  };
  const move = (from: number, to: number) => {
    if (to < 0 || to >= items.length || from === to) return;
    const next = items.slice();
    const [it] = next.splice(from, 1);
    next.splice(to, 0, it);
    // Moving an item above an enabled one enables it.
    if (!it.enabled && next.slice(to + 1).some((x) => x.enabled)) it.enabled = true;
    void save(normalize(next));
  };
  const normalize = (list: BootItem[]) => [...list.filter((x) => x.enabled), ...list.filter((x) => !x.enabled)];

  return (
    <Card title="Boot order" description="Drag to reorder. The machine tries each enabled device in turn." className="self-start">
      <ol className="-mx-1 space-y-1.5" aria-label="Boot order">
        {items.map((it, i) => (
          <li
            key={it.id}
            draggable
            onDragStart={(e) => {
              setDrag(it.id);
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", it.id);
            }}
            onDragEnd={() => setDrag(null)}
            onDragOver={(e) => {
              if (!drag || drag === it.id) return;
              e.preventDefault();
              const from = items.findIndex((x) => x.id === drag);
              if (from < 0) return;
              const next = items.slice();
              const [moved] = next.splice(from, 1);
              next.splice(i, 0, moved);
              setItems(next);
            }}
            onDrop={(e) => {
              e.preventDefault();
              const idx = items.findIndex((x) => x.id === drag);
              const next = items.slice();
              if (idx >= 0 && !next[idx].enabled && next.slice(idx + 1).some((x) => x.enabled)) next[idx] = { ...next[idx], enabled: true };
              setDrag(null);
              void save(normalize(next));
            }}
            className={cx(
              "group flex items-center gap-2 rounded-lg px-2 py-2 ring-1 transition-all ring-inset",
              drag === it.id ? "opacity-50 ring-indigo-400" : "ring-zinc-200 dark:ring-zinc-700/80",
              it.enabled ? "bg-white dark:bg-zinc-900" : "bg-zinc-50 dark:bg-zinc-900/40",
            )}
          >
            <Icon name="grip" className="size-4 cursor-grab text-zinc-300 active:cursor-grabbing dark:text-zinc-600" />
            <span className={cx("grid size-5 shrink-0 place-items-center rounded-full text-[11px] font-semibold tabular-nums", it.enabled ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-200 text-zinc-500 dark:bg-zinc-800")}>
              {it.enabled ? items.filter((x) => x.enabled).indexOf(it) + 1 : "–"}
            </span>
            <Icon name={it.icon} className={cx("size-4", it.enabled ? "text-zinc-500" : "text-zinc-300 dark:text-zinc-600")} />
            <div className="min-w-0 flex-1">
              <div className={cx("truncate text-[13px] font-medium", !it.enabled && "text-zinc-400")}>{it.label}</div>
              <div className="truncate font-mono text-[11px] text-zinc-400">{it.sub}</div>
            </div>
            <div className="flex opacity-60 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
              <IconButton icon="arrowUp" label={`Move ${it.label} up`} size="xs" disabled={i === 0 || saving} onClick={() => move(i, i - 1)} />
              <IconButton icon="arrowDown" label={`Move ${it.label} down`} size="xs" disabled={i === items.length - 1 || saving} onClick={() => move(i, i + 1)} />
            </div>
            <input
              type="checkbox"
              aria-label={`Boot from ${it.label}`}
              title={it.enabled ? "Don't boot from this" : "Boot from this"}
              className="size-4 accent-indigo-600"
              checked={it.enabled}
              disabled={saving}
              onChange={(e) => void save(normalize(items.map((x) => (x.id === it.id ? { ...x, enabled: e.target.checked } : x))))}
            />
          </li>
        ))}
        {!items.length && <li className="px-1 text-[13px] text-zinc-500">No bootable devices.</li>}
      </ol>
      <p className="mt-3 flex items-center gap-1.5 text-xs text-zinc-500">
        {saving ? <>Saving…</> : isOn(vm) ? <><Icon name="info" className="size-3.5" />Takes effect at the next boot.</> : null}
      </p>
    </Card>
  );
}

// ---------------------------------------------------------------- modals

function AddDisk({ vm, onClose }: { vm: Vm; onClose: () => void }) {
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
      deviceChange(qc, vm, mode === "new" ? `${sizeGib} GiB disk added` : `${chosen?.name} attached`, () =>
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

function AddNic({ vm, onClose }: { vm: Vm; onClose: () => void }) {
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
      deviceChange(qc, vm, "Network adapter added", () =>
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
          <Field label="VLAN" htmlFor="nic-vlan" hint="Optional. Puts this adapter on its own segment (2 to 4094): it only sees adapters on the same VLAN, not the gateway or DHCP, so give it a static address." error={vlanOk ? undefined : "Use a number from 2 to 4094."}>
            <Input id="nic-vlan" inputMode="numeric" value={vlanText} onChange={(e) => setVlanText(e.target.value.replace(/\D/g, ""))} placeholder="none" className="font-mono" aria-invalid={!vlanOk} />
          </Field>
        )}
        <Field label="MAC address" htmlFor="nic-mac" hint="Leave empty to pick a random one." error={macOk ? undefined : "Format: 52:54:00:12:34:56"}>
          <Input id="nic-mac" value={mac} onChange={(e) => setMac(e.target.value.trim())} placeholder="random" className="font-mono" aria-invalid={!macOk} />
        </Field>
      </div>
    </Modal>
  );
}

function Grow({ vm, disk, current, onClose }: { vm: Vm; disk: Disk; current?: number; onClose: () => void }) {
  const qc = useQueryClient();
  const cur = current ? Math.ceil(current / GiB) : 1;
  const [size, setSize] = useState(cur + 10);
  const m = useMutation({
    mutationFn: () => deviceChange(qc, vm, `${disk.target} grown to ${size} GiB`, () => api.updateDisk(vm.id, disk.id, { size_gib: size })),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title={`Grow ${disk.target}`}
      description="Disks can only grow. Afterwards, extend the partition inside the guest (cloud images do this by themselves at boot)."
      onClose={onClose}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={m.isPending} disabled={size <= cur} onClick={() => m.mutate()}>Grow to {size} GiB</Button>
        </>
      }
    >
      <Field label="New size (GiB)" htmlFor="grow" hint={current ? `Currently ${bytes(current)}.` : undefined}>
        <Input id="grow" type="number" min={cur + 1} value={size} onChange={(e) => setSize(+e.target.value)} autoFocus />
      </Field>
      {m.error && <ErrorText className="mt-3" error={m.error} />}
    </Modal>
  );
}

function Insert({ vm, disk, onClose }: { vm: Vm; disk: Disk; onClose: () => void }) {
  const qc = useQueryClient();
  const { data: volumes, loading } = useAllVolumes();
  const isos = volumes.filter((v) => v.kind === "iso" && v.pool === disk.pool);
  const [busy, setBusy] = useState<string | null>(null);
  return (
    <Modal title={`Insert a disc into ${disk.target}`} description={`ISOs from pool ${disk.pool}. Works while the machine runs.`} onClose={onClose}>
      {loading ? (
        <div className="h-20 animate-pulse rounded-lg bg-zinc-100 dark:bg-zinc-800" />
      ) : isos.length === 0 ? (
        <EmptyState className="!py-6" icon="disc" title="No ISOs in this pool" actions={<Link to="/storage" search={{ pool: disk.pool }}><Button size="sm" icon="upload">Upload one</Button></Link>}>
          Upload an ISO to pool {disk.pool} first.
        </EmptyState>
      ) : (
        <ul className="-mx-2 space-y-1">
          {isos.map((v) => (
            <li key={v.name}>
              <button
                disabled={!!busy}
                onClick={async () => {
                  setBusy(v.name);
                  const ok = await deviceChange(qc, vm, `${v.name} inserted`, () => api.updateDisk(vm.id, disk.id, { media: v.name }));
                  setBusy(null);
                  if (ok) onClose();
                }}
                className={cx(
                  "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left hover:bg-zinc-100 disabled:opacity-60 dark:hover:bg-white/5",
                  disk.volume === v.name && "bg-indigo-50 dark:bg-indigo-500/10",
                )}
              >
                <Icon name="disc" className="size-4 text-zinc-400" />
                <span className="min-w-0 flex-1 truncate font-mono text-[13px]">{v.name}</span>
                <span className="text-xs text-zinc-500 tabular-nums">{bytes(v.size_bytes)}</span>
                {disk.volume === v.name && <Badge tone="indigo">inserted</Badge>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}

function Detach({ vm, disk, onClose }: { vm: Vm; disk: Disk; onClose: () => void }) {
  const qc = useQueryClient();
  const owned = !!disk.volume?.startsWith(`${vm.name}-disk`);
  const [del, setDel] = useState(owned);
  const m = useMutation({
    mutationFn: () => deviceChange(qc, vm, del ? `${disk.target} removed and deleted` : `${disk.target} detached`, () => api.removeDisk(vm.id, disk.id, del)),
    onSuccess: (ok) => ok && onClose(),
  });
  return (
    <Modal
      title={`Detach ${disk.target}?`}
      size="sm"
      onClose={onClose}
      icon={<div className="grid size-9 shrink-0 place-items-center rounded-full bg-red-50 text-red-600 dark:bg-red-500/10 dark:text-red-400"><Icon name="disk" className="size-[18px]" /></div>}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="danger" loading={m.isPending} onClick={() => m.mutate()}>{del ? "Detach and delete" : "Detach"}</Button>
        </>
      }
    >
      <div className="space-y-3 text-sm text-zinc-600 dark:text-zinc-300">
        <p>
          <span className="font-mono">{disk.volume}</span> is unplugged{isOn(vm) ? " from the running machine. Unmount it in the guest first to avoid losing writes" : ""}.
        </p>
        <Checkbox checked={del} onChange={setDel}>Also delete the volume file. This can't be undone.</Checkbox>
      </div>
    </Modal>
  );
}
