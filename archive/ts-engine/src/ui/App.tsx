import { useEffect, useMemo, useState } from "react";
import { formatMhz } from "../config.ts";
import { callFile, clearCalls, exportCalls, storageEstimate, type StoredCall } from "../recording/opfsStore.ts";
import type { CallView } from "../worker/messages.ts";
import {
  dismissError,
  grantDevice,
  refreshDevices,
  refreshHistory,
  setListen,
  setNotice,
  start,
  startProblem,
  stop,
  useApp,
  type AppState,
} from "./controller.ts";
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
  const r = s.radio;
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
  const rateOk = r ? Math.abs(r.rateMeasured / r.rateHz - 1) < 0.05 : true;
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
            {st?.recording ?? 0} <small>/ {s.config.recording.maxRecorders}</small>
          </span>
        }
        sub={`${st?.activeCalls ?? 0} active calls · ${st?.callsConcluded ?? 0} saved this run`}
      />
      <Tile
        label="Radio"
        tone={!r ? undefined : !rateOk || r.ringDrops > 0 || r.usbRecoveries > 0 || s.backlogS > 1 ? "warn" : "ok"}
        value={<span className="mono">{r ? `${(r.rateMeasured / 1e6).toFixed(2)} MSPS` : "—"}</span>}
        sub={r ? `DSP ${(r.load * 100).toFixed(0)}% of a core · ${r.heads} channels${r.ringDrops ? ` · ${r.ringDrops} samples dropped` : ""}${r.usbRecoveries ? ` · USB restarted ${r.usbRecoveries}×` : ""}${s.backlogS > 0.5 ? ` · decode ${s.backlogS.toFixed(1)} s behind` : ""}` : "…"}
      />
      <Tile label="Uptime" value={<span className="mono">{r ? clock(r.airS) : "0:00"}</span>} sub={s.sourceKind === "file" ? "replaying capture" : "live"} />
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

function download(file: File, name: string): void {
  const url = URL.createObjectURL(file);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

function History({ s }: { s: AppState }) {
  const [playing, setPlaying] = useState<{ key: string; url: string } | null>(null);
  const [filter, setFilter] = useState("");
  const [usage, setUsage] = useState<{ usage: number; quota: number; persisted: boolean } | null>(null);
  const [exporting, setExporting] = useState<string | null>(null);
  const canExport = typeof (window as { showDirectoryPicker?: unknown }).showDirectoryPicker === "function";
  const onExport = async () => {
    let dest: FileSystemDirectoryHandle;
    try {
      dest = await (window as unknown as { showDirectoryPicker(o: { mode: string }): Promise<FileSystemDirectoryHandle> }).showDirectoryPicker({ mode: "readwrite" });
    } catch {
      return; // picker cancelled
    }
    try {
      const n = await exportCalls(dest, (d, t) => setExporting(`Exporting ${d} / ${t}…`));
      setNotice(`Exported ${n} call${n === 1 ? "" : "s"} (WAV + JSON, plus index.ndjson) to “${dest.name}”.`);
    } catch (e) {
      setNotice(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setExporting(null);
    }
  };
  useEffect(() => {
    void storageEstimate().then(setUsage);
  }, [s.history.length]);
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return f ? s.history.filter((c) => String(c.record.talkgroup).includes(f) || c.record.talkgroup_tag.toLowerCase().includes(f)) : s.history;
  }, [s.history, filter]);

  const play = async (c: StoredCall) => {
    if (playing) URL.revokeObjectURL(playing.url);
    try {
      setPlaying({ key: c.baseName, url: URL.createObjectURL(await callFile(c, "wav")) });
    } catch {
      setNotice(`${c.baseName}.wav is missing from storage.`);
    }
  };

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
                <tr key={`${c.dir}/${c.baseName}`} className={playing?.key === c.baseName ? "playing" : ""}>
                  <td className="mono">{new Date(c.record.start_time_ms).toLocaleTimeString()}</td>
                  <td>
                    <span className="tg">{c.record.talkgroup}</span>
                    {c.record.talkgroup_tag && <span className="tag">{c.record.talkgroup_tag}</span>}
                    {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                  </td>
                  <td className="mono">{(c.record.call_length_ms / 1000).toFixed(1)} s</td>
                  <td className="mono">{c.record.srcList.map((x) => x.src).join(", ") || "—"}</td>
                  <td className="actions">
                    <button className="btn ghost small" onClick={() => void play(c)}>
                      Play
                    </button>
                    <button className="btn ghost small" onClick={() => void callFile(c, "wav").then((f) => download(f, `${c.baseName}.wav`))}>
                      WAV
                    </button>
                    <button className="btn ghost small" onClick={() => void callFile(c, "json").then((f) => download(f, `${c.baseName}.json`))}>
                      JSON
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {playing && <audio className="player" src={playing.url} controls autoPlay onEnded={() => setPlaying(null)} />}
      <footer className="panel-foot muted small">
        {usage ? `${(usage.usage / 1e6).toFixed(1)} MB used of ${(usage.quota / 1e9).toFixed(1)} GB in this browser's private storage${usage.persisted ? "" : " (may be evicted under disk pressure)"}` : ""}
        <span className="spacer" />
        {exporting && <span>{exporting}</span>}
        {canExport && s.history.length > 0 && (
          <button className="btn ghost small" disabled={!!exporting} onClick={() => void onExport()} title="Copy every recorded call (WAV + JSON) into a folder on your disk">
            Export to folder…
          </button>
        )}
        {!usage?.persisted && (
          <button className="btn ghost small" onClick={() => void navigator.storage.persist().then(() => storageEstimate().then(setUsage))}>
            Keep storage
          </button>
        )}
        {s.history.length > 0 && s.phase === "idle" && (
          <button
            className="btn ghost small danger"
            onClick={() => {
              if (window.confirm("Delete every recorded call from this browser?")) void clearCalls().then(refreshHistory);
            }}
          >
            Delete all
          </button>
        )}
      </footer>
    </section>
  );
}

function Log({ s }: { s: AppState }) {
  const [show, setShow] = useState<"calls" | "all">("calls");
  const lines = show === "all" ? s.log : s.log.filter((l) => /grant|update|control|patch|status|sysid/.test(l.kind));
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
  useEffect(() => {
    void refreshDevices();
    void refreshHistory();
    navigator.usb?.addEventListener?.("connect", () => void refreshDevices());
    navigator.usb?.addEventListener?.("disconnect", () => void refreshDevices());
  }, []);
  useEffect(() => {
    const warn = (e: BeforeUnloadEvent) => {
      if (s.phase === "running") e.preventDefault();
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [s.phase]);

  const running = s.phase === "running" || s.phase === "starting";
  const problem = startProblem(s);

  const onStart = async () => {
    // WebUSB's chooser needs the click's user activation: ask before anything slow.
    if (s.sourceKind === "usb" && !s.devices.length && "usb" in navigator) {
      if (!(await grantDevice())) return;
    }
    await start();
  };

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <div>
            <h1>Trunk Recorder Pro</h1>
            <p className="muted small">P25 trunked radio, recorded in your browser</p>
          </div>
        </div>
        <div className="row">
          <span className={`pill pill-${s.phase}`}>{s.phase === "running" ? (s.sourceKind === "file" ? "Replaying" : "Recording") : s.phase === "idle" ? "Stopped" : s.phase === "starting" ? "Starting…" : "Stopping…"}</span>
          {running ? (
            <button className="btn primary" onClick={() => void stop()}>
              Stop
            </button>
          ) : (
            <button className="btn primary" disabled={s.phase === "stopping" || (!!problem && !problem.startsWith("This browser"))} title={problem ?? ""} onClick={() => void onStart()}>
              Start
            </button>
          )}
        </div>
      </header>

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
      {!running && problem && !s.error && <div className="banner subtle">{problem}</div>}

      <main>
        {running ? (
          <>
            <StatusTiles s={s} />
            <section className="panel flush">
              <Waterfall radio={s.radio} ccHz={s.status?.controlChannelHz ?? null} calls={s.calls} />
            </section>
            {s.sourceKind === "usb" && (
              <p className="muted small keep-open">Keep this tab open while recording. Your screen is kept awake; closing the tab or letting the computer sleep stops recording.</p>
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
