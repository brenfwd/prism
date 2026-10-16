import type { ReactNode } from "react";
import { humanError, type VmState } from "../../api";
import { Icon, Spinner, type IconName } from "../icons";
import { cx } from "./cx";

export function Card({ title, description, actions, children, className, padded = true, id }: {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  className?: string;
  padded?: boolean;
  id?: string;
}) {
  return (
    <section
      id={id}
      className={cx(
        "rounded-xl bg-white shadow-sm shadow-zinc-900/[0.03] ring-1 ring-zinc-200/90 dark:bg-zinc-900/70 dark:shadow-none dark:ring-zinc-800",
        className,
      )}
    >
      {(title || actions) && (
        <header className={cx("flex flex-wrap items-start justify-between gap-x-4 gap-y-2 px-5 pt-4", !padded && "pb-3")}>
          <div className="min-w-0">
            {title && <h2 className="text-[15px] font-semibold tracking-tight">{title}</h2>}
            {description && <p className="mt-0.5 text-[13px] text-zinc-500 dark:text-zinc-400">{description}</p>}
          </div>
          {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
        </header>
      )}
      {children !== undefined && <div className={padded ? "px-5 pt-4 pb-5" : ""}>{children}</div>}
    </section>
  );
}

type Tone = "neutral" | "green" | "amber" | "red" | "indigo" | "sky";
const tones: Record<Tone, string> = {
  neutral: "bg-zinc-100 text-zinc-700 ring-zinc-500/15 dark:bg-zinc-800 dark:text-zinc-300 dark:ring-white/10",
  green: "bg-emerald-50 text-emerald-700 ring-emerald-600/20 dark:bg-emerald-500/10 dark:text-emerald-300 dark:ring-emerald-400/20",
  amber: "bg-amber-50 text-amber-800 ring-amber-600/20 dark:bg-amber-400/10 dark:text-amber-300 dark:ring-amber-400/20",
  red: "bg-red-50 text-red-700 ring-red-600/15 dark:bg-red-500/10 dark:text-red-300 dark:ring-red-400/20",
  indigo: "bg-indigo-50 text-indigo-700 ring-indigo-600/15 dark:bg-indigo-500/10 dark:text-indigo-300 dark:ring-indigo-400/25",
  sky: "bg-sky-50 text-sky-700 ring-sky-600/15 dark:bg-sky-500/10 dark:text-sky-300 dark:ring-sky-400/20",
};

export function Badge({ tone = "neutral", children, className, icon, title }: {
  tone?: Tone; children: ReactNode; className?: string; icon?: IconName; title?: string;
}) {
  return (
    <span
      title={title}
      className={cx("inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] font-medium whitespace-nowrap ring-1 ring-inset", tones[tone], className)}
    >
      {icon && <Icon name={icon} className="size-3" />}
      {children}
    </span>
  );
}

export const stateMeta: Record<VmState, { label: string; tone: Tone; dot: string }> = {
  running: { label: "Running", tone: "green", dot: "bg-emerald-500" },
  paused: { label: "Paused", tone: "amber", dot: "bg-amber-500" },
  shutting_down: { label: "Shutting down", tone: "amber", dot: "bg-amber-500" },
  suspended: { label: "Suspended", tone: "amber", dot: "bg-amber-500" },
  stopped: { label: "Stopped", tone: "neutral", dot: "bg-zinc-400 dark:bg-zinc-500" },
  crashed: { label: "Crashed", tone: "red", dot: "bg-red-500" },
  missing: { label: "Missing", tone: "red", dot: "bg-red-500" },
  unknown: { label: "Unknown", tone: "neutral", dot: "bg-zinc-400" },
};

export function StateDot({ state, className }: { state: VmState; className?: string }) {
  const m = stateMeta[state];
  return (
    <span className={cx("relative inline-flex size-2.5 shrink-0", className)} aria-hidden="true">
      {(state === "running" || state === "shutting_down") && (
        <span className={cx("absolute inset-0 animate-ping rounded-full opacity-30", m.dot)} style={{ animationDuration: "2.4s" }} />
      )}
      <span className={cx("relative inline-flex size-2.5 rounded-full", m.dot)} />
    </span>
  );
}

export function StateBadge({ state, pending }: { state: VmState; pending?: string | null }) {
  const m = stateMeta[state];
  return (
    <span className={cx("inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ring-1 ring-inset", tones[m.tone])}>
      {pending ? <Spinner className="size-3" /> : <StateDot state={state} className="size-2 [&>span]:size-2" />}
      {pending ?? m.label}
    </span>
  );
}

export function Callout({ tone = "info", title, children, icon, className, action }: {
  tone?: "info" | "warning" | "error" | "success" | "tip";
  title?: ReactNode;
  children?: ReactNode;
  icon?: IconName;
  className?: string;
  action?: ReactNode;
}) {
  const style = {
    info: "bg-sky-50 text-sky-900 ring-sky-200 dark:bg-sky-400/[0.07] dark:text-sky-100 dark:ring-sky-400/20 [&_svg]:text-sky-600 dark:[&_svg]:text-sky-400",
    tip: "bg-indigo-50/70 text-indigo-950 ring-indigo-200/80 dark:bg-indigo-400/[0.07] dark:text-indigo-100 dark:ring-indigo-400/20 [&_svg]:text-indigo-600 dark:[&_svg]:text-indigo-400",
    warning: "bg-amber-50 text-amber-950 ring-amber-200 dark:bg-amber-400/[0.07] dark:text-amber-100 dark:ring-amber-400/20 [&_svg]:text-amber-600 dark:[&_svg]:text-amber-400",
    error: "bg-red-50 text-red-900 ring-red-200 dark:bg-red-500/[0.08] dark:text-red-100 dark:ring-red-400/20 [&_svg]:text-red-600 dark:[&_svg]:text-red-400",
    success: "bg-emerald-50 text-emerald-950 ring-emerald-200 dark:bg-emerald-400/[0.07] dark:text-emerald-100 dark:ring-emerald-400/20 [&_svg]:text-emerald-600 dark:[&_svg]:text-emerald-400",
  }[tone];
  const ic: IconName = icon ?? (tone === "warning" || tone === "error" ? "alert" : tone === "success" ? "check" : tone === "tip" ? "sparkles" : "info");
  return (
    <div role={tone === "error" ? "alert" : undefined} className={cx("flex gap-3 rounded-lg px-3.5 py-3 text-[13px] leading-relaxed ring-1 ring-inset", style, className)}>
      <Icon name={ic} className="mt-0.5 size-4" />
      <div className="min-w-0 flex-1">
        {title && <div className="font-medium">{title}</div>}
        {children && <div className={cx(!!title && "mt-0.5", "opacity-90")}>{children}</div>}
      </div>
      {action && <div className="shrink-0 self-center">{action}</div>}
    </div>
  );
}

export function ErrorText({ error, className }: { error: unknown; className?: string }) {
  if (!error) return null;
  return <Callout tone="error" className={className}>{humanError(error)}</Callout>;
}

export function EmptyState({ icon, title, children, actions, className }: {
  icon: IconName; title: ReactNode; children?: ReactNode; actions?: ReactNode; className?: string;
}) {
  return (
    <div className={cx("flex flex-col items-center px-6 py-12 text-center", className)}>
      <div className="mb-4 grid size-12 place-items-center rounded-2xl bg-gradient-to-b from-indigo-50 to-white text-indigo-600 shadow-sm ring-1 ring-indigo-100 dark:from-indigo-500/15 dark:to-indigo-500/5 dark:text-indigo-300 dark:ring-indigo-400/20">
        <Icon name={icon} className="size-6" />
      </div>
      <h3 className="text-base font-semibold tracking-tight">{title}</h3>
      {children && <div className="mt-1.5 max-w-md text-sm text-zinc-500 dark:text-zinc-400">{children}</div>}
      {actions && <div className="mt-5 flex flex-wrap justify-center gap-2">{actions}</div>}
    </div>
  );
}

export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={cx(
        "inline-flex h-5 min-w-5 items-center justify-center rounded border border-b-2 border-zinc-200 bg-white px-1 font-sans text-[11px] font-medium text-zinc-500 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-400",
        className,
      )}
    >
      {children}
    </kbd>
  );
}

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
export const modKey = isMac ? "⌘" : "Ctrl";
