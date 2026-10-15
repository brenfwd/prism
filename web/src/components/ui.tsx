import {
  forwardRef, useEffect, useId, useLayoutEffect, useRef, useState,
  type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";
import { createPortal } from "react-dom";
import { humanError, type VmState } from "../api";
import { Icon, Spinner, type IconName } from "./icons";

export const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");

// ---------------------------------------------------------------- buttons

type Variant = "primary" | "secondary" | "danger" | "ghost" | "subtle";
type Size = "xs" | "sm" | "md" | "lg";

const variants: Record<Variant, string> = {
  primary:
    "bg-indigo-600 text-white shadow-sm shadow-indigo-900/10 hover:bg-indigo-500 active:bg-indigo-700 disabled:bg-indigo-600/60 dark:bg-indigo-500 dark:hover:bg-indigo-400 dark:disabled:bg-indigo-500/50",
  secondary:
    "bg-white text-zinc-800 shadow-sm shadow-zinc-900/5 ring-1 ring-zinc-300/80 ring-inset hover:bg-zinc-50 active:bg-zinc-100 dark:bg-zinc-800/80 dark:text-zinc-100 dark:ring-zinc-700 dark:hover:bg-zinc-700/80",
  danger: "bg-red-600 text-white shadow-sm hover:bg-red-500 active:bg-red-700 disabled:bg-red-600/50",
  ghost: "text-zinc-600 hover:bg-zinc-900/5 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-100",
  subtle:
    "bg-zinc-900/5 text-zinc-700 hover:bg-zinc-900/10 dark:bg-white/5 dark:text-zinc-200 dark:hover:bg-white/10",
};

const sizes: Record<Size, string> = {
  xs: "h-7 gap-1 rounded-md px-2 text-xs",
  sm: "h-8 gap-1.5 rounded-md px-2.5 text-[13px]",
  md: "h-9 gap-2 rounded-lg px-3.5 text-sm",
  lg: "h-11 gap-2 rounded-lg px-5 text-[15px]",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  icon?: IconName;
  loading?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", icon, loading, className, children, disabled, type = "button", ...props },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      className={cx(
        "inline-flex shrink-0 select-none items-center justify-center font-medium whitespace-nowrap transition-[background-color,color,box-shadow,opacity] duration-100 disabled:cursor-not-allowed disabled:opacity-60",
        variants[variant],
        sizes[size],
        className,
      )}
      {...props}
    >
      {loading ? <Spinner className={size === "xs" ? "size-3" : "size-4"} /> : icon && <Icon name={icon} className={size === "xs" ? "size-3.5" : "size-4"} />}
      {children}
    </button>
  );
});

export function IconButton({
  icon, label, size = "sm", variant = "ghost", className, ...props
}: { icon: IconName; label: string; size?: "xs" | "sm" | "md"; variant?: Variant } & ButtonHTMLAttributes<HTMLButtonElement>) {
  const box = { xs: "size-7", sm: "size-8", md: "size-9" }[size];
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cx("inline-flex shrink-0 items-center justify-center rounded-md transition-colors disabled:opacity-40", variants[variant], box, className)}
      {...props}
    >
      <Icon name={icon} className={size === "xs" ? "size-3.5" : "size-4"} />
    </button>
  );
}

// ---------------------------------------------------------------- surfaces

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

// ---------------------------------------------------------------- forms

export function Field({ label, hint, error, children, htmlFor, className, aside }: {
  label: ReactNode; hint?: ReactNode; error?: ReactNode; children: ReactNode; htmlFor?: string; className?: string; aside?: ReactNode;
}) {
  return (
    <div className={className}>
      <div className="mb-1.5 flex items-baseline justify-between gap-2">
        <label htmlFor={htmlFor} className="text-[13px] font-medium text-zinc-700 dark:text-zinc-300">{label}</label>
        {aside && <span className="text-xs text-zinc-500">{aside}</span>}
      </div>
      {children}
      {error ? (
        <p className="mt-1.5 flex items-center gap-1 text-xs text-red-600 dark:text-red-400">
          <Icon name="alert" className="size-3.5" />
          {error}
        </p>
      ) : (
        hint && <p className="mt-1.5 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">{hint}</p>
      )}
    </div>
  );
}

export const inputClass =
  "block w-full rounded-lg bg-white px-3 py-2 text-sm text-zinc-900 shadow-sm shadow-zinc-900/[0.03] ring-1 ring-zinc-300 ring-inset outline-none placeholder:text-zinc-400 transition-shadow focus:ring-2 focus:ring-indigo-500 disabled:opacity-60 aria-invalid:ring-red-400 dark:bg-zinc-950/60 dark:text-zinc-100 dark:ring-zinc-700 dark:placeholder:text-zinc-500 dark:focus:ring-indigo-400 dark:aria-invalid:ring-red-500/70";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input(props, ref) {
  return <input ref={ref} {...props} className={cx(inputClass, props.className)} />;
});

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(function Textarea(props, ref) {
  return <textarea ref={ref} {...props} className={cx(inputClass, "font-mono text-[12.5px] leading-relaxed", props.className)} />;
});

export function Select(props: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <div className="relative">
      <select {...props} className={cx(inputClass, "appearance-none pr-9", props.className)} />
      <Icon name="chevronDown" className="pointer-events-none absolute top-1/2 right-3 size-4 -translate-y-1/2 text-zinc-400" />
    </div>
  );
}

export function Segmented<T extends string>({ value, onChange, options, label, size = "md", className }: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: ReactNode; hint?: string; disabled?: boolean }[];
  label: string;
  size?: "sm" | "md";
  className?: string;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const idx = options.findIndex((o) => o.value === value);
  const move = (d: number) => {
    let i = idx;
    for (let n = 0; n < options.length; n++) {
      i = (i + d + options.length) % options.length;
      if (!options[i].disabled) break;
    }
    onChange(options[i].value);
    refs.current[i]?.focus();
  };
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cx("inline-flex rounded-lg bg-zinc-900/[0.05] p-0.5 dark:bg-white/[0.06]", className)}
      onKeyDown={(e) => {
        if (e.key === "ArrowRight" || e.key === "ArrowDown") (e.preventDefault(), move(1));
        if (e.key === "ArrowLeft" || e.key === "ArrowUp") (e.preventDefault(), move(-1));
      }}
    >
      {options.map((o, i) => (
        <button
          key={o.value}
          ref={(el) => {
            refs.current[i] = el;
          }}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          tabIndex={o.value === value ? 0 : -1}
          disabled={o.disabled}
          title={o.hint}
          onClick={() => onChange(o.value)}
          className={cx(
            "flex-1 rounded-md font-medium whitespace-nowrap transition-all disabled:opacity-40",
            size === "sm" ? "px-2.5 py-1 text-xs" : "px-3 py-1.5 text-[13px]",
            o.value === value
              ? "bg-white text-zinc-900 shadow-sm ring-1 ring-zinc-900/5 dark:bg-zinc-700 dark:text-white dark:ring-white/10"
              : "text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Switch({ checked, onChange, label, description, disabled }: {
  checked: boolean; onChange: (v: boolean) => void; label: ReactNode; description?: ReactNode; disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <label htmlFor={id} className="text-sm font-medium">{label}</label>
        {description && <p className="mt-0.5 text-[13px] leading-relaxed text-zinc-500 dark:text-zinc-400">{description}</p>}
      </div>
      <button
        id={id}
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={cx(
          "relative mt-0.5 inline-flex h-6 w-10 shrink-0 rounded-full p-0.5 transition-colors disabled:opacity-50",
          checked ? "bg-indigo-600 dark:bg-indigo-500" : "bg-zinc-300 dark:bg-zinc-700",
        )}
      >
        <span className={cx("size-5 rounded-full bg-white shadow-sm transition-transform", checked && "translate-x-4")} />
      </button>
    </div>
  );
}

export function Checkbox({ checked, onChange, children, disabled }: {
  checked: boolean; onChange: (v: boolean) => void; children: ReactNode; disabled?: boolean;
}) {
  return (
    <label className={cx("flex items-start gap-2.5 text-sm", disabled && "opacity-60")}>
      <input
        type="checkbox"
        className="mt-0.5 size-4 rounded border-zinc-300 accent-indigo-600"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>{children}</span>
    </label>
  );
}

// ---------------------------------------------------------------- data display

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

// ---------------------------------------------------------------- overlays

function focusables(el: HTMLElement): HTMLElement[] {
  return [...el.querySelectorAll<HTMLElement>(
    'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])',
  )].filter((e) => e.offsetParent !== null || e === document.activeElement);
}

/** Keeps Tab inside `el`, closes on Escape and restores focus afterwards. */
export function useFocusTrap(ref: React.RefObject<HTMLElement | null>, onClose: () => void, autoFocus = true) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const before = document.activeElement as HTMLElement | null;
    if (autoFocus) {
      // Focus the dialog itself unless a field asks for it, so no button
      // shows a focus ring before the user does anything.
      const target = el.querySelector<HTMLElement>("[autofocus],[data-autofocus]") ?? el;
      target.focus({ preventScroll: true });
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close.current();
      } else if (e.key === "Tab") {
        const f = focusables(el);
        if (!f.length) return;
        const first = f[0];
        const last = f[f.length - 1];
        if (e.shiftKey && document.activeElement === first) (e.preventDefault(), last.focus());
        else if (!e.shiftKey && document.activeElement === last) (e.preventDefault(), first.focus());
      }
    };
    el.addEventListener("keydown", onKey);
    return () => {
      el.removeEventListener("keydown", onKey);
      before?.focus?.({ preventScroll: true });
    };
  }, [ref, autoFocus]);
}

export function Modal({ title, description, onClose, children, footer, size = "md", icon }: {
  title: ReactNode;
  description?: ReactNode;
  onClose: () => void;
  children?: ReactNode;
  footer?: ReactNode;
  size?: "sm" | "md" | "lg";
  icon?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useFocusTrap(ref, onClose);
  return createPortal(
    <div className="fixed inset-0 z-50 flex items-end justify-center bg-zinc-950/40 p-0 backdrop-blur-[2px] animate-fade sm:items-start sm:p-4 sm:pt-[12vh]" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        className={cx(
          "flex max-h-[92vh] w-full flex-col overflow-hidden rounded-t-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 outline-none animate-pop sm:rounded-2xl dark:bg-zinc-900 dark:ring-white/10",
          { sm: "sm:max-w-md", md: "sm:max-w-lg", lg: "sm:max-w-2xl" }[size],
        )}
      >
        <header className="flex items-start gap-3 px-5 pt-5 pb-1">
          {icon}
          <div className="min-w-0 flex-1">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">{title}</h2>
            {description && <p className="mt-1 text-[13px] leading-relaxed text-zinc-500 dark:text-zinc-400">{description}</p>}
          </div>
          <IconButton icon="x" label="Close" size="xs" onClick={onClose} className="-mt-1 -mr-1" />
        </header>
        {children && <div className="scrollbar-thin overflow-y-auto px-5 py-4">{children}</div>}
        {footer && (
          <footer className="flex flex-wrap items-center justify-end gap-2 border-t border-zinc-100 bg-zinc-50/70 px-5 py-3 dark:border-zinc-800 dark:bg-zinc-900/60">
            {footer}
          </footer>
        )}
      </div>
    </div>,
    document.body,
  );
}

/** A floating menu anchored under its trigger. */
export function Menu({ trigger, items, align = "end", label }: {
  trigger: (props: { onClick: () => void; "aria-expanded": boolean; "aria-haspopup": "menu"; ref: React.Ref<HTMLButtonElement> }) => ReactNode;
  items: ({ label: ReactNode; icon?: IconName; onSelect: () => void; danger?: boolean; disabled?: boolean; hint?: ReactNode } | "separator")[];
  align?: "start" | "end";
  label: string;
}) {
  const [open, setOpen] = useState(false);
  const btn = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left?: number; right?: number }>({ top: 0 });
  useLayoutEffect(() => {
    if (!open || !btn.current) return;
    const r = btn.current.getBoundingClientRect();
    setPos(align === "end" ? { top: r.bottom + 6, right: window.innerWidth - r.right } : { top: r.bottom + 6, left: r.left });
  }, [open, align]);
  useEffect(() => {
    if (!open) return;
    const items = () => [...(menu.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? [])];
    items()[0]?.focus();
    const onDown = (e: MouseEvent) => {
      if (!menu.current?.contains(e.target as Node) && !btn.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      const list = items();
      const i = list.indexOf(document.activeElement as HTMLElement);
      if (e.key === "Escape") (setOpen(false), btn.current?.focus());
      else if (e.key === "ArrowDown") (e.preventDefault(), list[(i + 1) % list.length]?.focus());
      else if (e.key === "ArrowUp") (e.preventDefault(), list[(i - 1 + list.length) % list.length]?.focus());
      else if (e.key === "Tab") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return (
    <>
      {trigger({ onClick: () => setOpen((o) => !o), "aria-expanded": open, "aria-haspopup": "menu", ref: btn })}
      {open &&
        createPortal(
          <div
            ref={menu}
            role="menu"
            aria-label={label}
            className="fixed z-50 min-w-48 overflow-hidden rounded-xl bg-white p-1 shadow-lg ring-1 ring-zinc-900/10 animate-pop dark:bg-zinc-800 dark:ring-white/10"
            style={pos}
          >
            {items.map((it, i) =>
              it === "separator" ? (
                <div key={i} className="my-1 h-px bg-zinc-100 dark:bg-white/10" />
              ) : (
                <button
                  key={i}
                  type="button"
                  role="menuitem"
                  disabled={it.disabled}
                  onClick={() => {
                    setOpen(false);
                    it.onSelect();
                  }}
                  className={cx(
                    "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-[13px] outline-none disabled:opacity-40",
                    it.danger
                      ? "text-red-600 hover:bg-red-50 focus:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10 dark:focus:bg-red-500/10"
                      : "text-zinc-700 hover:bg-zinc-100 focus:bg-zinc-100 dark:text-zinc-200 dark:hover:bg-white/10 dark:focus:bg-white/10",
                  )}
                >
                  {it.icon && <Icon name={it.icon} className="size-4 opacity-70" />}
                  <span className="flex-1">{it.label}</span>
                  {it.hint && <span className="text-xs text-zinc-400">{it.hint}</span>}
                </button>
              ),
            )}
          </div>,
          document.body,
        )}
    </>
  );
}
