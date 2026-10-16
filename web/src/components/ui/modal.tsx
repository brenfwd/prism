import { useEffect, useId, useRef, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { IconButton } from "./buttons";
import { cx } from "./cx";

function focusables(el: HTMLElement): HTMLElement[] {
  return [...el.querySelectorAll<HTMLElement>(
    'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])',
  )].filter((e) => e.offsetParent !== null || e === document.activeElement);
}

/** Keeps Tab inside `el`, closes on Escape and restores focus afterwards. */
export function useFocusTrap(ref: RefObject<HTMLElement | null>, onClose: () => void, autoFocus = true) {
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

