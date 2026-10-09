// Listen: a scanner over the recorded calls. The display and its buttons
// (live feed, hold, avoid, pause, replay, skip), the recent calls to pick
// from, and which systems and talkgroups to hear. See listen.ts.

import { useMemo, useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { downloadCall, useSelect, web, type AppState } from "../controller.ts";
import {
  avoid,
  avoided,
  holdSystem,
  holdTalkgroup,
  letGo,
  loadOlder,
  playable,
  playNow,
  replay,
  seek,
  setAll,
  setFeed,
  setSystemOn,
  setTalkgroupsOn,
  skip,
  subject,
  systemOf,
  tgKey,
  togglePause,
  useListen,
  wanted,
  type Selection,
} from "../listen.ts";
import type { CallEntry } from "../protocol.ts";
import { parseTalkgroupCsv } from "../talkgroups.ts";
import { BrowserStorage } from "../web/BrowserStorage.tsx";
import { aliasOf } from "./Calls.tsx";

/** A talkgroup as the scanner shows it. */
interface Tg {
  system: string;
  talkgroup: number;
  key: string;
  alphaTag: string;
  description: string;
  tag: string;
  group: string;
}

/**
 * Every system and talkgroup there is to hear: each system's talkgroup file,
 * and what calls were heard on (conventional channels, talkgroups not in a file).
 */
function catalog(s: Pick<AppState, "config">, calls: CallEntry[]): Map<string, Tg[]> {
  const out = new Map<string, Map<number, Tg>>();
  const sys = (name: string) => out.get(name) ?? out.set(name, new Map()).get(name)!;
  for (const x of s.config?.systems ?? []) {
    if (!x.enabled) continue;
    const m = sys(x.shortName);
    for (const t of parseTalkgroupCsv(x.talkgroupsCsv).values()) {
      if (!t.ignore) m.set(t.number, { system: x.shortName, talkgroup: t.number, key: tgKey(x.shortName, t.number), alphaTag: t.alphaTag, description: t.description, tag: t.tag, group: t.group });
    }
  }
  for (const v of s.config?.conventional ?? []) if (v.enabled) sys(v.shortName);
  for (const c of calls) {
    const name = systemOf(c);
    const m = sys(name);
    const r = c.record;
    if (!m.has(r.talkgroup)) {
      m.set(r.talkgroup, {
        system: name,
        talkgroup: r.talkgroup,
        key: tgKey(name, r.talkgroup),
        alphaTag: r.talkgroup_tag ?? "",
        description: r.talkgroup_description ?? "",
        tag: r.talkgroup_group_tag ?? "",
        group: r.talkgroup_group ?? "",
      });
    }
  }
  return new Map([...out].map(([name, m]) => [name, [...m.values()].sort((a, b) => a.talkgroup - b.talkgroup)]));
}

const labelOf = (c: CallEntry) => c.record.talkgroup_tag || `TG ${c.record.talkgroup}`;
const timeOf = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

/** The radio that spoke first, by name when known. */
function unitOf(s: AppState, c: CallEntry): string {
  const u = c.record.srcList?.find((x) => x.src > 0);
  if (!u) return "";
  return aliasOf(s, systemOf(c), u.src, u.tag || u.tag_ota) ?? String(u.src);
}

function Led({ color, blink }: { color: string; blink?: boolean }) {
  return <span className={`led${blink ? " blink" : ""}`} style={{ background: color, color }} aria-hidden="true" />;
}

function Display({ s }: { s: AppState }) {
  const l = useListen((x) => x);
  const c = l.playing;
  if (!c) {
    return (
      <div className="scan-display idle">
        <div className="scan-label">{!s.connected ? "No link" : l.feed ? (l.paused ? "Paused" : "Scanning…") : "Feed off"}</div>
        <div className="muted small">
          {!s.connected ? "Not connected to the recorder." : l.feed ? "New calls play here as they're recorded." : "Press Live feed to hear calls as they're recorded, or pick one from the list."}
        </div>
      </div>
    );
  }
  const r = c.record;
  const system = systemOf(c);
  const sub = [r.talkgroup_description, r.talkgroup_group_tag, r.talkgroup_group].filter((x, i, a) => x && a.indexOf(x) === i);
  const pct = l.duration > 0 ? Math.min(100, (100 * l.position) / l.duration) : 0;
  return (
    <div className="scan-display">
      <div className="scan-top">
        <Led color={systemColor(s.config, system)} blink={l.paused} />
        <span className="scan-sys">{system}</span>
        {r.emergency ? <span className="badge bad">EMERG</span> : null}
        <span className="spacer" />
        <span className="mono muted">{timeOf(r.start_time_ms)}</span>
        <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")} title="Save this call's audio">
          Save
        </button>
      </div>
      <div className="scan-label" title={`Talkgroup ${r.talkgroup}`}>
        {labelOf(c)}
      </div>
      {sub.length > 0 && <div className="scan-sub">{sub.join(" · ")}</div>}
      <dl className="scan-fields">
        <div>
          <dt>TGID</dt>
          <dd className="mono">{r.talkgroup}</dd>
        </div>
        <div>
          <dt>Freq</dt>
          <dd className="mono">{r.freq ? formatMhz(r.freq, 4) : "—"}</dd>
        </div>
        <div>
          <dt>Unit</dt>
          <dd className="mono">{unitOf(s, c) || "—"}</dd>
        </div>
        <div>
          <dt>Length</dt>
          <dd className="mono">
            {mmss(l.position)} / {mmss(l.duration)}
          </dd>
        </div>
      </dl>
      <div
        className="scan-progress"
        role="slider"
        aria-label="Position"
        aria-valuemin={0}
        aria-valuemax={Math.round(l.duration)}
        aria-valuenow={Math.round(l.position)}
        onClick={(e) => {
          const b = e.currentTarget.getBoundingClientRect();
          seek(((e.clientX - b.left) / b.width) * l.duration);
        }}
      >
        <span style={{ width: `${pct}%`, background: systemColor(s.config, system) }} />
      </div>
    </div>
  );
}

function Controls() {
  const l = useListen((x) => ({ playing: x.playing, played: x.played, sel: x.sel, feed: x.feed, paused: x.paused }));
  const c = subject(l as Parameters<typeof subject>[0]);
  const hold = l.sel.hold;
  const key = c ? tgKey(systemOf(c), c.record.talkgroup) : null;
  const isAvoided = key !== null && avoided(l.sel, key);
  const timed = key !== null && isAvoided && l.sel.avoid[key] !== 0;
  const b = (label: string, onClick: () => void, opts: { on?: boolean; disabled?: boolean; title?: string } = {}) => (
    <button className={`scan-btn${opts.on ? " on" : ""}`} onClick={onClick} disabled={opts.disabled} aria-pressed={opts.on} title={opts.title}>
      {label}
    </button>
  );
  return (
    <div className="scan-controls">
      <button className={`scan-feed${l.feed ? " on" : ""}`} onClick={() => setFeed(!l.feed)} aria-pressed={l.feed}>
        <span className="scan-feed-dot" />
        Live feed {l.feed ? "on" : "off"}
      </button>
      <div className="scan-grid">
        {b("Hold sys", holdSystem, { on: hold?.talkgroup === null, disabled: !c && !hold, title: "Only this system" })}
        {b("Hold TG", holdTalkgroup, { on: hold !== null && hold.talkgroup !== null, disabled: !c && !hold, title: "Only this talkgroup" })}
        {b(l.paused ? "Resume" : "Pause", togglePause, { on: l.paused, disabled: !l.playing && !l.feed })}
        {b("Replay", replay, { disabled: !c, title: "Play the current call from the start, or the last one again" })}
        {b("Skip", skip, { disabled: !l.playing })}
        {b(isAvoided && !timed ? "Avoided" : "Avoid", () => avoid(0), { on: isAvoided && !timed, disabled: !c, title: "Stop playing this talkgroup (again: play it again)" })}
      </div>
      <div className="scan-avoid-times">
        <span className="muted small">Avoid for</span>
        {[30, 60, 120].map((m) => (
          <button key={m} className="btn ghost small" onClick={() => avoid(m)} disabled={!c}>
            {m} min
          </button>
        ))}
      </div>
    </div>
  );
}

function Recent({ s }: { s: AppState }) {
  const played = useListen((x) => x.played);
  if (!played.length) return null;
  return (
    <div className="scan-recent">
      <h2>Recent</h2>
      {played.map((c) => (
        <button key={c.path} className="scan-recent-row" onClick={() => playNow(c)} title="Play again">
          <span className="mono muted">{timeOf(c.record.start_time_ms)}</span>
          <Led color={systemColor(s.config, systemOf(c))} />
          <span className="scan-recent-tg">{labelOf(c)}</span>
          <span className="muted small">{systemOf(c)}</span>
        </button>
      ))}
    </div>
  );
}

function Scanner({ s }: { s: AppState }) {
  const l = useListen((x) => ({ feed: x.feed, queue: x.queue.length, missed: x.missed, hold: x.sel.hold }));
  const status = !s.connected ? "No link" : l.feed ? "Live" : "Feed off";
  return (
    <section className="panel scanner">
      <header className="scan-head">
        <span className={`scan-status ${!s.connected ? "bad" : l.feed ? "live" : "off"}`}>{status}</span>
        {l.hold && (
          <span className="chip small">
            Holding {l.hold.system}
            {l.hold.talkgroup !== null ? ` TG ${l.hold.talkgroup}` : ""}
          </span>
        )}
        <span className="spacer" />
        {l.missed > 0 && <span className="small warn-text">{l.missed} missed</span>}
        <span className="mono small" title="Calls waiting to play">
          Q {l.queue}
        </span>
      </header>
      <Display s={s} />
      <Controls />
      <Recent s={s} />
    </section>
  );
}

// ── the calls ────────────────────────────────────────────────────────────────

function CallList({ s }: { s: AppState }) {
  const l = useListen((x) => ({ calls: x.calls, sel: x.sel, queue: x.queue, playing: x.playing, loadingOlder: x.loadingOlder, moreOlder: x.moreOlder }));
  const [onlySelected, setOnlySelected] = useState(true);
  const [filter, setFilter] = useState("");
  const multi = new Set(l.calls.map(systemOf)).size > 1;
  const queued = useMemo(() => new Set(l.queue.map((c) => c.path)), [l.queue]);
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return l.calls.filter(
      (c) =>
        playable(c) &&
        (!onlySelected || wanted(l.sel, c)) &&
        (!f || String(c.record.talkgroup).includes(f) || labelOf(c).toLowerCase().includes(f) || (c.record.talkgroup_description ?? "").toLowerCase().includes(f)),
    );
  }, [l.calls, l.sel, onlySelected, filter]);
  const oldest = l.calls.at(-1)?.record.start_time_ms;
  return (
    <>
      <div className="row listen-tools">
        <input className="search" placeholder="Filter talkgroup…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <label className="toggle">
          <input type="checkbox" checked={onlySelected} onChange={(e) => setOnlySelected(e.target.checked)} />
          <span>Selected talkgroups only</span>
        </label>
      </div>
      {rows.length === 0 ? (
        <p className="empty">{l.calls.length ? "No calls match." : "No recorded calls yet."}</p>
      ) : (
        <ul className="listen-calls">
          {rows.slice(0, 500).map((c) => {
            const playing = l.playing?.path === c.path;
            return (
              <li key={c.path}>
                <button className={playing ? "playing" : ""} onClick={() => playNow(c)} aria-current={playing || undefined}>
                  <span className="mono muted">{timeOf(c.record.start_time_ms)}</span>
                  <Led color={systemColor(s.config, systemOf(c))} />
                  <span className="lc-tg">
                    <span className="lc-name">{labelOf(c)}</span>
                    {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                    <span className="muted small lc-sub">
                      {multi ? `${systemOf(c)} · ` : ""}
                      {unitOf(s, c) || `TG ${c.record.talkgroup}`}
                    </span>
                  </span>
                  <span className="mono small muted">{(c.record.call_length_ms / 1000).toFixed(0)} s</span>
                  <span className="lc-state small">{playing ? "▶ playing" : queued.has(c.path) ? "queued" : ""}</span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <footer className="listen-more">
        {rows.length > 500 && <span className="muted small">Showing the newest 500. </span>}
        {l.moreOlder === false ? (
          <span className="muted small">That's every call from the last 24 hours.</span>
        ) : (
          <button className="btn ghost" onClick={loadOlder} disabled={l.loadingOlder || !s.connected}>
            {l.loadingOlder ? "Loading…" : "Load older"}
          </button>
        )}
        {oldest !== undefined && <span className="muted small">Back to {new Date(oldest).toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" })}</span>}
      </footer>
    </>
  );
}

// ── what to hear ─────────────────────────────────────────────────────────────

/** On, off or some, for a set of talkgroups. */
function share(sel: Selection, tgs: Tg[]): "on" | "off" | "some" {
  const on = tgs.filter((t) => !sel.offSystems.includes(t.system) && !sel.offTalkgroups.includes(t.key)).length;
  return on === tgs.length ? "on" : on === 0 ? "off" : "some";
}

function Chips({ title, by, sel, tgs, known }: { title: string; by: (t: Tg) => string; sel: Selection; tgs: Tg[]; known: string[] }) {
  const groups = useMemo(() => {
    const m = new Map<string, Tg[]>();
    for (const t of tgs) {
      const g = by(t);
      if (g) m.set(g, [...(m.get(g) ?? []), t]);
    }
    return [...m].sort((a, b) => a[0].localeCompare(b[0]));
  }, [tgs]);
  if (groups.length < 2) return null;
  return (
    <div className="tg-chips">
      <h2>{title}</h2>
      <div className="filter-row">
        {groups.map(([name, list]) => {
          const st = share(sel, list);
          return (
            <button key={name} className={`btn ghost small chip3 ${st}`} onClick={() => setTalkgroupsOn(list.map((t) => t.key), st !== "on", known)} aria-pressed={st === "some" ? "mixed" : st === "on"}>
              {name}
            </button>
          );
        })}
      </div>
    </div>
  );
}

function Talkgroups({ s }: { s: AppState }) {
  const l = useListen((x) => ({ calls: x.calls, sel: x.sel }));
  const cat = useMemo(() => catalog(s, l.calls), [s.config, l.calls]);
  const [filter, setFilter] = useState("");
  const all = useMemo(() => [...cat.values()].flat(), [cat]);
  const known = useMemo(() => all.map((t) => t.key), [all]);
  const f = filter.trim().toLowerCase();
  const match = (t: Tg) => !f || String(t.talkgroup).includes(f) || `${t.alphaTag} ${t.description} ${t.tag} ${t.group}`.toLowerCase().includes(f);
  const sel = l.sel;
  const now = Date.now();
  const avoids = Object.entries(sel.avoid).filter(([k]) => avoided(sel, k, now));
  const name = (k: string) => {
    const t = all.find((x) => x.key === k);
    return t ? `${t.alphaTag || `TG ${t.talkgroup}`}${cat.size > 1 ? ` (${t.system})` : ""}` : k;
  };
  return (
    <>
      <div className="row listen-tools">
        <input className="search" placeholder="Find talkgroup…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <span className="spacer" />
        <button className="btn ghost small" onClick={() => setAll(true, [], [])}>
          All on
        </button>
        <button className="btn ghost small" onClick={() => setAll(false, [...cat.keys()], all.map((t) => t.key))}>
          All off
        </button>
      </div>
      {sel.hold && (
        <div className="banner subtle">
          <span>
            Holding on {sel.hold.system}
            {sel.hold.talkgroup !== null ? `, talkgroup ${sel.hold.talkgroup}` : ""}: only it plays.
          </span>
          <button className="btn ghost small" onClick={sel.hold.talkgroup === null ? holdSystem : holdTalkgroup}>
            Release
          </button>
        </div>
      )}
      {avoids.length > 0 && (
        <div className="tg-chips">
          <h2>Avoided</h2>
          <div className="filter-row">
            {avoids.map(([k, until]) => (
              <button key={k} className="btn ghost small" onClick={() => letGo(k)} title="Play it again">
                {name(k)}
                <span className="muted"> {until ? `until ${new Date(until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}` : ""} ✕</span>
              </button>
            ))}
          </div>
        </div>
      )}
      <Chips title="Groups" by={(t) => t.group} sel={sel} tgs={all} known={known} />
      <Chips title="Tags" by={(t) => t.tag} sel={sel} tgs={all} known={known} />
      {[...cat].map(([system, tgs]) => {
        const sysOn = !sel.offSystems.includes(system);
        const shown = tgs.filter(match);
        if (f && !shown.length) return null;
        return (
          <div key={system} className="tg-system">
            <label className="tg-system-head">
              <input type="checkbox" checked={sysOn} onChange={(e) => setSystemOn(system, e.target.checked)} />
              <Led color={systemColor(s.config, system)} />
              <b>{system}</b>
              <span className="muted small">
                {sysOn ? tgs.filter((t) => !sel.offTalkgroups.includes(t.key)).length : 0} of {tgs.length} talkgroups
              </span>
            </label>
            <div className="tg-grid">
              {shown.map((t) => {
                // (Ticked: heard. A system that's off has none; ticking one brings it back.)
                const on = sysOn && !sel.offTalkgroups.includes(t.key);
                return (
                  <label key={t.key} className={`tg-cell${avoided(sel, t.key, now) ? " avoided" : ""}`} title={[t.description, t.tag, t.group].filter(Boolean).join(" · ")}>
                    <input type="checkbox" checked={on} onChange={(e) => setTalkgroupsOn([t.key], e.target.checked, known)} />
                    <span className="tg-cell-name">{t.alphaTag || `TG ${t.talkgroup}`}</span>
                    <span className="mono muted small">{t.talkgroup}</span>
                  </label>
                );
              })}
            </div>
          </div>
        );
      })}
    </>
  );
}

export function ListenPage() {
  const s = useSelect((x) => ({ connected: x.connected, config: x.config, units: x.units }) as AppState);
  const [tab, setTab] = useState<"calls" | "talkgroups">("calls");
  const off = useListen((x) => x.sel.offSystems.length + x.sel.offTalkgroups.length);
  return (
    <div className="listen">
      <Scanner s={s} />
      <section className="panel listen-side">
        <div className="tabs listen-tabs" role="tablist">
          <button role="tab" aria-selected={tab === "calls"} className={tab === "calls" ? "on" : ""} onClick={() => setTab("calls")}>
            Calls
          </button>
          <button role="tab" aria-selected={tab === "talkgroups"} className={tab === "talkgroups" ? "on" : ""} onClick={() => setTab("talkgroups")}>
            Talkgroups{off ? <span className="muted small"> · some off</span> : null}
          </button>
        </div>
        {tab === "calls" ? <CallList s={s} /> : <Talkgroups s={s} />}
        {web && (
          <footer className="panel-foot muted small">
            <BrowserStorage />
          </footer>
        )}
      </section>
    </div>
  );
}
