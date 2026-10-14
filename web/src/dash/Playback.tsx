// Playback: look through everything the recorder has saved and listen to it.
// Pick a system, a talkgroup, a radio, a stretch of time, or "talkgroups not
// in my list" to find out what a new one is. See playback.ts.

import { useEffect, useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { downloadCall, useSelect, type AppState } from "../controller.ts";
import {
  clearFilters,
  loadMore,
  play,
  playable,
  search,
  seek,
  setAutoplay,
  setChrono,
  setFilters,
  skipBack,
  skipNext,
  SPANS,
  stopPlayback,
  togglePause,
  usePlayback,
  type Filters,
  type Span,
} from "../playback.ts";
import type { CallEntry, PlaybackTalkgroup, PlaybackUnit } from "../protocol.ts";
import { aliasOf } from "./Calls.tsx";

const systemOf = (c: CallEntry) => c.record.short_name || c.path.split("/")[0] || "";
const labelOf = (c: CallEntry) => c.record.talkgroup_tag || `TG ${c.record.talkgroup}`;
const when = (ms: number) => new Date(ms).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });
const day = (ms: number) => new Date(ms).toLocaleDateString([], { month: "short", day: "numeric" });
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

function unitOf(s: AppState, c: CallEntry): string {
  const u = c.record.srcList?.find((x) => x.src > 0);
  return u ? (aliasOf(s, systemOf(c), u.src, u.tag || u.tag_ota) ?? String(u.src)) : "";
}

// ── the player ───────────────────────────────────────────────────────────────

function Player({ s }: { s: AppState }) {
  const p = usePlayback((x) => ({ playing: x.playing, paused: x.paused, position: x.position, duration: x.duration, autoplay: x.autoplay, chrono: x.chrono }));
  const c = p.playing;
  if (!c) {
    return (
      <section className="panel pb-player idle">
        <span className="muted">Pick a call below to play it.</span>
      </section>
    );
  }
  const r = c.record;
  const system = systemOf(c);
  const pct = p.duration > 0 ? Math.min(100, (100 * p.position) / p.duration) : 0;
  const sub = [r.talkgroup_description, r.talkgroup_group_tag, r.talkgroup_group].filter((x, i, a) => x && a.indexOf(x) === i).join(" · ");
  return (
    <section className="panel pb-player">
      <div className="pb-now">
        <span className="sys-dot" style={{ background: systemColor(s.config, system) }} />
        <b>{labelOf(c)}</b>
        <span className="muted">
          {system} · TG {r.talkgroup}
          {sub ? ` · ${sub}` : ""}
        </span>
        <span className="spacer" />
        <span className="mono muted">{when(r.start_time_ms)}</span>
      </div>
      <div className="small muted">
        {r.freq ? `${formatMhz(r.freq, 4)} MHz · ` : ""}
        {unitOf(s, c) ? `Radio ${unitOf(s, c)}` : "No radio ID"}
      </div>
      <div
        className="scan-progress"
        role="slider"
        aria-label="Position"
        aria-valuemin={0}
        aria-valuemax={Math.round(p.duration)}
        aria-valuenow={Math.round(p.position)}
        tabIndex={0}
        onKeyDown={(e) => {
          // The keys of a slider: arrows 5 s, Home / End the ends.
          const to = e.key === "ArrowRight" || e.key === "ArrowUp" ? p.position + 5 : e.key === "ArrowLeft" || e.key === "ArrowDown" ? p.position - 5 : e.key === "Home" ? 0 : e.key === "End" ? p.duration : null;
          if (to === null) return;
          e.preventDefault();
          seek(Math.max(0, Math.min(p.duration, to)));
        }}
        onClick={(e) => {
          const b = e.currentTarget.getBoundingClientRect();
          seek(((e.clientX - b.left) / b.width) * p.duration);
        }}
      >
        <span style={{ width: `${pct}%`, background: systemColor(s.config, system) }} />
      </div>
      <div className="row pb-controls">
        <button className="btn ghost" onClick={skipBack} title="Start of this call, or the call before" aria-label="Previous call">
          ⏮
        </button>
        <button className="btn primary" onClick={togglePause}>
          {p.paused ? "Play" : "Pause"}
        </button>
        <button className="btn ghost" onClick={skipNext} title="The next call in the list" aria-label="Next call">
          ⏭
        </button>
        <button className="btn ghost" onClick={stopPlayback}>
          Stop
        </button>
        <span className="mono small muted">
          {mmss(p.position)} / {mmss(p.duration)}
        </span>
        <span className="spacer" />
        <label className="toggle">
          <input type="checkbox" checked={p.autoplay} onChange={(e) => setAutoplay(e.target.checked)} />
          <span>Play on</span>
        </label>
        <label className="toggle" title="Next is the call after this one in time, not the one below">
          <input type="checkbox" checked={p.chrono} onChange={(e) => setChrono(e.target.checked)} />
          <span>In time order</span>
        </label>
        <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")} title="Save this call's audio">
          Save
        </button>
      </div>
    </section>
  );
}

// ── the filters ──────────────────────────────────────────────────────────────

/** A number field: applies on Enter or when left. */
function NumberField({ label, value, onChange }: { label: string; value: number | null; onChange: (v: number | null) => void }) {
  const [text, setText] = useState(value === null ? "" : String(value));
  useEffect(() => setText(value === null ? "" : String(value)), [value]);
  const apply = () => {
    const n = text.trim() === "" ? null : Number(text);
    const v = n !== null && Number.isInteger(n) && n >= 0 ? n : null;
    if (v !== value) onChange(v);
    else setText(v === null ? "" : String(v));
  };
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      <input inputMode="numeric" className="narrow" value={text} onChange={(e) => setText(e.target.value)} onBlur={apply} onKeyDown={(e) => e.key === "Enter" && apply()} />
    </label>
  );
}

function FilterBar() {
  const f = usePlayback((x) => x.filters);
  const systems = usePlayback((x) => x.systems);
  const [text, setText] = useState(f.text);
  useEffect(() => setText(f.text), [f.text]);
  const apply = () => text !== f.text && setFilters({ text });
  const active = f.system !== null || f.talkgroup !== null || f.unit !== null || f.text !== "" || f.newOnly;
  return (
    <section className="panel pb-filters">
      <div className="pb-filter-row">
        {systems.length > 1 || f.system ? (
          <label className="field">
            <span className="field-label">System</span>
            <select value={f.system ?? ""} onChange={(e) => setFilters({ system: e.target.value || null, talkgroup: null })}>
              <option value="">All</option>
              {systems.map((x) => (
                <option key={x}>{x}</option>
              ))}
            </select>
          </label>
        ) : null}
        <label className="field">
          <span className="field-label">When</span>
          <select value={f.span} onChange={(e) => setFilters({ span: e.target.value as Span })}>
            {SPANS.map((x) => (
              <option key={x.id} value={x.id}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
        <NumberField label="Talkgroup" value={f.talkgroup} onChange={(talkgroup) => setFilters({ talkgroup })} />
        <NumberField label="Radio ID" value={f.unit} onChange={(unit) => setFilters({ unit })} />
        <label className="field pb-text">
          <span className="field-label">Name or alias</span>
          <input value={text} placeholder="Fire, dispatch, Engine 5…" onChange={(e) => setText(e.target.value)} onBlur={apply} onKeyDown={(e) => e.key === "Enter" && apply()} />
        </label>
      </div>
      <div className="row">
        <label className="toggle" title="Only talkgroups that aren't in the system's talkgroup file">
          <input type="checkbox" checked={f.newOnly} onChange={(e) => setFilters({ newOnly: e.target.checked })} />
          <span>Only talkgroups not in my list</span>
        </label>
        <span className="spacer" />
        {active && (
          <button className="btn ghost small" onClick={clearFilters}>
            Clear filters
          </button>
        )}
        <button className="btn ghost small" onClick={search} title="Search again">
          Refresh
        </button>
      </div>
    </section>
  );
}

// ── who was heard ────────────────────────────────────────────────────────────

function TalkgroupList({ s }: { s: AppState }) {
  const l = usePlayback((x) => ({ talkgroups: x.talkgroups, f: x.filters }));
  const multi = new Set(l.talkgroups.map((t) => t.system)).size > 1;
  const rows = l.f.newOnly ? l.talkgroups.filter((t) => !t.known) : l.talkgroups;
  if (!rows.length) return null;
  const pick = (t: PlaybackTalkgroup) => {
    const on = l.f.talkgroup === t.talkgroup && l.f.system === t.system;
    setFilters(on ? { talkgroup: null } : { system: t.system, talkgroup: t.talkgroup });
  };
  return (
    <details className="pb-facet" open>
      <summary>
        <h2>Talkgroups heard</h2>
        <span className="muted small">{rows.length}</span>
      </summary>
      <ul className="pb-list">
        {rows.map((t) => {
          const on = l.f.talkgroup === t.talkgroup && (l.f.system === null || l.f.system === t.system);
          return (
            <li key={`${t.system}:${t.talkgroup}`}>
              <button className={on ? "playing" : ""} onClick={() => pick(t)} aria-pressed={on} title={[t.description, t.group].filter(Boolean).join(" · ")}>
                <span className="sys-dot" style={{ background: systemColor(s.config, t.system) }} />
                <span className="lc-tg">
                  <span className="lc-name">
                    {t.alphaTag || `TG ${t.talkgroup}`}
                    {!t.known && <span className="badge new">NEW</span>}
                  </span>
                  <span className="muted small lc-sub">
                    {multi ? `${t.system} · ` : ""}TG {t.talkgroup} · last {day(t.lastMs)}
                    {t.encrypted > 0 ? ` · ${t.encrypted} encrypted` : ""}
                  </span>
                </span>
                <span className="mono small muted">{t.calls}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </details>
  );
}

function UnitList({ s }: { s: AppState }) {
  const l = usePlayback((x) => ({ units: x.units, f: x.filters }));
  if (!l.units.length) return null;
  const pick = (u: PlaybackUnit) => setFilters(l.f.unit === u.unit ? { unit: null } : { unit: u.unit, ...(l.f.system ? {} : { system: u.system }) });
  return (
    <details className="pb-facet">
      <summary>
        <h2>Radios heard</h2>
        <span className="muted small">{l.units.length}</span>
      </summary>
      <ul className="pb-list">
        {l.units.map((u) => {
          const on = l.f.unit === u.unit;
          const name = aliasOf(s, u.system, u.unit, u.alias || undefined);
          return (
            <li key={`${u.system}:${u.unit}`}>
              <button className={on ? "playing" : ""} onClick={() => pick(u)} aria-pressed={on}>
                <span className="lc-tg">
                  <span className="lc-name">{name ?? u.unit}</span>
                  <span className="muted small lc-sub">{name ? `Radio ${u.unit} · ` : ""}last {day(u.lastMs)}</span>
                </span>
                <span className="mono small muted">{u.calls}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </details>
  );
}

// ── the calls ────────────────────────────────────────────────────────────────

function Calls({ s }: { s: AppState }) {
  const p = usePlayback((x) => ({ calls: x.calls, total: x.total, more: x.more, truncated: x.truncated, loading: x.loading, searched: x.searched, playing: x.playing?.path }));
  const multi = new Set(p.calls.map(systemOf)).size > 1;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Calls</h2>
        <span className="muted small">{p.searched ? `${p.total.toLocaleString()} found` : ""}</span>
      </header>
      {p.truncated && <div className="banner subtle">That's a lot of calls: only the newest were searched. Narrow the time or pick a system.</div>}
      {p.calls.length === 0 ? (
        <p className="empty">{p.loading || !p.searched ? "Searching…" : "No calls match."}</p>
      ) : (
        <ul className="listen-calls pb-calls">
          {p.calls.map((c) => {
            const playing = p.playing === c.path;
            const ok = playable(c);
            return (
              <li key={c.path}>
                <button className={playing ? "playing" : ""} onClick={() => void play(c)} disabled={!ok} aria-current={playing || undefined}>
                  <span className="mono muted pb-when">{when(c.record.start_time_ms)}</span>
                  <span className="sys-dot" style={{ background: systemColor(s.config, systemOf(c)) }} />
                  <span className="lc-tg">
                    <span className="lc-name">
                      {labelOf(c)}
                      {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                      {c.record.encrypted && c.record.decrypted ? <span className="badge enc" title="Encrypted on the air; decrypted with your key">DECRYPTED</span> : null}
                    </span>
                    <span className="muted small lc-sub">
                      {multi ? `${systemOf(c)} · ` : ""}
                      TG {c.record.talkgroup}
                      {unitOf(s, c) ? ` · ${unitOf(s, c)}` : ""}
                    </span>
                  </span>
                  <span className="mono small muted">{(c.record.call_length_ms / 1000).toFixed(0)} s</span>
                  <span className="lc-state small">{playing ? "▶ playing" : c.record.encrypted && !c.record.decrypted ? "encrypted" : c.audio === false ? "no audio" : ""}</span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <footer className="listen-more">
        {p.more && (
          <button className="btn ghost" onClick={loadMore} disabled={p.loading || !s.connected}>
            {p.loading ? "Loading…" : "Load older"}
          </button>
        )}
        {p.calls.length > 0 && !p.more && !p.truncated && <span className="muted small">That's every call that matches.</span>}
      </footer>
    </section>
  );
}

export function PlaybackPage() {
  const s = useSelect((x) => ({ connected: x.connected, config: x.config, units: x.units }) as AppState);
  const searched = usePlayback((x) => x.searched);
  // Search on opening, and again once the recorder is reachable.
  useEffect(() => {
    if (s.connected && !searched) search();
  }, [s.connected, searched]);
  return (
    <div className="playback">
      <div className="pb-main">
        <Player s={s} />
        <FilterBar />
        <Calls s={s} />
      </div>
      <aside className="pb-side panel">
        <TalkgroupList s={s} />
        <UnitList s={s} />
      </aside>
    </div>
  );
}
