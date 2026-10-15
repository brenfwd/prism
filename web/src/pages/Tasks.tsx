import { useState } from "react";
import { useTasks, useVms } from "../lib/hooks";
import { uploads } from "../lib/tasks";
import { useStore } from "../lib/store";
import { TaskRow, UploadRow } from "../components/TaskTray";
import { Card, EmptyState, Segmented } from "../components/ui";

type Filter = "all" | "running" | "failed";

export function TasksPage() {
  const tasks = useTasks();
  const vms = useVms().data;
  const ups = useStore(uploads);
  const [filter, setFilter] = useState<Filter>("all");
  const list = (tasks.data ?? []).filter((t) => filter === "all" || (filter === "running" ? t.state === "running" : t.state === "failed" || t.state === "cancelled"));
  return (
    <div className="animate-in">
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Tasks</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Long-running work like snapshots and uploads. Updates live; kept for a week.</p>
        </div>
        <Segmented label="Show" size="sm" value={filter} onChange={setFilter} options={[{ value: "all", label: "All" }, { value: "running", label: "Running" }, { value: "failed", label: "Failed" }]} />
      </div>
      {ups.length > 0 && (
        <Card title="Uploads" className="mb-5">
          <div className="-my-2 divide-y divide-zinc-100 dark:divide-zinc-800">{ups.map((u) => <UploadRow key={u.id} u={u} />)}</div>
        </Card>
      )}
      <Card padded={false}>
        {tasks.isLoading ? (
          <div className="space-y-2 p-5">{[0, 1, 2].map((i) => <div key={i} className="h-10 animate-pulse rounded bg-zinc-100 dark:bg-zinc-800/60" />)}</div>
        ) : list.length === 0 ? (
          <EmptyState icon="activity" title={filter === "all" ? "No tasks yet" : filter === "running" ? "Nothing running" : "No failures"}>
            {filter === "all" ? "Snapshots and other long operations show up here with live progress." : "All quiet."}
          </EmptyState>
        ) : (
          <div className="divide-y divide-zinc-100 px-5 dark:divide-zinc-800">{list.map((t) => <TaskRow key={t.id} task={t} vms={vms} />)}</div>
        )}
      </Card>
    </div>
  );
}
