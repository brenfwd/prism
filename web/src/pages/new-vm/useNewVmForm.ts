import { useEffect, useMemo, useReducer, useRef } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { api, type BalloonMode, type CpuModel, type CreateVm, type Firmware, type Volume } from "../../api";
import { putVm } from "../../events";
import { defaultUser, invalidKey, sshKeyLines, userData } from "../../lib/cloudinit";
import { hostAvailableBytes, looksLikeVmDisk, runAction, useAllVolumes, useNetworks, useNode, usePools, useVms, volumeUsers } from "../../lib/hooks";
import { local } from "../../lib/store";
import { toast } from "../../lib/toast";
import {
  baseName, blockers as findBlockers, capacity, defaultCustom, defaultPreset, effectiveNet, hostnameOf, minDiskGib, nameError,
  networkRequest, resolveSize, suggestName, type NetKind, type PresetId, type Size, type Source,
} from "./model";

/** The route's query parameters, which can pre-select a source file. */
export interface NewVmSearch {
  source?: Source;
  pool?: string;
  volume?: string;
}

/** A volume with the pool it lives in. */
export type PoolVolume = Volume & { pool: string };

const KEY_STORE = "prism.lastSshKey";
const USER_STORE = "prism.lastUser";

/** Every field the user can edit, plus the flags that stop auto-fill once they have. */
interface Fields {
  source: Source;
  picked: { pool: string; name: string } | null;
  name: string;
  nameTouched: boolean;
  preset: PresetId;
  presetTouched: boolean;
  custom: Size;
  username: string;
  userTouched: boolean;
  sshKeys: string;
  password: string;
  packages: string[];
  upgrade: boolean;
  /** Hand-edited user-data; `null` while the generated one is in use. */
  raw: string | null;
  netChoice: string | undefined;
  net: NetKind;
  bridge: string;
  firmware: Firmware;
  cpuModel: CpuModel;
  balloon: BalloonMode;
  autostart: boolean;
  startNow: boolean;
  description: string;
  blankPool: string | undefined;
}

function initialFields(search: NewVmSearch): Fields {
  return {
    source: search.source ?? "image",
    picked: search.volume && search.pool ? { pool: search.pool, name: search.volume } : null,
    name: "",
    nameTouched: false,
    preset: "small",
    presetTouched: false,
    custom: defaultCustom,
    username: local.get(USER_STORE) ?? "",
    userTouched: !!local.get(USER_STORE),
    sshKeys: local.get(KEY_STORE) ?? "",
    password: "",
    packages: ["qemu-guest-agent"],
    upgrade: false,
    raw: null,
    netChoice: undefined,
    net: "network",
    bridge: "br0",
    firmware: "bios",
    cpuModel: "host_model",
    balloon: "auto",
    autostart: false,
    startNow: true,
    description: "",
    blankPool: undefined,
  };
}

const reduce = (f: Fields, patch: Partial<Fields>): Fields => ({ ...f, ...patch });

/**
 * State and derived values behind the new-machine page: the editable
 * fields, what they resolve to (source volume, size, pool, network), the
 * capacity warnings, and the create mutation. The step components only
 * read this object and call `set`.
 */
export function useNewVmForm(search: NewVmSearch) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const node = useNode();
  const vms = useVms().data;
  const pools = usePools().data ?? [];
  const networks = useNetworks().data ?? [];
  const { data: volumes, loading: volumesLoading } = useAllVolumes();
  const users = useMemo(() => volumeUsers(vms), [vms]);
  const taken = useMemo(() => new Set((vms ?? []).map((v) => v.name)), [vms]);
  const avail = hostAvailableBytes(node);

  const [f, dispatch] = useReducer(reduce, search, initialFields);
  const set = dispatch;

  // Candidates for each source: free-standing disk images, or ISOs.
  const images = useMemo(
    () =>
      volumes
        .filter((v) => v.kind === "disk" && !users.has(`${v.pool}/${v.name}`) && !looksLikeVmDisk(v.name))
        .sort((a, b) => Number(b.name.endsWith(".img")) - Number(a.name.endsWith(".img"))),
    [volumes, users],
  );
  const isos = useMemo(() => volumes.filter((v) => v.kind === "iso"), [volumes]);
  const candidates = f.source === "image" ? images : f.source === "iso" ? isos : [];
  const sourceVol: PoolVolume | undefined =
    f.source === "blank"
      ? undefined
      : (candidates.find((v) => v.pool === f.picked?.pool && v.name === f.picked?.name) ?? (f.picked ? undefined : candidates[0]));

  // Select the first candidate when the source changes or the pick disappears.
  useEffect(() => {
    if (f.source === "blank") return;
    if (!f.picked || !candidates.some((v) => v.pool === f.picked!.pool && v.name === f.picked!.name)) {
      if (candidates[0]) set({ picked: { pool: candidates[0].pool, name: candidates[0].name } });
    }
  }, [f.source, candidates, f.picked, set]);

  // Name: follows the suggestion until the user types.
  const suggested = suggestName(baseName(sourceVol?.name), taken);
  useEffect(() => {
    if (!f.nameTouched) set({ name: suggested });
  }, [suggested, f.nameTouched, set]);
  const nameProblem = nameError(f.name, taken);
  const showNameError = f.nameTouched || f.name !== suggested;

  // Size: follows free memory until the user picks a card.
  useEffect(() => {
    if (!f.presetTouched && avail != null) set({ preset: defaultPreset(avail) });
  }, [avail, f.presetTouched, set]);
  const minDisk = minDiskGib(f.source, sourceVol?.size_bytes);
  const size = resolveSize(f.preset, f.custom, minDisk);

  // Sign-in: the default user follows the image until edited.
  useEffect(() => {
    if (!f.userTouched) set({ username: defaultUser(sourceVol?.name ?? "") });
  }, [sourceVol?.name, f.userTouched, set]);
  const generated = userData({
    hostname: hostnameOf(f.name),
    username: f.username,
    sshKeys: f.sshKeys,
    password: f.password,
    packages: f.packages,
    upgrade: f.upgrade,
  });
  const badKey = invalidKey(f.sshKeys);
  const noLogin = f.source === "image" && f.raw == null && !sshKeyLines(f.sshKeys).length && !f.password;

  // Network and pool.
  const netId = f.netChoice ?? networks[0]?.id;
  const netKind = effectiveNet(f.net, netId);
  const pool = sourceVol?.pool ?? f.blankPool ?? pools[0]?.name;
  const poolInfo = pools.find((p) => p.name === pool);

  const cap = capacity(size, avail, poolInfo?.available_bytes);
  const blockers = findBlockers({
    nameError: nameProblem,
    source: f.source,
    hasSource: !!sourceVol,
    badKey: !!badKey && f.source === "image" && f.raw == null,
    poolReady: !!pool,
  });

  const create = useMutation({
    mutationFn: async () => {
      const req: CreateVm = {
        name: f.name,
        vcpus: size.vcpus,
        memory_mib: size.mem,
        balloon: f.balloon,
        firmware: f.firmware,
        cpu_model: f.cpuModel,
        pool: pool!,
        disk_gib: size.disk,
        network: networkRequest(netKind, netId, f.bridge),
        autostart: f.autostart,
        description: f.description || undefined,
      };
      if (f.source === "image" && sourceVol) {
        req.image = sourceVol.name;
        req.cloud_init = { user_data: f.raw ?? generated };
      }
      if (f.source === "iso" && sourceVol) req.iso = sourceVol.name;
      return api.createVm(req);
    },
    onSuccess: (vm) => {
      putVm(qc, vm);
      if (f.source === "image") {
        if (f.sshKeys.trim()) local.set(KEY_STORE, f.sshKeys.trim());
        local.set(USER_STORE, f.username);
      }
      toast({ tone: "success", title: `${vm.name} created`, body: f.startNow ? "Starting it up…" : undefined });
      if (f.startNow) void runAction(qc, vm, "start");
      navigate({ to: "/vms/$id", params: { id: vm.id }, search: { tab: f.startNow && f.source !== "image" ? "console" : "overview" } });
    },
  });

  const submit = () => {
    if (blockers.length || create.isPending) return;
    create.mutate();
  };
  // Ctrl/Cmd+Enter submits from anywhere on the page.
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

  return {
    f, set, node, pools, networks, avail,
    volumesLoading, candidates, sourceVol,
    suggested, nameProblem, showNameError,
    minDisk, size, generated, badKey, noLogin,
    netId, netKind, pool, poolInfo, cap, blockers,
    create, submit,
    rememberedKey: local.get(KEY_STORE),
  };
}

/** The value returned by [`useNewVmForm`]. */
export type NewVmForm = ReturnType<typeof useNewVmForm>;

