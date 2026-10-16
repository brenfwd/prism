import type { ReactNode } from "react";
import { Icon, type IconName } from "../../components/icons";
import { cx } from "../../components/ui";

/** A numbered card heading. */
export function Step({ n, children }: { n: number; children: ReactNode }) {
  return (
    <span className="flex items-center gap-2.5">
      <span className="grid size-6 place-items-center rounded-full bg-zinc-900 text-xs font-semibold text-white dark:bg-white dark:text-zinc-900">{n}</span>
      {children}
    </span>
  );
}

/** A radio card for a preset size; a warning icon shows when `warn` is set. */
export function PresetCard({ active, onClick, title, children, warn }: { active: boolean; onClick: () => void; title: string; children: ReactNode; warn?: string }) {
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

/** A radio card with an icon, for choosing between a few options. */
export function Choice({ active, onClick, icon, title, body }: { active: boolean; onClick: () => void; icon: IconName; title: string; body: string }) {
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

/** One label and value line in the summary list. */
export function SumRow({ icon, k, v, warn }: { icon: IconName; k: string; v: ReactNode; warn?: boolean }) {
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
