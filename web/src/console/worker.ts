// Console worker: owns the WebSocket and the prism-rfb-wasm decoder, and
// renders to an OffscreenCanvas with WebGL2 (or Canvas2D). Without an
// OffscreenCanvas it sends changed pixels to the main thread instead.

import { RfbCore, type Action } from "./core";
import { CTRL_ALT_DEL, SHIFT, strokeFor } from "./keymap";
import type { FromWorker, Rect, ToWorker } from "./protocol";
import { createRenderer, type Renderer } from "./render";

const scope = self as unknown as {
  postMessage(m: FromWorker, transfer?: Transferable[]): void;
  addEventListener(type: "message", f: (e: MessageEvent<ToWorker>) => void): void;
  close(): void;
};
const post = (m: FromWorker, transfer: Transferable[] = []) => scope.postMessage(m, transfer);
const nextFrame: (f: () => void) => void =
  typeof requestAnimationFrame === "function" ? (f) => requestAnimationFrame(f) : (f) => setTimeout(f, 16);

const MAX_PASTE = 10_000;
const TYPE_INTERVAL_MS = 8;

let core: RfbCore | null = null;
let ws: WebSocket | null = null;
let renderer: Renderer | null = null;
let closed = false;
let lastError: string | undefined;

let pending: Rect[] = [];
let full = false;
let frameScheduled = false;

let decodeCtx: OffscreenCanvasRenderingContext2D | null = null;
const typing: [boolean, number, number][] = [];
let typingTimer: ReturnType<typeof setTimeout> | null = null;

const stats = { frames: 0, bytes: 0, decodeMs: 0, renderMs: 0 };

function offscreenWebgl2(): boolean {
  try {
    return typeof OffscreenCanvas !== "undefined" && new OffscreenCanvas(1, 1).getContext("webgl2") !== null;
  } catch {
    return false;
  }
}

function fail(e: unknown) {
  lastError = e instanceof Error ? e.message : String(e);
  ws?.close();
}

/** Runs a decoder call and applies its actions; protocol errors close. */
function run(f: (c: RfbCore) => Action[]) {
  if (!core || closed) return;
  let actions: Action[];
  try {
    actions = f(core);
  } catch (e) {
    fail(e);
    return;
  }
  apply(actions);
}

function apply(actions: Action[]) {
  for (const a of actions) {
    switch (a.kind) {
      case "send":
        if (ws?.readyState === WebSocket.OPEN) ws.send(a.bytes);
        break;
      case "connected":
        post({ type: "status", status: "connected", detail: a.name });
        break;
      case "resize":
        renderer?.resize(a.width, a.height);
        full = true;
        pending = [];
        post({ type: "size", width: a.width, height: a.height });
        break;
      case "dirty":
        pending.push(a);
        break;
      case "cursor":
        renderer?.setCursor(a.cursor);
        post({ type: "cursor", cursor: a.cursor });
        break;
      case "image":
        void decodeImage(a);
        break;
      case "bell":
        post({ type: "bell" });
        break;
      case "clipboard":
      case "leds":
        // Guest clipboard is only ever written on a user gesture; not yet.
        break;
    }
  }
  schedule();
}

/** Decodes a Tight JPEG/PNG rectangle; the decoder waits meanwhile. */
async function decodeImage(a: Extract<Action, { kind: "image" }>) {
  let rgba: Uint8ClampedArray | null = null;
  try {
    const bmp = await createImageBitmap(new Blob([a.data], { type: a.mime }), {
      colorSpaceConversion: "none",
      premultiplyAlpha: "none",
    });
    if (!decodeCtx) decodeCtx = new OffscreenCanvas(a.w, a.h).getContext("2d", { willReadFrequently: true });
    const ctx = decodeCtx!;
    if (ctx.canvas.width < a.w || ctx.canvas.height < a.h) {
      ctx.canvas.width = Math.max(ctx.canvas.width, a.w);
      ctx.canvas.height = Math.max(ctx.canvas.height, a.h);
    }
    ctx.drawImage(bmp, 0, 0, a.w, a.h);
    bmp.close();
    rgba = ctx.getImageData(0, 0, a.w, a.h).data;
  } catch {
    rgba = null;
  }
  run((c) => c.imageDone(rgba));
}

function schedule() {
  if (frameScheduled) return;
  frameScheduled = true;
  nextFrame(frame);
}

function frame() {
  frameScheduled = false;
  if (!core) return;
  const t0 = performance.now();
  const fb = core.framebuffer();
  if (fb.width && fb.height) {
    const area = pending.reduce((s, r) => s + r.w * r.h, 0);
    if (full || pending.length > 256 || area >= fb.width * fb.height) {
      pending = [{ x: 0, y: 0, w: fb.width, h: fb.height }];
    }
    if (renderer) {
      for (const r of pending) {
        renderer.upload(r.x, r.y, r.w, r.h, fb.memory, fb.offset + (r.y * fb.width + r.x) * 4, fb.width);
      }
      renderer.draw();
    } else if (pending.length) {
      // Main-thread rendering: hand over copies of the changed pixels.
      const rects = pending.map((r) => {
        const pixels = new Uint8Array(r.w * r.h * 4);
        for (let row = 0; row < r.h; row++) {
          const s = fb.offset + ((r.y + row) * fb.width + r.x) * 4;
          pixels.set(fb.memory.subarray(s, s + r.w * 4), row * r.w * 4);
        }
        return { ...r, pixels };
      });
      post({ type: "rects", rects }, rects.map((r) => r.pixels.buffer));
    }
    if (pending.length) stats.frames++;
  }
  pending = [];
  full = false;
  stats.renderMs += performance.now() - t0;
}

function pumpTyping() {
  if (typingTimer !== null) return;
  const step = () => {
    typingTimer = null;
    const k = typing.shift();
    if (!k) return;
    run((c) => c.key(k[0], k[1], k[2]));
    typingTimer = setTimeout(step, TYPE_INTERVAL_MS);
  };
  step();
}

function typeText(text: string) {
  for (const ch of [...text].slice(0, MAX_PASTE)) {
    const s = strokeFor(ch);
    if (!s) continue;
    if (s.shift) typing.push([true, SHIFT.keysym, SHIFT.scancode]);
    typing.push([true, s.keysym, s.scancode], [false, s.keysym, s.scancode]);
    if (s.shift) typing.push([false, SHIFT.keysym, SHIFT.scancode]);
  }
  pumpTyping();
}

async function init(m: Extract<ToWorker, { type: "init" }>) {
  if (m.canvas) {
    try {
      renderer = createRenderer(m.canvas, "webgl2");
    } catch (e) {
      post({ type: "status", status: "disconnected", detail: `renderer: ${e instanceof Error ? e.message : e}` });
      return;
    }
  }
  post({ type: "renderer", kind: renderer ? `offscreen-${renderer.kind}` : "main" });
  post({ type: "status", status: "connecting" });
  try {
    core = await RfbCore.load(m.quality, m.compression);
  } catch (e) {
    post({ type: "status", status: "disconnected", detail: e instanceof Error ? e.message : String(e) });
    return;
  }
  if (closed) return;
  const sock = new WebSocket(m.url, ["binary"]);
  sock.binaryType = "arraybuffer";
  ws = sock;
  sock.onmessage = (e: MessageEvent<ArrayBuffer>) => {
    const data = new Uint8Array(e.data);
    stats.bytes += data.length;
    const t0 = performance.now();
    run((c) => c.feed(data));
    stats.decodeMs += performance.now() - t0;
  };
  sock.onclose = (e) => {
    ws = null;
    if (!closed) post({ type: "status", status: "disconnected", detail: lastError ?? (e.reason || `closed (${e.code})`) });
  };
  setInterval(() => {
    post({ type: "stats", fps: stats.frames, kbps: (stats.bytes * 8) / 1000, decodeMs: stats.decodeMs, renderMs: stats.renderMs });
    stats.frames = stats.bytes = stats.decodeMs = stats.renderMs = 0;
  }, 1000);
}

scope.addEventListener("message", (e) => {
  const m = e.data;
  switch (m.type) {
    case "init":
      void init(m);
      break;
    case "viewport":
      renderer?.setViewport(m.width, m.height);
      schedule();
      break;
    case "key":
      run((c) => c.key(m.down, m.keysym, m.scancode));
      break;
    case "pointer":
      renderer?.moveCursor({ x: m.x, y: m.y });
      run((c) => c.pointer(m.x, m.y, m.buttons));
      break;
    case "leave":
      renderer?.moveCursor(null);
      schedule();
      break;
    case "type":
      typeText(m.text);
      break;
    case "ctrlAltDel":
      for (const k of CTRL_ALT_DEL) typing.push([true, k.keysym, k.scancode]);
      for (const k of [...CTRL_ALT_DEL].reverse()) typing.push([false, k.keysym, k.scancode]);
      pumpTyping();
      break;
    case "close":
      closed = true;
      ws?.close();
      scope.close();
      break;
  }
});

post({ type: "caps", offscreenWebgl2: offscreenWebgl2() });
