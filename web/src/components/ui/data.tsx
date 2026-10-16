import { useId, useState, type ReactNode } from "react";
import { Icon, type IconName } from "../icons";
import { IconButton } from "./buttons";
import { cx } from "./cx";

export function Meter({ value, max, className, tone }: { value: number; max: number; className?: string; tone?: "auto" | "indigo" }) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  const color = tone === "indigo" ? "bg-indigo-500" : pct > 90 ? "bg-red-500" : pct > 75 ? "bg-amber-500" : "bg-indigo-500";
  return (
    <div
      className={cx("h-1.5 w-full overflow-hidden rounded-full bg-zinc-900/[0.07] dark:bg-white/10", className)}
      role="meter"
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={value}
    >
      <div className={cx("h-full rounded-full transition-[width] duration-500", color)} style={{ width: `${pct}%` }} />
    </div>
  );
}

/** A progress bar; `null` shows an indeterminate shimmer. */
export function Progress({ value, className }: { value: number | null; className?: string }) {
  return (
    <div className={cx("relative h-1.5 w-full overflow-hidden rounded-full bg-zinc-900/[0.07] dark:bg-white/10", className)} role="progressbar" aria-valuemin={0} aria-valuemax={1} aria-valuenow={value ?? undefined}>
      {value == null ? (
        <div className="absolute inset-y-0 w-1/3 animate-[indeterminate_1.2s_ease-in-out_infinite] rounded-full bg-indigo-500" />
      ) : (
        <div className="h-full rounded-full bg-indigo-500 transition-[width] duration-300" style={{ width: `${Math.max(2, value * 100)}%` }} />
      )}
      <style>{"@keyframes indeterminate{0%{left:-35%}100%{left:100%}}"}</style>
    </div>
  );
}

/** A small area chart. Values are 0..max; `null` gaps are skipped. */
export function Sparkline({ values, max, capacity, className, color = "indigo", label }: {
  values: (number | null)[];
  max: number;
  capacity: number;
  className?: string;
  color?: "indigo" | "emerald" | "sky" | "amber";
  label: string;
}) {
  const id = useId().replace(/:/g, "");
  const W = 240;
  const H = 56;
  const pts: [number, number][] = [];
  const shown = values.slice(-capacity);
  const n = shown.length;
  // Until the ring fills, spread what we have over a shorter window.
  const span = Math.min(capacity, Math.max(n, 20));
  shown.forEach((v, i) => {
    if (v == null) return;
    const x = W - ((n - 1 - i) / Math.max(1, span - 1)) * W;
    const y = H - 2 - (Math.min(v, max) / (max || 1)) * (H - 6);
    pts.push([x, y]);
  });
  const stroke = { indigo: "#6366f1", emerald: "#10b981", sky: "#0ea5e9", amber: "#f59e0b" }[color];
  const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join("");
  const area = pts.length > 1 ? `${line}L${pts[pts.length - 1][0].toFixed(1)},${H}L${pts[0][0].toFixed(1)},${H}Z` : "";
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className={cx("h-14 w-full overflow-visible", className)} role="img" aria-label={label}>
      <defs>
        <linearGradient id={`g${id}`} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor={stroke} stopOpacity="0.28" />
          <stop offset="1" stopColor={stroke} stopOpacity="0" />
        </linearGradient>
      </defs>
      {[0.25, 0.5, 0.75].map((f) => (
        <line key={f} x1="0" x2={W} y1={H * f} y2={H * f} className="stroke-zinc-900/[0.05] dark:stroke-white/[0.06]" strokeWidth="1" vectorEffect="non-scaling-stroke" />
      ))}
      {area && <path d={area} fill={`url(#g${id})`} />}
      {pts.length > 1 && <path d={line} fill="none" stroke={stroke} strokeWidth="1.75" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />}
      {pts.length > 0 && <circle cx={pts[pts.length - 1][0]} cy={pts[pts.length - 1][1]} r="2.5" fill={stroke} vectorEffect="non-scaling-stroke" />}
    </svg>
  );
}

export function Stat({ label, value, sub, icon }: { label: ReactNode; value: ReactNode; sub?: ReactNode; icon?: IconName }) {
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-1.5 text-xs font-medium text-zinc-500 dark:text-zinc-400">
        {icon && <Icon name={icon} className="size-3.5" />}
        {label}
      </div>
      <div className="mt-1 truncate text-xl font-semibold tracking-tight tabular-nums">{value}</div>
      {sub && <div className="mt-0.5 truncate text-xs text-zinc-500 dark:text-zinc-400">{sub}</div>}
    </div>
  );
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <IconButton
      icon={done ? "check" : "copy"}
      label={done ? "Copied" : label}
      size="xs"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setDone(true);
          setTimeout(() => setDone(false), 1200);
        } catch {
          /* insecure context */
        }
      }}
    />
  );
}
