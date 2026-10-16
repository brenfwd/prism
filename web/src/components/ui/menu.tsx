import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type Ref } from "react";
import { createPortal } from "react-dom";
import { Icon, type IconName } from "../icons";
import { cx } from "./cx";

export function Menu({ trigger, items, align = "end", label }: {
  trigger: (props: { onClick: () => void; "aria-expanded": boolean; "aria-haspopup": "menu"; ref: Ref<HTMLButtonElement> }) => ReactNode;
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
