import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type ClusterStatus } from "../../api";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Button, Callout, Card, ErrorText, Modal } from "../../components/ui";

/** Leaving is one click, unless this node is the hub: then offer to hand the hub role over first. */
export function LeaveCard({ status }: { status: ClusterStatus }) {
  const qc = useQueryClient();
  const [guide, setGuide] = useState(false);
  const others = status.members.filter((m) => m.node_id !== status.me);
  const isHub = status.cluster?.mode === "hub_spoke" && status.members.some((m) => m.node_id === status.me && m.voter);
  const blocked = isHub && others.length > 0;
  const leave = useMutation({
    mutationFn: api.leaveCluster,
    onSuccess: () => toast({ tone: "info", title: "Left the cluster", body: "This node is restarting as standalone." }),
    onError: (e) => toast({ tone: "error", title: "Could not leave", body: humanError(e) }),
  });
  const removeOthers = useMutation({
    mutationFn: async () => {
      for (const m of others) await api.removeMember(m.node_id);
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["cluster"] });
      toast({ tone: "success", title: "Other nodes removed", body: "You can leave now." });
      setGuide(false);
    },
  });
  const candidates = others.filter((m) => m.state === "active");
  const [pick, setTarget] = useState("");
  const target = pick || candidates[0]?.node_id || "";
  const handover = useMutation({
    mutationFn: async () => {
      await api.transferHub(target);
      await api.leaveCluster();
    },
    onSuccess: () => {
      setGuide(false);
      toast({ tone: "success", title: "Hub handed over", body: "This node left and is restarting as standalone." });
    },
  });
  const start = async () => {
    if (blocked) return setGuide(true);
    if (await confirm({ title: "Leave this cluster?", body: "The node restarts as standalone.", confirmLabel: "Leave", danger: true })) leave.mutate();
  };
  return (
    <Card title="Leave cluster" description="Removes this node and returns it to standalone. Its machines stay on this node.">
      <Button variant="danger" loading={leave.isPending} onClick={start}>Leave cluster</Button>
      {guide && (
        <Modal
          title="This node is the hub"
          description="The other nodes follow it. Hand the hub role to one of them and this node can leave."
          onClose={() => setGuide(false)}
          footer={<Button variant="ghost" onClick={() => setGuide(false)}>Not now</Button>}
        >
          <div className="space-y-4 text-sm">
            {candidates.length > 0 ? (
              <>
                <label className="block space-y-1">
                  <span className="text-zinc-600 dark:text-zinc-400">New hub</span>
                  <select
                    className="w-full rounded-md border border-zinc-300 bg-transparent px-2 py-1.5 dark:border-zinc-700"
                    value={target}
                    onChange={(e) => setTarget(e.target.value)}
                  >
                    {candidates.map((m) => <option key={m.node_id} value={m.node_id}>{m.name}</option>)}
                  </select>
                </label>
                <Button variant="primary" loading={handover.isPending} onClick={() => handover.mutate()}>
                  Hand over the hub and leave
                </Button>
                <ErrorText error={handover.error} />
              </>
            ) : (
              <Callout tone="info" title="No node can take over yet">
                Other nodes are still joining. Wait until one is active, or remove them.
              </Callout>
            )}
            <details className="text-zinc-600 dark:text-zinc-400">
              <summary className="cursor-pointer">Or remove the other nodes instead</summary>
              <div className="mt-3 space-y-3">
                <p>Removes {others.map((m) => m.name).join(", ")}. They are cut off and cannot rejoin with the same identity.</p>
                <Button
                  variant="danger"
                  loading={removeOthers.isPending}
                  onClick={async () => {
                    if (await confirm({ title: `Remove ${others.length} other ${others.length === 1 ? "node" : "nodes"}?`, body: "This cannot be undone.", confirmLabel: "Remove them", danger: true })) removeOthers.mutate();
                  }}
                >
                  Remove the other nodes
                </Button>
                <ErrorText error={removeOthers.error} />
              </div>
            </details>
          </div>
        </Modal>
      )}
    </Card>
  );
}
