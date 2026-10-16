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
  | { type: "network"; network: string; ipv4?: string | null; ipv6?: string | null; egress?: string | null; vlan?: number | null; trunk?: number[]; forwards?: PortForward[] };

export interface PortForward {
  proto: "tcp" | "udp";
  /** Host address to listen on; every address on a Prism network, loopback on a user-mode NIC, when absent. */
  host_addr?: string | null;
  host_port: number;
  guest_port: number;
  /** Also reach the guest from the host itself (default on). Prism networks only. */
  local?: boolean;
  /** Also reach it through the host address from guests on the same network (default on). */
  hairpin?: boolean;
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

export interface Prereq {
  id: string;
  label: string;
  ok: boolean;
  required: boolean;
  detail: string;
  hint: string | null;
}

export interface NetworkCheck { name_error: string | null; subnet_error: string | null; suggestion: string | null; blocking_vms: string[] }

export interface NetworkInfo {
  id: string;
  name: string;
  kind: "nat" | "isolated";
  vni: number;
  subnet: string;
  egress: string | null;
  dhcp: boolean;
  allow_prefixes?: string[];
  /** The ULA /64 guests autoconfigure from; absent while IPv6 is off. */
  subnet6?: string | null;
  /** Masquerade IPv6 traffic out of the host (NAT66). */
  nat66?: boolean;
  created: number;
  bridge: string;
  domain: string;
  /** Names of the VMs with a NIC on it. */
  /** Every VM on this node. */
  vms: string[];
  /** VLANs with a subnet of their own, by VLAN id: a gateway, DHCP scope and NAT each. */
  vlan_subnets?: Record<string, { subnet: string }>;
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
  /** The IPv6 address each NIC on an IPv6 network forms by SLAAC, by NIC id. */
  nic_ipv6?: Record<string, string>;
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

export interface DeleteItem {
  pool: string;
  volume: string;
  kind: "disk" | "snapshot";
  size_bytes: number;
  /** Other VMs that use the file; a delete keeps it. */
  shared_with: string[];
}

export interface DeletePlan {
  items: DeleteItem[];
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
  /** Memory revert after a move: needs explicit confirmation. */
  confirm_required?: boolean;
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

/** A failed request: HTTP `status` (0 when the node was unreachable), a machine-readable `code` and a readable message. */
export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

/** Sends a JSON request under `/api/v1` and parses the JSON answer; empty bodies give `undefined`. */
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
export interface MemberProgress { node_id: string; name: string; applied_index: number | null }
export interface RemovalStatus {
  node_id: string; wg_pubkey: string; mesh_addr: string; removed: number; removed_index: number;
  pending: MemberProgress[]; complete: boolean;
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

/** One method per endpoint. Each rejects with an [`ApiError`] on a non-2xx answer or when the node cannot be reached. */
export const api = {
  /** Cluster state, members and this node's role. A standalone node answers with `clustered: false`. */
  cluster: () => request<ClusterStatus>("GET", "/cluster"),
  /** Turns this standalone node into the first member of a new cluster. */
  createCluster: (name: string, mode: "hub_spoke" | "consensus") => request<ClusterStatus>("POST", "/cluster", { name, mode }),
  /** Join invites that are pending or used. */
  invites: () => request<InviteDoc[]>("GET", "/cluster/invites"),
  /** Makes a single-use join invite; `name` is a hint for the joining node. */
  createInvite: (name?: string) => request<InviteView>("POST", "/cluster/invites", { name: name || undefined }),
  /** Revokes an unused invite. */
  revokeInvite: (id: string) => request<void>("DELETE", `/cluster/invites/${id}`),
  /** Starts joining from an invite blob and returns the reply to paste into the sponsor's accept step. */
  joinCluster: (invite: string) => request<JoinReply>("POST", "/cluster/join", { invite }),
  /** Previews a joiner's reply (`confirm: false`) or admits the node (`confirm: true`) on the sponsor. */
  acceptReply: (reply: string, confirm: boolean) => request<AcceptPreview>("POST", "/cluster/accept", { reply, confirm }),
  /** Removes a member from the cluster by node id. */
  removeMember: (id: string) => request<void>("DELETE", `/cluster/members/${id}`),
  /** Removals still waiting for every member to apply them. */
  removals: () => request<RemovalStatus[]>("GET", "/cluster/removed"),
  /** Hands the hub role to another node in a hub-and-spoke cluster. */
  transferHub: (node: string) => request<void>("POST", "/cluster/hub", { node }),
  /** Leaves the cluster; the node restarts as standalone. */
  leaveCluster: () => request<void>("POST", "/cluster/leave"),
  /** Makes a member a voting or non-voting member of a consensus cluster. */
  setVoter: (id: string, voter: boolean) => request<void>("POST", `/cluster/members/${id}/voter`, { voter }),
  /** WireGuard peers with endpoint, last handshake age and traffic counters. */
  mesh: () => request<MeshPeer[]>("GET", "/cluster/mesh"),
  /** Archive pools with their members and usage. */
  archivePools: () => request<ArchivePool[]>("GET", "/archive/pools"),
  /** Creates an archive pool that keeps `replicas` copies of each file. */
  createArchivePool: (name: string, replicas: number) => request<void>("POST", "/archive/pools", { name, replicas }),
  /** Changes how many copies of each file an archive pool keeps. */
  setArchiveReplicas: (pool: string, replicas: number) => request<void>("PUT", `/archive/pools/${enc(pool)}`, { replicas }),
  /** Starts a repair, scrub or rebalance on an archive pool and returns its task. */
  archiveAction: (pool: string, a: "repair" | "scrub" | "rebalance") => request<Task>("POST", `/archive/pools/${enc(pool)}/${a}`),
  /** Sets an archive member to active, draining or disabled. */
  setArchiveMember: (node: string, name: string, state: ArchiveMemberState) =>
    request<void>("PUT", `/archive/members/${node}/${enc(name)}`, { state }),
  /** Moves a member's files to other members and returns the task. */
  drainArchiveMember: (node: string, name: string) => request<Task>("POST", `/archive/members/${node}/${enc(name)}/drain`),
  /** The nodes Prism knows about, with host and memory-pressure data. */
  nodes: () => request<NodeView[]>("GET", "/nodes"),
  vms: () => request<Vm[]>("GET", "/vms"),
  /** One VM by id; rejects with code `not_found` for an unknown id. */
  vm: (id: string) => request<Vm>("GET", `/vms/${id}`),
  /** Creates a VM, copying `image` into its disk when one is given. */
  createVm: (req: CreateVm) => request<Vm>("POST", "/vms", req),
  /** Changes a VM's settings, boot order or description. */
  updateVm: (id: string, req: UpdateVm) => request<Vm>("PATCH", `/vms/${id}`, req),
  /** Deletes a VM and, with `deleteDisks`, its disk volumes. */
  deleteVm: (id: string, deleteDisks: boolean) =>
    request<void>("DELETE", `/vms/${id}?delete_disks=${deleteDisks}`),
  /** Lists what deleting the VM would remove, without deleting anything. */
  deletePlan: (id: string) => request<DeletePlan>("GET", `/vms/${id}/delete-plan`),
  /** Runs a power action and returns the updated VM. */
  action: (id: string, action: VmAction) => request<Vm>("POST", `/vms/${id}/actions/${action}`),

  /** Starts a clone of a VM under a new name and returns its task. */
  cloneVm: (id: string, name: string) => request<Task>("POST", `/vms/${id}/clone`, { name }),
  /** Addresses the guest agent reports. */
  guestNetwork: (id: string) => request<GuestNetwork>("GET", `/vms/${id}/guest/network`),
  /** Replaces the port forwards on one adapter. */
  setForwards: (id: string, nic: string, forwards: PortForward[]) =>
    request<Vm>("PUT", `/vms/${id}/nics/${nic}/forwards`, forwards),
  /** Linux bridges on the host, for bridged adapters. */
  bridges: () => request<HostBridge[]>("GET", "/nodes/local/bridges"),
  /** Host requirements and whether each is met. */
  prereqs: () => request<Prereq[]>("GET", "/nodes/local/prereqs"),
  /** Downloads a URL into a pool as a volume, checking `sha256` when given. Returns the task. */
  downloadVolume: (pool: string, url: string, name?: string, sha256?: string) =>
    request<Task>("POST", `/nodes/local/pools/${enc(pool)}/downloads`, { url, name: name || undefined, sha256: sha256 || undefined }),

  /** Prism networks. */
  networks: () => request<NetworkInfo[]>("GET", "/networks"),
  /** Creates a Prism network. Omitted fields take the server's defaults. */
  createNetwork: (req: { name?: string; kind?: "nat" | "isolated"; subnet?: string; dhcp?: boolean; allow_prefixes?: string[]; ipv6?: boolean; nat66?: boolean } = {}) =>
    request<NetworkInfo>("POST", "/networks", req),
  /** Checks a name and subnet against other networks and the host's routes without saving anything. */
  checkNetwork: (req: { id?: string; name?: string; subnet?: string }) => request<NetworkCheck>("POST", "/networks/check", req),
  /** Changes a network. A new subnet moves every address in it. */
  updateNetwork: (id: string, req: { name?: string; dhcp?: boolean; subnet?: string; kind?: "nat" | "isolated"; allow_prefixes?: string[]; ipv6?: boolean; nat66?: boolean }) => request<NetworkInfo>("PATCH", `/networks/${id}`, req),
  /** Deletes a network. */
  deleteNetwork: (id: string) => request<void>("DELETE", `/networks/${id}`),
  /** Gives a VLAN on a network its own subnet; one is chosen when `subnet` is omitted. */
  addVlanSubnet: (id: string, req: { vlan: number; subnet?: string }) => request<NetworkInfo>("POST", `/networks/${id}/vlans`, req),
  /** Removes a VLAN's subnet from a network. */
  removeVlanSubnet: (id: string, vlan: number) => request<NetworkInfo>("DELETE", `/networks/${id}/vlans/${vlan}`),
  /** A free subnet for a new VLAN on the network. */
  vlanSuggestion: (id: string) => request<{ subnet: string }>("GET", `/networks/${id}/vlan-suggestion`),

  /** Adds a disk or CD-ROM drive: a new empty disk (`size_gib`) or an existing `volume`. Hot-plugs into a running VM. */
  addDisk: (id: string, req: { pool: string; volume?: string; size_gib?: number; device: "disk" | "cdrom" }) =>
    request<Vm>("POST", `/vms/${id}/disks`, req),
  /** Inserts or ejects media (`media`) or grows a disk (`size_gib`). */
  updateDisk: (id: string, disk: string, req: { media?: string | null; size_gib?: number }) =>
    request<Vm>("PATCH", `/vms/${id}/disks/${disk}`, req),
  /** Detaches a disk and, with `deleteVolume`, deletes its volume. */
  removeDisk: (id: string, disk: string, deleteVolume: boolean) =>
    request<Vm>("DELETE", `/vms/${id}/disks/${disk}?delete_volume=${deleteVolume}`),
  /** Sets an adapter's access VLAN and trunk list. */
  setVlans: (id: string, nic: string, req: { vlan?: number | null; trunk: number[] }) =>
    request<Vm>("PUT", `/vms/${id}/nics/${nic}/vlans`, req),
  /** Adds a network adapter; the MAC is random when omitted. */
  addNic: (id: string, req: { network: NicNetwork; mac?: string }) => request<Vm>("POST", `/vms/${id}/nics`, req),
  /** Removes a network adapter. */
  removeNic: (id: string, nic: string) => request<Vm>("DELETE", `/vms/${id}/nics/${nic}`),

  /** A VM's snapshots. */
  snapshots: (id: string) => request<Snapshot[]>("GET", `/vms/${id}/snapshots`),
  /** Takes a snapshot, optionally with memory, and returns its task. */
  createSnapshot: (id: string, req: { name?: string; description: string; memory: boolean }) =>
    request<Task>("POST", `/vms/${id}/snapshots`, req),
  /** Deletes a snapshot and returns its task. */
  deleteSnapshot: (id: string, name: string) => request<Task>("DELETE", `/vms/${id}/snapshots/${enc(name)}`),
  /** Reverts to a snapshot. Pass `confirmMemory` to accept restoring a memory snapshot. */
  revertSnapshot: (id: string, name: string, confirmMemory = false) =>
    request<Task>("POST", `/vms/${id}/snapshots/${enc(name)}/revert${confirmMemory ? "?confirm_memory=true" : ""}`),

  /** The 100 most recent tasks. */
  tasks: () => request<Task[]>("GET", "/tasks?limit=100"),
  /** Asks a running task to stop. */
  cancelTask: (id: string) => request<void>("POST", `/tasks/${id}/cancel`),

  /** Storage pools on this node. */
  pools: () => request<Pool[]>("GET", "/nodes/local/pools"),
  /** Creates a storage pool with the given volume format. */
  createPool: (name: string, format: string) => request<Pool>("POST", "/nodes/local/pools", { name, format }),
  /** Deletes a storage pool. */
  deletePool: (name: string) => request<void>("DELETE", `/nodes/local/pools/${enc(name)}`),
  /** Volumes in a pool. */
  volumes: (pool: string) => request<Volume[]>("GET", `/nodes/local/pools/${enc(pool)}/volumes`),
  /** Creates an empty volume of `size_gib` in the given format. */
  createVolume: (pool: string, name: string, size_gib: number, format: string) =>
    request<Volume>("POST", `/nodes/local/pools/${enc(pool)}/volumes`, { name, size_gib, format }),
  /** Deletes a volume from a pool. */
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

/** The WebSocket URL for an API path, `ws:` or `wss:` to match the page. */
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
