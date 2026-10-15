import { useMutation } from "@tanstack/react-query";
import { api, type Vm } from "../api";
import { Button } from "../components/ui";

export function VmActions({ vm, compact = false }: { vm: Vm; compact?: boolean }) {
  const act = useMutation({ mutationFn: (a: string) => api.action(vm.id, a) });
  const s = vm.status.state;
  const btn = (action: string, label: string, variant: "primary" | "secondary" | "danger" = "secondary") => (
    <Button
      key={action}
      variant={variant}
      disabled={act.isPending}
      onClick={() => act.mutate(action)}
      className={compact ? "px-2 py-1 text-xs" : ""}
    >
      {label}
    </Button>
  );
  const buttons =
    s === "stopped" || s === "crashed" || s === "missing"
      ? [btn("start", "Start", "primary")]
      : s === "paused"
        ? [btn("resume", "Resume", "primary"), btn("stop", "Power off", "danger")]
        : s === "running" || s === "shutting_down"
          ? compact
            ? [btn("shutdown", "Shut down"), btn("stop", "Power off", "danger")]
            : [btn("shutdown", "Shut down"), btn("reboot", "Reboot"), btn("reset", "Reset"), btn("pause", "Pause"), btn("stop", "Power off", "danger")]
          : [];
  return (
    <div className="flex flex-wrap items-center gap-2">
      {buttons}
      {act.error && <span className="text-xs text-red-600" title={act.error.message}>{act.error.message}</span>}
    </div>
  );
}
