import { useEffect, type ReactNode, type ButtonHTMLAttributes, type InputHTMLAttributes } from "react";
import type { VmState } from "../api";

type Variant = "primary" | "secondary" | "danger" | "ghost";

const variants: Record<Variant, string> = {
  primary: "bg-indigo-600 text-white hover:bg-indigo-500 disabled:bg-indigo-600/50",
  secondary:
    "bg-white text-zinc-800 ring-1 ring-zinc-300 hover:bg-zinc-50 dark:bg-zinc-800 dark:text-zinc-100 dark:ring-zinc-700 dark:hover:bg-zinc-700",
  danger: "bg-red-600 text-white hover:bg-red-500 disabled:bg-red-600/50",
  ghost: "text-zinc-600 hover:bg-zinc-200/60 dark:text-zinc-300 dark:hover:bg-zinc-800",
};

export function Button({
  variant = "secondary",
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      className={`inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-60 ${variants[variant]} ${className}`}
      {...props}
    />
  );
}

export function Card({ title, actions, children, className = "" }: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`rounded-lg bg-white ring-1 ring-zinc-200 dark:bg-zinc-900 dark:ring-zinc-800 ${className}`}>
      {(title || actions) && (
        <header className="flex items-center justify-between gap-2 border-b border-zinc-200 px-4 py-3 dark:border-zinc-800">
          <h2 className="text-sm font-semibold">{title}</h2>
          <div className="flex gap-2">{actions}</div>
        </header>
      )}
      <div className="p-4">{children}</div>
    </section>
  );
}

const stateColors: Record<VmState, string> = {
  running: "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/50 dark:text-emerald-300",
  paused: "bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-300",
  shutting_down: "bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-300",
  suspended: "bg-amber-100 text-amber-800 dark:bg-amber-900/50 dark:text-amber-300",
  stopped: "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300",
  crashed: "bg-red-100 text-red-800 dark:bg-red-900/50 dark:text-red-300",
  missing: "bg-red-100 text-red-800 dark:bg-red-900/50 dark:text-red-300",
  unknown: "bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400",
};

export function StateBadge({ state }: { state: VmState }) {
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${stateColors[state]}`}>
      <span className={`size-1.5 rounded-full bg-current ${state === "running" ? "" : "opacity-60"}`} />
      {state.replace("_", " ")}
    </span>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs font-medium text-zinc-600 dark:text-zinc-400">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-xs text-zinc-500">{hint}</span>}
    </label>
  );
}

export const inputClass =
  "block w-full rounded-md bg-white px-2.5 py-1.5 text-sm ring-1 ring-zinc-300 outline-none focus:ring-2 focus:ring-indigo-500 dark:bg-zinc-950 dark:ring-zinc-700";

export function Input(props: InputHTMLAttributes<HTMLInputElement>) {
  return <input {...props} className={`${inputClass} ${props.className ?? ""}`} />;
}

export function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-black/40 p-4 sm:items-center" onClick={onClose}>
      <div
        className="w-full max-w-lg rounded-lg bg-white shadow-xl ring-1 ring-zinc-200 dark:bg-zinc-900 dark:ring-zinc-800"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label={title}
      >
        <header className="border-b border-zinc-200 px-5 py-3 text-sm font-semibold dark:border-zinc-800">{title}</header>
        <div className="p-5">{children}</div>
      </div>
    </div>
  );
}

export function ErrorText({ error }: { error: unknown }) {
  if (!error) return null;
  return <p className="rounded-md bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/50 dark:text-red-300">{String((error as Error).message ?? error)}</p>;
}

export function Meter({ value, max, className = "" }: { value: number; max: number; className?: string }) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  return (
    <div className={`h-1.5 w-full overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800 ${className}`}>
      <div className={`h-full rounded-full ${pct > 90 ? "bg-red-500" : pct > 75 ? "bg-amber-500" : "bg-indigo-500"}`} style={{ width: `${pct}%` }} />
    </div>
  );
}

export function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div>
      <div className="text-xs text-zinc-500">{label}</div>
      <div className="mt-0.5 text-lg font-semibold tabular-nums">{value}</div>
      {sub && <div className="text-xs text-zinc-500">{sub}</div>}
    </div>
  );
}
