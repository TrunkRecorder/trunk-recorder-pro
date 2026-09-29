import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `npm run dev` serves the UI on :5173 and proxies the app's API to a running
// `trunk-lite serve` (default :8080). `npm run build` → dist/, which the
// trunk-lite binary embeds.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", ws: true },
      "/calls": "http://127.0.0.1:8080",
    },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
