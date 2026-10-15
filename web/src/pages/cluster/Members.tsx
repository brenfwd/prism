import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type ClusterMember, type ClusterStatus, type MeshPeer } from "../../api";
import { ago } from "../../lib/format";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Badge, Card, IconButton } from "../../components/ui";

type Health = { label: string; tone: "green" | "amber" | "red" | "neutral" };

function health(m: ClusterMember, me: string, peer: MeshPeer | undefined): Health {
  if (m.node_id === me) return { label: "This node", tone: "green" };
  if (m.state === "joining") return { label: "Joining", tone: "amber" };
  const age = peer?.handshake_age_secs;
  if (age == null) return { label: "No contact", tone: "red" };
  return age < 180 ? { label: "Connected", tone: "green" } : { label: `Quiet ${age}s`, tone: "amber" };
}

export function Members({ status, peers }: { status: ClusterStatus; peers: MeshPeer[] }) {
  const qc = useQueryClient();
  const remove = useMutation({
    mutationFn: (id: string) => api.removeMember(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["cluster"] }),
    onError: (e) => toast({ tone: "error", title: "Could not remove node", body: humanError(e) }),
  });
  const consensus = status.cluster?.mode === "consensus";
  const voter = useMutation({
    mutationFn: (v: { id: string; voter: boolean }) => api.setVoter(v.id, v.voter),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["cluster"] }),
    onError: (e) => toast({ tone: "error", title: "Voter change refused", body: humanError(e) }),
  });
  return (
    <Card title="Members" description={`${status.members.length} in ${status.cluster?.name ?? "the cluster"}`} padded={false}>
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
        {status.members.map((m) => {
          const h = health(m, status.me, peers.find((p) => p.pubkey === m.wg_pubkey));
          const mine = m.node_id === status.me;
          return (
            <li key={m.node_id} className="flex flex-wrap items-center gap-3 px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{m.name}</span>
                  <Badge tone={h.tone}>{h.label}</Badge>
                  {m.voter && <Badge tone="indigo">{status.cluster?.mode === "hub_spoke" ? "Hub" : "Voter"}</Badge>}
                  {status.leader === m.node_id && <Badge>Leader</Badge>}
                </div>
                <div className="mt-0.5 truncate font-mono text-xs text-zinc-500">
                  {m.mesh_addr} · v{m.version} · joined {ago(m.joined * 1000)}
                </div>
              </div>
              {consensus && !mine && (
                <button className="text-xs text-zinc-500 hover:underline" onClick={() => voter.mutate({ id: m.node_id, voter: !m.voter })}>
                  {m.voter ? "Demote" : "Make voter"}
                </button>
              )}
              {!mine && (
                <IconButton
                  icon="trash"
                  label={`Remove ${m.name}`}
                  onClick={async () => {
                    if (await confirm({ title: `Remove ${m.name}?`, body: "It is cut off from the cluster and cannot rejoin with the same identity.", confirmLabel: "Remove", danger: true })) remove.mutate(m.node_id);
                  }}
                />
              )}
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
