import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Link } from "@tanstack/react-router";
import { type Task, type Vm } from "../api";
import { useCancelTask, useTasks, useVms } from "../lib/hooks";
import { dismissUpload, taskLabel, taskVmId, uploads, type Upload } from "../lib/tasks";
import { useStore } from "../lib/store";
import { ago, bytes } from "../lib/format";
import { Icon, Spinner } from "./icons";
import { IconButton, Progress, cx } from "./ui";

export function TaskRow({ task, vms, compact }: { task: Task; vms?: Vm[]; compact?: boolean }) {
  const cancel = useCancelTask();
  const vmId = taskVmId(task);
  const vm = vms?.find((v) => v.id === vmId);
  const label = taskLabel(task);
  const running = task.state === "running";
  const title = running ? label.running : task.state === "succeeded" ? label.done : task.state === "cancelled" ? `${label.running} (cancelled)` : label.failed;
  return (
    <div className={cx("flex items-start gap-3", compact ? "py-2" : "py-3")}>
      <span
        className={cx(
          "mt-0.5 grid size-6 shrink-0 place-items-center rounded-full",
          running ? "bg-indigo-50 text-indigo-600 dark:bg-indigo-500/15 dark:text-indigo-300"
            : task.state === "succeeded" ? "bg-emerald-50 text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-400"
            : task.state === "failed" ? "bg-red-50 text-red-600 dark:bg-red-500/15 dark:text-red-400"
            : "bg-zinc-100 text-zinc-500 dark:bg-zinc-800",
        )}
      >
        {running ? <Spinner className="size-3.5" /> : <Icon name={task.state === "succeeded" ? "check" : task.state === "failed" ? "alert" : "x"} className="size-3.5" />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <p className="min-w-0 flex-1 truncate text-[13px] font-medium">
            {title}
            {vm && (
              <>
                {" · "}
                <Link to="/vms/$id" params={{ id: vm.id }} className="text-indigo-600 hover:underline dark:text-indigo-400">{vm.name}</Link>
              </>
            )}
          </p>
          <span className="shrink-0 text-[11px] text-zinc-400" title={task.updated}>{ago(task.updated)}</span>
        </div>
        {running && (
          <div className="mt-1.5 flex items-center gap-2">
            <Progress value={task.progress} className="flex-1" />
            <IconButton
              icon="x"
              size="xs"
              label="Cancel task"
              onClick={() => cancel(task.id)}
            />
          </div>
        )}
        {task.message && running && <p className="mt-0.5 truncate text-xs text-zinc-500">{task.message}</p>}
        {task.error && !running && <p className="mt-0.5 line-clamp-2 text-xs text-red-600 dark:text-red-400">{task.error}</p>}
      </div>
    </div>
  );
}

export function UploadRow({ u }: { u: Upload }) {
  const frac = u.total ? u.loaded / u.total : 0;
  const eta = u.rate > 0 ? Math.max(0, (u.total - u.loaded) / u.rate) : null;
  return (
    <div className="flex items-start gap-3 py-2">
      <span
        className={cx(
          "mt-0.5 grid size-6 shrink-0 place-items-center rounded-full",
          u.state === "failed" ? "bg-red-50 text-red-600 dark:bg-red-500/15" : u.state === "done" ? "bg-emerald-50 text-emerald-600 dark:bg-emerald-500/15" : "bg-sky-50 text-sky-600 dark:bg-sky-500/15 dark:text-sky-300",
        )}
      >
        <Icon name={u.state === "failed" ? "alert" : u.state === "done" ? "check" : "upload"} className="size-3.5" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-[13px] font-medium">{u.state === "done" ? "Uploaded" : u.state === "failed" ? "Upload failed" : "Uploading"} {u.name}</p>
        {u.state === "uploading" && (
          <>
            <div className="mt-1.5 flex items-center gap-2">
              <Progress value={frac} className="flex-1" />
              <IconButton icon="x" size="xs" label="Cancel upload" onClick={u.abort} />
            </div>
            <p className="mt-0.5 text-xs text-zinc-500 tabular-nums">
              {bytes(u.loaded)} of {bytes(u.total)}
              {u.rate > 0 && ` · ${bytes(u.rate)}/s`}
              {eta != null && eta < 36000 && ` · ${eta < 60 ? `${Math.ceil(eta)}s` : `${Math.ceil(eta / 60)} min`} left`}
            </p>
          </>
        )}
        {u.state === "failed" && (
          <p className="mt-0.5 text-xs text-red-600 dark:text-red-400">
            {u.error} <button className="ml-1 underline" onClick={() => dismissUpload(u.id)}>Dismiss</button>
          </p>
        )}
      </div>
    </div>
  );
}

/** The top-bar activity button and its popover. */
export function TaskTray() {
  const tasks = useTasks().data ?? [];
  const vms = useVms().data;
  const ups = useStore(uploads);
  const running = tasks.filter((t) => t.state === "running");
  const active = running.length + ups.filter((u) => u.state === "uploading").length;
  const recent = tasks.filter((t) => t.state !== "running").slice(0, 5);
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!panel.current?.contains(e.target as Node) && !btn.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && (setOpen(false), btn.current?.focus());
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const r = btn.current?.getBoundingClientRect();
  return (
    <>
      <button
        ref={btn}
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-label={active ? `${active} activities in progress` : "Activity"}
        title="Activity"
        className={cx(
          "relative inline-flex h-8 items-center gap-1.5 rounded-md px-2 text-[13px] font-medium transition-colors",
          active ? "bg-indigo-50 text-indigo-700 hover:bg-indigo-100 dark:bg-indigo-500/15 dark:text-indigo-300" : "text-zinc-500 hover:bg-zinc-900/5 dark:hover:bg-white/5",
        )}
      >
        {active ? <Spinner className="size-4" /> : <Icon name="activity" />}
        {active > 0 && <span className="tabular-nums">{active}</span>}
      </button>
      {open &&
        createPortal(
          <div
            ref={panel}
            role="dialog"
            aria-label="Activity"
            className="fixed z-50 w-[min(24rem,calc(100vw-1.5rem))] overflow-hidden rounded-xl bg-white shadow-xl ring-1 ring-zinc-900/10 animate-pop dark:bg-zinc-900 dark:ring-white/10"
            style={{ top: (r?.bottom ?? 48) + 8, right: Math.max(12, window.innerWidth - (r?.right ?? window.innerWidth - 12)) }}
          >
            <div className="flex items-center justify-between border-b border-zinc-100 px-4 py-2.5 dark:border-zinc-800">
              <h2 className="text-sm font-semibold">Activity</h2>
              <Link to="/tasks" onClick={() => setOpen(false)} className="text-xs font-medium text-indigo-600 hover:underline dark:text-indigo-400">
                All tasks
              </Link>
            </div>
            <div className="scrollbar-thin max-h-[60vh] divide-y divide-zinc-100 overflow-y-auto px-4 dark:divide-zinc-800">
              {ups.map((u) => <UploadRow key={u.id} u={u} />)}
              {running.map((t) => <TaskRow key={t.id} task={t} vms={vms} compact />)}
              {active === 0 && !ups.length && (
                <p className="py-5 text-center text-[13px] text-zinc-500">Nothing running right now.</p>
              )}
              {recent.length > 0 && (
                <div className="py-2">
                  <p className="pt-1 text-[11px] font-semibold tracking-wide text-zinc-400 uppercase">Recent</p>
                  {recent.map((t) => <TaskRow key={t.id} task={t} vms={vms} compact />)}
                </div>
              )}
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
