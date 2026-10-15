import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: { target: "es2022", sourcemap: false, chunkSizeWarningLimit: 1500 },
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", ws: true, changeOrigin: false },
    },
  },
});
