import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Cross-origin isolation (COOP + COEP) unlocks SharedArrayBuffer, which the
// sample rings between workers are built on. Production hosts must send the same
// two headers — public/_headers does it for Cloudflare Pages / Netlify.
const isolation = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
};

export default defineConfig({
  plugins: [react()],
  worker: { format: "es" },
  server: { headers: isolation },
  preview: { headers: isolation },
});
