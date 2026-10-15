// Calls: what's on the air now, every call heard (the last 24 hours) with a
// drawer of one call's details, and the control channel's log. Playback goes
// through Listen's player, so there's one sound at a time.

import { useEffect, useMemo, useState, type ReactNode } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { downloadCall, setListen, setView, type AppState } from "../controller.ts";
import { num } from "../fmt.ts";
import { loadOlder, playNow, seek, systemOf, togglePause, useListen } from "../listen.ts";
import type { CallEntry, CallRecord } from "../protocol.ts";
import { ActiveCalls, aliasOf, Log, SystemFilter, Unit } from "./Calls.tsx";

const ROWS_MAX = 500;

const labelOf = (r: CallRecord) => r.talkgroup_tag || `TG ${r.talkgroup}`;
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
const hex = (v: number) => `0x${v.toString(16).toUpperCase().padStart(2, "0")}`;

/** The time of day; the day too when it wasn't today. */
function timeOf(ms: number): string {
  const d = new Date(ms);
  const t = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  return d.toDateString() === new Date().toDateString() ? t : `${d.toLocaleDateString([], { weekday: "short" })} ${t}`;
}

/** The radios that talked, each once, in the order they first did. */
function unitsOf(r: CallRecord): CallRecord["srcList"] {
  const seen = new Set<number>();
  return (r.srcList ?? []).filter((u) => u.src > 0 && !seen.has(u.src) && !!seen.add(u.src));
}

/** P25 ALGIDs by name. */
const ALGID: Record<number, string> = { 0x80: "none", 0x81: "DES-OFB", 0x83: "3DES", 0x84: "AES-256", 0x85: "AES-128", 0x9f: "DES-XL", 0xa0: "DVI-XL", 0xa1: "DVP-XL", 0xaa: "ADP (RC4)" };

/** Why a call can't be played, or null. */
function unplayable(c: CallEntry): string | null {
  if (c.record.encrypted && !c.record.decrypted) return "Encrypted — no audio";
  if (c.audio === false) return "Audio not kept";
  return null;
}

/** Play a call (live audio stops: one sound at a time), or pause / resume it. */
function playOrPause(s: AppState, c: CallEntry, playingPath: string | undefined): void {
  if (playingPath === c.path) return togglePause();
  if (s.listen) setListen(false);
  playNow(c);
}

/** Play a call from `t` s in (starting it first when it isn't the one playing). */
function playAt(s: AppState, c: CallEntry, t: number, playingPath: string | undefined): void {
  if (playingPath !== c.path) {
    if (s.listen) setListen(false);
    playNow(c);
  }
  seek(t);
}

function Status({ c }: { c: CallEntry }) {
  const r = c.record;
  return (
    <>
      {r.encrypted && !r.decrypted ? <span className="chip warn">Encrypted</span> : r.decrypted ? <span className="chip ok">Decrypted</span> : null}
      {r.patched_talkgroups?.length ? <span className="chip">Patched</span> : null}
      {c.audio === false && !r.encrypted ? <span className="chip">No audio</span> : null}
    </>
  );
}

// ── the list ────────────────────────────────────────────────────────────────

function HeardCalls({ s, open, onOpen }: { s: AppState; open: string | null; onOpen: (path: string | null) => void }) {
  const l = useListen((x) => ({ calls: x.calls, loadingOlder: x.loadingOlder, moreOlder: x.moreOlder, playing: x.playing?.path, paused: x.paused }));
  const [only, setOnly] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [emergency, setEmergency] = useState(false);
  const [encrypted, setEncrypted] = useState(false);
  const systems = useMemo(() => [...new Set(l.calls.map(systemOf))].sort(), [l.calls]);
  const multi = systems.length > 1;
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return l.calls.filter((c) => {
      const r = c.record;
      if (only !== null && systemOf(c) !== only) return false;
      if (emergency && !r.emergency) return false;
      if (encrypted && !r.encrypted) return false;
      if (!f) return true;
      return (
        String(r.talkgroup).includes(f) ||
        labelOf(r).toLowerCase().includes(f) ||
        (r.talkgroup_description ?? "").toLowerCase().includes(f) ||
        (r.srcList ?? []).some((u) => String(u.src).includes(f) || (aliasOf(s, systemOf(c), u.src, u.tag || u.tag_ota) ?? "").toLowerCase().includes(f))
      );
    });
  }, [l.calls, only, filter, emergency, encrypted, s.units, s.config]);

  // ↑ / ↓ step through the list while the drawer is open.
  useEffect(() => {
    if (open === null) return;
    const key = (e: KeyboardEvent) => {
      if ((e.key !== "ArrowUp" && e.key !== "ArrowDown") || e.target instanceof HTMLInputElement) return;
      const i = rows.findIndex((c) => c.path === open);
      const next = rows[i + (e.key === "ArrowDown" ? 1 : -1)];
      if (next) {
        e.preventDefault();
        onOpen(next.path);
      }
    };
    addEventListener("keydown", key);
    return () => removeEventListener("keydown", key);
  }, [open, rows]);

  const oldest = l.calls.at(-1)?.record.start_time_ms;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Heard calls</h2>
        <span className="muted small">{rows.length === l.calls.length ? `${l.calls.length} calls` : `${rows.length} of ${l.calls.length} calls`}</span>
      </header>
      <div className="row heard-tools">
        <input className="search" placeholder="Talkgroup or unit…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <label className="toggle">
          <input type="checkbox" checked={emergency} onChange={(e) => setEmergency(e.target.checked)} />
          <span>Emergency</span>
        </label>
        <label className="toggle">
          <input type="checkbox" checked={encrypted} onChange={(e) => setEncrypted(e.target.checked)} />
          <span>Encrypted</span>
        </label>
      </div>
      <SystemFilter s={s} choices={systems} value={only} onChange={setOnly} />
      {rows.length === 0 ? (
        <p className="empty">{l.calls.length ? "No calls match." : "No calls heard yet."}</p>
      ) : (
        <div className="table-wrap tall">
          <table className="calls heard">
            <thead>
              <tr>
                <th>Time</th>
                {multi && <th>System</th>}
                <th>Talkgroup</th>
                <th>Length</th>
                <th>Units</th>
                <th>Freq, MHz</th>
                <th>SNR, dB</th>
                <th />
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.slice(0, ROWS_MAX).map((c) => {
                const r = c.record;
                const sys = systemOf(c);
                const units = unitsOf(r);
                const playing = l.playing === c.path;
                const why = unplayable(c);
                return (
                  <tr
                    key={c.path}
                    className={`${open === c.path ? "selected" : ""}${playing ? " playing" : ""}${r.emergency ? " emergency" : ""}`}
                    onClick={() => onOpen(open === c.path ? null : c.path)}
                    aria-selected={open === c.path}
                  >
                    <td className="mono muted">{timeOf(r.start_time_ms)}</td>
                    {multi && (
                      <td className="sys-name">
                        <span className="sys-dot" style={{ background: systemColor(s.config, sys) }} />
                        {sys}
                      </td>
                    )}
                    <td>
                      <span className="tg">{r.talkgroup}</span>
                      {r.talkgroup_tag && <span className="tag">{r.talkgroup_tag}</span>}
                      {r.emergency ? <span className="badge bad">EMERG</span> : null}
                    </td>
                    <td className="mono">{mmss(r.call_length_ms / 1000)}</td>
                    <td className="unit-cell">
                      {units.length ? <Unit id={units[0].src} alias={aliasOf(s, sys, units[0].src, units[0].tag || units[0].tag_ota)} /> : "—"}
                      {units.length > 1 && <span className="muted small"> +{units.length - 1}</span>}
                    </td>
                    <td className="mono">{formatMhz(r.freq, 4)}</td>
                    <td className="mono">{num(r.snr, 1)}</td>
                    <td>
                      <Status c={c} />
                    </td>
                    <td className="actions">
                      <button
                        className="btn ghost small"
                        disabled={!!why}
                        title={why ?? (playing && !l.paused ? "Pause" : "Play")}
                        aria-label={playing && !l.paused ? "Pause" : "Play"}
                        onClick={(e) => {
                          e.stopPropagation();
                          playOrPause(s, c, l.playing);
                        }}
                      >
                        {playing && !l.paused ? "❚❚" : "▶"}
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <footer className="listen-more">
        {rows.length > ROWS_MAX && <span className="muted small">Showing the newest {ROWS_MAX}. </span>}
        {l.moreOlder === false ? (
          <span className="muted small">That's every call from the last 24 hours.</span>
        ) : (
          <button className="btn ghost" onClick={loadOlder} disabled={l.loadingOlder || !s.connected}>
            {l.loadingOlder ? "Loading…" : "Load older"}
          </button>
        )}
        {oldest !== undefined && <span className="muted small">Back to {new Date(oldest).toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" })}</span>}
      </footer>
    </section>
  );
}

// ── one call ────────────────────────────────────────────────────────────────

function Player({ s, c }: { s: AppState; c: CallEntry }) {
  const l = useListen((x) => ({ playing: x.playing?.path, paused: x.paused, position: x.position, duration: x.duration }));
  const r = c.record;
  const mine = l.playing === c.path;
  const length = mine && l.duration > 0 ? l.duration : r.call_length_ms / 1000;
  const at = mine ? l.position : 0;
  const why = unplayable(c);
  const pct = (s: number) => `${length > 0 ? Math.min(100, (100 * s) / length) : 0}%`;
  return (
    <div className="drawer-player">
      <button className="btn" disabled={!!why} onClick={() => playOrPause(s, c, l.playing)} aria-label={mine && !l.paused ? "Pause" : "Play"}>
        {mine && !l.paused ? "❚❚" : "▶"}
      </button>
      <div
        className={`drawer-bar${why ? " off" : ""}`}
        onClick={(e) => {
          if (why) return;
          const b = e.currentTarget.getBoundingClientRect();
          playAt(s, c, ((e.clientX - b.left) / b.width) * length, l.playing);
        }}
        title={why ?? "Seek"}
      >
        <div className="drawer-bar-fill" style={{ width: pct(at) }} />
        {/* Where each radio started talking. */}
        {(r.srcList ?? []).filter((u) => u.src > 0).map((u, i) => (
          <i key={i} style={{ left: pct(u.pos) }} />
        ))}
      </div>
      <span className="mono small muted">
        {mmss(at)} / {mmss(length)}
      </span>
      {why && <div className="muted small drawer-why">{why}</div>}
    </div>
  );
}

function Field({ k, children }: { k: string; children: ReactNode }) {
  return (
    <>
      <dt>{k}</dt>
      <dd>{children}</dd>
    </>
  );
}

function CallDrawer({ s, c, onClose }: { s: AppState; c: CallEntry; onClose: () => void }) {
  const playing = useListen((x) => x.playing?.path);
  const r = c.record;
  const sys = systemOf(c);
  const sub = [r.talkgroup_description, r.talkgroup_group_tag, r.talkgroup_group].filter((x, i, a) => x && a.indexOf(x) === i);
  const mode = [
    r.audio_type,
    r.phase2_tdma || r.audio_type === "digital tdma" ? `slot ${r.tdma_slot}` : "",
    r.color_code >= 0 ? `CC ${r.color_code}` : "",
    r.ran !== undefined && r.ran >= 0 ? `RAN ${r.ran}` : "",
    r.tone_detected ? `${r.tone_mode === "dcs" ? "DCS" : "tone"} ${r.tone_detected}` : "",
  ].filter(Boolean);
  const voiceFrames = (r.errorList ?? []).reduce((n, e) => n + e.frames, 0);
  const badFrames = (r.errorList ?? []).reduce((n, e) => n + e.bad_frames, 0);
  const talked = (r.srcList ?? []).filter((u) => u.src > 0);

  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    addEventListener("keydown", key);
    return () => removeEventListener("keydown", key);
  }, [onClose]);

  return (
    <aside className="drawer" aria-label={`Call on ${labelOf(r)}`}>
      <header className="drawer-head">
        <div className="drawer-title">
          <div className="muted small">
            <span className="sys-dot" style={{ background: systemColor(s.config, sys) }} />
            {sys} · {timeOf(r.start_time_ms)}
          </div>
          <h3 title={`Talkgroup ${r.talkgroup}`}>
            {labelOf(r)}
            {r.emergency ? <span className="badge bad">EMERG</span> : null}
          </h3>
          {sub.length > 0 && <div className="muted small">{sub.join(" · ")}</div>}
        </div>
        <button className="btn ghost small" onClick={onClose} aria-label="Close" title="Close (Esc)">
          ✕
        </button>
      </header>

      <div className="drawer-body">
        <Player s={s} c={c} />
        <div className="row">
          <Status c={c} />
          <span className="spacer" />
          {c.audio !== false && !unplayable(c) && (
            <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")}>
              Save audio
            </button>
          )}
          {c.json !== false && (
            <button className="btn ghost small" onClick={() => void downloadCall(c.path, "json")}>
              Save JSON
            </button>
          )}
        </div>

        <h4>Call</h4>
        <dl className="drawer-fields">
          <Field k="Talkgroup">
            <button className="linkish mono" onClick={() => setView("radio", sys, "tg", r.talkgroup)} title="On the Radio system page">
              {r.talkgroup}
            </button>
          </Field>
          <Field k="Started">{new Date(r.start_time_ms).toLocaleString()}</Field>
          <Field k="Length">{num(r.call_length_ms / 1000, 1)} s</Field>
          <Field k="Frequency">
            <span className="mono">{formatMhz(r.freq, 5)} MHz</span>
            {r.freq_error ? <span className="muted"> · {r.freq_error > 0 ? "+" : ""}{r.freq_error} Hz off</span> : null}
          </Field>
          <Field k="Mode">{mode.join(" · ")}</Field>
          {r.patched_talkgroups?.length ? <Field k="Patched">{r.patched_talkgroups.filter((t) => t !== r.talkgroup).join(", ")}</Field> : null}
          <Field k="Call number">{r.call_num}</Field>
        </dl>

        <h4>Reception</h4>
        <dl className="drawer-fields">
          <Field k="Signal">{num(r.signal, 1)} dBFS</Field>
          <Field k="Noise">{num(r.noise, 1)} dBFS</Field>
          <Field k="SNR">{num(r.snr, 1)} dB</Field>
          {r.audio_type !== "analog" && (
            <>
              <Field k="Clean voice">{r.clean_voice_pct === null || r.clean_voice_pct === undefined ? "—" : `${num(r.clean_voice_pct, 0)}%`}</Field>
              {voiceFrames > 0 && (
                <Field k="Bad frames">
                  {badFrames} of {voiceFrames}
                </Field>
              )}
            </>
          )}
        </dl>

        {!!(r.encrypted || r.decrypted) && (
          <>
            <h4>Encryption</h4>
            <dl className="drawer-fields">
              <Field k="Algorithm">{r.algid === undefined || r.algid < 0 ? "not heard" : `${ALGID[r.algid] ?? "unknown"} (${hex(r.algid)})`}</Field>
              <Field k="Key ID">{r.key_id === undefined || r.key_id < 0 ? "not heard" : <span className="mono">{hex(r.key_id)}</span>}</Field>
              <Field k="Decrypted">{r.decrypted ? "yes" : "no — no key for it"}</Field>
            </dl>
          </>
        )}

        <h4>Who talked</h4>
        {talked.length === 0 ? (
          <p className="empty">No unit IDs heard.</p>
        ) : (
          <table className="calls drawer-units">
            <thead>
              <tr>
                <th>At</th>
                <th>Unit</th>
                <th>Location</th>
              </tr>
            </thead>
            <tbody>
              {talked.map((u, i) => (
                <tr key={i} className={u.emergency ? "emergency" : ""} onClick={() => !unplayable(c) && playAt(s, c, u.pos, playing)} title="Play from here">
                  <td className="mono muted">{mmss(u.pos)}</td>
                  <td className="unit-cell">
                    <Unit id={u.src} alias={aliasOf(s, sys, u.src, u.tag || u.tag_ota)} />
                    {u.emergency ? <span className="badge bad">EMERG</span> : null}
                  </td>
                  <td className="mono small">{u.lat !== undefined && u.lon !== undefined ? `${u.lat.toFixed(5)}, ${u.lon.toFixed(5)}` : <span className="muted">—</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {(r.freqList ?? []).length > 0 && (
          <>
            <h4>Frequencies</h4>
            <table className="calls">
              <thead>
                <tr>
                  <th>MHz</th>
                  <th>At</th>
                  <th>For</th>
                  <th>Errors</th>
                  <th>Spikes</th>
                </tr>
              </thead>
              <tbody>
                {r.freqList.map((f, i) => (
                  <tr key={i}>
                    <td className="mono">{formatMhz(f.freq, 5)}</td>
                    <td className="mono muted">{mmss(f.pos)}</td>
                    <td className="mono">{num(f.len, 1)} s</td>
                    <td className="mono">{f.error_count}</td>
                    <td className="mono">{f.spike_count}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}

        <div className="muted small mono drawer-path" title="Under the recordings folder">
          {c.path}
        </div>
      </div>
    </aside>
  );
}

/** The Calls page: on the air now, every call heard, and the log. */
export function CallsPage({ s }: { s: AppState }) {
  const [open, setOpen] = useState<string | null>(null);
  // The open call as it is now (its files may change after it concluded).
  const calls = useListen((x) => x.calls);
  const c = open === null ? null : (calls.find((x) => x.path === open) ?? null);
  return (
    <div className={c ? "with-drawer" : ""}>
      <ActiveCalls s={s} />
      <HeardCalls s={s} open={open} onOpen={setOpen} />
      <Log s={s} />
      {c && <CallDrawer s={s} c={c} onClose={() => setOpen(null)} />}
    </div>
  );
}
