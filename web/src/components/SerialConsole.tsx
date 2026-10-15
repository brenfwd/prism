import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { wsUrl } from "../api";
import { Button } from "./ui";

export function SerialConsole({ vmId }: { vmId: string }) {
  const el = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState("connecting");
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!el.current) return;
    const term = new Terminal({ fontSize: 13, fontFamily: "ui-monospace, Menlo, Consolas, \"DejaVu Sans Mono\", monospace", cursorBlink: true, convertEol: false, theme: { background: "#09090b" } });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el.current);
    fit.fit();
    const ws = new WebSocket(wsUrl(`/vms/${vmId}/serial`));
    ws.binaryType = "arraybuffer";
    const enc = new TextEncoder();
    setStatus("connecting");
    ws.onopen = () => {
      setStatus("connected");
      term.focus();
    };
    ws.onclose = () => setStatus("disconnected");
    ws.onmessage = (m) => term.write(new Uint8Array(m.data as ArrayBuffer));
    const sub = term.onData((d) => ws.readyState === WebSocket.OPEN && ws.send(enc.encode(d)));
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(el.current);
    return () => {
      ro.disconnect();
      sub.dispose();
      ws.close();
      term.dispose();
    };
  }, [vmId, attempt]);

  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex items-center gap-2 text-xs text-zinc-500">
        <span className={status === "connected" ? "text-emerald-600" : ""}>{status}</span>
        <span>· press Enter if the screen is blank</span>
        <div className="flex-1" />
        {status === "disconnected" && (
          <Button size="xs" variant="primary" onClick={() => setAttempt((a) => a + 1)}>Reconnect</Button>
        )}
      </div>
      <div ref={el} data-console className="h-[calc(100dvh-17rem)] min-h-[320px] overflow-hidden rounded-xl bg-zinc-950 p-3 ring-1 ring-zinc-900/10 dark:ring-white/10" />
    </div>
  );
}
