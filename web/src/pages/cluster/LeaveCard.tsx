import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type ClusterStatus } from "../../api";
import { toast } from "../../lib/toast";
import { confirm } from "../../components/Confirm";
import { Button, Callout, Card, ErrorText, Modal } from "../../components/ui";

/** Leaving is one click, unless this node is the hub: then explain why and offer the way out. */
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
          description="The other nodes follow it, so it cannot leave while they depend on it."
          onClose={() => setGuide(false)}
          footer={<Button variant="ghost" onClick={() => setGuide(false)}>Not now</Button>}
        >
          <div className="space-y-4 text-sm">
            <Callout tone="info" title="Two ways forward">
              Leave from another node instead, or clear the way here by removing the {others.length} other {others.length === 1 ? "node" : "nodes"}. Handing the hub role to another node is not available yet.
            </Callout>
            <ol className="list-decimal space-y-1 pl-5 text-zinc-600 dark:text-zinc-400">
              <li>Remove: {others.map((m) => m.name).join(", ")}. They are cut off and cannot rejoin with the same identity.</li>
              <li>Come back here and leave. This node restarts as standalone with its machines.</li>
            </ol>
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
        </Modal>
      )}
    </Card>
  );
}
