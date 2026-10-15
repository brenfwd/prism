import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type AcceptPreview, type InviteView } from "../../api";
import { toast } from "../../lib/toast";
import { Button, Callout, CopyButton, ErrorText, Field, Input, Modal, Textarea } from "../../components/ui";

/** Two steps: hand out an invite, then check and admit the reply. */
export function InviteModal({ onClose }: { onClose: () => void }) {
  const qc = useQueryClient();
  const [hint, setHint] = useState("");
  const [invite, setInvite] = useState<InviteView | null>(null);
  const [reply, setReply] = useState("");
  const [preview, setPreview] = useState<AcceptPreview | null>(null);
  const make = useMutation({
    mutationFn: () => api.createInvite(hint.trim()),
    onSuccess: (v) => {
      setInvite(v);
      void qc.invalidateQueries({ queryKey: ["cluster-invites"] });
    },
  });
  const check = useMutation({ mutationFn: () => api.acceptReply(reply.trim(), false), onSuccess: setPreview });
  const admit = useMutation({
    mutationFn: () => api.acceptReply(reply.trim(), true),
    onSuccess: (p) => {
      toast({ tone: "success", title: `${p.name} is joining`, body: "It will show as active in a moment." });
      void qc.invalidateQueries({ queryKey: ["cluster"] });
      void qc.invalidateQueries({ queryKey: ["cluster-invites"] });
      onClose();
    },
  });
  return (
    <Modal
      size="lg"
      title={invite ? "Send this invite to the new node" : "Invite a node"}
      description={invite ? "Run Join on the new node's Cluster page and paste it. It expires in 24 hours." : "Creates a one-time invite. It carries no secrets."}
      onClose={onClose}
      footer={
        invite ? (
          <Button onClick={onClose}>Done</Button>
        ) : (
          <>
            <Button variant="ghost" onClick={onClose}>Cancel</Button>
            <Button variant="primary" loading={make.isPending} onClick={() => make.mutate()}>Create invite</Button>
          </>
        )
      }
    >
      {!invite ? (
        <div className="space-y-3">
          <Field label="Node name (optional)" hint="A suggestion the new node can keep or change." htmlFor="inv-name">
            <Input id="inv-name" value={hint} placeholder="e.g. rack2-host1" onChange={(e) => setHint(e.target.value)} />
          </Field>
          <ErrorText error={make.error} />
        </div>
      ) : (
        <div className="space-y-5">
          <Field label="Invite" aside={<CopyButton text={invite.blob} label="Copy invite" />}>
            <Textarea readOnly rows={4} value={invite.blob} className="font-mono text-xs" onFocus={(e) => e.currentTarget.select()} />
          </Field>
          <div className="border-t border-zinc-200 pt-4 dark:border-zinc-800">
            <Field label="Reply from the new node" hint="Paste what the new node gave you to finish." htmlFor="inv-reply">
              <Textarea id="inv-reply" rows={3} value={reply} className="font-mono text-xs" onChange={(e) => { setReply(e.target.value); setPreview(null); }} />
            </Field>
            {!preview ? (
              <Button className="mt-3" icon="check" loading={check.isPending} disabled={!reply.trim()} onClick={() => check.mutate()}>
                Review reply
              </Button>
            ) : (
              <div className="mt-3 space-y-3">
                <Callout tone="warning" title={`Admit ${preview.name}?`}>
                  Members are fully trusted. Confirm this fingerprint matches what the new node displays.
                </Callout>
                <div className="rounded-lg bg-zinc-100 px-3 py-2 font-mono text-sm tracking-wide dark:bg-zinc-800">{preview.fingerprint}</div>
                <Button variant="primary" icon="check" loading={admit.isPending} onClick={() => admit.mutate()}>
                  Fingerprint matches, admit
                </Button>
              </div>
            )}
            <ErrorText error={check.error ?? admit.error} className="mt-2" />
          </div>
        </div>
      )}
    </Modal>
  );
}
