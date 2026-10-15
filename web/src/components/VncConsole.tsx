import { useEffect, useRef, useState } from "react";
import { wsUrl } from "../api";
import { keysymFor, scancodeFor } from "../console/keymap";
import type { FromWorker, ToWorker } from "../console/protocol";
import { createRenderer, place, type Renderer } from "../console/render";
import { Button } from "./ui";

/** Rendering path: `auto` uses WebGL2 in the worker when it can. */
export type ConsoleRender = "auto" | "main-webgl2" | "main-2d";

const JPEG_QUALITY = 7;
const COMPRESSION = 2;
const WHEEL_STEP = 50;

/** DOM `buttons` (left 1, right 2, middle 4) → RFB mask (left 1, middle 2, right 4). */
function rfbButtons(b: number): number {
  return (b & 1) | (b & 4 ? 2 : 0) | (b & 2 ? 4 : 0);
}

/**
 * VNC console: a worker runs the WebSocket and the Rust/WASM RFB decoder
 * and draws with WebGL2 on an OffscreenCanvas; this component forwards
 * input and handles fallbacks.
 */
/** Developer override for testing fallbacks: localStorage `prism.console.render`. */
function renderOverride(): ConsoleRender | undefined {
  try {
    const v = localStorage.getItem("prism.console.render");
    return v === "main-webgl2" || v === "main-2d" ? v : undefined;
  } catch {
    return undefined;
  }
}

export function VncConsole({ vmId, render = renderOverride() ?? "auto" }: { vmId: string; render?: ConsoleRender }) {
  const box = useRef<HTMLDivElement>(null);
  const worker = useRef<Worker | null>(null);
  const [status, setStatus] = useState("connecting");
  const [detail, setDetail] = useState<string | undefined>();
  const [attempt, setAttempt] = useState(0);
  const [serverCursor, setServerCursor] = useState(false);
  const [renderer, setRenderer] = useState("");
  const [stats, setStats] = useState<Extract<FromWorker, { type: "stats" }> | null>(null);

  useEffect(() => {
    const host = box.current;
    if (!host) return;
    setStatus("connecting");
    setDetail(undefined);
    setServerCursor(false);
    const canvas = document.createElement("canvas");
    canvas.className = "block h-full w-full touch-none";
    host.appendChild(canvas);

    const w = new Worker(new URL("../console/worker.ts", import.meta.url), { type: "module", name: "vnc-console" });
    worker.current = w;
    const send = (m: ToWorker, transfer: Transferable[] = []) => w.postMessage(m, transfer);

    let fbW = 0;
    let fbH = 0;
    let main: Renderer | null = null;
    let raf = 0;
    const view = { w: 0, h: 0 };
    const redraw = () => {
      if (main && !raf) {
        raf = requestAnimationFrame(() => {
          raf = 0;
          main?.draw();
        });
      }
    };

    const resize = () => {
      const r = host.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      view.w = Math.max(1, Math.round(r.width * dpr));
      view.h = Math.max(1, Math.round(r.height * dpr));
      if (main) {
        main.setViewport(view.w, view.h);
        redraw();
      } else {
        send({ type: "viewport", width: view.w, height: view.h });
      }
    };
    const observer = new ResizeObserver(resize);

    w.onmessage = (e: MessageEvent<FromWorker>) => {
      const m = e.data;
      switch (m.type) {
        case "caps": {
          const url = wsUrl(`/vms/${vmId}/console`);
          if (render === "auto" && m.offscreenWebgl2 && "transferControlToOffscreen" in canvas) {
            const off = canvas.transferControlToOffscreen();
            send({ type: "init", url, canvas: off, quality: JPEG_QUALITY, compression: COMPRESSION }, [off]);
          } else {
            main = createRenderer(canvas, render === "main-2d" ? "2d" : "webgl2");
            setRenderer(main ? `main-${main.kind}` : "none");
            send({ type: "init", url, quality: JPEG_QUALITY, compression: COMPRESSION });
          }
          observer.observe(host);
          break;
        }
        case "renderer":
          if (m.kind !== "main") setRenderer(m.kind);
          break;
        case "status":
          setStatus(m.status);
          setDetail(m.detail);
          break;
        case "size":
          fbW = m.width;
          fbH = m.height;
          main?.resize(m.width, m.height);
          redraw();
          break;
        case "cursor":
          setServerCursor(!!m.cursor && m.cursor.width > 0);
          main?.setCursor(m.cursor);
          redraw();
          break;
        case "rects":
          for (const r of m.rects) main?.upload(r.x, r.y, r.w, r.h, r.pixels, 0, r.w);
          redraw();
          break;
        case "stats":
          setStats(m);
          break;
        case "bell":
          break;
      }
    };

    // Pointer: map through the same placement the renderer uses.
    let buttons = 0;
    const toFb = (e: MouseEvent) => {
      const r = canvas.getBoundingClientRect();
      const p = place(fbW, fbH, view.w, view.h);
      if (!p.w || !r.width) return null;
      const dpr = view.w / r.width;
      const x = Math.floor(((e.clientX - r.left) * dpr - p.x) / p.scale);
      const y = Math.floor(((e.clientY - r.top) * dpr - p.y) / p.scale);
      return { x: Math.min(Math.max(x, 0), fbW - 1), y: Math.min(Math.max(y, 0), fbH - 1) };
    };
    const pointer = (e: PointerEvent) => {
      const pos = toFb(e);
      if (!pos) return;
      buttons = rfbButtons(e.buttons);
      send({ type: "pointer", ...pos, buttons });
      main?.moveCursor(pos);
      redraw();
    };
    const down = (e: PointerEvent) => {
      host.focus();
      canvas.setPointerCapture(e.pointerId);
      e.preventDefault();
      pointer(e);
    };
    const leave = () => {
      send({ type: "leave" });
      main?.moveCursor(null);
      redraw();
    };
    let wheelX = 0;
    let wheelY = 0;
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const pos = toFb(e);
      if (!pos) return;
      const scale = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? 20 : e.deltaMode === WheelEvent.DOM_DELTA_PAGE ? 400 : 1;
      wheelX += e.deltaX * scale;
      wheelY += e.deltaY * scale;
      const click = (bit: number) => {
        send({ type: "pointer", ...pos, buttons: buttons | bit });
        send({ type: "pointer", ...pos, buttons });
      };
      for (; wheelY <= -WHEEL_STEP; wheelY += WHEEL_STEP) click(8);
      for (; wheelY >= WHEEL_STEP; wheelY -= WHEEL_STEP) click(16);
      for (; wheelX <= -WHEEL_STEP; wheelX += WHEEL_STEP) click(32);
      for (; wheelX >= WHEEL_STEP; wheelX -= WHEEL_STEP) click(64);
    };
    const menu = (e: Event) => e.preventDefault();

    // Keyboard: scancodes from `code`, so the guest's layout applies.
    const held = new Map<string, number>();
    const key = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      e.preventDefault();
      const down = e.type === "keydown";
      const scancode = scancodeFor(e.code);
      const keysym = down ? keysymFor(e) : (held.get(e.code) ?? keysymFor(e));
      if (down) held.set(e.code, keysym);
      else held.delete(e.code);
      send({ type: "key", down, keysym, scancode });
    };
    const blur = () => {
      for (const [code, keysym] of held) send({ type: "key", down: false, keysym, scancode: scancodeFor(code) });
      held.clear();
    };

    canvas.addEventListener("pointermove", pointer);
    canvas.addEventListener("pointerdown", down);
    canvas.addEventListener("pointerup", pointer);
    canvas.addEventListener("pointerleave", leave);
    canvas.addEventListener("wheel", wheel, { passive: false });
    canvas.addEventListener("contextmenu", menu);
    host.addEventListener("keydown", key);
    host.addEventListener("keyup", key);
    host.addEventListener("blur", blur);

    return () => {
      observer.disconnect();
      cancelAnimationFrame(raf);
      host.removeEventListener("keydown", key);
      host.removeEventListener("keyup", key);
      host.removeEventListener("blur", blur);
      send({ type: "close" });
      w.terminate();
      worker.current = null;
      canvas.remove();
    };
  }, [vmId, attempt, render]);

  const fullscreen = async () => {
    await box.current?.requestFullscreen();
    box.current?.focus();
    const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void> } }).keyboard;
    await kb?.lock?.().catch(() => {});
  };

  const paste = async () => {
    try {
      const text = await navigator.clipboard.readText();
      worker.current?.postMessage({ type: "type", text } satisfies ToWorker);
      box.current?.focus();
    } catch {
      setDetail("clipboard not readable (permission denied?)");
    }
  };

  const connected = status === "connected";
  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <span className={`inline-flex items-center gap-1.5 font-medium ${connected ? "text-emerald-600 dark:text-emerald-400" : ""}`}>
          <span className={`size-1.5 rounded-full ${connected ? "bg-emerald-500" : status === "connecting" ? "animate-pulse bg-amber-500" : "bg-zinc-400"}`} />
          {status}
        </span>
        {detail && <span className="truncate" title={detail}>{detail}</span>}
        {connected && stats && (
          <span className="hidden tabular-nums text-zinc-400 md:inline" title={`renderer: ${renderer}`}>
            {stats.fps} fps · {Math.round(stats.kbps)} kbit/s
          </span>
        )}
        <div className="flex-1" />
        <Button
          size="xs"
          icon="keyboard"
          onClick={() => worker.current?.postMessage({ type: "ctrlAltDel" } satisfies ToWorker)}
          disabled={!connected}
        >
          Ctrl+Alt+Del
        </Button>
        <Button size="xs" icon="clipboard" onClick={paste} disabled={!connected} title="Type the clipboard text as keystrokes (US layout)">
          Paste
        </Button>
        <Button size="xs" icon="maximize" onClick={fullscreen}>Fullscreen</Button>
        {status === "disconnected" && (
          <Button size="xs" variant="primary" icon="rotate" onClick={() => setAttempt((a) => a + 1)}>Reconnect</Button>
        )}
      </div>
      <div
        ref={box}
        tabIndex={0}
        data-console
        aria-label="Machine screen. Click to type into the guest."
        data-status={status}
        data-renderer={renderer}
        data-fps={stats?.fps}
        data-decode-ms={stats?.decodeMs.toFixed(1)}
        data-render-ms={stats?.renderMs.toFixed(1)}
        className={`h-[calc(100dvh-17rem)] min-h-[360px] overflow-hidden rounded-xl bg-black ring-1 ring-zinc-900/10 dark:ring-white/10 outline-none focus-visible:ring-2 focus-visible:ring-indigo-500 ${serverCursor ? "cursor-none" : ""}`}
      />
    </div>
  );
}
