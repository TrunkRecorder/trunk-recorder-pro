// `npm test`'s loader: the sources (.ts and .tsx) through esbuild, as vite
// builds them, so a test can render a page (react-dom/server) and the
// modules reading vite's import.meta.env load under Node.

import { readFile } from "node:fs/promises";
import { register } from "node:module";
import { transform } from "esbuild";

if (!import.meta.url.includes("?hooks")) register(`${import.meta.url}?hooks`);

export async function load(url, context, next) {
  const tsx = url.endsWith(".tsx");
  if (!tsx && !url.endsWith(".ts")) return next(url, context);
  const source = await readFile(new URL(url), "utf8");
  const out = await transform(source, {
    loader: tsx ? "tsx" : "ts",
    jsx: "automatic",
    format: "esm",
    target: "esnext",
    sourcefile: url,
    sourcemap: "inline",
    define: { "import.meta.env.MODE": '"test"' },
  });
  return { format: "module", source: out.code, shortCircuit: true };
}
