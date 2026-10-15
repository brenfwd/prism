import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, humanError } from "../api";
import { dateTime } from "../lib/format";
import { Badge, Button, Callout, Card, IconButton } from "../components/ui";
import { InviteModal } from "./cluster/InviteModal";
import { Members } from "./cluster/Members";
import { LeaveCard } from "./cluster/LeaveCard";
import { Setup } from "./cluster/Setup";

export function ClusterPage() {
  const qc = useQueryClient();
  const [inviting, setInviting] = useState(false);
  const status = useQuery({ queryKey: ["cluster"], queryFn: api.cluster, refetchInterval: 4000 });
  const clustered = status.data?.clustered ?? false;
  const mesh = useQuery({ queryKey: ["cluster-mesh"], queryFn: api.mesh, refetchInterval: 5000, enabled: clustered });
  const invites = useQuery({ queryKey: ["cluster-invites"], queryFn: api.invites, enabled: clustered });
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeInvite(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["cluster-invites"] }),
  });

  if (status.isLoading) return <div className="h-40 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />;
  const s = status.data;
  if (status.isError || !s) return <Callout tone="warning" title="Clustering is not available on this node">{humanError(status.error)}</Callout>;
  const open = (invites.data ?? []).filter((i) => !i.consumed_by && i.exp * 1000 > Date.now());

  return (
    <div className="animate-in space-y-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">{s.cluster?.name ?? "Cluster"}</h1>
          <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
            {clustered ? "Every node can manage every machine. Add nodes with a copy-paste invite." : "This node runs on its own. Start a cluster or join one."}
          </p>
        </div>
        {clustered && (
          <div className="flex gap-2">
            <Link to="/archive"><Button icon="database">Archive</Button></Link>
            <Button variant="primary" icon="plus" onClick={() => setInviting(true)}>Invite node</Button>
          </div>
        )}
      </div>
      {s.degraded && <Callout tone="warning" title="Degraded">This node cannot reach the rest of the cluster. Reads work, changes are paused.</Callout>}
      {!clustered ? (
        <Setup pendingJoin={s.pending_join} />
      ) : (
        <>
          <Members status={s} peers={mesh.data ?? []} />
          {open.length > 0 && (
            <Card title="Open invites" padded={false}>
              <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
                {open.map((i) => (
                  <li key={i.invite_id} className="flex items-center justify-between gap-3 px-4 py-2.5 text-sm">
                    <span>{i.name_hint ?? "Unnamed"} <Badge>expires {dateTime(i.exp * 1000)}</Badge></span>
                    <IconButton icon="x" label="Revoke invite" onClick={() => revoke.mutate(i.invite_id)} />
                  </li>
                ))}
              </ul>
            </Card>
          )}
          <LeaveCard status={s} />
        </>
      )}
      {inviting && <InviteModal onClose={() => setInviting(false)} />}
    </div>
  );
}
