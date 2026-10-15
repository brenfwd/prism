import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import type { Vm } from "../api";
import { actionMeta, availableActions, runAction, usePools, useVms } from "../lib/hooks";
import { closeOverlay, openShortcuts, pickFiles } from "../lib/overlays";
import { startUpload } from "../lib/tasks";
import { toggleTheme } from "../lib/theme";
import { Icon, type IconName } from "./icons";
import { Kbd, StateDot, cx } from "./ui";

interface Command {
  id: string;
  group: "Machines" | "Actions" | "Go to" | "General";
  label: string;
  /** Extra words that match but aren't shown. */
  keywords?: string;
  icon: IconName;
  vm?: Vm;
  hint?: ReactNode;
  run: () => void;
}

/** Scores `text` against the query; 0 means no match. */
function score(query: string, text: string): number {
  const q = query.toLowerCase().trim();
  if (!q) return 1;
  const t = text.toLowerCase();
  let total = 0;
  for (const token of q.split(/\s+/)) {
    const i = t.indexOf(token);
    if (i < 0) {
      // Fall back to an in-order subsequence match for typos like "shdn".
      let j = 0;
      for (const ch of t) if (ch === token[j]) j++;
      if (j < token.length) return 0;
      total += 1;
      continue;
    }
    const wordStart = i === 0 || /[\s\-_.]/.test(t[i - 1]);
    total += wordStart ? 10 : 4;
    if (i === 0) total += 4;
  }
  return total;
}

export function CommandPalette() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const vms = useVms().data ?? [];
  const pools = usePools().data ?? [];
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const before = useRef<HTMLElement | null>(document.activeElement as HTMLElement | null);

  const go = (fn: () => void) => () => {
    closeOverlay();
    fn();
  };

  const commands = useMemo<Command[]>(() => {
    const cmds: Command[] = [
      { id: "new", group: "General", label: "New virtual machine", keywords: "create vm add machine", icon: "plus", hint: <Kbd>N</Kbd>, run: go(() => navigate({ to: "/vms/new" })) },
      {
        id: "upload", group: "General", label: "Upload an ISO or disk image…", keywords: "import file cloud image", icon: "upload",
        run: go(async () => {
          const pool = pools[0]?.name;
          if (!pool) return navigate({ to: "/storage" });
          const files = await pickFiles(".iso,.img,.qcow2,.raw");
          if (!files.length) return;
          navigate({ to: "/storage" });
          for (const f of files) void startUpload(qc, pool, f);
        }),
      },
      { id: "theme", group: "General", label: "Toggle light / dark theme", keywords: "appearance dark mode", icon: "moon", run: go(toggleTheme) },
      { id: "keys", group: "General", label: "Keyboard shortcuts", keywords: "help hotkeys", icon: "keyboard", hint: <Kbd>?</Kbd>, run: () => openShortcuts() },
      { id: "go-home", group: "Go to", label: "Overview", keywords: "dashboard home", icon: "home", hint: <><Kbd>G</Kbd><Kbd>O</Kbd></>, run: go(() => navigate({ to: "/" })) },
      { id: "go-vms", group: "Go to", label: "Machines", keywords: "vms list", icon: "server", hint: <><Kbd>G</Kbd><Kbd>M</Kbd></>, run: go(() => navigate({ to: "/vms" })) },
      { id: "go-storage", group: "Go to", label: "Storage", keywords: "pools volumes images iso", icon: "database", hint: <><Kbd>G</Kbd><Kbd>S</Kbd></>, run: go(() => navigate({ to: "/storage" })) },
      { id: "go-networks", group: "Go to", label: "Networks", keywords: "network subnet switch vlan dhcp", icon: "network", run: go(() => navigate({ to: "/networks" })) },
      { id: "go-tasks", group: "Go to", label: "Tasks", keywords: "activity jobs", icon: "activity", hint: <><Kbd>G</Kbd><Kbd>T</Kbd></>, run: go(() => navigate({ to: "/tasks" })) },
    ];
    for (const vm of vms) {
      const open = (tab?: string) => go(() => navigate({ to: "/vms/$id", params: { id: vm.id }, search: tab ? { tab } : {} }));
      cmds.push({ id: `vm-${vm.id}`, group: "Machines", label: vm.name, keywords: `open ${vm.description}`, icon: "server", vm, run: open() });
      for (const a of availableActions(vm.status.state)) {
        const m = actionMeta[a];
        cmds.push({
          id: `${a}-${vm.id}`, group: "Actions", label: `${m.label} ${vm.name}`,
          keywords: a === "start" ? "power on boot run" : a === "shutdown" ? "power off stop halt" : a === "stop" ? "kill power off" : "",
          icon: m.icon, vm, run: go(() => void runAction(qc, vm, a)),
        });
      }
      cmds.push({ id: `console-${vm.id}`, group: "Actions", label: `Open console of ${vm.name}`, keywords: "vnc screen display", icon: "monitor", vm, run: open("console") });
      cmds.push({ id: `snap-${vm.id}`, group: "Actions", label: `Snapshot ${vm.name}`, keywords: "take checkpoint backup", icon: "camera", vm, run: go(() => navigate({ to: "/vms/$id", params: { id: vm.id }, search: { tab: "snapshots", take: "now" } })) });
      cmds.push({ id: `hw-${vm.id}`, group: "Actions", label: `Edit hardware of ${vm.name}`, keywords: "disk nic cdrom iso insert", icon: "disk", vm, run: open("hardware") });
      cmds.push({ id: `set-${vm.id}`, group: "Actions", label: `Settings of ${vm.name}`, keywords: "cpu memory delete autostart", icon: "settings", vm, run: open("settings") });
    }
    return cmds;
  }, [vms, pools]);

  const results = useMemo(() => {
    const q = query.trim();
    if (!q) {
      // A calm default: general commands, machines, then pages.
      return [
        ...commands.filter((c) => c.group === "General"),
        ...commands.filter((c) => c.group === "Machines").slice(0, 8),
        ...commands.filter((c) => c.group === "Go to"),
      ];
    }
    return commands
      .map((c) => ({ c, s: Math.max(score(q, c.label), score(q, `${c.label} ${c.keywords ?? ""}`) * 0.6) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s || a.c.label.localeCompare(b.c.label))
      .slice(0, 40)
      .map((x) => x.c);
  }, [commands, query]);

  useEffect(() => setActive(0), [query]);
  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);
  useEffect(() => {
    input.current?.focus();
    const el = before.current;
    return () => el?.focus?.({ preventScroll: true });
  }, []);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      e.preventDefault();
      setActive((a) => Math.min(results.length - 1, a + 1));
    } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      e.preventDefault();
      setActive((a) => Math.max(0, a - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      results[active]?.run();
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeOverlay();
    }
  };

  // Group in result order when browsing; flat ranking when searching.
  let lastGroup = "";
  return createPortal(
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-zinc-950/40 p-3 pt-[10vh] backdrop-blur-[2px] animate-fade" onMouseDown={(e) => e.target === e.currentTarget && closeOverlay()}>
      <div role="dialog" aria-modal="true" aria-label="Command palette" className="flex max-h-[70vh] w-full max-w-xl flex-col overflow-hidden rounded-2xl bg-white shadow-2xl ring-1 ring-zinc-900/10 animate-pop dark:bg-zinc-900 dark:ring-white/10">
        <div className="flex items-center gap-3 border-b border-zinc-100 px-4 dark:border-zinc-800">
          <Icon name="search" className="size-[18px] text-zinc-400" />
          <input
            ref={input}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKey}
            placeholder="Search machines, or type a command like “start web”…"
            className="h-14 flex-1 bg-transparent text-[15px] outline-none placeholder:text-zinc-400"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={results[active] ? `cmd-${results[active].id}` : undefined}
            spellCheck={false}
            autoComplete="off"
          />
          <Kbd>Esc</Kbd>
        </div>
        <div ref={list} id="palette-list" role="listbox" className="scrollbar-thin overflow-y-auto p-2">
          {results.length === 0 && (
            <div className="px-3 py-10 text-center text-sm text-zinc-500">
              No matches for “{query}”.
            </div>
          )}
          {results.map((c, i) => {
            const header = !query.trim() && c.group !== lastGroup ? c.group : null;
            lastGroup = c.group;
            return (
              <div key={c.id}>
                {header && <div className="px-3 pt-2.5 pb-1 text-[11px] font-semibold tracking-wide text-zinc-400 uppercase">{header}</div>}
                <div
                  id={`cmd-${c.id}`}
                  data-index={i}
                  role="option"
                  aria-selected={i === active}
                  onMouseMove={() => setActive(i)}
                  onClick={() => c.run()}
                  className={cx(
                    "flex cursor-pointer items-center gap-3 rounded-lg px-3 py-2 text-sm",
                    i === active ? "bg-indigo-50 text-indigo-950 dark:bg-indigo-500/15 dark:text-white" : "text-zinc-700 dark:text-zinc-300",
                  )}
                >
                  <span className={cx("grid size-7 place-items-center rounded-md", i === active ? "bg-white text-indigo-600 shadow-sm dark:bg-indigo-500/20 dark:text-indigo-300" : "bg-zinc-100 text-zinc-500 dark:bg-zinc-800")}>
                    <Icon name={c.icon} className="size-4" />
                  </span>
                  <span className="min-w-0 flex-1 truncate">{c.label}</span>
                  {c.vm && c.group === "Machines" && (
                    <span className="flex items-center gap-1.5 text-xs text-zinc-500">
                      <StateDot state={c.vm.status.state} className="size-2 [&>span]:size-2" />
                      {c.vm.status.state.replace("_", " ")}
                    </span>
                  )}
                  {c.hint && <span className="flex gap-1">{c.hint}</span>}
                  {i === active && <Icon name="arrowRight" className="size-3.5 text-indigo-500" />}
                </div>
              </div>
            );
          })}
        </div>
        <div className="flex items-center gap-4 border-t border-zinc-100 px-4 py-2 text-[11px] text-zinc-500 dark:border-zinc-800">
          <span className="flex items-center gap-1"><Kbd>↑</Kbd><Kbd>↓</Kbd> move</span>
          <span className="flex items-center gap-1"><Kbd>↵</Kbd> run</span>
          <span className="ml-auto">Tip: try “snapshot”, “console” or a machine name</span>
        </div>
      </div>
    </div>,
    document.body,
  );
}
