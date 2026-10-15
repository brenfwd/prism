import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Link, Outlet, RouterProvider, createRootRoute, createRoute, createRouter,
} from "@tanstack/react-router";
import "./index.css";
import { api } from "./api";
import { useEventStream } from "./events";
import { DashboardPage } from "./pages/Dashboard";
import { VmsPage } from "./pages/Vms";
import { VmDetailPage } from "./pages/VmDetail";
import { StoragePage } from "./pages/Storage";

const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: Infinity, retry: 1, refetchOnWindowFocus: false } },
});

function Layout() {
  const qc = useQueryClient();
  const stream = useEventStream(qc);
  const nodes = useQuery({ queryKey: ["nodes"], queryFn: api.nodes });
  const node = nodes.data?.[0];
  const nav = "rounded-md px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-200/60 dark:text-zinc-300 dark:hover:bg-zinc-800";
  const active = { className: "bg-zinc-200/80 font-medium text-zinc-900 dark:bg-zinc-800 dark:text-white" };
  return (
    <div className="flex min-h-full flex-col">
      <header className="sticky top-0 z-40 border-b border-zinc-200 bg-white/80 backdrop-blur dark:border-zinc-800 dark:bg-zinc-950/80">
        <div className="mx-auto flex max-w-7xl items-center gap-2 px-4 py-2.5">
          <Link to="/" className="mr-4 flex items-center gap-2 font-semibold">
            <img src="/prism.svg" alt="" className="size-5" />
            Prism
          </Link>
          <nav className="flex gap-1">
            <Link to="/" className={nav} activeProps={active} activeOptions={{ exact: true }}>Overview</Link>
            <Link to="/vms" className={nav} activeProps={active}>VMs</Link>
            <Link to="/storage" className={nav} activeProps={active}>Storage</Link>
          </nav>
          <div className="flex-1" />
          <span className="hidden text-xs text-zinc-500 sm:inline">{node?.name}</span>
          <span
            className={`size-2 rounded-full ${stream === "live" && node?.libvirt_connected ? "bg-emerald-500" : stream === "live" ? "bg-amber-500" : "bg-red-500"}`}
            title={stream !== "live" ? `event stream ${stream}` : node?.libvirt_connected ? "live" : "libvirt disconnected"}
          />
        </div>
      </header>
      {node && !node.libvirt_connected && (
        <div className="bg-amber-100 px-4 py-2 text-center text-sm text-amber-900 dark:bg-amber-900/40 dark:text-amber-200">
          Not connected to libvirt. Prism keeps retrying; check that virtqemud is running.
        </div>
      )}
      <main className="mx-auto w-full max-w-7xl flex-1 px-4 py-6">
        <Outlet />
      </main>
    </div>
  );
}

const rootRoute = createRootRoute({ component: Layout });
const vmRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/vms/$id",
  component: function VmRoute() {
    const { id } = vmRoute.useParams();
    return <VmDetailPage id={id} />;
  },
});
const routeTree = rootRoute.addChildren([
  createRoute({ getParentRoute: () => rootRoute, path: "/", component: DashboardPage }),
  createRoute({ getParentRoute: () => rootRoute, path: "/vms", component: VmsPage }),
  vmRoute,
  createRoute({ getParentRoute: () => rootRoute, path: "/storage", component: StoragePage }),
]);

const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
