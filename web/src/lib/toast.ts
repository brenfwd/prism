import { createStore } from "./store";

export type ToastTone = "success" | "error" | "info" | "warning";

export interface Toast {
  id: number;
  tone: ToastTone;
  title: string;
  body?: string;
  /** In-app link to a VM. */
  vm?: { id: string; label?: string; tab?: string };
  action?: { label: string; run: () => void };
  /** ms; 0 keeps it until dismissed. */
  timeout: number;
}

export const toasts = createStore<Toast[]>([]);
let seq = 0;

export function toast(t: Omit<Toast, "id" | "timeout"> & { timeout?: number }): number {
  const id = ++seq;
  const timeout = t.timeout ?? (t.tone === "error" ? 9000 : 5000);
  toasts.set((all) => [...all.slice(-3), { ...t, id, timeout }]);
  if (timeout) setTimeout(() => dismiss(id), timeout);
  return id;
}

export function dismiss(id: number) {
  toasts.set((all) => all.filter((t) => t.id !== id));
}
