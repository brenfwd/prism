// Framebuffer renderers shared by the console worker and the main-thread
// fallback: WebGL2 (one texture, dirty-rect uploads, cursor as its own quad,
// scaling in the vertex shader) and Canvas2D.

export type AnyCanvas = HTMLCanvasElement | OffscreenCanvas;

export interface CursorImage {
  width: number;
  height: number;
  hotX: number;
  hotY: number;
  rgba: Uint8Array<ArrayBuffer>;
}

/** Where the framebuffer lands on the canvas, in device pixels. */
export interface Placement {
  x: number;
  y: number;
  w: number;
  h: number;
  scale: number;
  /** Integer scale: sample with nearest-neighbour for crisp pixels. */
  nearest: boolean;
}

/**
 * Fits the framebuffer into the canvas preserving aspect ratio. Upscaling
 * uses whole multiples only (crisp text); downscaling is smooth.
 */
export function place(fbW: number, fbH: number, cw: number, ch: number): Placement {
  if (!fbW || !fbH || !cw || !ch) return { x: 0, y: 0, w: 0, h: 0, scale: 1, nearest: true };
  let scale = Math.min(cw / fbW, ch / fbH);
  const nearest = scale >= 1;
  if (nearest) scale = Math.floor(scale);
  const w = Math.round(fbW * scale);
  const h = Math.round(fbH * scale);
  return { x: Math.floor((cw - w) / 2), y: Math.floor((ch - h) / 2), w, h, scale, nearest };
}

export interface Renderer {
  readonly kind: "webgl2" | "2d";
  /** Canvas size in device pixels. */
  setViewport(width: number, height: number): void;
  /** Reallocates for a new framebuffer size. */
  resize(width: number, height: number): void;
  /**
   * Uploads a `w` x `h` RGBA area at (`x`, `y`) from `src`, starting at byte
   * `offset`, with `stride` pixels per source row.
   */
  upload(x: number, y: number, w: number, h: number, src: Uint8Array, offset: number, stride: number): void;
  setCursor(cursor: CursorImage | null): void;
  /** Pointer position in framebuffer coordinates, or null when outside. */
  moveCursor(pos: { x: number; y: number } | null): void;
  draw(): void;
}

const VERTEX = `#version 300 es
in vec2 a_pos;
uniform vec4 u_dst;    // x, y, w, h in device pixels, origin top-left
uniform vec2 u_canvas; // canvas size in device pixels
out vec2 v_uv;
void main() {
  vec2 clip = (u_dst.xy + a_pos * u_dst.zw) / u_canvas * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_uv = a_pos;
}`;

const FRAGMENT = `#version 300 es
precision highp float;
uniform sampler2D u_tex;
in vec2 v_uv;
out vec4 color;
void main() {
  color = texture(u_tex, v_uv);
}`;

export function createRenderer(canvas: AnyCanvas, prefer: "webgl2" | "2d" = "webgl2"): Renderer | null {
  if (prefer === "webgl2") {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: false }) as
      | WebGL2RenderingContext
      | null;
    if (gl) return new GlRenderer(canvas, gl);
  }
  const ctx = canvas.getContext("2d", { alpha: false }) as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null;
  return ctx ? new Canvas2dRenderer(canvas, ctx) : null;
}

class GlRenderer implements Renderer {
  readonly kind = "webgl2";
  private fbW = 0;
  private fbH = 0;
  private fbTex: WebGLTexture;
  private curTex: WebGLTexture;
  private cursor: CursorImage | null = null;
  private pos: { x: number; y: number } | null = null;
  private uDst: WebGLUniformLocation | null;
  private uCanvas: WebGLUniformLocation | null;

  constructor(private canvas: AnyCanvas, private gl: WebGL2RenderingContext) {
    const prog = gl.createProgram();
    for (const [type, src] of [[gl.VERTEX_SHADER, VERTEX], [gl.FRAGMENT_SHADER, FRAGMENT]] as const) {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`shader: ${gl.getShaderInfoLog(s)}`);
      gl.attachShader(prog, s);
    }
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(`program: ${gl.getProgramInfoLog(prog)}`);
    gl.useProgram(prog);
    this.uDst = gl.getUniformLocation(prog, "u_dst");
    this.uCanvas = gl.getUniformLocation(prog, "u_canvas");
    gl.uniform1i(gl.getUniformLocation(prog, "u_tex"), 0);

    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "a_pos");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

    this.fbTex = this.texture();
    this.curTex = this.texture();
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    gl.clearColor(0, 0, 0, 1);
  }

  private texture(): WebGLTexture {
    const gl = this.gl;
    const t = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return t;
  }

  setViewport(width: number, height: number) {
    this.canvas.width = width;
    this.canvas.height = height;
  }

  resize(width: number, height: number) {
    const gl = this.gl;
    this.fbW = width;
    this.fbH = height;
    gl.bindTexture(gl.TEXTURE_2D, this.fbTex);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
  }

  upload(x: number, y: number, w: number, h: number, src: Uint8Array, offset: number, stride: number) {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.fbTex);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, stride);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, w, h, gl.RGBA, gl.UNSIGNED_BYTE, src, offset);
  }

  setCursor(cursor: CursorImage | null) {
    this.cursor = cursor && cursor.width && cursor.height ? cursor : null;
    if (!this.cursor) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.curTex);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, cursor!.width, cursor!.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, cursor!.rgba);
  }

  moveCursor(pos: { x: number; y: number } | null) {
    this.pos = pos;
  }

  private quad(tex: WebGLTexture, x: number, y: number, w: number, h: number, nearest: boolean) {
    const gl = this.gl;
    const filter = nearest ? gl.NEAREST : gl.LINEAR;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.uniform4f(this.uDst, x, y, w, h);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  draw() {
    const gl = this.gl;
    const { width: cw, height: ch } = this.canvas;
    gl.viewport(0, 0, cw, ch);
    gl.uniform2f(this.uCanvas, cw, ch);
    gl.clear(gl.COLOR_BUFFER_BIT);
    const p = place(this.fbW, this.fbH, cw, ch);
    if (!p.w) return;
    gl.disable(gl.BLEND);
    this.quad(this.fbTex, p.x, p.y, p.w, p.h, p.nearest);
    const c = this.cursor;
    if (c && this.pos) {
      gl.enable(gl.BLEND);
      const s = p.scale;
      this.quad(this.curTex, p.x + (this.pos.x - c.hotX) * s, p.y + (this.pos.y - c.hotY) * s, c.width * s, c.height * s, p.nearest);
    }
  }
}

class Canvas2dRenderer implements Renderer {
  readonly kind = "2d";
  private backing: AnyCanvas | null = null;
  private backingCtx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null = null;
  private image: ImageData | null = null;
  private cursorCanvas: AnyCanvas | null = null;
  private cursor: CursorImage | null = null;
  private pos: { x: number; y: number } | null = null;

  constructor(private canvas: AnyCanvas, private ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D) {}

  private static make(w: number, h: number): AnyCanvas {
    if (typeof OffscreenCanvas !== "undefined") return new OffscreenCanvas(w, h);
    const c = document.createElement("canvas");
    c.width = w;
    c.height = h;
    return c;
  }

  setViewport(width: number, height: number) {
    this.canvas.width = width;
    this.canvas.height = height;
  }

  resize(width: number, height: number) {
    this.backing = Canvas2dRenderer.make(width, height);
    this.backingCtx = this.backing.getContext("2d") as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
    this.image = new ImageData(width, height);
  }

  upload(x: number, y: number, w: number, h: number, src: Uint8Array, offset: number, stride: number) {
    const img = this.image;
    if (!img || !this.backingCtx) return;
    const rowBytes = w * 4;
    for (let row = 0; row < h; row++) {
      const s = offset + row * stride * 4;
      img.data.set(src.subarray(s, s + rowBytes), ((y + row) * img.width + x) * 4);
    }
    this.backingCtx.putImageData(img, 0, 0, x, y, w, h);
  }

  setCursor(cursor: CursorImage | null) {
    this.cursor = cursor && cursor.width && cursor.height ? cursor : null;
    if (!this.cursor) return;
    const c = this.cursor;
    this.cursorCanvas = Canvas2dRenderer.make(c.width, c.height);
    const ctx = this.cursorCanvas.getContext("2d") as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(c.rgba.buffer, c.rgba.byteOffset, c.rgba.byteLength), c.width, c.height), 0, 0);
  }

  moveCursor(pos: { x: number; y: number } | null) {
    this.pos = pos;
  }

  draw() {
    const { ctx, canvas } = this;
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    if (!this.backing || !this.image) return;
    const p = place(this.image.width, this.image.height, canvas.width, canvas.height);
    if (!p.w) return;
    ctx.imageSmoothingEnabled = !p.nearest;
    ctx.drawImage(this.backing, p.x, p.y, p.w, p.h);
    const c = this.cursor;
    if (c && this.pos && this.cursorCanvas) {
      const s = p.scale;
      ctx.drawImage(this.cursorCanvas, p.x + (this.pos.x - c.hotX) * s, p.y + (this.pos.y - c.hotY) * s, c.width * s, c.height * s);
    }
  }
}
