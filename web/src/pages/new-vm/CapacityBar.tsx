import { bytes } from "../../lib/format";
import { cx } from "../../components/ui";

/** Used space plus the space this machine would add, as a striped bar that turns amber when it overflows. */
export function CapacityBar({ label, used, adding, total, className, note }: { label: string; used: number; adding: number; total: number; className?: string; note?: string }) {
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
