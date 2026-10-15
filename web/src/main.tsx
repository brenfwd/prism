import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createRootRoute, createRoute, createRouter } from "@tanstack/react-router";
import "./index.css";
import "./lib/theme";
import { Layout } from "./components/Layout";
import { OverviewPage } from "./pages/Overview";
import { MachinesPage } from "./pages/Machines";
import { NewVmPage, type NewVmSearch } from "./pages/NewVm";
import { VmPage } from "./pages/vm/VmPage";
import { StoragePage } from "./pages/Storage";
import { TasksPage } from "./pages/Tasks";
import { VM_TABS } from "./components/Shortcuts";

const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: Infinity, retry: 1, refetchOnWindowFocus: false } },
});

const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

const rootRoute = createRootRoute({ component: Layout });

export type VmTab = (typeof VM_TABS)[number];
const vmRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/vms/$id",
  validateSearch: (s: Record<string, unknown>): { tab?: VmTab; take?: string } => ({
    tab: VM_TABS.includes(s.tab as VmTab) ? (s.tab as VmTab) : undefined,
    take: str(s.take),
  }),
  component: function VmRoute() {
    const { id } = vmRoute.useParams();
    const { tab, take } = vmRoute.useSearch();
    return <VmPage id={id} tab={tab ?? "overview"} take={!!take} />;
  },
});

const newVmRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/vms/new",
  validateSearch: (s: Record<string, unknown>): NewVmSearch => ({
    source: s.source === "image" || s.source === "iso" || s.source === "blank" ? s.source : undefined,
    pool: str(s.pool),
    volume: str(s.volume),
  }),
  component: function NewVmRoute() {
    return <NewVmPage search={newVmRoute.useSearch()} />;
  },
});

const storageRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/storage",
  validateSearch: (s: Record<string, unknown>): { pool?: string } => ({ pool: str(s.pool) }),
  component: function StorageRoute() {
    return <StoragePage pool={storageRoute.useSearch().pool} />;
  },
});

const routeTree = rootRoute.addChildren([
  createRoute({ getParentRoute: () => rootRoute, path: "/", component: OverviewPage }),
  createRoute({ getParentRoute: () => rootRoute, path: "/vms", component: MachinesPage }),
  newVmRoute,
  vmRoute,
  storageRoute,
  createRoute({ getParentRoute: () => rootRoute, path: "/tasks", component: TasksPage }),
]);

const router = createRouter({ routeTree, defaultPreload: "intent", scrollRestoration: true });

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
