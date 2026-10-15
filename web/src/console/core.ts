// Typed wrapper over prism-rfb-wasm's hand-written export ABI
// (see crates/prism-rfb-wasm/src/ffi.rs for the action encoding).

import wasmUrl from "@prism/rfb.wasm?url";
import type { CursorImage } from "./render";

interface Exports {
  memory: WebAssembly.Memory;
  rfb_create(quality: number, compression: number): void;
  rfb_input(len: number): number;
  rfb_feed(len: number): number;
  rfb_image_done(ok: number, len: number): number;
  rfb_key(down: number, keysym: number, scancode: number): number;
  rfb_pointer(x: number, y: number, buttons: number): number;
  rfb_cut_text(len: number): number;
  rfb_set_quality(quality: number, compression: number): number;
  rfb_refresh(): number;
  rfb_actions(): number;
  rfb_data(): number;
  rfb_fb(): number;
  rfb_fb_width(): number;
  rfb_fb_height(): number;
  rfb_error(): number;
  rfb_error_len(): number;
}

export type Action =
  | { kind: "send"; bytes: Uint8Array<ArrayBuffer> }
  | { kind: "connected"; name: string }
  | { kind: "resize"; width: number; height: number }
  | { kind: "dirty"; x: number; y: number; w: number; h: number }
  | { kind: "cursor"; cursor: CursorImage }
  | { kind: "image"; x: number; y: number; w: number; h: number; mime: string; data: Uint8Array<ArrayBuffer> }
  | { kind: "bell" }
  | { kind: "clipboard"; text: string }
  | { kind: "leds"; state: number };

const utf8 = new TextDecoder();

async function instantiate(): Promise<WebAssembly.Instance> {
  const res = await fetch(wasmUrl);
  if (!res.ok) throw new Error(`loading decoder: HTTP ${res.status}`);
  if (WebAssembly.instantiateStreaming && res.headers.get("content-type")?.startsWith("application/wasm")) {
    return (await WebAssembly.instantiateStreaming(res)).instance;
  }
  return (await WebAssembly.instantiate(await res.arrayBuffer())).instance;
}

export class RfbCore {
  private constructor(private x: Exports) {}

  /** Loads the decoder and starts a connection's state machine. */
  static async load(quality: number, compression: number): Promise<RfbCore> {
    const inst = await instantiate();
    const core = new RfbCore(inst.exports as unknown as Exports);
    core.x.rfb_create(quality, compression);
    return core;
  }

  private bytes(): Uint8Array {
    return new Uint8Array(this.x.memory.buffer);
  }

  private input(data: Uint8Array) {
    const ptr = this.x.rfb_input(data.length);
    this.bytes().set(data, ptr);
  }

  private decode(words: number): Action[] {
    if (words < 0) {
      const msg = utf8.decode(this.bytes().slice(this.x.rfb_error(), this.x.rfb_error() + this.x.rfb_error_len()));
      throw new Error(msg);
    }
    if (words === 0) return [];
    const mem = this.x.memory.buffer;
    const w = new Uint32Array(mem, this.x.rfb_actions(), words);
    const dataPtr = this.x.rfb_data();
    const u8 = new Uint8Array(mem);
    const slice = (off: number, len: number) => u8.slice(dataPtr + off, dataPtr + off + len);
    const out: Action[] = [];
    for (let i = 0; i < words; ) {
      switch (w[i]) {
        case 1: out.push({ kind: "send", bytes: slice(w[i + 1], w[i + 2]) }); i += 3; break;
        case 2: out.push({ kind: "connected", name: utf8.decode(slice(w[i + 1], w[i + 2])) }); i += 3; break;
        case 3: out.push({ kind: "resize", width: w[i + 1], height: w[i + 2] }); i += 3; break;
        case 4: out.push({ kind: "dirty", x: w[i + 1], y: w[i + 2], w: w[i + 3], h: w[i + 4] }); i += 5; break;
        case 5:
          out.push({
            kind: "cursor",
            cursor: { hotX: w[i + 1], hotY: w[i + 2], width: w[i + 3], height: w[i + 4], rgba: slice(w[i + 5], w[i + 6]) },
          });
          i += 7;
          break;
        case 6:
          out.push({
            kind: "image", x: w[i + 1], y: w[i + 2], w: w[i + 3], h: w[i + 4],
            mime: w[i + 5] === 1 ? "image/jpeg" : "image/png", data: slice(w[i + 6], w[i + 7]),
          });
          i += 8;
          break;
        case 7: out.push({ kind: "bell" }); i += 1; break;
        case 8: out.push({ kind: "clipboard", text: utf8.decode(slice(w[i + 1], w[i + 2])) }); i += 3; break;
        case 9: out.push({ kind: "leds", state: w[i + 1] }); i += 2; break;
        default: throw new Error(`decoder ABI: unknown action ${w[i]}`);
      }
    }
    return out;
  }

  feed(data: Uint8Array): Action[] {
    this.input(data);
    return this.decode(this.x.rfb_feed(data.length));
  }

  /** Completes an image decode with `rgba` (w*h*4 bytes) or null on failure. */
  imageDone(rgba: Uint8ClampedArray | null): Action[] {
    if (!rgba) return this.decode(this.x.rfb_image_done(0, 0));
    const ptr = this.x.rfb_input(rgba.length);
    this.bytes().set(rgba, ptr);
    return this.decode(this.x.rfb_image_done(1, rgba.length));
  }

  key(down: boolean, keysym: number, scancode: number): Action[] {
    return this.decode(this.x.rfb_key(down ? 1 : 0, keysym, scancode));
  }

  pointer(x: number, y: number, buttons: number): Action[] {
    return this.decode(this.x.rfb_pointer(x, y, buttons));
  }

  /** The framebuffer: all of WebAssembly memory plus the pixels' offset. */
  framebuffer(): { memory: Uint8Array; offset: number; width: number; height: number } {
    return { memory: this.bytes(), offset: this.x.rfb_fb(), width: this.x.rfb_fb_width(), height: this.x.rfb_fb_height() };
  }
}
