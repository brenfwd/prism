import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, type Disk, type Vm } from "../../../api";
import { bytes } from "../../../lib/format";
import { useAllVolumes } from "../../../lib/hooks";
import { Icon } from "../../../components/icons";
import { Badge, Button, EmptyState, Modal, cx } from "../../../components/ui";
import { deviceChange } from "./deviceChange";

/** Dialog that lists the ISOs in the drive's pool and inserts the one you click. */
export function Insert({ vm, disk, onClose }: { vm: Vm; disk: Disk; onClose: () => void }) {
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
                type="button"
                disabled={!!busy}
                onClick={async () => {
                  setBusy(v.name);
                  const ok = await deviceChange(qc, `${v.name} inserted`, () => api.updateDisk(vm.id, disk.id, { media: v.name }));
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
