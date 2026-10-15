export type VmState =
  | "stopped" | "running" | "paused" | "shutting_down" | "crashed" | "suspended" | "missing" | "unknown";

export interface MemoryStats {
  actual_kib: number | null;
  unused_kib: number | null;
  available_kib: number | null;
  usable_kib: number | null;
  disk_caches_kib: number | null;
  rss_kib: number | null;
}

export interface VmRuntime {
  state: VmState;
  vcpus: number;
  memory_kib: number;
  max_memory_kib: number;
  cpu_time_ns: number;
  memory_stats: MemoryStats;
  pending_restart: boolean;
}

export type BalloonMode = "off" | "manual" | "auto";
export type NicNetwork = { type: "user" } | { type: "bridge"; bridge: string };

export interface Vm {
  id: string;
  name: string;
  description: string;
  generation: number;
  cpu: { vcpus: number };
  memory: { max_mib: number; min_mib: number; target_mib: number; balloon: BalloonMode };
  firmware: "bios" | "uefi";
  machine: string;
  disks: {
    id: string; pool: string; volume: string; device: "disk" | "cdrom"; format: string; boot_order: number | null;
  }[];
  nics: { id: string; mac: string; network: NicNetwork }[];
  node: string;
  status: VmRuntime;
}

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
  allocated_bytes: number;
  kind: "disk" | "iso" | "file";
}

export interface CreateVm {
  name: string;
  vcpus: number;
  memory_mib: number;
  min_memory_mib?: number;
  firmware: "bios" | "uefi";
  pool: string;
  disk_gib?: number;
  iso?: string;
  network?: NicNetwork;
  description?: string;
}

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api/v1${path}`, {
    method,
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
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
  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  nodes: () => request<NodeView[]>("GET", "/nodes"),
  vms: () => request<Vm[]>("GET", "/vms"),
  vm: (id: string) => request<Vm>("GET", `/vms/${id}`),
  createVm: (req: CreateVm) => request<Vm>("POST", "/vms", req),
  updateVm: (id: string, req: unknown) => request<Vm>("PATCH", `/vms/${id}`, req),
  deleteVm: (id: string, deleteDisks: boolean) =>
    request<void>("DELETE", `/vms/${id}?delete_disks=${deleteDisks}`),
  action: (id: string, action: string) => request<Vm>("POST", `/vms/${id}/actions/${action}`),
  pools: () => request<Pool[]>("GET", "/nodes/local/pools"),
  createPool: (name: string, format: string) =>
    request<Pool>("POST", "/nodes/local/pools", { name, format }),
  deletePool: (name: string) => request<void>("DELETE", `/nodes/local/pools/${encodeURIComponent(name)}`),
  volumes: (pool: string) => request<Volume[]>("GET", `/nodes/local/pools/${encodeURIComponent(pool)}/volumes`),
  createVolume: (pool: string, name: string, size_gib: number) =>
    request<Volume>("POST", `/nodes/local/pools/${encodeURIComponent(pool)}/volumes`, { name, size_gib }),
  deleteVolume: (pool: string, name: string) =>
    request<void>("DELETE", `/nodes/local/pools/${encodeURIComponent(pool)}/volumes/${encodeURIComponent(name)}`),
};

/** Uploads a file with progress, as a raw PUT. */
export function uploadVolume(
  pool: string,
  file: File,
  onProgress: (fraction: number) => void,
): { promise: Promise<Volume>; abort: () => void } {
  const xhr = new XMLHttpRequest();
  const promise = new Promise<Volume>((resolve, reject) => {
    xhr.open("PUT", `/api/v1/nodes/local/pools/${encodeURIComponent(pool)}/volumes/${encodeURIComponent(file.name)}`);
    xhr.setRequestHeader("content-type", "application/octet-stream");
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve(JSON.parse(xhr.responseText));
      else {
        let detail = xhr.statusText;
        try {
          detail = JSON.parse(xhr.responseText).detail ?? detail;
        } catch {
          /* ignore */
        }
        reject(new ApiError(xhr.status, "upload_failed", detail));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, "network", "upload failed"));
    xhr.onabort = () => reject(new ApiError(0, "aborted", "upload cancelled"));
    xhr.send(file);
  });
  return { promise, abort: () => xhr.abort() };
}

export function wsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}/api/v1${path}`;
}
