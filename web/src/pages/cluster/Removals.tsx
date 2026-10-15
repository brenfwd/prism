import { useQuery } from "@tanstack/react-query";
import { api } from "../../api";
import { ago } from "../../lib/format";
import { Badge, Card } from "../../components/ui";

/** Removed nodes and which members have not yet applied each removal. Hidden when none is in flight. */
export function Removals() {
  const q = useQuery({ queryKey: ["cluster-removals"], queryFn: api.removals, refetchInterval: 5000 });
  const open = (q.data ?? []).filter((r) => !r.complete);
  if (open.length === 0) return null;
  return (
    <Card title="Removals in progress" description="Until every member applies a removal, some still trust the removed node." padded={false}>
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
        {open.map((r) => (
          <li key={r.node_id} className="px-4 py-3 text-sm">
            <div className="font-mono text-xs text-zinc-500">{r.mesh_addr} · removed {ago(r.removed * 1000)}</div>
            <div className="mt-1.5 flex flex-wrap gap-2">
              {r.pending.map((p) => (
                <Badge key={p.node_id} tone={p.applied_index == null ? "red" : "amber"}>
                  {p.name}: {p.applied_index == null ? "unreachable" : `at ${p.applied_index} of ${r.removed_index}`}
                </Badge>
              ))}
            </div>
          </li>
        ))}
      </ul>
    </Card>
  );
}
