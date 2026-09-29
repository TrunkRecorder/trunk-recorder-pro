import { useEffect, useMemo, useState } from "react";
import { formatMhz, startProblem } from "./config.ts";
import { dismissError, downloadCall, setListen, setNotice, start, stop, transport, useApp, web, type AppState } from "./controller.ts";
import { BrowserStorage } from "./web/BrowserStorage.tsx";
import type { CallEntry, CallView } from "./protocol.ts";
import { Setup } from "./Setup.tsx";
import { Waterfall } from "./Waterfall.tsx";

const hex = (v: number | null | undefined) => (v === null || v === undefined ? "—" : v.toString(16).toUpperCase());

function clock(s: number): string {
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(ss).padStart(2, "0")}` : `${m}:${String(ss).padStart(2, "0")}`;
}

function Tile(props: { label: string; value: React.ReactNode; sub?: React.ReactNode; tone?: "ok" | "warn" | "bad" }) {
  return (
    <div className={`tile${props.tone ? ` tone-${props.tone}` : ""}`}>
      <div className="tile-label">{props.label}</div>
      <div className="tile-value">{props.value}</div>
      {props.sub && <div className="tile-sub">{props.sub}</div>}
    </div>
  );
}

function StatusTiles({ s }: { s: AppState }) {
  const st = s.status;
  const total = (st?.good ?? 0) + (st?.bad ?? 0);
  const pct = total ? Math.round((100 * (st?.good ?? 0)) / total) : null;
  const [rate, setRate] = useState<{ good: number; t: number; perS: number } | null>(null);
  useEffect(() => {
    if (!st) return;
    setRate((prev) => {
      const t = st.nowS;
      if (!prev || t - prev.t >= 2) return { good: st.good, t, perS: prev && t > prev.t ? (st.good - prev.good) / (t - prev.t) : 0 };
      return prev;
    });
  }, [st]);
  const ccTone = !st ? undefined : rate && rate.perS > 5 ? "ok" : rate && rate.perS > 0 ? "warn" : "bad";
  const srcTrouble = s.sources.some((x) => x.dropped > 0 || x.errors > 0 || (!x.ended && x.rateMeasured > 0 && Math.abs(x.rateMeasured / x.rateHz - 1) > 0.05));
  const maxRecorders = s.config?.recording.maxRecorders ?? 0;
  return (
    <div className="tiles">
      <Tile
        label="Control channel"
        tone={ccTone}
        value={st?.controlChannelHz ? <span className="mono">{formatMhz(st.controlChannelHz)}</span> : "—"}
        sub={
          st ? (
            <>
              {rate ? `${rate.perS.toFixed(1)} msg/s` : "…"} · {pct === null ? "no decodes yet" : `${pct}% decoded`} · {st.modulation ?? "detecting"}
            </>
          ) : (
            "starting…"
          )
        }
      />
      <Tile
        label="System"
        value={<span className="mono">{st?.identity.nac != null ? `NAC ${hex(st.identity.nac)}` : "—"}</span>}
        sub={<span className="mono">WACN {hex(st?.identity.wacn)} · SysID {hex(st?.identity.sysId)} · RFSS {st?.identity.rfss ?? "—"} Site {st?.identity.site ?? "—"}</span>}
      />
      <Tile
        label="Recorders"
        value={
          <span className="mono">
            {st?.recording ?? 0} <small>/ {maxRecorders}</small>
          </span>
        }
        sub={`${st?.activeCalls ?? 0} active calls · ${st?.callsConcluded ?? 0} saved this run`}
      />
      <Tile
        label={s.sources.length > 1 ? `Radios (${s.sources.length})` : "Radio"}
        tone={!s.sources.length ? undefined : srcTrouble ? "warn" : "ok"}
        value={<span className="mono">{s.sources.length ? s.sources.map((x) => (x.rateMeasured / 1e6).toFixed(2)).join(" · ") + " MSPS" : "—"}</span>}
        sub={
          s.sources.length
            ? `DSP ${(s.load * 100).toFixed(1)}% of a core${s.sources.map((x) => (x.dropped ? ` · ${x.label}: ${x.dropped} samples dropped` : "") + (x.errors ? ` · ${x.label}: ${x.lastError}` : "")).join("")}`
            : "…"
        }
      />
      <Tile label="Uptime" value={<span className="mono">{st ? clock(st.nowS) : "0:00"}</span>} sub={s.sources.every((x) => x.label.startsWith("file")) && s.sources.length ? "replaying capture" : "live"} />
    </div>
  );
}

function reasonText(c: CallView): string {
  if (c.state === "recording") return c.encrypted ? "recording (encrypted)" : "recording";
  switch (c.reason) {
    case "encrypted":
      return "encrypted — not recorded";
    case "no_source":
      return "outside tuned range";
    case "no_recorder":
      return "no free recorder";
    case "unknown_tg":
      return "not in talkgroup list";
    default:
      return "monitoring";
  }
}

function ActiveCalls({ s }: { s: AppState }) {
  const now = s.status?.nowS ?? 0;
  const calls = [...s.calls].sort((a, b) => Number(b.state === "recording") - Number(a.state === "recording") || b.startS - a.startS);
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Active calls</h2>
        <div className="row">
          <label className="toggle">
            <input type="checkbox" checked={s.listen} onChange={(e) => setListen(e.target.checked, s.listenTalkgroup)} />
            <span>Listen live</span>
          </label>
          {s.listen && s.listenTalkgroup !== null && (
            <button className="btn ghost" onClick={() => setListen(true, null)}>
              TG {s.listenTalkgroup} only ✕
            </button>
          )}
        </div>
      </header>
      {s.nowPlaying && s.listen && <div className="now-playing">▶ TG {s.nowPlaying.talkgroup}</div>}
      {calls.length === 0 ? (
        <p className="empty">No calls right now.</p>
      ) : (
        <div className="table-wrap">
          <table className="calls">
            <thead>
              <tr>
                <th>Talkgroup</th>
                <th>Freq, MHz</th>
                <th>Source</th>
                <th>Time</th>
                <th>State</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {calls.map((c) => (
                <tr key={c.id} className={`st-${c.state}${c.emergency ? " emergency" : ""}`}>
                  <td>
                    <span className="tg">{c.talkgroup}</span>
                    {c.alphaTag && <span className="tag">{c.alphaTag}</span>}
                    {c.emergency && <span className="badge bad">EMERG</span>}
                  </td>
                  <td className="mono">
                    {formatMhz(c.freqHz, 4)}
                    {c.slot !== null && <span className="muted"> · s{c.slot}</span>}
                  </td>
                  <td className="mono">{c.sources.at(-1) ?? "—"}</td>
                  <td className="mono">{clock(Math.max(0, now - c.startS))}</td>
                  <td>
                    <span className={`dot dot-${c.state}${c.encrypted ? " dot-enc" : ""}`} /> {reasonText(c)}
                  </td>
                  <td>
                    {c.state === "recording" && (
                      <button className="btn ghost small" onClick={() => setListen(true, c.talkgroup)} title="Listen to this talkgroup only">
                        Listen
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function History({ s }: { s: AppState }) {
  const [playing, setPlaying] = useState<string | null>(null);
  const [playingUrl, setPlayingUrl] = useState<string | null>(null);
  useEffect(() => {
    setPlayingUrl(null);
    if (!playing) return;
    let live = true;
    transport.callUrl(playing, "wav").then(
      (u) => live && setPlayingUrl(u),
      () => live && setPlaying(null),
    );
    return () => {
      live = false;
    };
  }, [playing]);
  const [filter, setFilter] = useState("");
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const ok = (c: CallEntry) => String(c.record.talkgroup).includes(f) || (c.record.talkgroup_tag ?? "").toLowerCase().includes(f);
    return f ? s.history.filter(ok) : s.history;
  }, [s.history, filter]);

  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Recent calls</h2>
        <input className="search" placeholder="Filter talkgroup…" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </header>
      {rows.length === 0 ? (
        <p className="empty">Recorded calls will appear here.</p>
      ) : (
        <div className="table-wrap history">
          <table className="calls">
            <thead>
              <tr>
                <th>Time</th>
                <th>Talkgroup</th>
                <th>Length</th>
                <th>Sources</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.slice(0, 200).map((c) => (
                <tr key={c.path} className={playing === c.path ? "playing" : ""}>
                  <td className="mono">{new Date(c.record.start_time_ms).toLocaleTimeString()}</td>
                  <td>
                    <span className="tg">{c.record.talkgroup}</span>
                    {c.record.talkgroup_tag && <span className="tag">{c.record.talkgroup_tag}</span>}
                    {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                  </td>
                  <td className="mono">{(c.record.call_length_ms / 1000).toFixed(1)} s</td>
                  <td className="mono srcs" title={[...new Set(c.record.srcList?.map((x) => x.src))].join(", ")}>
                    {[...new Set(c.record.srcList?.map((x) => x.src))].join(", ") || "—"}
                  </td>
                  <td className="actions">
                    <button className="btn ghost small" onClick={() => setPlaying(c.path)}>
                      Play
                    </button>
                    <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")}>
                      WAV
                    </button>
                    <button className="btn ghost small" onClick={() => void downloadCall(c.path, "json")}>
                      JSON
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {playing && playingUrl && <audio className="player" src={playingUrl} controls autoPlay onEnded={() => setPlaying(null)} />}
      <footer className="panel-foot muted small">
        {web ? (
          <BrowserStorage />
        ) : s.config ? (
          <span>
            Saved to <span className="mono">{s.config.recording.captureDir}</span>
          </span>
        ) : null}
      </footer>
    </section>
  );
}

function Log({ s }: { s: AppState }) {
  const [show, setShow] = useState<"calls" | "all">("calls");
  const lines = show === "all" ? s.log : s.log.filter((l) => /grant|update|control|patch|status|sysid|error/.test(l.kind));
  return (
    <details className="panel log">
      <summary className="panel-head">
        <h2>Control channel log</h2>
        <span className="muted small">{s.log.length} recent messages</span>
      </summary>
      <div className="row log-filter">
        <label className="toggle">
          <input type="checkbox" checked={show === "all"} onChange={(e) => setShow(e.target.checked ? "all" : "calls")} />
          <span>Show unit activity (affiliations, registrations…)</span>
        </label>
      </div>
      <pre className="log-lines">
        {lines
          .slice(-150)
          .reverse()
          .map((l, i) => (
            <div key={i} className={`k-${l.kind}`}>
              <span className="muted">{l.timeS.toFixed(1).padStart(7)}s</span> {l.text}
            </div>
          ))}
      </pre>
    </details>
  );
}

export function App() {
  const s = useApp();
  const running = s.phase === "running" || s.phase === "starting";
  const problem = s.config ? startProblem(s.config) : "Connecting to the recorder…";
  const liveDongle = s.config?.sources.some((x) => x.kind === "rtlsdr") ?? false;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <div>
            <h1>Trunk Recorder Lite</h1>
            <p className="muted small">
              P25 trunked radio recorder{s.version ? ` · v${s.version}` : ""}
            </p>
          </div>
        </div>
        <div className="row">
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
        </div>
      </header>

      {!s.connected && <div className="banner bad">Not connected to the recorder — is trunk-lite running? Retrying…</div>}
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
      {!running && problem && s.connected && !s.error && <div className="banner subtle">{problem}</div>}

      <main>
        {running ? (
          <>
            <StatusTiles s={s} />
            {s.spectra.map((sp, i) =>
              sp ? (
                <section className="panel flush" key={i}>
                  <Waterfall radio={sp} label={s.sources[i]?.label} ccHz={s.status?.controlChannelHz ?? null} calls={s.calls} />
                </section>
              ) : null,
            )}
            <div className="columns">
              <ActiveCalls s={s} />
              <History s={s} />
            </div>
            <Log s={s} />
          </>
        ) : (
          <div className="columns setup-cols">
            <Setup />
            <History s={s} />
          </div>
        )}
      </main>
    </div>
  );
}
