// A system's calls on the air now (with live audio) and its control
// channel's log (made only while it's open), on its Radio system page; and
// what other pages show of calls. Recorded calls are on Listen.

import { useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { setListen, setView, useTopic, type AppState } from "../controller.ts";
import type { CallView, TalkgroupName } from "../protocol.ts";
import { unitName } from "../units.ts";
import { tgText } from "./Systems.tsx";

/** An analog call on one of its conventional system's AM channels (calls say only analog). */
function isAm(s: AppState, c: CallView): boolean {
  const v = s.config?.conventional.find((x) => x.shortName === c.systemName);
  return !!v?.channels.some((ch) => ch.mode === "am" && Math.abs(ch.freqHz - c.freqHz) < 1);
}

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
export const aliasOf = (s: Pick<AppState, "config" | "units">, system: string, id: number, saved?: string) => {
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
  if (c.state === "recording") return c.decrypted ? "recording (decrypted)" : c.encrypted ? "recording (encrypted)" : "recording";
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

/** A system's calls on the air now (with live audio): on its Radio system page. */
export function ActiveCalls({ s, system }: { s: AppState; system: string }) {
  const now = s.status?.nowS ?? 0;
  const calls = s.calls.filter((c) => c.systemName === system).sort((a, b) => Number(b.state === "recording") - Number(a.state === "recording") || b.startS - a.startS);
  // (Live audio is one system's at a time: this one's, or another's.)
  const mine = s.listen && s.listenSystem === system;
  const playingCall = mine && s.nowPlaying ? s.calls.find((c) => c.id === s.nowPlaying?.callId) : undefined;
  const talkerId = playingCall?.sources.at(-1);
  const talker = playingCall && talkerId !== undefined ? { id: talkerId, alias: aliasOf(s, system, talkerId) } : null;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Active calls</h2>
        <div className="row">
          {s.listen && !mine && <span className="muted small">Listening to {s.listenSystem ?? "every system"}</span>}
          <label className="toggle">
            <input type="checkbox" checked={mine} onChange={(e) => setListen(e.target.checked, e.target.checked ? system : null, null)} />
            <span>Listen live</span>
          </label>
          {mine && s.listenTalkgroup !== null && (
            <button className="btn ghost" onClick={() => setListen(true, system, null)}>
              TG {s.listenTalkgroup} only ✕
            </button>
          )}
        </div>
      </header>
      {s.nowPlaying && mine && (
        <div className="now-playing">
          ▶ TG {s.nowPlaying.talkgroup}
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
                <th>Talkgroup</th>
                <th>Freq, MHz</th>
                <th>Source</th>
                <th>Time</th>
                <th>State</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {calls.map((c) => {
                const unit = c.sources.at(-1);
                return (
                  <tr key={c.id} className={`st-${c.state}${c.emergency ? " emergency" : ""}`}>
                    <td>
                      <button className="linkish" onClick={() => setView("radio", system, "tg", c.talkgroup)} title="This talkgroup's page">
                        <span className="tg">{c.talkgroup}</span>
                        {c.alphaTag && <span className="tag">{c.alphaTag}</span>}
                      </button>
                      {c.emergency && <span className="badge bad">EMERG</span>}
                      {c.site && c.site !== system && <span className="muted small"> · {c.site}</span>}
                      <PatchedWith tgs={c.patched ?? []} />
                      {c.alsoOn?.length ? (
                        <div className="patched small" title="The same call on other sites of its system; the best copy is saved">
                          also on {c.alsoOn.join(", ")}
                        </div>
                      ) : null}
                    </td>
                    <td className="mono">
                      {formatMhz(c.freqHz, 4)}
                      {c.slot !== null && <span className="muted"> · s{c.slot}</span>}
                      {c.analog && <span className="muted"> · {isAm(s, c) ? "AM" : "FM"}{c.tone ? ` ${c.tone}` : ""}</span>}
                      {c.nxdn && <span className="muted"> · {c.nxdn.toUpperCase()}{c.ran != null ? ` RAN ${c.ran}` : ""}</span>}
                    </td>
                    <td className="unit-cell">
                      {unit !== undefined ? (
                        <button className="linkish" onClick={() => setView("radio", system, "unit", unit)} title="This radio's page">
                          <Unit id={unit} alias={aliasOf(s, system, unit)} />
                        </button>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="mono">{clock(Math.max(0, now - c.startS))}</td>
                    <td>
                      <span className={`dot dot-${c.state}${c.encrypted ? " dot-enc" : ""}`} /> {reasonText(c)}
                    </td>
                    <td>
                      {c.state === "recording" && (
                        // (Heard on several sites: one copy plays, from whichever site has it.)
                        <button className="btn ghost small" onClick={() => setListen(true, system, c.talkgroup)} title="This talkgroup only">
                          Listen
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

/** A system's control channel log (made only while it's open): on its Radio system page. */
export function Log({ s, system }: { s: AppState; system: string }) {
  const [show, setShow] = useState<"calls" | "all">("calls");
  // Every control message as a line costs the recorder: only while it's open.
  const [open, setOpen] = useState(false);
  useTopic(open ? "log" : null);
  const mine = s.log.filter((l) => l.system === system);
  const lines = show === "all" ? mine : mine.filter((l) => /grant|update|control|patch|status|sysid|adjacent|error|note|alias|plugin|duplicate/.test(l.kind));
  return (
    <details className="panel log" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary className="panel-head">
        <h2>Control channel log</h2>
        {open && <span className="muted small">{mine.length} recent messages</span>}
      </summary>
      <div className="row log-filter">
        <label className="toggle">
          <input type="checkbox" checked={show === "all"} onChange={(e) => setShow(e.target.checked ? "all" : "calls")} />
          <span>Show unit activity</span>
        </label>
      </div>
      <pre className="log-lines">
        {lines
          .slice(-150)
          .reverse()
          .map((l, i) => (
            <div key={i} className={`k-${l.kind}`}>
              <span className="muted">{l.time ? new Date(l.time * 1000).toLocaleTimeString([], { hour12: false }).padStart(8) : `${l.timeS.toFixed(1).padStart(7)}s`}</span> {l.site && l.site !== system ? <span className="muted">[{l.site}] </span> : null}
              {l.text}
            </div>
          ))}
      </pre>
    </details>
  );
}
