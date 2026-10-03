import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

const version = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8"))?.[1] ?? "dev";

// `npm run dev` serves the UI on :5173 and proxies the app's API to a running
// `trunk-pro serve` (default :8080). `npm run build` → dist/, which the
// trunk-pro binary embeds.
//
// `npm run build:web` → dist-web/: the standalone browser version (engine in
// WebAssembly, run from src/web/pkg, which `npm run wasm` builds).
export default defineConfig(({ mode }) => ({
  // Relative URLs: the browser version runs from any folder (e.g. unzipped
  // from the -browser.zip release onto any web server), and the desktop
  // app's interface from / or /builtin/ (when / shows another interface).
  base: "./",
  plugins: [react()],
  define: { __APP_VERSION__: JSON.stringify(version) },
  worker: { format: "es" },
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", ws: true },
      "/calls": "http://127.0.0.1:8080",
    },
  },
  build: { outDir: mode === "web" ? "dist-web" : "dist", emptyOutDir: true, target: "es2022" },
}));
