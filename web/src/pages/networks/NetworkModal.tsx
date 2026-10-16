import { useEffect, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type NetworkCheck, type NetworkInfo } from "../../api";
import { toast } from "../../lib/toast";
import { Button, ErrorText, Field, Input, Modal, Segmented, Switch } from "../../components/ui";

/** Create a network (everything optional) or edit an existing one. */
export function NetworkModal({ existing, onClose }: { existing?: NetworkInfo; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(existing?.name ?? "");
  const [kind, setKind] = useState<"nat" | "isolated">(existing?.kind ?? "nat");
  const [ipv6, setIpv6] = useState(existing ? !!existing.subnet6 : true);
  const [nat66, setNat66] = useState(existing ? !!existing.nat66 : true);
  const [custom, setCustom] = useState(false);
  const [subnet, setSubnet] = useState(existing?.subnet ?? "");
  const [allowText, setAllowText] = useState((existing?.allow_prefixes ?? []).join(", "));
  const allow = allowText.split(/[\s,]+/).filter(Boolean);
  const allowOk = allow.every((a) => /^\d{1,3}(\.\d{1,3}){3}\/\d{1,2}$/.test(a) || /^[0-9a-fA-F:]*:[0-9a-fA-F:]*\/\d{1,3}$/.test(a));
  const [check, setCheck] = useState<NetworkCheck | null>(null);

  // Live validation, debounced, against the other networks and the host's own routes.
  const subnetChanged = !!existing && subnet.trim() !== existing.subnet;
  const wantSubnet = existing ? (subnetChanged ? subnet.trim() : "") : custom ? subnet.trim() : "";
  useEffect(() => {
    if (!name.trim() && !wantSubnet) return setCheck(null);
    let stale = false;
    const t = setTimeout(() => {
      api
        .checkNetwork({ id: existing?.id, name: name.trim() || undefined, subnet: wantSubnet || undefined })
        .then((c) => !stale && setCheck(c))
        .catch(() => {
          // A failed check only hides the inline hint; save still validates on the server.
          if (!stale) setCheck(null);
        });
    }, 250);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [name, wantSubnet, existing?.id]);

  const save = useMutation({
    mutationFn: () =>
      existing
        ? api.updateNetwork(existing.id, {
            name: name.trim(),
            kind: kind !== existing.kind ? kind : undefined,
            subnet: subnetChanged ? wantSubnet : undefined,
            allow_prefixes: kind === "nat" ? allow : [],
            ipv6: ipv6 !== !!existing.subnet6 ? ipv6 : undefined,
            nat66: ipv6 && kind === "nat" && nat66 !== !!existing.nat66 ? nat66 : undefined,
          })
        : api.createNetwork({
            name: name.trim() || undefined,
            kind,
            subnet: wantSubnet || undefined,
            allow_prefixes: kind === "nat" && allow.length ? allow : undefined,
            ipv6,
            nat66: ipv6 && kind === "nat" ? nat66 : undefined,
          }),
    onSuccess: (n) => {
      toast({ tone: "success", title: existing ? `${n.name} updated` : `Network ${n.name} created on ${n.subnet}` });
      void qc.invalidateQueries({ queryKey: ["networks"] });
      onClose();
    },
  });
  const subnetFormat = !wantSubnet || /^\d{1,3}(\.\d{1,3}){3}\/\d{1,2}$/.test(wantSubnet);
  const subnetError = !subnetFormat ? "Write it like 10.50.0.0/24." : check?.subnet_error;
  const blocked = !!check?.name_error || !!subnetError || (custom && !existing && !wantSubnet) || (!!existing && !name.trim()) || !allowOk || !!check?.blocking_vms?.length;

  return (
    <Modal
      title={existing ? `Edit ${existing.name}` : "New network"}
      description={existing ? "Machines resolve each other as <machine>.<name>.prism. Changing the subnet moves every address; stopped machines keep their place in the range." : "Leave everything as is for a private network with internet access on a free subnet."}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={blocked} loading={save.isPending} onClick={() => save.mutate()}>{existing ? "Save changes" : "Create network"}</Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="Name" htmlFor="net-name" error={check?.name_error} hint={existing ? undefined : "Optional. Lowercase letters, digits and hyphens."}>
          <Input id="net-name" autoFocus value={name} aria-invalid={!!check?.name_error} placeholder={existing ? undefined : "e.g. lab"} onChange={(e) => setName(e.target.value.toLowerCase())} />
        </Field>
        {(
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
            {existing ? (
              <Field
                label="Subnet"
                htmlFor="net-subnet"
                error={subnetError}
                hint="Machines must be stopped to move to a new range; they get matching addresses automatically."
              >
                <Input id="net-subnet" className="font-mono" value={subnet} aria-invalid={!!subnetError} onChange={(e) => setSubnet(e.target.value)} />
                {check?.subnet_error && check.suggestion && (
                  <button type="button" className="mt-1.5 text-xs text-indigo-600 hover:underline dark:text-indigo-400" onClick={() => setSubnet(check.suggestion ?? "")}>
                    Use {check.suggestion} instead
                  </button>
                )}
                {!!check?.blocking_vms?.length && (
                  <p role="alert" className="mt-1.5 text-xs text-amber-600 dark:text-amber-400">
                    Stop {check.blocking_vms.join(", ")} first. Running machines would keep their old address and lose the network.
                  </p>
                )}
              </Field>
            ) : !custom ? (
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
        <Switch
          label="IPv6"
          checked={ipv6}
          onChange={setIpv6}
          description={
            existing?.subnet6
              ? <>Machines get an address in <span className="font-mono">{existing.subnet6}</span> automatically and resolve each other over IPv6 too.</>
              : "Machines get a private IPv6 address automatically and resolve each other over IPv6 too. Prism picks a free range."
          }
        />
        {ipv6 && kind === "nat" && (
          <Switch
            label="IPv6 internet access"
            checked={nat66}
            onChange={setNat66}
            description="Translate IPv6 traffic from the private range to the host's address, so machines reach IPv6 sites without a public range of their own."
          />
        )}
        {kind === "nat" && (
          <Field
            label="Allowed destinations"
            htmlFor="net-allow"
            hint="Optional. Machines normally cannot reach the tailnet (100.64.0.0/10, fd7a:115c:a1e0::/48) or the cluster's own addresses. List IPv4 or IPv6 prefixes, like 100.64.2.0/24 or fd7a:115c:a1e0:ab12::/64, to let them through."
            error={allowOk ? undefined : "Write prefixes like 100.64.2.0/24 or fd7a:115c:a1e0:ab12::/64, separated by commas."}
          >
            <Input id="net-allow" className="font-mono" value={allowText} aria-invalid={!allowOk} placeholder="none" onChange={(e) => setAllowText(e.target.value)} />
          </Field>
        )}
        <ErrorText error={save.error} />
      </div>
    </Modal>
  );
}
