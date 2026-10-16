import { useQueryClient } from "@tanstack/react-query";
import { DropBox } from "../../components/DropZone";
import { Icon } from "../../components/icons";
import { Progress, cx } from "../../components/ui";
import { bytes } from "../../lib/format";
import { useStore } from "../../lib/store";
import { startUpload, uploads } from "../../lib/tasks";
import type { PoolVolume } from "./useNewVmForm";

/**
 * Lists the images or ISOs that can be booted, with uploads in progress
 * and a drop target for adding more. Dropped files go to `defaultPool`.
 */
export function SourcePicker({ kind, items, loading, picked, onPick, defaultPool, onUploaded }: {
  kind: "image" | "iso";
  items: PoolVolume[];
  loading: boolean;
  picked: PoolVolume | undefined;
  onPick: (v: PoolVolume) => void;
  defaultPool: string | undefined;
  onUploaded: (pool: string, name: string) => void;
}) {
  const qc = useQueryClient();
  const ups = useStore(uploads).filter((u) => u.state === "uploading");
  const accept = kind === "image" ? ".img,.qcow2,.raw" : ".iso";
  const upload = (files: File[]) => {
    if (!defaultPool) return;
    for (const f of files) {
      void startUpload(qc, defaultPool, f).then((v) => v && onUploaded(defaultPool, v.name));
    }
  };
  const label = kind === "image" ? "cloud image" : "ISO";
  if (loading && !items.length) return <div className="mt-4 h-16 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-800/60" />;
  return (
    <div className="mt-4">
      {items.length > 0 && (
        <div role="radiogroup" aria-label={`Choose a ${label}`} className="scrollbar-thin grid max-h-64 gap-2 overflow-y-auto sm:grid-cols-2">
          {items.map((v) => {
            const on = picked?.pool === v.pool && picked.name === v.name;
            return (
              <button
                key={`${v.pool}/${v.name}`}
                type="button"
                role="radio"
                aria-checked={on}
                onClick={() => onPick(v)}
                className={cx(
                  "flex items-center gap-3 rounded-lg px-3 py-2.5 text-left ring-1 transition-colors ring-inset",
                  on ? "bg-indigo-50/60 ring-indigo-400 dark:bg-indigo-500/10 dark:ring-indigo-400/60" : "ring-zinc-200 hover:bg-zinc-50 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
                )}
              >
                <span className={cx("grid size-4 shrink-0 place-items-center rounded-full ring-1 ring-inset", on ? "bg-indigo-600 ring-indigo-600 dark:bg-indigo-500" : "ring-zinc-300 dark:ring-zinc-600")}>
                  {on && <span className="size-1.5 rounded-full bg-white" />}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-[13px]">{v.name}</span>
                  <span className="block text-xs text-zinc-500 tabular-nums">
                    {bytes(v.size_bytes)}{kind === "image" && v.file_bytes !== v.size_bytes ? ` virtual · ${bytes(v.file_bytes)} file` : ""} · {v.pool}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}
      {ups.map((u) => (
        <div key={u.id} className="mt-2 rounded-lg px-3 py-2.5 ring-1 ring-zinc-200 ring-inset dark:ring-zinc-700/80">
          <div className="mb-1.5 flex justify-between text-[13px]">
            <span className="truncate font-mono">{u.name}</span>
            <span className="text-zinc-500 tabular-nums">{Math.round((u.loaded / (u.total || 1)) * 100)}%</span>
          </div>
          <Progress value={u.total ? u.loaded / u.total : 0} />
        </div>
      ))}
      {items.length === 0 && !ups.length ? (
        <DropBox onFiles={upload} accept={accept} className="mt-1">
          <Icon name="upload" className="mx-auto mb-2 size-6 text-zinc-400" />
          <p className="text-sm font-medium">{kind === "image" ? "No cloud images yet" : "No ISOs yet"}</p>
          <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-zinc-500">
            {kind === "image" ? (
              <>Drop a cloud image here (for example Ubuntu's <span className="font-mono">noble-server-cloudimg-amd64.img</span> from cloud-images.ubuntu.com).</>
            ) : (
              <>Drop an installer ISO here, such as a Debian netinst or Windows ISO.</>
            )}
          </p>
        </DropBox>
      ) : (
        <DropBox onFiles={upload} accept={accept} className="mt-2 !p-3">
          <p className="text-xs text-zinc-500">Drop another {label} here to upload it</p>
        </DropBox>
      )}
    </div>
  );
}
