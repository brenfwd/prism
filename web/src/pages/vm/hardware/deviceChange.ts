import type { QueryClient } from "@tanstack/react-query";
import { humanError, type Vm } from "../../../api";
import { putVm } from "../../../events";
import { toast } from "../../../lib/toast";
import { isOn } from "../VmPage";

/**
 * Runs a device change, stores the returned VM and toasts the outcome:
 * applied live, saved for the next power cycle, or failed. Resolves to
 * `true` when the request succeeded, even if the guest could not apply it live.
 */
export async function deviceChange(qc: QueryClient, what: string, run: () => Promise<Vm>): Promise<boolean> {
  try {
    const view = await run();
    putVm(qc, view);
    const h = view.hotplug;
    if (!isOn(view) || !h) toast({ tone: "success", title: what });
    else if (h.applied) toast({ tone: "success", title: `${what} live`, body: "The running guest sees the change now." });
    else
      toast({
        tone: "warning",
        title: `${what}: saved for the next power cycle`,
        body: h.error ? `The running machine refused it live (${h.error}). It applies after it powers off and starts again.` : "The running machine couldn't take this change live. It applies after it powers off and starts again.",
      });
    return true;
  } catch (e) {
    toast({ tone: "error", title: `${what} failed`, body: humanError(e) });
    return false;
  }
}
