import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const root = createRoot(document.getElementById("root")!);

// The config's rules are the recorder's own, compiled to WebAssembly (rules.ts):
// a browser that can't run it (no WebAssembly SIMD) is told, not left blank.
let rulesFailed: unknown = null;
try {
  await (await import("./rules.ts")).loadRules();
} catch (e) {
  rulesFailed = e;
}

// The browser version can't do anything without WebUSB; say so up front
// instead of loading the app (which starts the engine worker).
const unsupported = import.meta.env.MODE === "web" ? await import("./web/NoWebUsb.tsx") : null;
if (rulesFailed !== null) {
  root.render(
    <main style={{ padding: "2rem", fontFamily: "system-ui, sans-serif", maxWidth: "40rem" }}>
      <h1>This browser can't run the dashboard</h1>
      <p>It needs WebAssembly with SIMD (Safari 16.4, Chrome 91, Firefox 89 or later). Try another browser, or reload if the page didn't load fully.</p>
      <p style={{ opacity: 0.6, fontSize: "0.9em" }}>{String(rulesFailed)}</p>
    </main>,
  );
} else if (unsupported && !unsupported.hasWebUsb()) {
  root.render(<unsupported.NoWebUsb />);
} else {
  const { App } = await import("./App.tsx");
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
