// Live: the calls on the air now (with live audio), and the control
// channel's log (made only while it's open). Recorded calls are on Listen.

import { useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { setListen, useTopic, type AppState } from "../controller.ts";
import type { CallView, TalkgroupName } from "../protocol.ts";
import { unitName } from "../units.ts";
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

/** The Live page: on the air now, and the log. */
export function LivePage({ s }: { s: AppState }) {
  return (
    <>
      <ActiveCalls s={s} />
      <Log s={s} />
    </>
  );
}
