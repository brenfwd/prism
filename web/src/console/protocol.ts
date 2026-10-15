// Messages between the console component (main thread) and its worker.

import type { CursorImage } from "./render";

export type ToWorker =
  | { type: "init"; url: string; canvas?: OffscreenCanvas; quality: number; compression: number }
  | { type: "viewport"; width: number; height: number }
  | { type: "key"; down: boolean; keysym: number; scancode: number }
  | { type: "pointer"; x: number; y: number; buttons: number }
  | { type: "leave" }
  | { type: "type"; text: string }
  | { type: "ctrlAltDel" }
  | { type: "close" };

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type FromWorker =
  | { type: "caps"; offscreenWebgl2: boolean }
  | { type: "status"; status: "connecting" | "connected" | "disconnected"; detail?: string }
  | { type: "renderer"; kind: string }
  | { type: "size"; width: number; height: number }
  /** Server cursor shape (null: none). Drawn locally either way. */
  | { type: "cursor"; cursor: CursorImage | null }
  /** Main-thread rendering only: pixels for changed areas. */
  | { type: "rects"; rects: (Rect & { pixels: Uint8Array<ArrayBuffer> })[] }
  | { type: "bell" }
  /** Per second: frames drawn, kbit/s received, ms decoding and rendering. */
  | { type: "stats"; fps: number; kbps: number; decodeMs: number; renderMs: number };
