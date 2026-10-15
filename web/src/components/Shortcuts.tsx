import { useEffect } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { closeOverlay, openPalette, openShortcuts, overlay } from "../lib/overlays";
import { useStore } from "../lib/store";
import { toggleTheme } from "../lib/theme";
import { CommandPalette } from "./CommandPalette";
import { Kbd, Modal, modKey } from "./ui";

export const VM_TABS = ["overview", "console", "serial", "hardware", "snapshots", "settings"] as const;

function typing(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  if (!t) return false;
  return t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName) || !!t.closest("[data-console]");
}

/** Global keyboard shortcuts plus the palette and cheat-sheet overlays. */
export function Shortcuts() {
  const navigate = useNavigate();
  const open = useStore(overlay);
  const path = useRouterState({ select: (s) => s.location.pathname });

  useEffect(() => {
    let leader = 0;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "k") {
        // The console keeps Ctrl+K for the guest; Cmd+K still works on macOS.
        if ((e.target as HTMLElement)?.closest?.("[data-console]") && !e.metaKey) return;
        e.preventDefault();
        overlay.set((o) => (o === "palette" ? null : "palette"));
        return;
      }
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || typing(e) || overlay.get()) return;
      const k = e.key;
      if (Date.now() - leader < 1200) {
        leader = 0;
        const to = { o: "/", m: "/vms", v: "/vms", s: "/storage", t: "/tasks" }[k.toLowerCase()];
        if (to) {
          e.preventDefault();
          navigate({ to });
        }
        return;
      }
      if (k === "g") leader = Date.now();
      else if (k === "?") (e.preventDefault(), openShortcuts());
      else if (k === "/") (e.preventDefault(), openPalette());
      else if (k === "n" || k === "c") (e.preventDefault(), navigate({ to: "/vms/new" }));
      else if (k === "T") toggleTheme();
      else if (/^[1-6]$/.test(k)) {
        const m = /^\/vms\/([0-9a-f-]{36})$/.exec(path);
        if (m) {
          e.preventDefault();
          navigate({ to: "/vms/$id", params: { id: m[1] }, search: { tab: VM_TABS[+k - 1] } });
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate, path]);

  if (open === "palette") return <CommandPalette />;
  if (open === "shortcuts") return <CheatSheet />;
  return null;
}

const groups: { title: string; keys: [string[], string][] }[] = [
  {
    title: "Anywhere",
    keys: [
      [[modKey, "K"], "Command palette"],
      [["/"], "Command palette"],
      [["N"], "New virtual machine"],
      [["?"], "This cheat sheet"],
      [["Shift", "T"], "Toggle light / dark"],
    ],
  },
  {
    title: "Go to",
    keys: [
      [["G", "O"], "Overview"],
      [["G", "M"], "Machines"],
      [["G", "S"], "Storage"],
      [["G", "T"], "Tasks"],
    ],
  },
  {
    title: "On a machine",
    keys: [
      [["1"], "Overview"],
      [["2"], "Console"],
      [["3"], "Serial"],
      [["4"], "Hardware"],
      [["5"], "Snapshots"],
      [["6"], "Settings"],
    ],
  },
];

function CheatSheet() {
  return (
    <Modal title="Keyboard shortcuts" description="Shortcuts pause while you type in a field or the console has focus." onClose={closeOverlay} size="lg">
      <div className="grid gap-6 sm:grid-cols-3">
        {groups.map((g) => (
          <div key={g.title}>
            <h3 className="mb-2 text-[11px] font-semibold tracking-wide text-zinc-400 uppercase">{g.title}</h3>
            <ul className="space-y-2">
              {g.keys.map(([keys, what]) => (
                <li key={what + keys.join()} className="flex items-center justify-between gap-3 text-[13px]">
                  <span className="text-zinc-600 dark:text-zinc-300">{what}</span>
                  <span className="flex gap-1">
                    {keys.map((k, i) => (
                      <Kbd key={i}>{k}</Kbd>
                    ))}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </Modal>
  );
}
