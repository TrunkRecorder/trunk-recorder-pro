import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
// The chosen theme, before anything is drawn.
import "./theme.ts";

const root = createRoot(document.getElementById("root")!);

// The browser version can't do anything without WebUSB; say so up front
// instead of loading the app (which starts the engine worker).
const unsupported = import.meta.env.MODE === "web" ? await import("./web/NoWebUsb.tsx") : null;
// The desktop app's recorder may want a login first (before the interface connects).
const login = import.meta.env.MODE === "web" ? null : await import("./Login.tsx");
const who = login ? await login.needsLogin() : null;
if (unsupported && !unsupported.hasWebUsb()) {
  root.render(<unsupported.NoWebUsb />);
} else if (login && who) {
  root.render(<login.Login who={who} />);
} else {
  const { App } = await import("./App.tsx");
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
