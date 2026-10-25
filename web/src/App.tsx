// The dashboard's frame: the sections on the left (each with a light for
// how it's doing), the recorder's controls on top, the section below.

import { useEffect, useRef, type ReactNode } from "react";
import { startProblem } from "./config.ts";
import { ARRIVED_TO_IMPORT, DirectoryImport } from "./directoryImport.tsx";
import { can, closeGuide, dismissError, openGuide, quitApp, setNotice, setView, start, stop, useApp, useSelect, web, type View } from "./controller.ts";
import { guideWanted, SetupGuide } from "./Onboarding.tsx";
import { InterfacesPage } from "./Interfaces.tsx";
import { PluginPage, PluginsPage, usePluginPages } from "./Plugins.tsx";
import { Setup } from "./Setup.tsx";
import { Light } from "./charts.tsx";
import { ListenPage } from "./dash/Listen.tsx";
import { pageHealth, type Health } from "./dash/data.ts";
import { Overview } from "./dash/Overview.tsx";
import { RfPage } from "./dash/Rf.tsx";
import { DecodePage } from "./dash/Decode.tsx";
import { RadioPage } from "./dash/Radio.tsx";
import { PluginHealthPage } from "./dash/PluginHealth.tsx";
import { PlatformPage } from "./dash/Platform.tsx";
import { useWakeLock } from "./web/wakeLock.ts";

const ICON: Record<View, ReactNode> = {
  overview: <path d="M3 3h6v8H3zM11 3h6v5h-6zM11 10h6v7h-6zM3 13h6v4H3z" />,
  rf: <path d="M2 14c2-8 4-8 6 0s4 8 6 0 3-6 4-2M2 17h16" fill="none" strokeWidth="1.6" />,
  decode: <path d="M3 5h14M3 10h9M3 15h12M14 9l3 1.5-3 1.5" fill="none" strokeWidth="1.6" />,
  radio: <path d="M10 10m-2 0a2 2 0 1 0 4 0a2 2 0 1 0-4 0M5 5a7 7 0 0 0 0 10M15 5a7 7 0 0 1 0 10M2.5 2.5a10.5 10.5 0 0 0 0 15M17.5 2.5a10.5 10.5 0 0 1 0 15" fill="none" strokeWidth="1.6" />,
  listen: <path d="M3 11a7 7 0 0 1 14 0v4a2 2 0 0 1-2 2h-1v-6h3M3 11v4a2 2 0 0 0 2 2h1v-6H3" fill="none" strokeWidth="1.6" />,
  plugin: <path d="M4 4h5v3a1.5 1.5 0 0 0 3 0V4h4v5h-3a1.5 1.5 0 0 0 0 3h3v4H4z" fill="none" strokeWidth="1.6" />,
  plugins: <path d="M7 2v4M13 2v4M5 6h10v4a5 5 0 0 1-10 0zM10 15v3" fill="none" strokeWidth="1.6" />,
  interfaces: <path d="M4 3.5h12a1.5 1.5 0 0 1 1.5 1.5v10a1.5 1.5 0 0 1-1.5 1.5H4A1.5 1.5 0 0 1 2.5 15V5A1.5 1.5 0 0 1 4 3.5zM2.5 7.5h15" fill="none" strokeWidth="1.6" />,
  platform: <path d="M3 4h14v9H3zM7 17h6M10 13v4" fill="none" strokeWidth="1.6" />,
  setup: <path d="M10 7a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM10 1.5v3M10 15.5v3M1.5 10h3M15.5 10h3M4 4l2 2M14 14l2 2M4 16l2-2M14 6l2-2" fill="none" strokeWidth="1.6" />,
};

const LABEL: Record<View, string> = {
  overview: "Overview",
  rf: "RF",
  decode: "Decode",
  radio: "Radio system",
  listen: "Listen",
  plugin: "Plugin",
  plugins: "Plugins",
  interfaces: "Interfaces",
  platform: "Platform",
  setup: "Setup",
};

function Nav({ view, health }: { view: View; health: Record<string, Health> }) {
  const pages: View[] = ["overview", "listen", "rf", "decode", "radio", ...((["platform", "plugins", "interfaces"] as const).filter((v) => can(v === "platform" ? "host" : v)) as View[]), "setup"];
  // Plugins with a page of their own go under Plugins (in a row: between two thin rules).
  const plugins = usePluginPages();
  const own = can("plugins") ? plugins : [];
  const at = useSelect((x) => (x.view === "plugin" ? (x.path[0] ?? "") : ""));
  const item = (key: string, on: boolean, icon: ReactNode, label: string, go: () => void, h?: Health, extra = "") => (
    <button key={key} className={`${on ? "on" : ""}${extra}`} aria-current={on ? "page" : undefined} onClick={go} title={h && h.level !== "ok" && h.level !== "idle" ? h.why : label}>
      <svg viewBox="0 0 20 20" className="nav-icon" stroke="currentColor" fill="currentColor" aria-hidden="true">
        {icon}
      </svg>
      <span className="nav-label">{label}</span>
      {h && h.level !== "idle" && <Light level={h.level} title={h.why} />}
    </button>
  );
  return (
    <nav className="rail-nav" aria-label="Sections">
      {pages.flatMap((p) => [
        item(p, view === p, ICON[p], LABEL[p], () => setView(p), health[p], p === "listen" ? " nav-listen" : ""),
        ...(p === "plugins" && own.length
          ? [
              <span key="plugins-from" className="nav-rule" aria-hidden="true" />,
              ...own.map((o) => item(`plugin-${o.id}`, at === o.id, ICON.plugin, o.title, () => setView("plugin", o.id), undefined, " nav-sub")),
              <span key="plugins-to" className="nav-rule" aria-hidden="true" />,
            ]
          : []),
      ])}
    </nav>
  );
}

function Page({ view }: { view: View }) {
  const s = useApp();
  switch (view) {
    case "overview":
      return <Overview />;
    case "rf":
      return <RfPage />;
    case "decode":
      return <DecodePage />;
    case "radio":
      return <RadioPage />;
    case "listen":
      return <ListenPage />;
    case "plugin":
      return can("plugins") ? <PluginPage id={s.path[0] ?? ""} /> : null;
    case "plugins":
      return can("plugins") ? <PluginHealthPage manage={<PluginsPage />} /> : null;
    case "interfaces":
      return can("interfaces") ? <InterfacesPage /> : null;
    case "platform":
      return can("host") ? <PlatformPage /> : null;
    case "setup":
      return <Setup />;
  }
}

export function App() {
  const s = useSelect((x) => ({
    connected: x.connected,
    quit: x.quit,
    restartNeeded: x.restartNeeded,
    phase: x.phase,
    config: x.config,
    version: x.version,
    error: x.error,
    notice: x.notice,
    ended: x.ended,
    guide: x.guide,
    view: x.view,
    path: x.path,
  }));
  const own = usePluginPages();
  // The lights: recomputed with what they depend on, a few times a second at most.
  const health = useSelect((x) => pageHealth(x), (a, b) => Object.keys(a).every((k) => a[k].level === b[k].level && a[k].why === b[k].why));
  const running = s.phase === "running" || s.phase === "starting";
  // The browser build records in this tab: keep the screen (and so the tab) awake.
  useWakeLock(!!web && running);
  const problem = s.config ? startProblem(s.config) : "Connecting to the recorder…";
  const liveDongle = s.config?.sources.some((x) => x.type !== "file") ?? false;
  // The setup guide: offered once, when the recorder first reports an empty config.
  const offered = useRef(false);
  useEffect(() => {
    if (offered.current || !s.config || !s.connected) return;
    offered.current = true;
    // (Not over a system being imported from the directory: that's what they came for.)
    if (s.phase === "idle" && guideWanted(s.config) && !ARRIVED_TO_IMPORT) openGuide("start");
  }, [s.config, s.connected]);

  if (s.quit) {
    return (
      <div className="app">
        <div className="quit-screen">
          <span className="logo" aria-hidden="true" />
          <h1>Trunk Recorder Pro has quit</h1>
          <p className="muted">Recording stopped; calls in progress were saved.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="shell">
      {s.guide && <SetupGuide key={s.guide} mode={s.guide} onClose={closeGuide} />}
      <DirectoryImport />
      <aside className="rail">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <div className="brand-text">
            <b>Trunk Recorder Pro</b>
            <span className="muted small">{s.version ? `v${s.version}` : ""}</span>
          </div>
        </div>
        <Nav view={s.view} health={health} />
      </aside>
      <div className="main">
        <header className="topbar">
          <div>
            <h1>{s.view === "plugin" ? (own.find((o) => o.id === s.path[0])?.title ?? LABEL.plugin) : LABEL[s.view]}</h1>
          </div>
          <div className="row">
            {!running && s.connected && (
              <button className="btn ghost" onClick={() => openGuide("start")} title="Set up a new system step by step">
                Setup guide
              </button>
            )}
            <span className={`pill pill-${s.phase}`}>
              {!s.connected ? "Disconnected" : s.phase === "running" ? (liveDongle ? "Recording" : "Replaying") : s.phase === "idle" ? "Stopped" : s.phase === "starting" ? "Starting…" : "Stopping…"}
            </span>
            {running ? (
              <>
                {s.restartNeeded && (
                  <button className="btn ghost" onClick={start} title="Stop and start again, with the settings as they are now">
                    Restart
                  </button>
                )}
                <button className="btn primary" onClick={stop}>
                  Stop
                </button>
              </>
            ) : (
              <button className="btn primary" disabled={!s.connected || s.phase === "stopping" || !!problem} title={problem ?? ""} onClick={start}>
                Start
              </button>
            )}
            {can("service") && s.connected && (
              <button className="btn ghost" onClick={quitApp} title="Stop recording and quit the app">
                Quit
              </button>
            )}
          </div>
        </header>
        <div className="banners">
          {!s.connected && <div className="banner bad">Not connected to trunk-pro. Retrying…</div>}
          {s.error && (
            <div className="banner bad" role="alert">
              <span>{s.error}</span>
              <button className="btn ghost small" onClick={dismissError}>
                Dismiss
              </button>
            </div>
          )}
          {s.notice && (
            <div className="banner" role="status">
              <span>{s.notice}</span>
              <button className="btn ghost small" onClick={() => setNotice(null)}>
                OK
              </button>
            </div>
          )}
          {running && s.restartNeeded && (
            <div className="banner" role="status">
              Change requires restart — some settings you changed only apply when recording starts again.
            </div>
          )}
          {s.ended && s.phase === "idle" && <div className="banner">Replay finished.</div>}
          {!running && problem && s.connected && !s.error && s.view !== "setup" && (
            <div className="banner subtle">
              <span>{problem}</span>
              <button className="btn ghost small" onClick={() => setView("setup")}>
                Open setup
              </button>
            </div>
          )}
        </div>
        <main>
          <Page view={s.view} />
        </main>
      </div>
    </div>
  );
}
