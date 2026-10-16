import { forwardRef, type ButtonHTMLAttributes } from "react";
import { Icon, Spinner, type IconName } from "../icons";
import { cx } from "./cx";

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
