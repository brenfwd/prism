import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The prism-rfb-wasm decoder. Cargo builds set PRISM_RFB_WASM (see
// build.rs); otherwise `bun run wasm` builds it here.
const rfbWasm =
  process.env.PRISM_RFB_WASM ??
  fileURLToPath(new URL("../target/wasm32-unknown-unknown/release/prism_rfb_wasm.wasm", import.meta.url));

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: [{ find: /^@prism\/rfb\.wasm(\?.*)?$/, replacement: `${rfbWasm}$1` }],
  },
  worker: {
    format: "es",
  },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
    // Cargo builds set this to OUT_DIR (see build.rs).
    outDir: process.env.PRISM_WEB_OUT_DIR ?? "dist",
    emptyOutDir: true,
    // Always emit the decoder as a file (never a data: URL).
    assetsInlineLimit: (file) => (file.endsWith(".wasm") ? false : undefined),
  },
  server: {
    fs: { allow: [".", rfbWasm] },
    proxy: {
      // PRISM_API points the dev proxy at another node, e.g. http://192.168.122.4:8080.
      "/api": { target: process.env.PRISM_API ?? "http://127.0.0.1:8080", ws: true, changeOrigin: false },
    },
  },
});
