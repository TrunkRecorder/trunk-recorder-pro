// Calls: on the air now (with live audio), recorded, and the control
// channel's log (made only while it's open).

import { Fragment, useEffect, useMemo, useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { downloadCall, setListen, transport, useTopic, web, type AppState } from "../controller.ts";
import type { CallEntry, CallView, TalkgroupName } from "../protocol.ts";
import { parseTalkgroupCsv } from "../talkgroups.ts";
import { unitName } from "../units.ts";
import { BrowserStorage } from "../web/BrowserStorage.tsx";
import { tgText } from "./Systems.tsx";

export function clock(s: number): string {
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(ss).padStart(2, "0")}` : `${m}:${String(ss).padStart(2, "0")}`;
}

/** A radio: its talker alias when known (the unit ID on hover), else its unit ID. */
export function Unit({ id, alias }: { id: number; alias: string | undefined }) {
  return alias ? (
    <span className="unit" title={`Unit ${id}`}>
      {alias}
    </span>
  ) : (
    <span className="mono">{id}</span>
  );
}

/** A unit's alias on a system: as learned, else as the call record saved it. */
/**
 * A unit's name, by its system's unit names mode: the unit names file's, the
 * talker alias heard (live, or `saved` with the call), as the recorder picks
 * a call's `tag`.
 */
export const aliasOf = (s: AppState, system: string, id: number, saved?: string) => {
  const c = s.config;
  const names = c?.systems.find((x) => x.shortName === system)?.unitNames ?? c?.conventional.find((x) => x.shortName === system)?.unitNames;
  const heard = () => s.units[system]?.[id] || saved || undefined;
  const user = () => (names?.csv ? unitName(names.csv, id) : undefined);
  switch (names?.mode || "user") {
    case "ota":
      return heard() ?? user();
    case "user_only":
      return user();
    case "none":
      return undefined;
    default:
      return user() ?? heard();
  }
};

/** A saved call's reception: the SNR, with the levels and clean share on hover. */
function Reception({ r }: { r: CallEntry["record"] }) {
  if (r.snr === undefined || r.snr === null) return <td className="muted">—</td>;
  const tone = r.snr >= 20 ? "ok" : r.snr >= 10 ? "warn" : "bad";
  const clean = r.clean_voice_pct ?? null;
  const title = `Signal ${r.signal?.toFixed(1)} dBFS, noise ${r.noise?.toFixed(1)} dBFS${clean !== null ? `, ${clean.toFixed(1)} % of voice frames decoded cleanly` : ""}`;
  return (
    <td className="mono" title={title}>
      <span className={`snr ${tone}`}>{r.snr.toFixed(0)} dB</span>
      {clean !== null && clean < 99.5 && <span className="muted small"> · {clean.toFixed(0)} %</span>}
    </td>
  );
}

/** Under a call's talkgroup: what's patched with it. */
export function PatchedWith({ tgs }: { tgs: TalkgroupName[] }) {
  if (!tgs.length) return null;
  const text = tgs.map(tgText).join(", ");
  return (
    <div className="patched small" title={`Patched with ${text}`}>
      patched with {text}
    </div>
  );
}

export function reasonText(c: CallView): string {
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
    case "ignored":
      return "ignored (talkgroup list)";
    default:
      return "monitoring";
  }
}

/** The systems calls can come from, by short name: each running system, then the conventional ones. */
export function systemChoices(s: AppState): string[] {
  const out = (s.status?.systems ?? []).map((x) => x.shortName);
  for (const v of s.config?.conventional ?? []) if (v.enabled && v.channels.some((ch) => ch.enabled)) out.push(v.shortName);
  return out;
}

/** Pick one system (or all), by short name: filter chips. */
export function SystemFilter(props: { s: AppState; choices: string[]; value: string | null; onChange: (v: string | null) => void }) {
  if (props.choices.length < 2) return null;
  return (
    <div className="filter-row" role="radiogroup" aria-label="System">
      <button className={`btn ghost small${props.value === null ? " on" : ""}`} role="radio" aria-checked={props.value === null} onClick={() => props.onChange(null)}>
        All
      </button>
      {props.choices.map((name) => (
        <button
          key={name}
          className={`btn ghost small${props.value === name ? " on" : ""}`}
          role="radio"
          aria-checked={props.value === name}
          onClick={() => props.onChange(name)}
        >
          <span className="sys-dot" style={{ background: systemColor(props.s.config, name) }} />
          {name}
        </button>
      ))}
    </div>
  );
}

export function ActiveCalls({ s }: { s: AppState }) {
  const now = s.status?.nowS ?? 0;
  const choices = systemChoices(s);
  const multi = choices.length > 1;
  const [only, setOnly] = useState<string | null>(null);
  const pick = (v: string | null) => {
    setOnly(v);
    // Live audio follows the filter.
    if (s.listen) setListen(true, v, null);
  };
  const calls = [...s.calls]
    .filter((c) => only === null || c.systemName === only)
    .sort((a, b) => Number(b.state === "recording") - Number(a.state === "recording") || b.startS - a.startS);
  const playingCall = s.nowPlaying && s.calls.find((c) => c.id === s.nowPlaying?.callId);
  const talkerId = playingCall?.sources.at(-1);
  const talker = playingCall && talkerId !== undefined ? { id: talkerId, alias: aliasOf(s, playingCall.systemName, talkerId) } : null;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Active calls</h2>
        <div className="row">
          <label className="toggle">
            <input type="checkbox" checked={s.listen} onChange={(e) => setListen(e.target.checked, e.target.checked ? only : null, e.target.checked ? s.listenTalkgroup : null)} />
            <span>Listen live</span>
          </label>
          {s.listen && (s.listenTalkgroup !== null || s.listenSystem !== null) && (
            <button className="btn ghost" onClick={() => setListen(true, null, null)}>
              {[s.listenSystem !== null && multi ? s.listenSystem : "", s.listenTalkgroup !== null ? `TG ${s.listenTalkgroup}` : ""].filter(Boolean).join(" ")} only ✕
            </button>
          )}
        </div>
      </header>
      <SystemFilter s={s} choices={choices} value={only} onChange={pick} />
      {s.nowPlaying && s.listen && (
        <div className="now-playing">
          ▶ {multi ? `${s.nowPlaying.system} · ` : ""}TG {s.nowPlaying.talkgroup}
          {talker && (
            <>
              {" · "}
              <Unit id={talker.id} alias={talker.alias} />
            </>
          )}
        </div>
      )}
      {calls.length === 0 ? (
        <p className="empty">No calls right now.</p>
      ) : (
        <div className="table-wrap">
          <table className="calls">
            <thead>
              <tr>
                {multi && <th>System</th>}
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
                  {multi && (
                    <td className="sys-name">
                      <span className="sys-dot" style={{ background: systemColor(s.config, c.systemName) }} />
                      {c.systemName}
                    </td>
                  )}
                  <td>
                    <span className="tg">{c.talkgroup}</span>
                    {c.alphaTag && <span className="tag">{c.alphaTag}</span>}
                    {c.emergency && <span className="badge bad">EMERG</span>}
                    <PatchedWith tgs={c.patched ?? []} />
                    {c.alsoOn?.length ? (
                      <div className="patched small" title="Same call on other sites; best copy saved">
                        also on {c.alsoOn.join(", ")}
                      </div>
                    ) : null}
                  </td>
                  <td className="mono">
                    {formatMhz(c.freqHz, 4)}
                    {c.slot !== null && <span className="muted"> · s{c.slot}</span>}
                    {c.analog && <span className="muted"> · FM{c.tone ? ` ${c.tone}` : ""}</span>}
                    {c.nxdn && <span className="muted"> · {c.nxdn.toUpperCase()}{c.ran != null ? ` RAN ${c.ran}` : ""}</span>}
                  </td>
                  <td className="unit-cell">{c.sources.length ? <Unit id={c.sources.at(-1)!} alias={aliasOf(s, c.systemName, c.sources.at(-1)!)} /> : "—"}</td>
                  <td className="mono">{clock(Math.max(0, now - c.startS))}</td>
                  <td>
                    <span className={`dot dot-${c.state}${c.encrypted ? " dot-enc" : ""}`} /> {reasonText(c)}
                  </td>
                  <td>
                    {c.state === "recording" && (
                      // Heard on several sites: one copy plays, from whichever site has it.
                      <button className="btn ghost small" onClick={() => setListen(true, c.alsoOn?.length ? null : c.systemName, c.talkgroup)} title="This talkgroup only">
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

/** A recorded call's system: its record's short_name, else its folder. */
const systemOf = (c: CallEntry) => c.record.short_name || c.path.split("/")[0] || "";

export function History({ s }: { s: AppState }) {
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
  const [only, setOnly] = useState<string | null>(null);
  const systems = useMemo(() => [...new Set(s.history.map(systemOf))].sort(), [s.history]);
  // Each system's talkgroup file, by short name: names for a call's patched talkgroups.
  const files = useMemo(() => new Map((s.config?.systems ?? []).map((x) => [x.shortName, parseTalkgroupCsv(x.talkgroupsCsv)])), [s.config]);
  const patchedWith = (c: CallEntry): TalkgroupName[] =>
    (c.record.patched_talkgroups ?? [])
      .filter((t) => t !== c.record.talkgroup)
      .map((t) => ({ talkgroup: t, alphaTag: files.get(systemOf(c))?.get(t)?.alphaTag ?? "" }));
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const unitMatch = (c: CallEntry) =>
      c.record.srcList?.some((x) => String(x.src).includes(f) || (aliasOf(s, systemOf(c), x.src, x.tag_ota) ?? "").toLowerCase().includes(f));
    const ok = (c: CallEntry) =>
      (only === null || systemOf(c) === only) &&
      (!f ||
        String(c.record.talkgroup).includes(f) ||
        (c.record.talkgroup_tag ?? "").toLowerCase().includes(f) ||
        patchedWith(c).some((t) => String(t.talkgroup).includes(f) || t.alphaTag.toLowerCase().includes(f)) ||
        unitMatch(c));
    return f || only !== null ? s.history.filter(ok) : s.history;
  }, [s.history, s.units, filter, only, files]);
  const multi = systems.length > 1;
  const colorOf = (name: string) => systemColor(s.config, name);

  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Recent calls</h2>
        <div className="row">
          {multi && (
            <select value={only ?? ""} onChange={(e) => setOnly(e.target.value || null)} aria-label="System">
              <option value="">All systems</option>
              {systems.map((x) => (
                <option key={x} value={x}>
                  {x}
                </option>
              ))}
            </select>
          )}
          <input className="search" placeholder="Filter talkgroup or unit…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </div>
      </header>
      {rows.length === 0 ? (
        <p className="empty">No recorded calls yet.</p>
      ) : (
        <div className="table-wrap history">
          <table className="calls">
            <thead>
              <tr>
                <th>Time</th>
                {multi && <th>System</th>}
                <th>Talkgroup</th>
                <th>Length</th>
                <th title="Signal over noise">Reception</th>
                <th>Sources</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.slice(0, 200).map((c) => (
                <tr key={c.path} className={playing === c.path ? "playing" : ""}>
                  <td className="mono">{new Date(c.record.start_time_ms).toLocaleTimeString()}</td>
                  {multi && (
                    <td className="sys-name small">
                      <span className="sys-dot" style={{ background: colorOf(systemOf(c)) }} />
                      {systemOf(c)}
                    </td>
                  )}
                  <td>
                    {c.record.talkgroup_tag ? (
                      <span className="tg-name" title={`Talkgroup ${c.record.talkgroup}`}>
                        {c.record.talkgroup_tag}
                      </span>
                    ) : (
                      <span className="tg">{c.record.talkgroup}</span>
                    )}
                    {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                    <PatchedWith tgs={patchedWith(c)} />
                  </td>
                  <td className="mono">{(c.record.call_length_ms / 1000).toFixed(1)} s</td>
                  <Reception r={c.record} />
                  <Sources s={s} entry={c} />
                  <td className="actions">
                    {c.audio !== false && (
                      <>
                        <button className="btn ghost small" onClick={() => setPlaying(c.path)}>
                          Play
                        </button>
                        <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")}>
                          WAV
                        </button>
                      </>
                    )}
                    {c.json !== false && (
                      <button className="btn ghost small" onClick={() => void downloadCall(c.path, "json")}>
                        JSON
                      </button>
                    )}
                    {c.audio === false && c.json === false && (
                      <span className="muted small" title="Deleted after upload (Setup → Recording)">
                        not kept
                      </span>
                    )}
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

/** A recorded call's radios, in the order they spoke: aliases first, every one on hover. */
function Sources({ s, entry }: { s: AppState; entry: CallEntry }) {
  const system = systemOf(entry);
  const seen = new Map<number, string | undefined>();
  for (const x of entry.record.srcList ?? []) if (!seen.has(x.src)) seen.set(x.src, aliasOf(s, system, x.src, x.tag || x.tag_ota));
  const units = [...seen];
  return (
    <td className="srcs" title={units.map(([id, alias]) => (alias ? `${alias} (${id})` : String(id))).join(", ")}>
      {units.length
        ? units.map(([id, alias], i) => (
            <Fragment key={id}>
              {i > 0 && ", "}
              <Unit id={id} alias={alias} />
            </Fragment>
          ))
        : "—"}
    </td>
  );
}

export function Log({ s }: { s: AppState }) {
  const [show, setShow] = useState<"calls" | "all">("calls");
  // Every control message as a line costs the recorder: only while it's open.
  const [open, setOpen] = useState(false);
  useTopic(open ? "log" : null);
  const [only, setOnly] = useState<string | null>(null);
  const systems = s.status?.systems ?? [];
  const multi = systems.length > 1;
  const lines = (show === "all" ? s.log : s.log.filter((l) => /grant|update|control|patch|status|sysid|adjacent|error|note|alias|plugin|duplicate/.test(l.kind))).filter(
    (l) => only === null || l.system === only,
  );
  return (
    <details className="panel log" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary className="panel-head">
        <h2>Control channel log</h2>
        {open && <span className="muted small">{s.log.length} recent messages</span>}
      </summary>
      <div className="row log-filter">
        <label className="toggle">
          <input type="checkbox" checked={show === "all"} onChange={(e) => setShow(e.target.checked ? "all" : "calls")} />
          <span>Show unit activity</span>
        </label>
        {multi && <SystemFilter s={s} choices={systems.map((x) => x.shortName)} value={only} onChange={setOnly} />}
      </div>
      <pre className="log-lines">
        {lines
          .slice(-150)
          .reverse()
          .map((l, i) => (
            <div key={i} className={`k-${l.kind}`}>
              <span className="muted">{l.timeS.toFixed(1).padStart(7)}s</span> {multi && l.system ? <span className="muted">[{l.system}] </span> : null}
              {l.text}
            </div>
          ))}
      </pre>
    </details>
  );
}

/** The Calls page: on the air now, recorded, and the log. */
export function CallsPage({ s }: { s: AppState }) {
  return (
    <>
      <div className="columns">
        <ActiveCalls s={s} />
        <History s={s} />
      </div>
      <Log s={s} />
    </>
  );
}
