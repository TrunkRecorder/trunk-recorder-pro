// How well signals turn into messages and voice: each system's control
// channel (messages a second, how many are lost, how open the demodulator's
// eye is, the framer's misses) and its voice (decode errors) — by frequency,
// so a bad channel, a band edge or a source's ppm shows up as a pattern.

import { useState } from "react";
import { formatMhz, systemColor, usableHalfWidth } from "../config.ts";
import { radioQuery, setView, usePolled, useApp, useTopic, type AppState, type StatsRange } from "../controller.ts";
import { Card, Choice, HeatStrip, LiveSpark, Stat, TimeSeries, Trend, type Line } from "../charts.tsx";
import { mhz, num, signed } from "../fmt.ts";
import type { FreqRow, SeriesData, SourceStatus } from "../protocol.ts";
import { change, dashSystems, K, KIND_LABEL, recentAvg, running, sourceGuard, systemHealth, useHistory, useUsual, val, type DashSystem, type Usual } from "./data.ts";
import { DmrSites, hex, Neighbours, NxdnSites, Patches } from "./Systems.tsx";

/** `a / b` step by step (errors per frame from the two rates). */
function ratio(a: SeriesData | undefined, b: SeriesData | undefined, mul = 1): SeriesData | null {
  if (!a || !b || a.t0 !== b.t0 || a.stepS !== b.stepS) return null;
  const f = (x: number | null, y: number | null) => (x !== null && y !== null && y > 0 ? (x / y) * mul : null);
  return { t0: a.t0, stepS: a.stepS, v: a.v.map((x, i) => f(x, b.v[i])), lo: a.v.map(() => null), hi: a.v.map(() => null), n: a.n };
}

/** Voice frames that couldn't be decoded, %, over the last `sinceS` (live). */
export function voiceBadPct(name: string, sinceS = 600): number | null {
  const f = recentAvg(K.sys(name, "voice/frames"), sinceS) ?? 0;
  return f > 0 ? (100 * (recentAvg(K.sys(name, "voice/bad"), sinceS) ?? 0)) / f : null;
}

/** How a bad-frame share reads. */
export const badLevel = (p: number | null) => (p === null ? "" : p < 2 ? "ok" : p < 8 ? "warn" : "bad");

/** The voice quality strip's scale: 20% bad frames is the darkest. */
export const BAD_MAX = 20;
/** CQPSK phase error, RMS degrees: errors creep in past ~13°, a few % by 20°. */
const PHASE_WARN = 15;
const PHASE_BAD = 20;

/** A channel's demodulation quality: the eye opening, or CQPSK's phase error. */
export function quality(c: { quality: number | null; phaseErrDeg: number | null }): string {
  return c.quality !== null ? num(c.quality, 1) : c.phaseErrDeg !== null ? `${num(c.phaseErrDeg, 1)}°` : "—";
}

/**
 * The source covering `hz` (the one it's deepest in), and how far inside the
 * usable part of its band it is: 0 at the usable edge (half the band less the
 * source's guard) or out in the guard, 1 at the centre.
 */
function placeOn(s: AppState, hz: number): { src: SourceStatus; depth: number } | null {
  let best: { src: SourceStatus; depth: number } | null = null;
  for (const src of s.sources) {
    const off = Math.abs(hz - src.centerHz);
    if (off > src.rateHz / 2) continue;
    const depth = Math.max(0, 1 - off / usableHalfWidth(src.rateHz, sourceGuard(s, src)));
    if (!best || depth > best.depth) best = { src, depth };
  }
  return best;
}

/** What the frequencies say together: a common offset, trouble at a band edge, one bad channel. */
function freqHints(rows: FreqRow[], s: AppState): string[] {
  const out: string[] = [];
  const withErr = rows.filter((r) => r.freqError !== null && r.calls > 0);
  if (withErr.length >= 3) {
    const ppm = withErr.map((r) => (r.freqError! / r.freqHz) * 1e6);
    const mean = ppm.reduce((a, b) => a + b, 0) / ppm.length;
    const same = ppm.every((p) => Math.sign(p) === Math.sign(mean));
    if (same && Math.abs(mean) > 0.8) out.push(`Every frequency is about ${signed(mean, 1)} ppm off: correct the source's ppm or turn on AutoTune.`);
  }
  const rated = rows.filter((r) => r.badPct !== null && r.frames > 500);
  if (rated.length >= 3) {
    const sorted = rated.map((r) => r.badPct!).sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)];
    for (const r of rated) {
      if (r.badPct! < Math.max(3, median * 3)) continue;
      const p = placeOn(s, r.freqHz);
      if (p && p.depth < 0.15) out.push(`${mhz(r.freqHz)} MHz decodes worse, at the edge of ${p.src.label}'s band: move the centre toward it.`);
      else out.push(`${mhz(r.freqHz)} MHz decodes worse (${num(r.badPct, 1)}% lost vs ${num(median, 1)}%): interference or a weak path.`);
    }
  }
  return out.slice(0, 4);
}

function DecodeCard({ s, x, usual, freqs }: { s: AppState; x: DashSystem; usual: Usual | null; freqs?: FreqRow[] }) {
  const h = systemHealth(s, x, usual);
  const k = (n: string) => K.sys(x.name, n);
  const good = recentAvg(k("cc/good"), 60) ?? 0;
  const bad = recentAvg(k("cc/bad"), 60) ?? 0;
  const lost = good + bad > 0 ? (100 * bad) / (good + bad) : null;
  const vbad = voiceBadPct(x.name);
  const vf = recentAvg(k("voice/frames"), 600) ?? 0;
  const ve = recentAvg(k("voice/errors"), 600) ?? 0;
  const syncs = recentAvg(k("cc/syncs"), 60) ?? 0;
  const misses = (recentAvg(k("cc/nidFails"), 60) ?? 0) + (recentAvg(k("cc/flywheels"), 60) ?? 0);
  const sep = val(s, k("cc/sep"));
  const perr = val(s, k("cc/phaseErr"));
  const st = x.status;
  // CQPSK carries it (or only CQPSK runs): its phase error, not C4FM's eye.
  const cqpsk = st?.modulation === "CQPSK" || (sep === null && perr !== null);
  return (
    <Card
      title={
        <span className="row">
          <span className="sys-dot" style={{ background: systemColor(s.config, x.name) }} />
          {x.name}
        </span>
      }
      level={h.level}
      why={h.why}
      onTitle={() => setView("decode", x.name)}
      actions={<span className="badge-kind">{st?.modulation ? `${KIND_LABEL[x.kind]} · ${st.modulation}` : KIND_LABEL[x.kind]}</span>}
    >
      <div className="src-grid">
        {x.kind !== "conventional" && (
          <>
            <Stat
              label="Control messages"
              value={num(val(s, k("cc/good")))}
              unit="/s"
              trend={<Trend delta={change(recentAvg(k("cc/good"), 60), usual)} />}
              spark={<LiveSpark k={k("cc/good")} band={usual ? [usual.lo, usual.hi] : null} lo={0} />}
              sub={st?.controlChannelHz ? `${formatMhz(st.controlChannelHz)} MHz` : "hunting"}
            />
            <Stat
              label="Control lost"
              value={lost === null ? "—" : num(lost, 1)}
              unit="%"
              level={lost !== null && lost > 10 ? (lost > 30 ? "bad" : "warn") : undefined}
              spark={<LiveSpark k={k("cc/bad")} color="var(--serious)" lo={0} />}
              hint="Control messages not decoded. A few % is normal."
            />
            {x.kind === "smartnet" ? (
              <Stat label="Deviation" value={num(val(s, k("cc/deviation")), 0)} unit="Hz" sub={`carrier ${signed(val(s, k("cc/offset")), 0)} Hz`} hint="Normal is about ±2.4 kHz; less means weak or filtered." />
            ) : cqpsk ? (
              <Stat
                label="Phase error"
                value={perr === null ? "—" : num(perr, 1)}
                unit="° rms"
                level={perr === null ? undefined : perr >= PHASE_BAD ? "bad" : perr >= PHASE_WARN ? "warn" : undefined}
                spark={<LiveSpark k={k("cc/phaseErr")} color="var(--series-3)" lo={0} />}
                sub={`carrier ${signed(val(s, k("cc/offset")), 0)} Hz`}
                hint="CQPSK phase error: under 10° clean, 20°+ marginal, ~25° noise."
              />
            ) : (
              <Stat
                label="Eye opening"
                value={sep === null ? "—" : num(sep, 1)}
                level={sep !== null && sep < 4 ? "warn" : undefined}
                spark={<LiveSpark k={k("cc/sep")} color="var(--series-3)" lo={0} />}
                sub={`carrier ${signed(val(s, k("cc/offset")), 0)} Hz`}
                hint="C4FM symbol separation: 10+ clean, under 4 errors, ~1 noise. Simulcast reads low."
              />
            )}
            {x.kind === "p25" && (
              <Stat
                label="Framing"
                value={syncs > 0 ? num((100 * misses) / (syncs + misses), 1) : "—"}
                unit="% missed"
                sub={`${num((recentAvg(k("cc/eqResets"), 60) ?? 0) * 60, 0)} equaliser resets/min`}
                hint="Frames with a failed header (NID) or missed sync."
              />
            )}
          </>
        )}
        <Stat
          label="Voice lost"
          value={vbad === null ? "—" : num(vbad, 1)}
          unit="% of frames"
          level={vbad === null ? undefined : vbad >= 8 ? "bad" : vbad >= 2 ? "warn" : undefined}
          sub={vf > 0 ? `${num(ve / vf, 1)} bit errors fixed per frame` : "no voice yet"}
          hint="Unusable voice frames, 10 min. Under 2% is clean."
        />
      </div>
      {freqs && freqs.length > 0 && (
        <div className="freq-strip">
          <HeatStrip
            v={freqs.map((f) => f.badPct)}
            ramp="hot"
            max={BAD_MAX}
            h={18}
            titles={freqs.map((f) => `${mhz(f.freqHz)} MHz · ${f.calls} calls · ${f.badPct ?? "—"}% of voice frames lost · SNR ${f.snr ?? "—"} dB · ${signed(f.freqError, 0)} Hz`)}
            label="voice quality by frequency"
          />
          <span className="muted small">voice lost by frequency, 24 h</span>
        </div>
      )}
    </Card>
  );
}

const RANGES: { v: StatsRange; label: string }[] = [
  { v: "1h", label: "1 h" },
  { v: "24h", label: "24 h" },
  { v: "7d", label: "7 days" },
];

function SystemDetail({ s, name }: { s: AppState; name: string }) {
  const x = dashSystems(s).find((y) => y.name === name);
  const [range, setRange] = useState<StatsRange>("24h");
  useTopic(`decode:${name}`);
  const k = (n: string) => K.sys(name, n);
  const hist = useHistory([k("cc/good"), k("cc/bad"), k("cc/sep"), k("cc/phaseErr"), k("cc/snr"), k("cc/offset"), k("voice/frames"), k("voice/bad")], range, 360);
  const usual = useUsual([k("cc/good")]);
  const freqs = usePolled(`dfreqs:${name}:${range}`, () => radioQuery({ what: "freqs", system: name, hours: range === "7d" ? 168 : 24 }), 120_000);
  const rows = (freqs?.rows ?? []) as FreqRow[];
  const live = s.decodeDetail[name];
  const line = (key: string, label: string, c: string, extra: Partial<Line> = {}): Line => ({ label, color: c, data: hist?.[key] ?? null, ...extra });
  if (!x) return <p className="empty">No system called {name}.</p>;
  const st = x.status;
  const hints = freqHints(rows, s);
  return (
    <div className="stack">
      <div className="row">
        <button className="btn ghost small" onClick={() => setView("decode")}>
          ← All systems
        </button>
      </div>
      <DecodeCard s={s} x={x} usual={usual[k("cc/good")]} freqs={rows} />
      {st && !st.dmr && !st.nxdn && (
        <div className="kv small">
          <span>
            NAC <b className="mono">{hex(st.identity.nac)}</b>
          </span>
          <span>
            WACN <b className="mono">{hex(st.identity.wacn)}</b>
          </span>
          <span>
            SysID <b className="mono">{hex(st.identity.sysId)}</b>
          </span>
          <span>
            Site <b className="mono">{st.identity.site != null ? `${st.identity.rfss ?? "?"}-${st.identity.site}` : "—"}</b>
          </span>
          <span>
            {st.callsConcluded} calls saved this run · {st.activeCalls} active
          </span>
        </div>
      )}
      {live && live.channels.length > 0 && (
        <Card title="Channels now">
          <div className="table-wrap">
            <table className="calls">
              <thead>
                <tr>
                  <th>MHz</th>
                  <th>What</th>
                  <th>SNR</th>
                  <th>Offset</th>
                  <th title="Eye opening (10+ clean) or phase error (under 10° clean)">Quality</th>
                  <th>Calls</th>
                </tr>
              </thead>
              <tbody>
                {live.channels.map((c) => (
                  <tr key={c.freqHz}>
                    <td className="mono">{formatMhz(c.freqHz)}</td>
                    <td>{c.kind}</td>
                    <td className="mono">
                      <span className={`snr ${c.snrDb >= 15 ? "ok" : c.snrDb >= 8 ? "warn" : "bad"}`}>{num(c.snrDb, 0)} dB</span>
                    </td>
                    <td className="mono">{c.offsetHz !== null ? `${signed(c.offsetHz, 0)} Hz` : "—"}</td>
                    <td className="mono">{quality(c)}</td>
                    <td className="mono">{c.calls || ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      )}
      <div className="row">
        <h2 className="section-title">Over time</h2>
        <span className="spacer" />
        <Choice value={range} options={RANGES} onChange={setRange} label="Span" />
      </div>
      <div className="columns">
        {x.kind !== "conventional" && (
          <Card title="Control channel">
            <TimeSeries
              lines={[line(k("cc/good"), "Decoded /s", "var(--series-1)", { band: true }), line(k("cc/bad"), "Lost /s", "var(--serious)")]}
              zero
              marks={usual[k("cc/good")] ? [{ v: usual[k("cc/good")]!.avg, label: "usual" }] : []}
            />
          </Card>
        )}
        <Card title="Voice frames lost">
          <TimeSeries lines={[{ label: "Lost, %", color: "var(--serious)", data: ratio(hist?.[k("voice/bad")], hist?.[k("voice/frames")], 100) }]} zero fmt={(v) => v.toFixed(1)} empty="No voice decoded yet." />
        </Card>
        {x.kind !== "conventional" && (
          <Card title="Signal and demodulation">
            <TimeSeries lines={[line(k("cc/snr"), "SNR, dB", "var(--series-1)"), x.kind === "p25" && (hist?.[k("cc/sep")] == null || st?.modulation === "CQPSK") ? line(k("cc/phaseErr"), "Phase error, °", "var(--series-3)") : line(k("cc/sep"), "Eye opening", "var(--series-3)")]} />
          </Card>
        )}
        {x.kind !== "conventional" && (
          <Card title="Carrier offset">
            <TimeSeries lines={[line(k("cc/offset"), "Hz from nominal", "var(--series-2)", { band: true })]} fmt={(v) => v.toFixed(0)} />
          </Card>
        )}
      </div>
      <Card title={`Frequencies, ${range === "7d" ? "7 days" : "24 h"}`}>
        {hints.map((t, i) => (
          <p key={i} className="why why-warn">
            {t}
          </p>
        ))}
        {rows.length === 0 ? (
          <p className="empty">No recorded calls yet.</p>
        ) : (
          <div className="table-wrap">
            <table className="calls">
              <thead>
                <tr>
                  <th>MHz</th>
                  <th>Source</th>
                  <th title="0% at the edge of the source's usable band (its guard left out), 100% at the centre">In band</th>
                  <th>Calls</th>
                  <th>SNR</th>
                  <th>Offset</th>
                  <th title="Voice frames decoded cleanly">Clean</th>
                  <th title="Voice frames lost">Lost</th>
                  <th>By hour</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => {
                  const p = placeOn(s, r.freqHz);
                  return (
                    <tr key={r.freqHz}>
                      <td className="mono">{formatMhz(r.freqHz)}</td>
                      <td className="small">{p?.src.label ?? "—"}</td>
                      <td className="mono">{p ? `${Math.round(p.depth * 100)}%` : "—"}</td>
                      <td className="mono">{r.calls}</td>
                      <td className="mono">{r.snr !== null ? `${num(r.snr, 0)} dB` : "—"}</td>
                      <td className="mono">{r.freqError !== null ? `${signed(r.freqError, 0)} Hz` : "—"}</td>
                      <td className="mono">{r.clean !== null ? `${num(r.clean, 0)}%` : "—"}</td>
                      <td className="mono">
                        <span className={`snr ${badLevel(r.badPct)}`} title={`${num(r.errPerFrame, 1)} bit errors fixed per frame`}>{r.badPct === null ? "—" : `${num(r.badPct, 1)}%`}</span>
                      </td>
                      <td className="spark-cell">
                        <HeatStrip v={r.hourly} ramp="hot" max={BAD_MAX} titles={r.hourly.map((v, i) => `${r.hourly.length - i} h ago: ${v === null ? "no calls" : `${v}% lost`}`)} />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>
      {st && <Neighbours s={s} systems={[st]} />}
      {st && <Patches systems={[st]} />}
      {st?.dmr && <DmrSites systems={[st]} />}
      {st?.nxdn && <NxdnSites systems={[st]} />}
    </div>
  );
}

export function DecodePage() {
  const s = useApp();
  const systems = dashSystems(s);
  const on = running(s);
  const usual = useUsual(systems.filter((x) => x.kind !== "conventional").map((x) => K.sys(x.name, "cc/good")));
  const names = systems.map((x) => x.name).join(",");
  const freqs = usePolled(`freqs:${names}:${on}`, () => Promise.all(systems.map((x) => radioQuery({ what: "freqs", system: x.name, hours: 24 }))), on ? 120_000 : 0);
  if (s.path[0]) return <SystemDetail s={s} name={s.path[0]} />;
  if (!systems.length) return <p className="empty">No systems set up yet.</p>;
  return (
    <div className="page">
      {!on && <p className="empty">Not recording — showing past data.</p>}
      <div className="card-grid wide">
        {systems.map((x) => (
          <DecodeCard key={x.name} s={s} x={x} usual={usual[K.sys(x.name, "cc/good")]} freqs={freqs?.find((f) => f.system === x.name)?.rows as FreqRow[] | undefined} />
        ))}
      </div>
    </div>
  );
}
