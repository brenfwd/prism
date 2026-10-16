import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, type Disk, type Nic, type Vm } from "../../api";
import { bytes } from "../../lib/format";
import { useAllVolumes, useNetworks } from "../../lib/hooks";
import { nicLabel } from "../../lib/networks";
import { confirm } from "../../components/Confirm";
import { Badge, Button, Card, EmptyState, IconButton, Menu } from "../../components/ui";
import { isOn } from "./VmPage";
import { Forwards, NicSub } from "./Nics";
import { VlanEditor } from "./Vlans";
import { AddDisk } from "./hardware/AddDisk";
import { AddNic } from "./hardware/AddNic";
import { BootOrder } from "./hardware/BootOrder";
import { deviceChange } from "./hardware/deviceChange";
import { Detach } from "./hardware/Detach";
import { DeviceRow } from "./hardware/DeviceRow";
import { Grow } from "./hardware/Grow";
import { Insert } from "./hardware/Insert";

/** The Hardware tab: disks, CD drives and network adapters with their dialogs, next to the boot order. */
export function HardwareTab({ vm }: { vm: Vm }) {
  const nets = useNetworks().data;
  const { data: volumes } = useAllVolumes();
  const size = (d: Disk) => volumes.find((v) => v.pool === d.pool && v.name === d.volume);
  const disks = vm.disks.filter((d) => d.device === "disk");
  const cdroms = vm.disks.filter((d) => d.device === "cdrom");
  const [modal, setModal] = useState<null | { kind: "addDisk" } | { kind: "addNic" } | { kind: "grow"; disk: Disk } | { kind: "insert"; disk: Disk } | { kind: "detach"; disk: Disk } | { kind: "forwards"; nic: Nic } | { kind: "vlans"; nic: Nic }>(null);
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
                onClick={() => deviceChange(qc, "CD-ROM drive added", () => api.addDisk(vm.id, { pool: vm.disks[0]?.pool ?? "default", device: "cdrom" }))}
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
                          <Button size="xs" variant="ghost" icon="eject" onClick={() => deviceChange(qc, "Ejected", () => api.updateDisk(vm.id, d.id, { media: null }))}>
                            Eject
                          </Button>
                        )}
                        <IconButton
                          icon="trash"
                          label="Remove drive"
                          size="xs"
                          onClick={async () => {
                            if (await confirm({ title: `Remove drive ${d.target}?`, body: "The ISO file stays in storage.", confirmLabel: "Remove", danger: true }))
                              void deviceChange(qc, "Drive removed", () => api.removeDisk(vm.id, d.id, false));
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
                      {n.network.type === "network" && <IconButton icon="network" label="VLANs" size="xs" onClick={() => setModal({ kind: "vlans", nic: n })} />}
                      {(n.network.type === "user" || n.network.type === "network") && <IconButton icon="link" label="Port forwards" size="xs" onClick={() => setModal({ kind: "forwards", nic: n })} />}
                      <IconButton
                        icon="trash"
                        label="Remove adapter"
                        size="xs"
                        onClick={async () => {
                          if (await confirm({ title: "Remove this network adapter?", body: isOn(vm) ? "It's unplugged from the running machine right away." : undefined, confirmLabel: "Remove", danger: true }))
                            void deviceChange(qc, "Network adapter removed", () => api.removeNic(vm.id, n.id));
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
      {modal?.kind === "vlans" && <VlanEditor vm={vm} nic={modal.nic} onClose={close} />}
      {modal?.kind === "forwards" && <Forwards vm={vm} nic={modal.nic} onClose={close} />}
      {modal?.kind === "addNic" && <AddNic vm={vm} onClose={close} />}
      {modal?.kind === "grow" && <Grow vm={vm} disk={modal.disk} current={size(modal.disk)?.size_bytes} onClose={close} />}
      {modal?.kind === "insert" && <Insert vm={vm} disk={modal.disk} onClose={close} />}
      {modal?.kind === "detach" && <Detach vm={vm} disk={modal.disk} onClose={close} />}
    </div>
  );
}

