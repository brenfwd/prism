import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { api, type BalloonMode, type CpuModel, type CreateVm, type Firmware, type Volume } from "../api";
import { putVm } from "../events";
import { defaultUser, invalidKey, sshKeyLines, userData } from "../lib/cloudinit";
import { GiB, bytes, memLabel } from "../lib/format";
import { hostAvailableBytes, looksLikeVmDisk, runAction, useAllVolumes, useNode, usePools, useVms, volumeUsers } from "../lib/hooks";
import { local, useStore } from "../lib/store";
import { startUpload, uploads } from "../lib/tasks";
import { toast } from "../lib/toast";
import { DropBox } from "../components/DropZone";
import { Icon, type IconName } from "../components/icons";
import {
  Badge, Button, Callout, Card, Checkbox, ErrorText, Field, Input, Kbd, Progress, Segmented, Select, Switch, Textarea, cx, modKey,
} from "../components/ui";

export interface NewVmSearch {
  source?: Source;
  pool?: string;
  volume?: string;
}

type Source = "image" | "iso" | "blank";
type PresetId = "small" | "medium" | "large" | "custom";

const presets: Record<Exclude<PresetId, "custom">, { label: string; vcpus: number; mem: number; disk: number; blurb: string }> = {
  small: { label: "Small", vcpus: 1, mem: 1024, disk: 10, blurb: "Services, tests, a quick sandbox" },
  medium: { label: "Medium", vcpus: 2, mem: 4096, disk: 32, blurb: "Web apps, build agents, desktops" },
  large: { label: "Large", vcpus: 4, mem: 8192, disk: 64, blurb: "Databases and heavier work" },
};

const NAME_RE = /^[A-Za-z0-9_][A-Za-z0-9._-]{0,63}$/;
const KEY_STORE = "prism.lastSshKey";
const USER_STORE = "prism.lastUser";

function baseName(volume: string | undefined, source: Source): string {
  if (!volume) return source === "blank" ? "vm" : "vm";
  const word = volume.toLowerCase().replace(/\.(img|qcow2|raw|iso)$/, "").match(/[a-z]+/)?.[0];
  return word && word.length > 1 ? word : "vm";
}

function suggestName(base: string, taken: Set<string>): string {
  for (let i = 1; ; i++) if (!taken.has(`${base}-${i}`)) return `${base}-${i}`;
}

const hostnameOf = (name: string) => name.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/^-+|-+$/g, "").slice(0, 63) || "vm";

export function NewVmPage({ search }: { search: NewVmSearch }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const node = useNode();
  const vms = useVms().data;
  const pools = usePools().data ?? [];
  const { data: volumes, loading: volumesLoading } = useAllVolumes();
  const users = useMemo(() => volumeUsers(vms), [vms]);
  const taken = useMemo(() => new Set((vms ?? []).map((v) => v.name)), [vms]);

  const [source, setSource] = useState<Source>(search.source ?? "image");
  const [picked, setPicked] = useState<{ pool: string; name: string } | null>(
    search.volume && search.pool ? { pool: search.pool, name: search.volume } : null,
  );

  // Candidates for each source: free-standing disk images, or ISOs.
  const images = useMemo(
    () => volumes.filter((v) => v.kind === "disk" && !users.has(`${v.pool}/${v.name}`) && !looksLikeVmDisk(v.name)).sort((a, b) => Number(b.name.endsWith(".img")) - Number(a.name.endsWith(".img"))),
    [volumes, users],
  );
  const isos = useMemo(() => volumes.filter((v) => v.kind === "iso"), [volumes]);
  const candidates = source === "image" ? images : source === "iso" ? isos : [];
  const sourceVol: (Volume & { pool: string }) | undefined =
    source === "blank" ? undefined : candidates.find((v) => v.pool === picked?.pool && v.name === picked?.name) ?? (picked ? undefined : candidates[0]);

  // Auto-select the first candidate when switching source.
  useEffect(() => {
    if (source === "blank") return;
    if (!picked || !candidates.some((v) => v.pool === picked.pool && v.name === picked.name)) {
      if (candidates[0]) setPicked({ pool: candidates[0].pool, name: candidates[0].name });
    }
  }, [source, candidates, picked]);

  // ---- name
  const [name, setName] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  const suggested = suggestName(baseName(sourceVol?.name, source), taken);
  useEffect(() => {
    if (!nameTouched) setName(suggested);
  }, [suggested, nameTouched]);
  const nameError = !name
    ? "Give it a name."
    : !NAME_RE.test(name)
      ? "Use letters, digits, “.”, “_” or “-” (no spaces), up to 64."
      : taken.has(name)
        ? `There's already a machine called ${name}.`
        : null;

  // ---- size
  const avail = hostAvailableBytes(node);
  const [preset, setPreset] = useState<PresetId>("small");
  const [presetTouched, setPresetTouched] = useState(false);
  useEffect(() => {
    if (!presetTouched && avail != null) setPreset(avail >= 10 * GiB ? "medium" : "small");
  }, [avail, presetTouched]);
  const [custom, setCustom] = useState({ vcpus: 2, mem: 2048, disk: 20 });
  const base = preset === "custom" ? custom : presets[preset];
  const minDiskGib = sourceVol && source === "image" ? Math.ceil(sourceVol.size_bytes / GiB) : 1;
  const diskGib = Math.max(base.disk, minDiskGib);
  const memMib = base.mem;
  const vcpus = base.vcpus;

  // ---- access (cloud-init)
  const [username, setUsername] = useState(() => local.get(USER_STORE) ?? "");
  const [userTouched, setUserTouched] = useState(!!local.get(USER_STORE));
  useEffect(() => {
    if (!userTouched) setUsername(defaultUser(sourceVol?.name ?? ""));
  }, [sourceVol?.name, userTouched]);
  const [sshKeys, setSshKeys] = useState(() => local.get(KEY_STORE) ?? "");
  const [password, setPassword] = useState("");
  const [showPw, setShowPw] = useState(false);
  const [packages, setPackages] = useState<string[]>(["qemu-guest-agent"]);
  const [upgrade, setUpgrade] = useState(false);
  const [raw, setRaw] = useState<string | null>(null);
  const [showRaw, setShowRaw] = useState(false);
  const generated = userData({ hostname: hostnameOf(name), username, sshKeys, password, packages, upgrade });
  const badKey = invalidKey(sshKeys);
  const noLogin = source === "image" && raw == null && !sshKeyLines(sshKeys).length && !password;

  // ---- network & advanced
  const [net, setNet] = useState<"user" | "bridge">("user");
  const [bridge, setBridge] = useState("br0");
  const [firmware, setFirmware] = useState<Firmware>("bios");
  const [cpuModel, setCpuModel] = useState<CpuModel>("host_model");
  const [balloon, setBalloon] = useState<BalloonMode>("auto");
  const [autostart, setAutostart] = useState(false);
  const [startNow, setStartNow] = useState(true);
  const [description, setDescription] = useState("");
  const [blankPool, setBlankPool] = useState<string | undefined>();
  const pool = sourceVol?.pool ?? blankPool ?? pools[0]?.name;
  const poolInfo = pools.find((p) => p.name === pool);

  // ---- capacity checks
  const memBytes = memMib * 1024 * 1024;
  const memOver = avail != null && memBytes > avail;
  const memTight = avail != null && !memOver && memBytes > avail * 0.8;
  const diskBytes = source === "iso" || source === "blank" || source === "image" ? diskGib * GiB : 0;
  const diskOver = poolInfo != null && diskBytes > poolInfo.available_bytes;

  const missingSource = source !== "blank" && !sourceVol;
  const blockers = [nameError, missingSource ? (source === "image" ? "Pick a cloud image." : "Pick an ISO.") : null, badKey && source === "image" && raw == null ? "Fix the SSH key." : null, !pool ? "Create a storage pool first." : null].filter(Boolean) as string[];

  const create = useMutation({
    mutationFn: async () => {
      const req: CreateVm = {
        name,
        vcpus,
        memory_mib: memMib,
        balloon,
        firmware,
        cpu_model: cpuModel,
        pool: pool!,
        disk_gib: diskGib,
        network: net === "bridge" ? { type: "bridge", bridge } : { type: "user" },
        autostart,
        description: description || undefined,
      };
      if (source === "image" && sourceVol) {
        req.image = sourceVol.name;
        req.cloud_init = { user_data: raw ?? generated };
      }
      if (source === "iso" && sourceVol) req.iso = sourceVol.name;
      return api.createVm(req);
    },
    onSuccess: (vm) => {
      putVm(qc, vm);
      if (source === "image") {
        if (sshKeys.trim()) local.set(KEY_STORE, sshKeys.trim());
        local.set(USER_STORE, username);
      }
      toast({ tone: "success", title: `${vm.name} created`, body: startNow ? "Starting it up…" : undefined });
      if (startNow) void runAction(qc, vm, "start");
      navigate({ to: "/vms/$id", params: { id: vm.id }, search: { tab: startNow && source !== "image" ? "console" : "overview" } });
    },
  });

  const submit = () => {
    if (blockers.length || create.isPending) return;
    create.mutate();
  };
  const submitRef = useRef(submit);
  submitRef.current = submit;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        submitRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const sources: { id: Source; title: string; body: string; icon: IconName; badge?: string }[] = [
    { id: "image", title: "Cloud image", body: "Boots in seconds, set up for you with your SSH key.", icon: "cloud", badge: "Fastest" },
    { id: "iso", title: "Install from ISO", body: "Run an OS installer on a fresh, empty disk.", icon: "disc" },
    { id: "blank", title: "Blank machine", body: "An empty disk. Network boot or attach media later.", icon: "disk" },
  ];

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
      className="animate-in"
    >
      <div className="mb-6">
        <h1 className="text-2xl font-semibold tracking-tight">New machine</h1>
        <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">Pick what it boots, give it a size, and go. Everything here can be changed later.</p>
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_19rem]">
        <div className="space-y-5">
          {/* 1. Source */}
          <Card title={<Step n={1}>Start from</Step>}>
            <div role="radiogroup" aria-label="Start from" className="grid gap-3 sm:grid-cols-3">
              {sources.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  role="radio"
                  aria-checked={source === s.id}
                  onClick={() => setSource(s.id)}
                  className={cx(
                    "relative flex flex-col items-start rounded-xl p-3.5 text-left ring-1 transition-all ring-inset",
                    source === s.id
                      ? "bg-indigo-50/70 ring-2 ring-indigo-500 dark:bg-indigo-500/10 dark:ring-indigo-400"
                      : "ring-zinc-200 hover:bg-zinc-50 hover:ring-zinc-300 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
                  )}
                >
                  <span className={cx("mb-2.5 grid size-8 place-items-center rounded-lg", source === s.id ? "bg-indigo-600 text-white dark:bg-indigo-500" : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300")}>
                    <Icon name={s.icon} className="size-[18px]" />
                  </span>
                  <span className="text-sm font-semibold">{s.title}</span>
                  <span className="mt-0.5 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">{s.body}</span>
                  {s.badge && <Badge tone="indigo" className="absolute top-3 right-3">{s.badge}</Badge>}
                </button>
              ))}
            </div>
            {source !== "blank" && (
              <SourcePicker
                kind={source}
                items={candidates}
                loading={volumesLoading}
                picked={sourceVol}
                onPick={(v) => setPicked({ pool: v.pool, name: v.name })}
                defaultPool={pools[0]?.name}
                onUploaded={(pool, name) => setPicked({ pool, name })}
              />
            )}
          </Card>

          {/* 2. Name & size */}
          <Card title={<Step n={2}>Name and size</Step>}>
            <Field
              label="Name"
              htmlFor="vm-name"
              error={nameTouched || name !== suggested ? nameError : null}
              hint={source === "image" ? <>Also used as the hostname (<span className="font-mono">{hostnameOf(name)}</span>).</> : "Letters, digits, dots, dashes."}
            >
              <div className="relative">
                <Input
                  id="vm-name"
                  value={name}
                  aria-invalid={!!nameError && (nameTouched || name !== suggested)}
                  onChange={(e) => {
                    setNameTouched(true);
                    setName(e.target.value.replace(/\s/g, "-"));
                  }}
                  spellCheck={false}
                  autoComplete="off"
                  className="pr-24 font-medium"
                />
                {!nameError && <span className="absolute top-1/2 right-3 flex -translate-y-1/2 items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"><Icon name="check" className="size-3.5" />Available</span>}
              </div>
            </Field>

            <div className="mt-5">
              <div className="mb-1.5 text-[13px] font-medium text-zinc-700 dark:text-zinc-300">Size</div>
              <div role="radiogroup" aria-label="Size" className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
                {(Object.keys(presets) as Exclude<PresetId, "custom">[]).map((id) => {
                  const p = presets[id];
                  const tooBig = avail != null && p.mem * 1024 * 1024 > avail;
                  return (
                    <PresetCard key={id} active={preset === id} onClick={() => (setPreset(id), setPresetTouched(true))} title={p.label} warn={tooBig ? "More than free memory" : undefined}>
                      <span className="tabular-nums">{p.vcpus} vCPU · {memLabel(p.mem)}</span>
                      <span className="tabular-nums">{Math.max(p.disk, minDiskGib)} GiB disk</span>
                    </PresetCard>
                  );
                })}
                <PresetCard active={preset === "custom"} onClick={() => (setPreset("custom"), setPresetTouched(true))} title="Custom">
                  <span>Your numbers</span>
                  <span className="tabular-nums">{custom.vcpus} · {memLabel(custom.mem)} · {custom.disk} G</span>
                </PresetCard>
              </div>
              {preset !== "custom" && <p className="mt-2 text-xs text-zinc-500">{presets[preset].blurb}.</p>}
              {preset === "custom" && (
                <div className="mt-4 grid gap-4 sm:grid-cols-3">
                  <Field label="vCPUs" htmlFor="c-cpu" hint={node ? `This host has ${node.host.cpus}.` : undefined}>
                    <Input id="c-cpu" type="number" min={1} max={512} value={custom.vcpus} onChange={(e) => setCustom({ ...custom, vcpus: Math.max(1, +e.target.value) })} />
                  </Field>
                  <Field label="Memory (MiB)" htmlFor="c-mem" hint={avail != null ? `${bytes(avail)} free on the host.` : undefined}>
                    <Input id="c-mem" type="number" min={128} step={256} value={custom.mem} onChange={(e) => setCustom({ ...custom, mem: Math.max(64, +e.target.value) })} />
                  </Field>
                  <Field label="Disk (GiB)" htmlFor="c-disk" hint={minDiskGib > 1 ? `At least ${minDiskGib} GiB for this image.` : undefined}>
                    <Input id="c-disk" type="number" min={minDiskGib} value={custom.disk} onChange={(e) => setCustom({ ...custom, disk: Math.max(1, +e.target.value) })} />
                  </Field>
                </div>
              )}
              {node?.memory_pressure?.level === "red" && (
                <Callout tone="warning" className="mt-4" title="The host is short on memory right now">
                  Prism won't start new machines until the pressure eases. You can still create this one and start it later.
                </Callout>
              )}
              {memOver && (
                <Callout tone="warning" className="mt-4" title={`${memLabel(memMib)} is more than the ${bytes(avail!)} this host has free`}>
                  The machine may fail to start, or push the host into swapping. Pick a smaller size, or stop another machine first.
                </Callout>
              )}
              {memTight && (
                <Callout tone="info" className="mt-4">
                  That uses most of the host's free memory. With automatic ballooning on, Prism hands unused guest memory back when the host gets tight.
                </Callout>
              )}
              {diskOver && (
                <Callout tone="warning" className="mt-4" title="Bigger than the free space in this pool">
                  Disks are thin, so this works until the guest actually fills it, then writes will fail. {bytes(poolInfo!.available_bytes)} is free in {pool}.
                </Callout>
              )}
            </div>
          </Card>

          {/* 3. Access */}
          {source === "image" && (
            <Card title={<Step n={3}>Sign-in</Step>} description="Prism writes a cloud-init config so the machine is ready to use on first boot.">
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Username" htmlFor="ci-user">
                  <Input id="ci-user" value={username} onChange={(e) => (setUsername(e.target.value.replace(/[^a-z0-9_-]/gi, "")), setUserTouched(true))} disabled={raw != null} autoComplete="off" spellCheck={false} />
                </Field>
                <Field label="Password" htmlFor="ci-pw" hint="Optional. Leave empty for key-only SSH." aside={password && <button type="button" onClick={() => setShowPw(!showPw)} className="text-indigo-600 hover:underline dark:text-indigo-400">{showPw ? "Hide" : "Show"}</button>}>
                  <Input id="ci-pw" type={showPw ? "text" : "password"} value={password} onChange={(e) => setPassword(e.target.value)} disabled={raw != null} autoComplete="new-password" />
                </Field>
              </div>
              <Field
                className="mt-4"
                label={<span className="inline-flex items-center gap-1.5"><Icon name="key" className="size-3.5" />SSH public key</span>}
                htmlFor="ci-key"
                aside={local.get(KEY_STORE) && sshKeys === local.get(KEY_STORE) ? "Remembered from last time" : undefined}
                error={badKey ? `This doesn't look like a public key: “${badKey.slice(0, 32)}…”` : undefined}
                hint={<>Paste the contents of <span className="font-mono">~/.ssh/id_ed25519.pub</span>. One key per line.</>}
              >
                <Textarea
                  id="ci-key"
                  rows={3}
                  value={sshKeys}
                  onChange={(e) => setSshKeys(e.target.value)}
                  placeholder="ssh-ed25519 AAAAC3Nza… you@laptop"
                  disabled={raw != null}
                  aria-invalid={!!badKey}
                  spellCheck={false}
                  className="break-all"
                />
              </Field>
              <PackageInput className="mt-4" value={packages} onChange={setPackages} disabled={raw != null} />
              <div className="mt-4">
                <Checkbox checked={upgrade} onChange={setUpgrade} disabled={raw != null}>
                  Install OS updates on first boot <span className="text-zinc-500">(slower first boot)</span>
                </Checkbox>
              </div>
              {noLogin && (
                <Callout tone="warning" className="mt-4">
                  Without an SSH key or a password you won't be able to sign in, except through the serial console if the image allows it.
                </Callout>
              )}
              <div className="mt-5 border-t border-zinc-100 pt-4 dark:border-zinc-800">
                <button type="button" onClick={() => setShowRaw(!showRaw)} className="flex items-center gap-1.5 text-[13px] font-medium text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100" aria-expanded={showRaw}>
                  <Icon name={showRaw ? "chevronDown" : "chevronRight"} className="size-4" />
                  <Icon name="code" className="size-4" />
                  {raw != null ? "Custom user-data" : "Edit raw user-data"}
                  {raw != null && <Badge tone="amber">edited</Badge>}
                </button>
                {showRaw && (
                  <div className="mt-3">
                    <Textarea rows={14} value={raw ?? generated} onChange={(e) => setRaw(e.target.value)} spellCheck={false} aria-label="cloud-init user-data" />
                    <div className="mt-2 flex items-center justify-between text-xs text-zinc-500">
                      <span>{raw != null ? "Your edits replace the fields above." : "Edits here take over from the fields above."}</span>
                      {raw != null && <Button size="xs" variant="ghost" icon="rotate" onClick={() => setRaw(null)}>Back to generated</Button>}
                    </div>
                  </div>
                )}
              </div>
            </Card>
          )}

          {/* 4. Network */}
          <Card title={<Step n={source === "image" ? 4 : 3}>Network</Step>}>
            <div role="radiogroup" aria-label="Network" className="grid gap-2.5 sm:grid-cols-2">
              <Choice active={net === "user"} onClick={() => setNet("user")} icon="cloud" title="Private (NAT)" body="Reaches the internet through the host. Works everywhere; not reachable from your LAN." />
              <Choice active={net === "bridge"} onClick={() => setNet("bridge")} icon="network" title="Bridged to a LAN" body="Joins a host bridge and gets an address from that network, like a physical box." />
            </div>
            {net === "user" && source === "image" && (
              <p className="mt-3 flex items-start gap-1.5 text-xs leading-relaxed text-zinc-500">
                <Icon name="info" className="mt-0.5 size-3.5 shrink-0" />
                With private NAT you sign in through the Serial or Console tab. Pick Bridged if you want to SSH in from your network.
              </p>
            )}
            {net === "bridge" && (
              <Field className="mt-4 max-w-xs" label="Host bridge" htmlFor="br" hint="The bridge interface on this host, e.g. br0.">
                <Input id="br" value={bridge} onChange={(e) => setBridge(e.target.value)} spellCheck={false} className="font-mono" />
              </Field>
            )}
          </Card>

          {/* Advanced */}
          <details className="group rounded-xl bg-white ring-1 ring-zinc-200/90 dark:bg-zinc-900/70 dark:ring-zinc-800">
            <summary className="flex list-none items-center gap-2 px-5 py-4 text-[15px] font-semibold tracking-tight [&::-webkit-details-marker]:hidden">
              <Icon name="chevronRight" className="size-4 transition-transform group-open:rotate-90" />
              Advanced
              <span className="ml-1 text-[13px] font-normal text-zinc-500">firmware, CPU model, memory ballooning, storage pool</span>
            </summary>
            <div className="grid gap-5 px-5 pb-5 sm:grid-cols-2">
              <Field label="Firmware" hint={firmware === "uefi" ? "Needed for Secure Boot and some newer OSes." : "Classic BIOS. Works with nearly every image."}>
                <Segmented label="Firmware" value={firmware} onChange={setFirmware} options={[{ value: "bios", label: "BIOS" }, { value: "uefi", label: "UEFI" }]} />
              </Field>
              <Field label="CPU model" hint={cpuModel === "host_model" ? "The host CPU's features; can move between similar hosts." : "Exactly the host CPU: fastest, but can only move to identical hosts."}>
                <Segmented label="CPU model" value={cpuModel} onChange={setCpuModel} options={[{ value: "host_model", label: "Host model" }, { value: "host_passthrough", label: "Passthrough" }]} />
              </Field>
              <Field
                label="Memory ballooning"
                hint={
                  balloon === "auto"
                    ? "Prism reclaims memory the guest isn't using when the host is under pressure, never below a quarter of its size."
                    : balloon === "manual"
                      ? "You can resize the guest's memory live from its page."
                      : "The guest always keeps its full memory."
                }
              >
                <Segmented label="Memory ballooning" value={balloon} onChange={setBalloon} options={[{ value: "auto", label: "Automatic" }, { value: "manual", label: "Manual" }, { value: "off", label: "Off" }]} />
              </Field>
              <Field label="Storage pool" hint={source === "blank" ? "Where the new disk lives." : "The disk is created next to the source image."}>
                <Select value={pool ?? ""} disabled={source !== "blank"} onChange={(e) => setBlankPool(e.target.value)}>
                  {pools.map((p) => (
                    <option key={p.name} value={p.name}>{p.name} · {bytes(p.available_bytes)} free</option>
                  ))}
                </Select>
              </Field>
              <Field label="Description" className="sm:col-span-2">
                <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="What's it for?" />
              </Field>
              <div className="sm:col-span-2">
                <Switch checked={autostart} onChange={setAutostart} label="Start when the host boots" description="Prism starts it automatically after the host restarts." />
              </div>
            </div>
          </details>
        </div>

        {/* Summary */}
        <aside className="lg:sticky lg:top-20">
          <Card className="overflow-hidden" padded={false}>
            <div className="border-b border-zinc-100 bg-gradient-to-b from-indigo-50/80 to-transparent px-5 pt-5 pb-4 dark:border-zinc-800 dark:from-indigo-500/10">
              <div className="text-xs font-medium text-zinc-500">Summary</div>
              <div className="mt-1 truncate text-lg font-semibold tracking-tight">{name || "Unnamed"}</div>
              <div className="mt-0.5 truncate text-[13px] text-zinc-500">
                {source === "blank" ? "Blank disk" : sourceVol ? sourceVol.name : source === "image" ? "No image picked" : "No ISO picked"}
              </div>
            </div>
            <dl className="space-y-2.5 px-5 py-4 text-[13px]">
              <SumRow icon="cpu" k="vCPUs" v={vcpus} />
              <SumRow icon="memory" k="Memory" v={memLabel(memMib)} warn={memOver} />
              <SumRow icon="disk" k="Disk" v={`${diskGib} GiB`} warn={diskOver} />
              <SumRow icon="network" k="Network" v={net === "user" ? "Private NAT" : bridge} />
              {source === "image" && <SumRow icon="key" k="Sign-in" v={raw != null ? "Custom" : `${username || "?"}${sshKeyLines(sshKeys).length ? " · key" : ""}${password ? " · password" : ""}`} warn={noLogin} />}
            </dl>
            {avail != null && node && (
              <div className="px-5 pb-4">
                <CapacityBar label="Host memory" used={node.host.memory_kib * 1024 - avail} adding={memBytes} total={node.host.memory_kib * 1024} />
                {poolInfo && <CapacityBar className="mt-3" label={`Pool ${poolInfo.name}`} used={poolInfo.capacity_bytes - poolInfo.available_bytes} adding={diskBytes} total={poolInfo.capacity_bytes} note="max, thin" />}
              </div>
            )}
            <div className="space-y-3 border-t border-zinc-100 px-5 py-4 dark:border-zinc-800">
              <Checkbox checked={startNow} onChange={setStartNow}>Start it right away</Checkbox>
              <ErrorText error={create.error} />
              <Button type="submit" variant="primary" size="lg" className="w-full" loading={create.isPending} disabled={blockers.length > 0}>
                {create.isPending ? (source === "image" ? "Copying image…" : "Creating…") : "Create machine"}
              </Button>
              {blockers.length > 0 ? (
                <p className="text-center text-xs text-zinc-500">{blockers[0]}</p>
              ) : (
                <p className="hidden items-center justify-center gap-1 text-xs text-zinc-400 sm:flex">
                  or press <Kbd>{modKey}</Kbd><Kbd>↵</Kbd>
                </p>
              )}
            </div>
          </Card>
        </aside>
      </div>
    </form>
  );
}

function Step({ n, children }: { n: number; children: ReactNode }) {
  return (
    <span className="flex items-center gap-2.5">
      <span className="grid size-6 place-items-center rounded-full bg-zinc-900 text-xs font-semibold text-white dark:bg-white dark:text-zinc-900">{n}</span>
      {children}
    </span>
  );
}

function PresetCard({ active, onClick, title, children, warn }: { active: boolean; onClick: () => void; title: string; children: ReactNode; warn?: string }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      onClick={onClick}
      className={cx(
        "flex flex-col items-start gap-0.5 rounded-xl px-3.5 py-3 text-left text-xs text-zinc-500 ring-1 transition-all ring-inset dark:text-zinc-400",
        active ? "bg-indigo-50/70 ring-2 ring-indigo-500 dark:bg-indigo-500/10 dark:ring-indigo-400" : "ring-zinc-200 hover:bg-zinc-50 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
      )}
    >
      <span className="mb-0.5 flex w-full items-center justify-between text-sm font-semibold text-zinc-900 dark:text-zinc-100">
        {title}
        {warn && <span title={warn}><Icon name="alert" className="size-3.5 text-amber-500" /></span>}
      </span>
      {children}
    </button>
  );
}

function Choice({ active, onClick, icon, title, body }: { active: boolean; onClick: () => void; icon: IconName; title: string; body: string }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      onClick={onClick}
      className={cx(
        "flex items-start gap-3 rounded-xl p-3.5 text-left ring-1 transition-all ring-inset",
        active ? "bg-indigo-50/70 ring-2 ring-indigo-500 dark:bg-indigo-500/10 dark:ring-indigo-400" : "ring-zinc-200 hover:bg-zinc-50 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
      )}
    >
      <Icon name={icon} className={cx("mt-0.5 size-[18px]", active ? "text-indigo-600 dark:text-indigo-400" : "text-zinc-400")} />
      <span>
        <span className="block text-sm font-semibold">{title}</span>
        <span className="mt-0.5 block text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">{body}</span>
      </span>
    </button>
  );
}

function SumRow({ icon, k, v, warn }: { icon: IconName; k: string; v: ReactNode; warn?: boolean }) {
  return (
    <div className="flex items-center gap-2.5">
      <Icon name={icon} className="size-4 text-zinc-400" />
      <dt className="text-zinc-500">{k}</dt>
      <dd className={cx("ml-auto truncate font-medium tabular-nums", warn && "text-amber-600 dark:text-amber-400")}>
        {warn && <Icon name="alert" className="mr-1 inline size-3.5 align-[-2px]" />}
        {v}
      </dd>
    </div>
  );
}

function CapacityBar({ label, used, adding, total, className, note }: { label: string; used: number; adding: number; total: number; className?: string; note?: string }) {
  const u = Math.min(100, (used / total) * 100);
  const a = Math.min(100 - u, (adding / total) * 100);
  const over = used + adding > total;
  return (
    <div className={className}>
      <div className="mb-1 flex justify-between text-xs text-zinc-500">
        <span>{label}</span>
        <span className={cx("tabular-nums", over && "font-medium text-amber-600 dark:text-amber-400")}>
          +{bytes(adding)} {note && <span className="text-zinc-400">({note})</span>}
        </span>
      </div>
      <div className="flex h-2 overflow-hidden rounded-full bg-zinc-900/[0.07] dark:bg-white/10" aria-hidden="true">
        <div className="bg-zinc-400 dark:bg-zinc-500" style={{ width: `${u}%` }} />
        <div
          style={{
            width: `${a}%`,
            background: over
              ? "repeating-linear-gradient(135deg,#f59e0b 0 4px,#fbbf24 4px 8px)"
              : "repeating-linear-gradient(135deg,#6366f1 0 4px,#818cf8 4px 8px)",
          }}
        />
      </div>
      <div className="mt-1 flex justify-between text-[11px] text-zinc-400 tabular-nums">
        <span>{bytes(used)} in use</span>
        <span>{bytes(total)}</span>
      </div>
    </div>
  );
}

function PackageInput({ value, onChange, disabled, className }: { value: string[]; onChange: (v: string[]) => void; disabled?: boolean; className?: string }) {
  const [draft, setDraft] = useState("");
  const add = () => {
    const parts = draft.split(/[\s,]+/).filter((p) => p && !value.includes(p));
    if (parts.length) onChange([...value, ...parts]);
    setDraft("");
  };
  return (
    <Field className={className} label="Packages" htmlFor="pkgs" hint="Installed on first boot. The guest agent lets Prism talk to the machine for clean shutdowns and status.">
      <div className={cx("flex flex-wrap items-center gap-1.5 rounded-lg px-2 py-1.5 ring-1 ring-zinc-300 ring-inset focus-within:ring-2 focus-within:ring-indigo-500 dark:ring-zinc-700", disabled && "opacity-60")}>
        {value.map((p) => (
          <span key={p} className="inline-flex items-center gap-1 rounded-md bg-zinc-100 py-0.5 pr-1 pl-2 font-mono text-xs dark:bg-zinc-800">
            {p}
            <button type="button" disabled={disabled} aria-label={`Remove ${p}`} onClick={() => onChange(value.filter((x) => x !== p))} className="rounded p-0.5 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:hover:bg-zinc-700">
              <Icon name="x" className="size-3" />
            </button>
          </span>
        ))}
        <input
          id="pkgs"
          value={draft}
          disabled={disabled}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === "," || e.key === " ") {
              e.preventDefault();
              add();
            } else if (e.key === "Backspace" && !draft && value.length) onChange(value.slice(0, -1));
          }}
          onBlur={add}
          placeholder={value.length ? "Add more…" : "e.g. htop nginx"}
          className="min-w-24 flex-1 bg-transparent py-0.5 font-mono text-xs outline-none placeholder:font-sans placeholder:text-zinc-400"
        />
      </div>
    </Field>
  );
}

function SourcePicker({ kind, items, loading, picked, onPick, defaultPool, onUploaded }: {
  kind: "image" | "iso";
  items: (Volume & { pool: string })[];
  loading: boolean;
  picked: (Volume & { pool: string }) | undefined;
  onPick: (v: Volume & { pool: string }) => void;
  defaultPool: string | undefined;
  onUploaded: (pool: string, name: string) => void;
}) {
  const qc = useQueryClient();
  const ups = useStore(uploads).filter((u) => u.state === "uploading");
  const accept = kind === "image" ? ".img,.qcow2,.raw" : ".iso";
  const upload = (files: File[]) => {
    if (!defaultPool) return;
    for (const f of files) {
      void startUpload(qc, defaultPool, f).then((v) => v && onUploaded(defaultPool, v.name));
    }
  };
  const label = kind === "image" ? "cloud image" : "ISO";
  if (loading && !items.length) return <div className="mt-4 h-16 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-800/60" />;
  return (
    <div className="mt-4">
      {items.length > 0 && (
        <div role="radiogroup" aria-label={`Choose a ${label}`} className="scrollbar-thin grid max-h-64 gap-2 overflow-y-auto sm:grid-cols-2">
          {items.map((v) => {
            const on = picked?.pool === v.pool && picked.name === v.name;
            return (
              <button
                key={`${v.pool}/${v.name}`}
                type="button"
                role="radio"
                aria-checked={on}
                onClick={() => onPick(v)}
                className={cx(
                  "flex items-center gap-3 rounded-lg px-3 py-2.5 text-left ring-1 transition-colors ring-inset",
                  on ? "bg-indigo-50/60 ring-indigo-400 dark:bg-indigo-500/10 dark:ring-indigo-400/60" : "ring-zinc-200 hover:bg-zinc-50 dark:ring-zinc-700/80 dark:hover:bg-white/[0.03]",
                )}
              >
                <span className={cx("grid size-4 shrink-0 place-items-center rounded-full ring-1 ring-inset", on ? "bg-indigo-600 ring-indigo-600 dark:bg-indigo-500" : "ring-zinc-300 dark:ring-zinc-600")}>
                  {on && <span className="size-1.5 rounded-full bg-white" />}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-[13px]">{v.name}</span>
                  <span className="block text-xs text-zinc-500 tabular-nums">
                    {bytes(v.size_bytes)}{kind === "image" && v.file_bytes !== v.size_bytes ? ` virtual · ${bytes(v.file_bytes)} file` : ""} · {v.pool}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      )}
      {ups.map((u) => (
        <div key={u.id} className="mt-2 rounded-lg px-3 py-2.5 ring-1 ring-zinc-200 ring-inset dark:ring-zinc-700/80">
          <div className="mb-1.5 flex justify-between text-[13px]">
            <span className="truncate font-mono">{u.name}</span>
            <span className="text-zinc-500 tabular-nums">{Math.round((u.loaded / (u.total || 1)) * 100)}%</span>
          </div>
          <Progress value={u.total ? u.loaded / u.total : 0} />
        </div>
      ))}
      {items.length === 0 && !ups.length ? (
        <DropBox onFiles={upload} accept={accept} className="mt-1">
          <Icon name="upload" className="mx-auto mb-2 size-6 text-zinc-400" />
          <p className="text-sm font-medium">{kind === "image" ? "No cloud images yet" : "No ISOs yet"}</p>
          <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-zinc-500">
            {kind === "image" ? (
              <>Drop a cloud image here (for example Ubuntu's <span className="font-mono">noble-server-cloudimg-amd64.img</span> from cloud-images.ubuntu.com).</>
            ) : (
              <>Drop an installer ISO here, such as a Debian netinst or Windows ISO.</>
            )}
          </p>
        </DropBox>
      ) : (
        <DropBox onFiles={upload} accept={accept} className="mt-2 !p-3">
          <p className="text-xs text-zinc-500">Drop another {label} here to upload it</p>
        </DropBox>
      )}
    </div>
  );
}
