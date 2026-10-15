import { useEffect, useRef, useState } from "react";
import RFB from "@novnc/novnc";
import { wsUrl } from "../api";
import { Button } from "./ui";

/** Interim VNC console via noVNC; to be replaced by the WASM/WebGL2 decoder. */
export function VncConsole({ vmId }: { vmId: string }) {
  const el = useRef<HTMLDivElement>(null);
  const rfb = useRef<RFB | null>(null);
  const [status, setStatus] = useState("connecting");
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!el.current) return;
    setStatus("connecting");
    const r = new RFB(el.current, wsUrl(`/vms/${vmId}/console`), { wsProtocols: ["binary"], shared: true });
    r.scaleViewport = true;
    r.qualityLevel = 7;
    r.compressionLevel = 2;
    r.addEventListener("connect", () => setStatus("connected"));
    r.addEventListener("disconnect", () => setStatus("disconnected"));
    rfb.current = r;
    return () => {
      r.disconnect();
      rfb.current = null;
    };
  }, [vmId, attempt]);

  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex items-center gap-2 text-xs text-zinc-500">
        <span className={status === "connected" ? "text-emerald-600" : ""}>{status}</span>
        <div className="flex-1" />
        <Button className="px-2 py-1 text-xs" onClick={() => rfb.current?.sendCtrlAltDel()} disabled={status !== "connected"}>
          Ctrl+Alt+Del
        </Button>
        <Button className="px-2 py-1 text-xs" onClick={() => el.current?.requestFullscreen()}>Fullscreen</Button>
        {status === "disconnected" && (
          <Button className="px-2 py-1 text-xs" variant="primary" onClick={() => setAttempt((a) => a + 1)}>Reconnect</Button>
        )}
      </div>
      <div ref={el} className="h-[calc(100vh-15rem)] min-h-[360px] overflow-hidden rounded-md bg-black" />
    </div>
  );
}
