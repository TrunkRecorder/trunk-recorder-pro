// A source's guard band: the part of each edge of its band left unused, where
// the radio's anti-alias filter rolls off. Setup can measure the roll-off
// (profileSource → sourceProfile): a moment of the radio's band as a waterfall
// and an averaged spectrum with its noise floor, the guard band shaded at
// each edge — dragged there, or set with the slider — and the channels it
// would leave out.

import { useEffect, useRef, useState } from "react";
import { activeSystems, DEFAULT_GUARD_HZ, enabledChannels, formatMhz, resolvedCenters, watchedChannels } from "./config.ts";
import { profileSource, updateConfig, useApp } from "./controller.ts";
import type { Config, Source, SourceProfile } from "./protocol.ts";
import { color as heat } from "./Waterfall.tsx";

type Measured = Extract<SourceProfile, { suggestedGuardHz: number }>;

const STEP_HZ = 5_000;
const snap = (hz: number, rate: number) => Math.max(0, Math.min(rate / 4, Math.round(hz / STEP_HZ) * STEP_HZ));
const khz = (hz: number) => `${Math.round(hz / 1000)} kHz`;

/** Every channel the config watches, with what it is. */
function watched(c: Config): { hz: number; label: string }[] {
  const out: { hz: number; label: string }[] = [];
  for (const x of activeSystems(c)) {
    for (const f of x.controlChannelsHz) out.push({ hz: f, label: `${x.shortName} control` });
    for (const f of watchedChannels(x)) out.push({ hz: f, label: `${x.shortName} ${x.type === "dmr" ? "DMR" : "NXDN"}` });
    for (const f of x.voiceChannelsHz) out.push({ hz: f, label: `${x.shortName} voice` });
  }
  for (const ch of enabledChannels(c)) out.push({ hz: ch.freqHz, label: ch.name || "conventional" });
  return out;
}

/**
 * Source `i`'s guard band, and measuring it. `fresh`: just added, so the
 * measuring is offered (it's optional); `onFreshDone` once taken up or turned down.
 */
export function GuardBand(props: { c: Config; i: number; fresh?: boolean; onFreshDone?: () => void }) {
  const { c, i } = props;
  const src = c.sources[i];
  const guard = src.guardHz ?? DEFAULT_GUARD_HZ;
  const [open, setOpen] = useState(false);
  const setGuard = (hz: number) =>
    updateConfig((x) => {
      x.sources[i].guardHz = hz;
    });
  const start = () => {
    setOpen(true);
    props.onFreshDone?.();
  };

  return (
    <div className="field wide">
      <span className="field-label">Guard band</span>
      <div className="row">
        <input
          type="number"
          className="guard-input"
          min={0}
          max={src.rateHz / 4 / 1000}
          step={5}
          value={Math.round(guard / 1000)}
          onChange={(e) => {
            const v = Number(e.target.value);
            if (Number.isFinite(v)) setGuard(snap(v * 1000, src.rateHz));
          }}
          aria-label="Guard band at each edge, kHz"
        />
        <span className="muted">kHz at each edge</span>
        <span className="spacer" />
        <button className="btn ghost small" onClick={() => (open ? setOpen(false) : start())}>
          {open ? "Close" : "Profile roll-off…"}
        </button>
      </div>
      <span className="field-hint">
        Unused at each edge, where the filter rolls off. Default {khz(DEFAULT_GUARD_HZ)}.
      </span>
      {props.fresh && !open && (
        <div className="banner small">
          <span>New radio — measure its edge roll-off?</span>
          <span className="row">
            <button className="btn small" onClick={start}>
              Profile roll-off
            </button>
            <button className="btn ghost small" onClick={() => props.onFreshDone?.()}>
              Skip
            </button>
          </span>
        </div>
      )}
      {open && <Profiler c={c} i={i} src={src} guard={guard} onApply={setGuard} />}
    </div>
  );
}

function Profiler(props: { c: Config; i: number; src: Source; guard: number; onApply: (hz: number) => void }) {
  const { c, i, src, guard } = props;
  const s = useApp();
  const center = resolvedCenters(c)[i];
  const [text, setText] = useState(center ? formatMhz(center) : "");
  const lookAt = src.type === "file" ? (center ?? 0) : Math.round(parseFloat(text) * 1e6) || 0;
  const busy = s.profiling === i;
  const profile = s.profile?.source === i ? s.profile : null;
  const result = profile && "suggestedGuardHz" in profile ? profile : null;
  const recording = s.phase === "running" || s.phase === "starting";
  const [draft, setDraft] = useState(guard);
  // A new measurement proposes its suggestion.
  useEffect(() => {
    if (result) setDraft(result.suggestedGuardHz);
  }, [result]);

  return (
    <div className="rolloff">
      <div className="row">
        {src.type !== "file" && (
          <label className="row">
            <span className="muted">Look at</span>
            <input className="mhz" inputMode="decimal" value={text} placeholder="MHz" onChange={(e) => setText(e.target.value)} aria-label="Frequency to look at, MHz" />
            <span className="muted">MHz</span>
          </label>
        )}
        <button className="btn small" disabled={busy || recording || !(lookAt > 0)} onClick={() => profileSource(i, lookAt)}>
          {result ? "Measure again" : "Measure"}
        </button>
        <span className="muted small">
          {recording ? "Stop recording first." : busy ? "Listening…" : null}
        </span>
      </div>
      {!result && !busy && !profile && (
        <p className="muted small">Best on a quiet frequency, antenna connected, gain as you'll record.</p>
      )}
      {profile && "error" in profile && <p className="bad small">{profile.error}</p>}
      {result && <Measurement c={c} i={i} r={result} draft={draft} setDraft={setDraft} guard={guard} onApply={props.onApply} />}
    </div>
  );
}

function Measurement(props: { c: Config; i: number; r: Measured; draft: number; setDraft: (hz: number) => void; guard: number; onApply: (hz: number) => void }) {
  const { c, i, r, draft, setDraft, guard } = props;
  const rate = r.rateHz;
  const plot = useRef<HTMLDivElement>(null);
  const fall = useRef<HTMLCanvasElement>(null);
  const pct = (hz: number) => `${(hz / rate) * 100}%`;
  const center = resolvedCenters(c)[i];
  // The channels this source would carry, and which the guard band would leave out.
  const near = center ? watched(c).filter((w) => Math.abs(w.hz - center) <= rate / 2) : [];
  const out = near.filter((w) => Math.abs(w.hz - center!) > rate / 2 - draft);
  // Ticks on the plot only when it shows the band as configured.
  const ticks = center && Math.abs(center - r.centerHz) < 1 ? near : [];

  // Waterfall: oldest row at the top, coloured from the floor up.
  useEffect(() => {
    const cv = fall.current;
    if (!cv || !r.rows.length) return;
    const w = r.rows[0].length;
    cv.width = w;
    cv.height = r.rows.length;
    const g = cv.getContext("2d")!;
    const img = g.createImageData(w, r.rows.length);
    const lo = r.referenceDb - 6;
    const hi = r.referenceDb + 30;
    r.rows.forEach((row, y) =>
      row.forEach((db, x) => {
        const [cr, cg, cb] = heat((db - lo) / (hi - lo));
        const k = (y * w + x) * 4;
        img.data[k] = cr;
        img.data[k + 1] = cg;
        img.data[k + 2] = cb;
        img.data[k + 3] = 255;
      }),
    );
    g.putImageData(img, 0, 0);
  }, [r]);

  // The spectrum's scale: from just under the lowest floor to the strongest signal (at most 40 dB over the floor).
  const n = r.spectrum.length;
  const lo = Math.min(...r.floor) - 2;
  const hi = Math.min(Math.max(...r.spectrum), r.referenceDb + 40) + 2;
  const y = (db: number) => 100 - ((Math.max(lo, Math.min(hi, db)) - lo) / (hi - lo)) * 100;
  const line = (v: number[]) => v.map((db, k) => `${((k + 0.5) / n) * 1000},${y(db).toFixed(1)}`).join(" ");

  // Drag anywhere on the plot: the guard band follows the pointer from the nearer edge.
  const drag = (e: React.PointerEvent) => {
    if (!(e.buttons & 1) || !plot.current) return;
    const b = plot.current.getBoundingClientRect();
    const f = Math.max(0, Math.min(1, (e.clientX - b.left) / b.width));
    setDraft(snap(rate * Math.min(f, 1 - f), rate));
  };

  return (
    <>
      <div
        className="rolloff-plot"
        ref={plot}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          drag(e);
        }}
        onPointerMove={drag}
        role="img"
        aria-label={`Measured band ${formatMhz(r.centerHz - rate / 2, 3)} to ${formatMhz(r.centerHz + rate / 2, 3)} MHz with a guard band of ${khz(draft)} at each edge`}
      >
        <canvas ref={fall} className="rolloff-fall" />
        <svg viewBox="0 0 1000 100" preserveAspectRatio="none" className="rolloff-trace">
          <line x1={0} x2={1000} y1={y(r.referenceDb)} y2={y(r.referenceDb)} className="ref" />
          <line x1={0} x2={1000} y1={y(r.referenceDb - 3)} y2={y(r.referenceDb - 3)} className="ref3" />
          <polyline points={line(r.spectrum)} className="spec" />
          <polyline points={line(r.floor)} className="floor" />
        </svg>
        <div className="guard lo" style={{ width: pct(draft) }} />
        <div className="guard hi" style={{ width: pct(draft) }} />
        <div className="sugg" style={{ left: pct(r.suggestedGuardHz) }} title={`suggested: ${khz(r.suggestedGuardHz)}`} />
        <div className="sugg" style={{ right: pct(r.suggestedGuardHz) }} title={`suggested: ${khz(r.suggestedGuardHz)}`} />
        {ticks.map((t, k) => {
          const off = t.hz - r.centerHz;
          const cut = Math.abs(off) > rate / 2 - draft;
          return <span key={k} className={`tick${cut ? " cut" : ""}`} style={{ left: pct(off + rate / 2) }} title={`${t.label} ${formatMhz(t.hz)} MHz${cut ? " — in the guard band" : ""}`} />;
        })}
      </div>
      <div className="rolloff-axis mono small">
        <span>{formatMhz(r.centerHz - rate / 2, 3)}</span>
        <span>{formatMhz(r.centerHz, 3)} MHz</span>
        <span>{formatMhz(r.centerHz + rate / 2, 3)}</span>
      </div>
      <div className="row rolloff-controls">
        <input type="range" min={0} max={rate / 4} step={STEP_HZ} value={draft} onChange={(e) => setDraft(Number(e.target.value))} aria-label="Guard band at each edge" />
        <span className="mono">{khz(draft)}</span>
        <button className="btn ghost small" disabled={draft === r.suggestedGuardHz} onClick={() => setDraft(r.suggestedGuardHz)}>
          Suggested ({khz(r.suggestedGuardHz)})
        </button>
        <span className="spacer" />
        <button className="btn small" disabled={draft === guard} onClick={() => props.onApply(draft)}>
          {draft === guard ? "In use" : `Use ${khz(draft)}`}
        </button>
      </div>
      <p className="muted small">
        Noise floor (blue) is 3 dB down <b>{khz(r.lowHz)}</b> / <b>{khz(r.highHz)}</b> from the low / high edge, {r.lowDropDb.toFixed(1)} /{" "}
        {r.highDropDb.toFixed(1)} dB at the edges. Suggested: the larger plus half a channel.
      </p>
      {out.length > 0 && (
        <p className="warn small">
          {out.length === 1 ? "A channel" : `${out.length} channels`} in the guard band: {out.map((w) => `${w.label} ${formatMhz(w.hz)}`).join(", ")} MHz.
        </p>
      )}
    </>
  );
}
