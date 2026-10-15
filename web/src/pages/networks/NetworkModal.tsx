import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type NetworkCheck, type NetworkInfo } from "../../api";
import { toast } from "../../lib/toast";
import { Button, ErrorText, Field, Input, Modal, Segmented } from "../../components/ui";

/** Create a network (everything optional) or rename an existing one. */
export function NetworkModal({ existing, onClose }: { existing?: NetworkInfo; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(existing?.name ?? "");
  const [kind, setKind] = useState<"nat" | "isolated">("nat");
  const [custom, setCustom] = useState(false);
  const [subnet, setSubnet] = useState("");
  const [check, setCheck] = useState<NetworkCheck | null>(null);

  // Live validation, debounced, against the other networks and the host's own routes.
  const wantSubnet = custom && !existing ? subnet.trim() : "";
  useEffect(() => {
    if (!name.trim() && !wantSubnet) return setCheck(null);
    let stale = false;
    const t = setTimeout(() => {
      api
        .checkNetwork({ id: existing?.id, name: name.trim() || undefined, subnet: wantSubnet || undefined })
        .then((c) => !stale && setCheck(c))
        .catch(() => !stale && setCheck(null));
    }, 250);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [name, wantSubnet, existing?.id]);

  const save = useMutation({
    mutationFn: () =>
      existing
        ? api.updateNetwork(existing.id, { name: name.trim() })
        : api.createNetwork({ name: name.trim() || undefined, kind, subnet: wantSubnet || undefined }),
    onSuccess: (n) => {
      toast({ tone: "success", title: existing ? `Renamed to ${n.name}` : `Network ${n.name} created on ${n.subnet}` });
      void qc.invalidateQueries({ queryKey: ["networks"] });
      onClose();
    },
  });
  const subnetFormat = !wantSubnet || /^\d{1,3}(\.\d{1,3}){3}\/\d{1,2}$/.test(wantSubnet);
  const subnetError = !subnetFormat ? "Write it like 10.50.0.0/24." : check?.subnet_error;
  const blocked = !!check?.name_error || !!subnetError || (custom && !existing && !wantSubnet) || (!!existing && !name.trim());

  return (
    <Modal
      title={existing ? `Rename ${existing.name}` : "New network"}
      description={existing ? "Machines resolve each other as <machine>.<name>.prism, so the new name applies to DNS right away." : "Leave everything as is for a private network with internet access on a free subnet."}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={blocked} loading={save.isPending} onClick={() => save.mutate()}>{existing ? "Rename" : "Create network"}</Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="Name" htmlFor="net-name" error={check?.name_error} hint={existing ? undefined : "Optional. Lowercase letters, digits and hyphens."}>
          <Input id="net-name" autoFocus value={name} aria-invalid={!!check?.name_error} placeholder={existing ? undefined : "e.g. lab"} onChange={(e) => setName(e.target.value.toLowerCase())} />
        </Field>
        {!existing && (
          <>
            <Field label="Access" hint={kind === "nat" ? "Machines can reach the internet and each other." : "Machines can only talk to each other. No internet, no gateway."}>
              <Segmented
                label="Network kind"
                value={kind}
                onChange={setKind}
                options={[
                  { value: "nat", label: "Internet access" },
                  { value: "isolated", label: "Isolated" },
                ]}
              />
            </Field>
            {!custom ? (
              <button type="button" className="text-[13px] text-indigo-600 hover:underline dark:text-indigo-400" onClick={() => setCustom(true)}>
                Choose the subnet myself
              </button>
            ) : (
              <Field
                label="Subnet"
                htmlFor="net-subnet"
                error={subnetError}
                hint="A /16 to /29 range that no other network or route on this host uses."
                aside={<button type="button" className="hover:underline" onClick={() => { setCustom(false); setSubnet(""); }}>Pick for me</button>}
              >
                <Input id="net-subnet" className="font-mono" value={subnet} aria-invalid={!!subnetError} placeholder="10.50.0.0/24" onChange={(e) => setSubnet(e.target.value)} />
                {check?.subnet_error && check.suggestion && (
                  <button type="button" className="mt-1.5 text-xs text-indigo-600 hover:underline dark:text-indigo-400" onClick={() => setSubnet(check.suggestion ?? "")}>
                    Use {check.suggestion} instead
                  </button>
                )}
              </Field>
            )}
          </>
        )}
        <ErrorText error={save.error} />
      </div>
    </Modal>
  );
}
