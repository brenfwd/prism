import { Link, Outlet, useRouterState } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useEventStream, useStream } from "../events";
import { hostAvailableBytes, useNode, useVms } from "../lib/hooks";
import { openPalette, openShortcuts } from "../lib/overlays";
import { useStore } from "../lib/store";
import { dark, toggleTheme } from "../lib/theme";
import { bytes } from "../lib/format";
import { ConfirmHost } from "./Confirm";
import { Icon, type IconName } from "./icons";
import { Shortcuts } from "./Shortcuts";
import { TaskTray } from "./TaskTray";
import { Toaster } from "./Toaster";
import { Button, IconButton, Kbd, Meter, cx, modKey } from "./ui";

const nav: { to: "/" | "/vms" | "/storage" | "/networks" | "/tasks"; label: string; icon: IconName; exact?: boolean }[] = [
  { to: "/", label: "Overview", icon: "home", exact: true },
  { to: "/vms", label: "Machines", icon: "server" },
  { to: "/storage", label: "Storage", icon: "database" },
  { to: "/networks", label: "Networks", icon: "network" },
  { to: "/tasks", label: "Tasks", icon: "activity" },
];

export function Logo({ className = "size-6" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
      <defs>
        <linearGradient id="prism-a" x1="0" x2="1" y1="0" y2="1">
          <stop offset="0" stopColor="#818cf8" />
          <stop offset="1" stopColor="#4f46e5" />
        </linearGradient>
      </defs>
      <path d="M16 3 29 27H3z" fill="url(#prism-a)" />
      <path d="M16 3 22 27H10z" fill="#c7d2fe" opacity=".9" />
    </svg>
  );
}

function ConnectionDot() {
  const stream = useStream();
  const node = useNode();
  const ok = stream === "live" && node?.libvirt_connected !== false;
  const label = stream !== "live" ? (stream === "connecting" ? "Connecting…" : "Offline") : node?.libvirt_connected === false ? "libvirt offline" : "Live";
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-zinc-500" title={ok ? "Receiving live updates" : label}>
      <span className={cx("size-1.5 rounded-full", ok ? "bg-emerald-500" : stream === "connecting" ? "animate-pulse bg-amber-500" : "bg-red-500")} />
      {label}
    </span>
  );
}

function Sidebar() {
  const node = useNode();
  const vms = useVms().data ?? [];
  const running = vms.filter((v) => v.status.state === "running").length;
  const avail = hostAvailableBytes(node);
  const total = (node?.host.memory_kib ?? 0) * 1024;
  const isDark = useStore(dark);
  return (
    <aside className="sticky top-0 hidden h-screen w-60 shrink-0 flex-col border-r border-zinc-200/80 bg-white/60 px-3 py-4 lg:flex dark:border-zinc-800/80 dark:bg-zinc-900/30">
      <Link to="/" className="mb-5 flex items-center gap-2.5 px-2">
        <Logo />
        <span className="text-[17px] font-semibold tracking-tight">Prism</span>
        {node && (
          <span className="ml-auto truncate rounded-md bg-zinc-900/5 px-1.5 py-0.5 font-mono text-[11px] text-zinc-500 dark:bg-white/5" title={`Node ${node.name}`}>
            {node.name}
          </span>
        )}
      </Link>
      <Link
        to="/vms/new"
        className="mb-4 flex h-9 items-center justify-center gap-2 rounded-lg bg-indigo-600 text-sm font-medium text-white shadow-sm shadow-indigo-900/20 transition-colors hover:bg-indigo-500 dark:bg-indigo-500 dark:hover:bg-indigo-400"
      >
        <Icon name="plus" />
        New machine
      </Link>
      <button
        onClick={openPalette}
        className="mb-4 flex h-9 items-center gap-2 rounded-lg bg-zinc-900/[0.04] px-3 text-left text-[13px] text-zinc-500 ring-1 ring-zinc-900/5 transition-colors ring-inset hover:bg-zinc-900/[0.07] dark:bg-white/[0.04] dark:ring-white/5 dark:hover:bg-white/[0.07]"
      >
        <Icon name="search" className="size-4" />
        <span className="flex-1">Search…</span>
        <span className="flex gap-0.5"><Kbd>{modKey}</Kbd><Kbd>K</Kbd></span>
      </button>
      <nav className="space-y-0.5" aria-label="Main">
        {nav.map((n) => (
          <Link
            key={n.to}
            to={n.to}
            activeOptions={{ exact: n.exact }}
            className="group flex h-9 items-center gap-3 rounded-lg px-3 text-sm font-medium text-zinc-600 transition-colors hover:bg-zinc-900/5 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-white/5 dark:hover:text-zinc-100"
            activeProps={{ className: "!bg-indigo-50 !text-indigo-700 dark:!bg-indigo-500/10 dark:!text-indigo-300" }}
          >
            <Icon name={n.icon} className="size-[18px] opacity-80" />
            <span className="flex-1">{n.label}</span>
            {n.to === "/vms" && vms.length > 0 && (
              <span className="text-xs font-normal text-zinc-400 tabular-nums" title={`${running} running of ${vms.length}`}>
                {running}/{vms.length}
              </span>
            )}
          </Link>
        ))}
      </nav>
      <div className="flex-1" />
      {node && (
        <div className="mb-3 rounded-lg px-3 py-2.5 text-xs ring-1 ring-zinc-900/5 dark:ring-white/5">
          <div className="mb-1.5 flex justify-between text-zinc-500">
            <span>Host memory</span>
            {avail != null && <span className="tabular-nums">{bytes(avail)} free</span>}
          </div>
          {avail != null && <Meter value={total - avail} max={total} />}
          <div className="mt-2 flex justify-between text-zinc-500">
            <span>{node.host.cpus} CPUs</span>
            <span>{bytes(total)}</span>
          </div>
        </div>
      )}
      <div className="flex items-center justify-between px-1">
        <ConnectionDot />
        <div className="flex">
          <IconButton icon="keyboard" label="Keyboard shortcuts (?)" onClick={openShortcuts} />
          <IconButton icon={isDark ? "sun" : "moon"} label={isDark ? "Switch to light (Shift+T)" : "Switch to dark (Shift+T)"} onClick={toggleTheme} />
        </div>
      </div>
    </aside>
  );
}

function MobileTabs() {
  return (
    <nav aria-label="Main" className="fixed inset-x-0 bottom-0 z-40 grid grid-cols-4 border-t border-zinc-200 bg-white/90 pb-[env(safe-area-inset-bottom)] backdrop-blur-lg lg:hidden dark:border-zinc-800 dark:bg-zinc-950/90">
      {nav.map((n) => (
        <Link
          key={n.to}
          to={n.to}
          activeOptions={{ exact: n.exact }}
          className="flex flex-col items-center gap-0.5 py-2 text-[11px] font-medium text-zinc-500"
          activeProps={{ className: "!text-indigo-600 dark:!text-indigo-400" }}
        >
          <Icon name={n.icon} className="size-5" />
          {n.label}
        </Link>
      ))}
    </nav>
  );
}

function ThemeButton() {
  const isDark = useStore(dark);
  return <IconButton icon={isDark ? "sun" : "moon"} label={isDark ? "Switch to light" : "Switch to dark"} onClick={toggleTheme} />;
}

function TopBar() {
  return (
    <header className="sticky top-0 z-30 flex h-14 items-center gap-2 border-b border-zinc-200/70 bg-zinc-50/80 px-4 backdrop-blur-lg sm:px-6 dark:border-zinc-800/70 dark:bg-zinc-950/70">
      <Link to="/" className="flex items-center gap-2 lg:hidden" aria-label="Prism home">
        <Logo />
        <span className="font-semibold tracking-tight">Prism</span>
      </Link>
      <Breadcrumbs />
      <div className="flex-1" />
      <Button size="sm" variant="ghost" icon="search" className="text-zinc-500 lg:hidden" aria-label="Search" onClick={openPalette} />
      <span className="hidden sm:inline lg:hidden">
        <ConnectionDot />
      </span>
      <TaskTray />
      <span className="lg:hidden">
        <ThemeButton />
      </span>
      <Link to="/vms/new" className="lg:hidden" aria-label="New machine">
        <span className="inline-flex size-8 items-center justify-center rounded-md bg-indigo-600 text-white dark:bg-indigo-500">
          <Icon name="plus" />
        </span>
      </Link>
    </header>
  );
}

function Breadcrumbs() {
  const path = useRouterState({ select: (s) => s.location.pathname });
  const vms = useVms().data;
  const m = /^\/vms\/([0-9a-f-]{36})/.exec(path);
  const crumbs: { label: string; to?: string }[] = [];
  if (path.startsWith("/vms")) crumbs.push({ label: "Machines", to: "/vms" });
  if (path === "/vms/new") crumbs.push({ label: "New machine" });
  if (m) crumbs.push({ label: vms?.find((v) => v.id === m[1])?.name ?? "…" });
  if (path === "/storage") crumbs.push({ label: "Storage" });
  if (path === "/networks") crumbs.push({ label: "Networks" });
  if (path === "/tasks") crumbs.push({ label: "Tasks" });
  if (path === "/") crumbs.push({ label: "Overview" });
  return (
    <nav aria-label="Breadcrumb" className="hidden items-center gap-1.5 text-sm lg:flex">
      {crumbs.map((c, i) => (
        <span key={i} className="flex items-center gap-1.5">
          {i > 0 && <Icon name="chevronRight" className="size-3.5 text-zinc-400" />}
          {c.to && i < crumbs.length - 1 ? (
            <Link to={c.to} className="text-zinc-500 hover:text-zinc-900 dark:hover:text-zinc-100">{c.label}</Link>
          ) : (
            <span className="font-medium">{c.label}</span>
          )}
        </span>
      ))}
    </nav>
  );
}

function Banners() {
  const stream = useStream();
  const node = useNode();
  if (stream === "down" || stream === "connecting") {
    return stream === "down" ? (
      <div className="flex items-center justify-center gap-2 bg-red-600 px-4 py-1.5 text-center text-[13px] font-medium text-white">
        <Icon name="alert" className="size-4" />
        Lost connection to Prism. Retrying… what you see may be out of date.
      </div>
    ) : null;
  }
  if (node && !node.libvirt_connected) {
    return (
      <div className="flex items-center justify-center gap-2 bg-amber-100 px-4 py-1.5 text-center text-[13px] text-amber-900 dark:bg-amber-500/15 dark:text-amber-200">
        <Icon name="alert" className="size-4" />
        Prism can't reach libvirt on {node.name}. It keeps retrying; check that virtqemud is running.
      </div>
    );
  }
  return null;
}

export function Layout() {
  const qc = useQueryClient();
  useEventStream(qc);
  return (
    <div className="flex min-h-full">
      <a href="#main" className="sr-only z-50 rounded bg-white px-3 py-2 focus:not-sr-only focus:fixed focus:top-2 focus:left-2">Skip to content</a>
      <Sidebar />
      <div className="flex min-w-0 flex-1 flex-col overflow-x-clip">
        <Banners />
        <TopBar />
        <main id="main" className="mx-auto w-full max-w-6xl flex-1 px-4 pt-6 pb-24 sm:px-6 lg:px-8 lg:pb-12">
          <Outlet />
        </main>
      </div>
      <MobileTabs />
      <Shortcuts />
      <ConfirmHost />
      <Toaster />
    </div>
  );
}
