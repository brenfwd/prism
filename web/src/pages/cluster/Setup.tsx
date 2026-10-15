import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, humanError, type JoinReply } from "../../api";
import { toast } from "../../lib/toast";
import { Button, Callout, Card, CopyButton, ErrorText, Field, Input, Segmented, Textarea } from "../../components/ui";

export function Setup({ pendingJoin }: { pendingJoin: boolean }) {
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <CreateCard />
      <JoinCard pendingJoin={pendingJoin} />
    </div>
  );
}

function CreateCard() {
  const qc = useQueryClient();
  const [name, setName] = useState("prism");
  const [mode, setMode] = useState<"hub_spoke" | "consensus">("hub_spoke");
  const create = useMutation({
    mutationFn: () => api.createCluster(name.trim(), mode),
    onSuccess: () => {
      toast({ tone: "success", title: `Cluster ${name.trim()} created`, body: "This node is the hub. Invite more nodes next." });
      void qc.invalidateQueries({ queryKey: ["cluster"] });
    },
  });
  return (
    <Card title="Start a cluster" description="Make this node the first member. You can invite others right after.">
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) create.mutate();
        }}
      >
        <Field label="Cluster name" htmlFor="cl-name">
          <Input id="cl-name" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field
          label="Topology"
          hint={mode === "hub_spoke" ? "This node is the hub; others follow it. Simple, and spokes keep running if the hub is away." : "Three or five nodes vote. Survives losing a node, needs a majority online to change anything."}
        >
          <Segmented
            label="Topology"
            value={mode}
            onChange={setMode}
            options={[
              { value: "hub_spoke", label: "Hub and spokes" },
              { value: "consensus", label: "Consensus" },
            ]}
          />
        </Field>
        <ErrorText error={create.error} />
        <Button type="submit" variant="primary" icon="network" loading={create.isPending} disabled={!name.trim()}>
          Create cluster
        </Button>
      </form>
    </Card>
  );
}

function JoinCard({ pendingJoin }: { pendingJoin: boolean }) {
  const [invite, setInvite] = useState("");
  const [reply, setReply] = useState<JoinReply | null>(null);
  const join = useMutation({ mutationFn: () => api.joinCluster(invite.trim()), onSuccess: setReply });
  return (
    <Card title="Join a cluster" description="Paste an invite from any member of the cluster.">
      {reply ? (
        <div className="space-y-4">
          <Callout tone="success" title={`Almost there: ${reply.cluster_name}`}>
            Copy this reply to the inviting node ({reply.sponsor}). It will ask you to confirm the fingerprint below.
          </Callout>
          <Field label="Your reply" aside={<CopyButton text={reply.reply} label="Copy reply" />}>
            <Textarea readOnly rows={4} value={reply.reply} className="font-mono text-xs" onFocus={(e) => e.currentTarget.select()} />
          </Field>
          <Field label="Fingerprint" hint="It must match what the inviting node shows.">
            <div className="rounded-lg bg-zinc-100 px-3 py-2 font-mono text-sm tracking-wide dark:bg-zinc-800">{reply.fingerprint}</div>
          </Field>
          <p className="text-[13px] text-zinc-500">Waiting for approval. This page updates by itself once you are in.</p>
        </div>
      ) : (
        <form
          className="space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (invite.trim()) join.mutate();
          }}
        >
          {pendingJoin && <Callout tone="info">A join is already in progress. Paste the invite again to get the reply.</Callout>}
          <Field label="Invite" htmlFor="cl-invite">
            <Textarea id="cl-invite" rows={4} value={invite} placeholder="Paste the invite here" className="font-mono text-xs" onChange={(e) => setInvite(e.target.value)} />
          </Field>
          <ErrorText error={join.error ? humanError(join.error) : null} />
          <Button type="submit" variant="primary" icon="link" loading={join.isPending} disabled={!invite.trim()}>
            Join cluster
          </Button>
        </form>
      )}
    </Card>
  );
}
