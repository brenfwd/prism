import {
  forwardRef, useId, useRef,
  type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes,
} from "react";
import { Icon } from "../icons";
import { cx } from "./cx";

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
