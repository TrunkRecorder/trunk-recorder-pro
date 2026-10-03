// The dashboard's frame: the sections on the left (each with a light for
// how it's doing), the recorder's controls on top, the section below.

import { useEffect, useRef, type ReactNode } from "react";
import { startProblem } from "./config.ts";
import { closeGuide, dismissError, openGuide, quitApp, setNotice, setView, start, stop, useApp, useSelect, web, type View } from "./controller.ts";
import { guideWanted, SetupGuide } from "./Onboarding.tsx";
import { PluginsPage } from "./Plugins.tsx";
import { Setup } from "./Setup.tsx";
import { Light } from "./charts.tsx";
import { CallsPage } from "./dash/Calls.tsx";
import { pageHealth, type Health } from "./dash/data.ts";
import { Overview } from "./dash/Overview.tsx";
import { RfPage } from "./dash/Rf.tsx";
import { DecodePage } from "./dash/Decode.tsx";
import { RadioPage } from "./dash/Radio.tsx";
import { PluginHealthPage } from "./dash/PluginHealth.tsx";
import { PlatformPage } from "./dash/Platform.tsx";

const ICON: Record<View, ReactNode> = {
  overview: <path d="M3 3h6v8H3zM11 3h6v5h-6zM11 10h6v7h-6zM3 13h6v4H3z" />,
  rf: <path d="M2 14c2-8 4-8 6 0s4 8 6 0 3-6 4-2M2 17h16" fill="none" strokeWidth="1.6" />,
  decode: <path d="M3 5h14M3 10h9M3 15h12M14 9l3 1.5-3 1.5" fill="none" strokeWidth="1.6" />,
  radio: <path d="M10 10m-2 0a2 2 0 1 0 4 0a2 2 0 1 0-4 0M5 5a7 7 0 0 0 0 10M15 5a7 7 0 0 1 0 10M2.5 2.5a10.5 10.5 0 0 0 0 15M17.5 2.5a10.5 10.5 0 0 1 0 15" fill="none" strokeWidth="1.6" />,
  calls: <path d="M4 3h3l2 4-2 1.5a10 10 0 0 0 4.5 4.5L13 11l4 2v3a1 1 0 0 1-1 1A14 14 0 0 1 3 4a1 1 0 0 1 1-1z" fill="none" strokeWidth="1.6" />,
  plugins: <path d="M7 2v4M13 2v4M5 6h10v4a5 5 0 0 1-10 0zM10 15v3" fill="none" strokeWidth="1.6" />,
  platform: <path d="M3 4h14v9H3zM7 17h6M10 13v4" fill="none" strokeWidth="1.6" />,
  setup: <path d="M10 7a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM10 1.5v3M10 15.5v3M1.5 10h3M15.5 10h3M4 4l2 2M14 14l2 2M4 16l2-2M14 6l2-2" fill="none" strokeWidth="1.6" />,
};

const LABEL: Record<View, string> = {
  overview: "Overview",
  rf: "RF",
  decode: "Decode",
  radio: "Radio system",
  calls: "Calls",
  plugins: "Plugins",
  platform: "Platform",
  setup: "Setup",
};

/** What each section is for (under its title). */
const PURPOSE: Record<View, string> = {
  overview: "Every system at a glance",
  rf: "How well the radios capture the air",
  decode: "How well signals turn into messages and voice",
  radio: "Talkgroups and radios: who's active, and with whom",
  calls: "On the air now, and recorded",
  plugins: "Where calls go, and how that's going",
  platform: "The computer this runs on",
  setup: "Sources, systems, recording",
};

function Nav({ view, health }: { view: View; health: Record<string, Health> }) {
  const pages: View[] = ["overview", "rf", "decode", "radio", "calls", ...(web ? [] : (["plugins", "platform"] as View[])), "setup"];
  return (
    <nav className="rail-nav" aria-label="Sections">
      {pages.map((p) => {
        const h = health[p];
        return (
          <button key={p} className={view === p ? "on" : ""} aria-current={view === p ? "page" : undefined} onClick={() => setView(p)} title={h && h.level !== "ok" && h.level !== "idle" ? h.why : LABEL[p]}>
            <svg viewBox="0 0 20 20" className="nav-icon" stroke="currentColor" fill="currentColor" aria-hidden="true">
              {ICON[p]}
            </svg>
            <span className="nav-label">{LABEL[p]}</span>
            {h && h.level !== "idle" && <Light level={h.level} title={h.why} />}
          </button>
        );
      })}
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
    case "calls":
      return <CallsPage s={s} />;
    case "plugins":
      return web ? null : <PluginHealthPage manage={<PluginsPage />} />;
    case "platform":
      return web ? null : <PlatformPage />;
    case "setup":
      return <Setup />;
  }
}

export function App() {
  const s = useSelect((x) => ({
    connected: x.connected,
    quit: x.quit,
    phase: x.phase,
    config: x.config,
    version: x.version,
    error: x.error,
    notice: x.notice,
    ended: x.ended,
    guide: x.guide,
    view: x.view,
  }));
  // The lights: recomputed with what they depend on, a few times a second at most.
  const health = useSelect((x) => pageHealth(x), (a, b) => Object.keys(a).every((k) => a[k].level === b[k].level && a[k].why === b[k].why));
  const running = s.phase === "running" || s.phase === "starting";
  const problem = s.config ? startProblem(s.config) : "Connecting to the recorder…";
  const liveDongle = s.config?.sources.some((x) => x.type !== "file") ?? false;
  // The setup guide: offered once, when the recorder first reports an empty config.
  const offered = useRef(false);
  useEffect(() => {
    if (offered.current || !s.config || !s.connected) return;
    offered.current = true;
    if (s.phase === "idle" && guideWanted(s.config)) openGuide("start");
  }, [s.config, s.connected]);

  if (s.quit) {
    return (
      <div className="app">
        <div className="quit-screen">
          <span className="logo" aria-hidden="true" />
          <h1>Trunk Recorder Pro has quit</h1>
          <p className="muted">Recording stopped and calls in progress were saved. You can close this tab; open the app again to start it.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="shell">
      {s.guide && <SetupGuide key={s.guide} mode={s.guide} onClose={closeGuide} />}
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
            <h1>{LABEL[s.view]}</h1>
            <p className="muted small">{PURPOSE[s.view]}</p>
          </div>
          <div className="row">
            {!running && s.connected && (
              <button className="btn ghost" onClick={() => openGuide("start")} title="Step-by-step setup for a new system">
                Setup guide
              </button>
            )}
            <span className={`pill pill-${s.phase}`}>
              {!s.connected ? "Disconnected" : s.phase === "running" ? (liveDongle ? "Recording" : "Replaying") : s.phase === "idle" ? "Stopped" : s.phase === "starting" ? "Starting…" : "Stopping…"}
            </span>
            {running ? (
              <button className="btn primary" onClick={stop}>
                Stop
              </button>
            ) : (
              <button className="btn primary" disabled={!s.connected || s.phase === "stopping" || !!problem} title={problem ?? ""} onClick={start}>
                Start
              </button>
            )}
            {!web && s.connected && (
              <button className="btn ghost" onClick={quitApp} title="Stop recording and quit the app">
                Quit
              </button>
            )}
          </div>
        </header>
        <div className="banners">
          {!s.connected && <div className="banner bad">Not connected to the recorder — is trunk-pro running? Retrying…</div>}
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
