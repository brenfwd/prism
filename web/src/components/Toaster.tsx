import { Link } from "@tanstack/react-router";
import { dismiss, toasts, type Toast } from "../lib/toast";
import { useStore } from "../lib/store";
import { Icon, type IconName } from "./icons";
import { cx } from "./ui";

const toneIcon: Record<Toast["tone"], { icon: IconName; cls: string }> = {
  success: { icon: "check", cls: "bg-emerald-500 text-white" },
  error: { icon: "alert", cls: "bg-red-500 text-white" },
  warning: { icon: "alert", cls: "bg-amber-500 text-white" },
  info: { icon: "info", cls: "bg-zinc-500 text-white dark:bg-zinc-600" },
};

export function Toaster() {
  const list = useStore(toasts);
  return (
    <div
      aria-live="polite"
      className="pointer-events-none fixed inset-x-0 bottom-0 z-[60] flex flex-col items-center gap-2 p-3 sm:inset-x-auto sm:right-0 sm:items-end sm:p-5"
    >
      {list.map((t) => {
        const ti = toneIcon[t.tone];
        return (
          <div
            key={t.id}
            role={t.tone === "error" ? "alert" : "status"}
            className="pointer-events-auto flex w-full max-w-sm items-start gap-3 rounded-xl bg-white/95 p-3 pr-2 shadow-lg shadow-zinc-900/10 ring-1 ring-zinc-900/10 backdrop-blur animate-in dark:bg-zinc-800/95 dark:ring-white/10"
          >
            <span className={cx("mt-0.5 grid size-5 shrink-0 place-items-center rounded-full", ti.cls)}>
              <Icon name={ti.icon} className="size-3" strokeWidth={3} />
            </span>
            <div className="min-w-0 flex-1 text-sm">
              <p className="font-medium">{t.title}</p>
              {t.body && <p className="mt-0.5 line-clamp-3 text-[13px] text-zinc-500 dark:text-zinc-400">{t.body}</p>}
              {(t.vm || t.action) && (
                <div className="mt-1.5 flex gap-3 text-[13px] font-medium">
                  {t.vm && (
                    <Link
                      to="/vms/$id"
                      params={{ id: t.vm.id }}
                      search={t.vm.tab ? { tab: t.vm.tab } : {}}
                      onClick={() => dismiss(t.id)}
                      className="text-indigo-600 hover:underline dark:text-indigo-400"
                    >
                      {t.vm.label ?? "View VM"}
                    </Link>
                  )}
                  {t.action && (
                    <button
                      className="text-indigo-600 hover:underline dark:text-indigo-400"
                      onClick={() => {
                        dismiss(t.id);
                        t.action!.run();
                      }}
                    >
                      {t.action.label}
                    </button>
                  )}
                </div>
              )}
            </div>
            <button
              aria-label="Dismiss"
              onClick={() => dismiss(t.id)}
              className="rounded-md p-1 text-zinc-400 hover:bg-zinc-900/5 hover:text-zinc-600 dark:hover:bg-white/10 dark:hover:text-zinc-200"
            >
              <Icon name="x" className="size-3.5" />
            </button>
          </div>
        );
      })}
    </div>
  );
}
