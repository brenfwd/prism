export type VmState =
  | "stopped" | "running" | "paused" | "shutting_down" | "crashed" | "suspended" | "missing" | "unknown";

export interface MemoryStats {
  actual_kib: number | null;
  unused_kib: number | null;
  available_kib: number | null;
  usable_kib: number | null;
  disk_caches_kib: number | null;
  rss_kib: number | null;
  major_faults?: number | null;
  swap_in_kib?: number | null;
}

export interface BalloonStatus {
  target_kib: number;
  reason: string;
  unresponsive: boolean;
}

export interface VmRuntime {
  state: VmState;
  vcpus: number;
  memory_kib: number;
  max_memory_kib: number;
  cpu_time_ns: number;
  memory_stats: MemoryStats;
  pending_restart: boolean;
  agent?: boolean;
  balloon?: BalloonStatus | null;
}

export type BalloonMode = "off" | "manual" | "auto";
export type CpuModel = "host_model" | "host_passthrough";
export type Firmware = "bios" | "uefi";
export type NicNetwork =
  | { type: "user"; forwards?: PortForward[] }
  | { type: "bridge"; bridge: string }
  | { type: "network"; network: string; ipv4?: string | null; egress?: string | null };

export interface PortForward {
  proto: "tcp" | "udp";
  /** Host address to listen on; loopback when absent. */
  host_addr?: string | null;
  host_port: number;
  guest_port: number;
}

/** How a device change reached the running guest. */
export interface HotplugResult {
  applied: boolean;
  restart_required: boolean;
  error?: string;
}

export interface GuestNic {
  name: string;
  mac: string | null;
  addrs: { addr: string; prefix: number; ipv6: boolean }[];
}

export interface GuestNetwork {
  agent: boolean;
  interfaces: GuestNic[];
}

export interface HostBridge {
  name: string;
  up: boolean;
  prism: boolean;
  ports: string[];
}

export interface NetworkInfo {
  id: string;
  name: string;
  kind: "nat" | "isolated";
  vni: number;
  subnet: string;
  egress: string | null;
  dhcp: boolean;
  created: number;
  bridge: string;
  domain: string;
  /** Names of the VMs with a NIC on it. */
  vms: string[];
}

export interface Disk {
  id: string;
  pool: string;
  /** `null` is an empty CD-ROM tray. */
  volume: string | null;
  device: "disk" | "cdrom";
  format: string;
  boot_order: number | null;
  target?: string;
  backing?: string[];
}

export interface Nic {
  id: string;
  mac: string;
  network: NicNetwork;
  boot_order?: number | null;
}

export interface Vm {
  id: string;
  name: string;
  description: string;
  generation: number;
  cpu: { vcpus: number; model?: CpuModel };
  memory: { max_mib: number; min_mib: number; target_mib: number; balloon: BalloonMode };
  firmware: Firmware;
  machine: string;
  disks: Disk[];
  nics: Nic[];
  node: string;
  autostart?: boolean;
  /** `user_data` is a `blake3:` fingerprint; the API never returns secrets. */
  cloud_init?: { user_data: string; network_config?: string | null } | null;
  status: VmRuntime;
  /** Present only on responses to device changes. */
  hotplug?: HotplugResult;
}

export type PressureLevel = "green" | "yellow" | "red";

export interface NodeView {
  id: string;
  name: string;
  version: string;
  libvirt_connected: boolean;
  domain_type: string;
  host: {
    hostname: string; libvirt_version: string; cpu_model: string; cpus: number; memory_kib: number;
    free_memory_bytes: number;
  };
  memory_pressure?: {
    level: PressureLevel; some_avg10: number | null; full_avg10: number | null; mem_available_pct: number;
  } | null;
}

export interface Pool {
  name: string;
  path: string;
  type: string;
  format: string;
  static_config: boolean;
  capacity_bytes: number;
  available_bytes: number;
}

export interface Volume {
  name: string;
  size_bytes: number;
  file_bytes: number;
  allocated_bytes: number;
  kind: "disk" | "iso" | "file";
}

export interface CloudInit {
  user_data: string;
  network_config?: string;
}

export interface CreateVm {
  name: string;
  vcpus: number;
  memory_mib: number;
  min_memory_mib?: number;
  balloon?: BalloonMode;
  firmware: Firmware;
  pool: string;
  disk_gib?: number;
  iso?: string;
  image?: string;
  cloud_init?: CloudInit;
  network?: NicNetwork;
  description?: string;
  autostart?: boolean;
  cpu_model?: CpuModel;
}

export interface UpdateVm {
  description?: string;
  vcpus?: number;
  cpu_model?: CpuModel;
  memory?: { max_mib?: number; min_mib?: number; target_mib?: number; balloon?: BalloonMode };
  autostart?: boolean;
  cloud_init?: CloudInit | null;
  boot_order?: string[];
  generation?: number;
}

export interface Snapshot {
  name: string;
  description: string;
  /** Unix seconds. */
  created: number | null;
  state: string;
  parent: string | null;
  memory: boolean;
  current: boolean;
}

export type TaskState = "running" | "succeeded" | "failed" | "cancelled";

export interface Task {
  id: string;
  kind: string;
  resource: string;
  node: string;
  state: TaskState;
  progress: number | null;
  message: string;
  error: string | null;
  result: unknown;
  created: string;
  updated: string;
}

export type VmAction = "start" | "shutdown" | "stop" | "reboot" | "reset" | "pause" | "resume";

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`/api/v1${path}`, {
      method,
      headers: body === undefined ? {} : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, "network", "Prism didn't answer. Check that the node is reachable.");
  }
  if (!res.ok) {
    let code = "error";
    let detail = res.statusText;
    try {
      const p = await res.json();
      code = p.code ?? code;
      detail = p.detail ?? detail;
    } catch {
      /* not problem+json */
    }
    throw new ApiError(res.status, code, detail);
  }
  if (res.status === 204 || res.headers.get("content-length") === "0") return undefined as T;
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}


export interface ClusterDoc { cluster_id: string; name: string; mode: "hub_spoke" | "consensus"; mesh_prefix: string; wg_mtu: number; created: number }
export interface ClusterMember {
  node_id: string; raft_id: number; name: string; wg_pubkey: string; mesh_addr: string; endpoints: string[];
  state: "joining" | "active"; voter: boolean; version: string; joined: number;
}
export interface ClusterStatus {
  clustered: boolean; cluster: ClusterDoc | null; me: string; role: string; leader: string | null;
  members: ClusterMember[]; degraded: boolean; pending_join: boolean;
}
export interface InviteView { invite_id: string; blob: string; expires: number; assigned_addr: string }
export interface InviteDoc { invite_id: string; assigned_addr: string; exp: number; name_hint: string | null; consumed_by: string | null }
export interface AcceptPreview { node_id: string; name: string; endpoints: string[]; fingerprint: string; invite_id: string; admitted: boolean }
export interface JoinReply { reply: string; fingerprint: string; cluster_name: string; sponsor: string }
export interface MeshPeer { pubkey: string; endpoint: string | null; handshake_age_secs: number | null; rx_bytes: number; tx_bytes: number }
export type ArchiveMemberState = "active" | "draining" | "disabled";
export interface ArchiveMember {
  node: string; name: string; pool: string; weight: number; reserve: number; state: ArchiveMemberState;
  node_name: string; capacity: { total: number; free: number } | null;
}
export interface ArchivePool { name: string; replicas: number; members: ArchiveMember[]; files: number; used_bytes: number }

const enc = encodeURIComponent;

export const api = {
  cluster: () => request<ClusterStatus>("GET", "/cluster"),
  createCluster: (name: string, mode: "hub_spoke" | "consensus") => request<ClusterStatus>("POST", "/cluster", { name, mode }),
  invites: () => request<InviteDoc[]>("GET", "/cluster/invites"),
  createInvite: (name?: string) => request<InviteView>("POST", "/cluster/invites", { name: name || undefined }),
  revokeInvite: (id: string) => request<void>("DELETE", `/cluster/invites/${id}`),
  joinCluster: (invite: string) => request<JoinReply>("POST", "/cluster/join", { invite }),
  acceptReply: (reply: string, confirm: boolean) => request<AcceptPreview>("POST", "/cluster/accept", { reply, confirm }),
  removeMember: (id: string) => request<void>("DELETE", `/cluster/members/${id}`),
  leaveCluster: () => request<void>("POST", "/cluster/leave"),
  setVoter: (id: string, voter: boolean) => request<void>("POST", `/cluster/members/${id}/voter`, { voter }),
  mesh: () => request<MeshPeer[]>("GET", "/cluster/mesh"),
  archivePools: () => request<ArchivePool[]>("GET", "/archive/pools"),
  createArchivePool: (name: string, replicas: number) => request<void>("POST", "/archive/pools", { name, replicas }),
  setArchiveReplicas: (pool: string, replicas: number) => request<void>("PUT", `/archive/pools/${enc(pool)}`, { replicas }),
  archiveAction: (pool: string, a: "repair" | "scrub") => request<Task>("POST", `/archive/pools/${enc(pool)}/${a}`),
  setArchiveMember: (node: string, name: string, state: ArchiveMemberState) =>
    request<void>("PUT", `/archive/members/${node}/${enc(name)}`, { state }),
  drainArchiveMember: (node: string, name: string) => request<Task>("POST", `/archive/members/${node}/${enc(name)}/drain`),
  nodes: () => request<NodeView[]>("GET", "/nodes"),
  vms: () => request<Vm[]>("GET", "/vms"),
  vm: (id: string) => request<Vm>("GET", `/vms/${id}`),
  createVm: (req: CreateVm) => request<Vm>("POST", "/vms", req),
  updateVm: (id: string, req: UpdateVm) => request<Vm>("PATCH", `/vms/${id}`, req),
  deleteVm: (id: string, deleteDisks: boolean) =>
    request<void>("DELETE", `/vms/${id}?delete_disks=${deleteDisks}`),
  action: (id: string, action: VmAction) => request<Vm>("POST", `/vms/${id}/actions/${action}`),

  cloneVm: (id: string, name: string) => request<Task>("POST", `/vms/${id}/clone`, { name }),
  guestNetwork: (id: string) => request<GuestNetwork>("GET", `/vms/${id}/guest/network`),
  setForwards: (id: string, nic: string, forwards: PortForward[]) =>
    request<Vm>("PUT", `/vms/${id}/nics/${nic}/forwards`, forwards),
  bridges: () => request<HostBridge[]>("GET", "/nodes/local/bridges"),
  downloadVolume: (pool: string, url: string, name?: string) =>
    request<Task>("POST", `/nodes/local/pools/${enc(pool)}/downloads`, { url, name: name || undefined }),

  networks: () => request<NetworkInfo[]>("GET", "/networks"),
  createNetwork: (req: { name?: string; kind?: "nat" | "isolated"; subnet?: string; dhcp?: boolean } = {}) =>
    request<NetworkInfo>("POST", "/networks", req),
  updateNetwork: (id: string, req: { name?: string; dhcp?: boolean }) => request<NetworkInfo>("PATCH", `/networks/${id}`, req),
  deleteNetwork: (id: string) => request<void>("DELETE", `/networks/${id}`),

  addDisk: (id: string, req: { pool: string; volume?: string; size_gib?: number; device: "disk" | "cdrom" }) =>
    request<Vm>("POST", `/vms/${id}/disks`, req),
  updateDisk: (id: string, disk: string, req: { media?: string | null; size_gib?: number }) =>
    request<Vm>("PATCH", `/vms/${id}/disks/${disk}`, req),
  removeDisk: (id: string, disk: string, deleteVolume: boolean) =>
    request<Vm>("DELETE", `/vms/${id}/disks/${disk}?delete_volume=${deleteVolume}`),
  addNic: (id: string, req: { network: NicNetwork; mac?: string }) => request<Vm>("POST", `/vms/${id}/nics`, req),
  removeNic: (id: string, nic: string) => request<Vm>("DELETE", `/vms/${id}/nics/${nic}`),

  snapshots: (id: string) => request<Snapshot[]>("GET", `/vms/${id}/snapshots`),
  createSnapshot: (id: string, req: { name?: string; description: string; memory: boolean }) =>
    request<Task>("POST", `/vms/${id}/snapshots`, req),
  deleteSnapshot: (id: string, name: string) => request<Task>("DELETE", `/vms/${id}/snapshots/${enc(name)}`),
  revertSnapshot: (id: string, name: string) => request<Task>("POST", `/vms/${id}/snapshots/${enc(name)}/revert`),

  tasks: () => request<Task[]>("GET", "/tasks?limit=100"),
  cancelTask: (id: string) => request<void>("POST", `/tasks/${id}/cancel`),

  pools: () => request<Pool[]>("GET", "/nodes/local/pools"),
  createPool: (name: string, format: string) => request<Pool>("POST", "/nodes/local/pools", { name, format }),
  deletePool: (name: string) => request<void>("DELETE", `/nodes/local/pools/${enc(name)}`),
  volumes: (pool: string) => request<Volume[]>("GET", `/nodes/local/pools/${enc(pool)}/volumes`),
  createVolume: (pool: string, name: string, size_gib: number, format: string) =>
    request<Volume>("POST", `/nodes/local/pools/${enc(pool)}/volumes`, { name, size_gib, format }),
  deleteVolume: (pool: string, name: string) =>
    request<void>("DELETE", `/nodes/local/pools/${enc(pool)}/volumes/${enc(name)}`),
};

/** Uploads a file with progress, as a raw PUT. */
export function uploadVolume(
  pool: string,
  file: File,
  name: string,
  onProgress: (loaded: number, total: number) => void,
): { promise: Promise<Volume>; abort: () => void } {
  const xhr = new XMLHttpRequest();
  const promise = new Promise<Volume>((resolve, reject) => {
    xhr.open("PUT", `/api/v1/nodes/local/pools/${enc(pool)}/volumes/${enc(name)}`);
    xhr.setRequestHeader("content-type", "application/octet-stream");
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded, e.total);
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve(JSON.parse(xhr.responseText));
      else {
        let detail = xhr.statusText;
        let code = "upload_failed";
        try {
          const p = JSON.parse(xhr.responseText);
          detail = p.detail ?? detail;
          code = p.code ?? code;
        } catch {
          /* ignore */
        }
        reject(new ApiError(xhr.status, code, detail));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, "network", "The upload was interrupted."));
    xhr.onabort = () => reject(new ApiError(0, "aborted", "Upload cancelled."));
    xhr.send(file);
  });
  return { promise, abort: () => xhr.abort() };
}

export function wsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}/api/v1${path}`;
}

/** Turns API and network errors into one calm sentence. */
export function humanError(e: unknown): string {
  if (!e) return "";
  if (e instanceof ApiError) {
    const d = e.message.trim();
    const sentence = d ? d[0].toUpperCase() + d.slice(1) + (/[.!?]$/.test(d) ? "" : ".") : "";
    switch (e.code) {
      case "libvirt_unavailable":
        return `Prism can't reach libvirt on this host right now. ${sentence}`;
      case "network":
      case "aborted":
        return sentence;
      case "not_found":
        return sentence || "That no longer exists.";
      case "conflict":
      case "exists":
        return sentence || "Something changed in the meantime. Try again.";
      case "invalid_body":
        return `The request was malformed: ${d}`;
      default:
        if (e.status >= 500) return `Something went wrong on the host. ${sentence}`;
        return sentence || `Request failed (${e.status}).`;
    }
  }
  if (e instanceof Error) return e.message;
  return String(e);
}
