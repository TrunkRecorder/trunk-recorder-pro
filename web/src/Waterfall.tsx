// Spectrum trace + waterfall of one source's whole bandwidth, drawn from the
// channelizer's own FFT (no extra DSP). Markers: each system's control channel,
// named along the top (stacked where names would overlap), an adjacent site's
// as a faint line (its address on hover), and every active call colored by
// state on a row of its own, its tag fading after a few seconds (with several
// systems, tagged with its system's color). The pointer reads the frequency
// under it, snapped to a signal's peak when one is near: on the waterfall's
// history too, for a conventional channel that's already gone quiet; and what
// the line under it is.

import { useEffect, useRef, useState } from "react";
import { Clip, SystemDot, useWidth } from "./charts.tsx";
import type { CallView, Spectrum } from "./protocol.ts";

/** A control channel marker. */
export interface CcMark {
  hz: number;
  label: string;
  color: string;
  /** An adjacent site's, not one followed here: drawn fainter, its label below. */
  faint?: boolean;
  /** What hovering it says ("Station 1 control channel", "Adjacent site … announced by …"). */
  title: string;
}

const ROWS = 160;
/** How near the pointer (bins either side) a signal's peak is snapped to. */
const SNAP_BINS = 6;
/** A peak this far over the floor, dB, is a signal to snap to. */
const SNAP_DB = 10;

/** What's under the pointer: where (0…1 across), the frequency, and when on a signal, how strong. */
export interface Readout {
  x: number;
  hz: number;
  overDb: number | null;
}

/**
 * The readout at `x` (0…1 across `spec`, a spectrum centred on `centerHz`, `rateHz` wide):
 * the strongest bin within [`SNAP_BINS`] when it's [`SNAP_DB`] over `floorDb`, else the
 * frequency under the pointer.
 */
export function readout(spec: ArrayLike<number>, centerHz: number, rateHz: number, floorDb: number, x: number): Readout {
  const n = spec.length;
  const at = (i: number) => centerHz + (i / (n - 1) - 0.5) * rateHz;
  const i = Math.round(Math.max(0, Math.min(1, x)) * (n - 1));
  let best = i;
  for (let j = Math.max(0, i - SNAP_BINS); j <= Math.min(n - 1, i + SNAP_BINS); j++) if (spec[j] > spec[best]) best = j;
  const over = spec[best] - floorDb;
  if (over >= SNAP_DB) return { x: best / (n - 1), hz: at(best), overDb: over };
  return { x, hz: centerHz + (x - 0.5) * rateHz, overDb: null };
}

/** Where a label on the trace goes: its row (0 the top of its band) and whether it's drawn left of its line. */
export interface Placed {
  row: number;
  left: boolean;
}

/** A label right of this, % across, is drawn left of its line, as the cursor's readout is. */
const FLIP_AT = 75;

/** About how wide a marker's label is, px: 11 px text, its padding and border. */
export const labelPx = (text: string): number => Math.ceil(text.length * 6.6) + 12;

/**
 * Rows for labels on lines `at` % across a trace `width` px wide, each `px` wide, so that none is printed over
 * another: left to right, each in the first of `rows` rows it fits in, `gap` px clear of the one before. One that
 * fits in none goes in the row that frees up first (overlapping least). Right of [`FLIP_AT`] a label is drawn left
 * of its line, so it stays on the trace.
 */
export function stackLabels(items: { at: number; px: number }[], width: number, rows: number, gap = 4): Placed[] {
  const spans = items.map((x, i) => {
    const line = (x.at / 100) * width;
    const left = x.at > FLIP_AT;
    // (Its label starts 3 px off its line: .mk i.)
    return { i, left, from: left ? line - 3 - x.px : line + 3, to: left ? line - 3 : line + 3 + x.px };
  });
  const ends: number[] = Array.from({ length: Math.max(1, rows) }, () => -Infinity);
  const out: Placed[] = items.map(() => ({ row: 0, left: false }));
  for (const sp of [...spans].sort((a, b) => a.from - b.from || a.i - b.i)) {
    let row = ends.findIndex((end) => end + gap <= sp.from);
    if (row < 0) row = ends.indexOf(Math.min(...ends));
    ends[row] = Math.max(ends[row], sp.to);
    out[sp.i] = { row, left: sp.left };
  }
  return out;
}

/** The marker nearest `x` (0…1 across a trace `width` px wide) among lines at `at` (% across), within `tolPx`; -1: none. */
export function nearestMark(at: number[], x: number, width: number, tolPx = 6): number {
  let best = -1;
  let bestPx = tolPx;
  at.forEach((a, i) => {
    const d = Math.abs((a / 100 - x) * width);
    if (d <= bestPx) [best, bestPx] = [i, d];
  });
  return best;
}

/** Site names' rows (the top of the trace), then calls' rows below them, px from the top; a row is 18 px. */
const SITE_TOP = 4;
const SITE_ROWS = 2;
const CALL_TOP = SITE_TOP + SITE_ROWS * 18 + 4;
const CALL_ROWS = 2;

/** The waterfall's colour for `t` (0 = floor … 1 = strong), RGB. */
export function color(t: number): [number, number, number] {
  // dark blue → cyan → yellow → white
  const c = Math.max(0, Math.min(1, t));
  const stops: [number, number, number][] = [
    [8, 12, 30],
    [20, 60, 140],
    [30, 170, 200],
    [240, 220, 80],
    [255, 255, 255],
  ];
  const x = c * (stops.length - 1);
  const i = Math.min(stops.length - 2, Math.floor(x));
  const f = x - i;
  const a = stops[i];
  const b = stops[i + 1];
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f];
}

/** `colorOf`: a system's colour, by short name (for calls' markers). `label`: the source, short; `labelTitle`: the whole of it, on hover. */
export function Waterfall(props: { radio: Spectrum | undefined; label?: string; labelTitle?: string; ccs: CcMark[]; calls: CallView[]; multi?: boolean; colorOf?: (shortName: string) => string }) {
  const trace = useRef<HTMLCanvasElement>(null);
  const fall = useRef<HTMLCanvasElement>(null);
  const floor = useRef<number | null>(null);
  const [hover, setHover] = useState<number | null>(null);
  // (Labels are stacked in the width they're shown at.)
  const [measure, width] = useWidth(800);
  const { radio } = props;

  useEffect(() => {
    if (!radio || !fall.current || !trace.current) return;
    const spec = radio.bins;
    const n = spec.length;
    // Track the noise floor (median) so the colors follow the gain.
    const sorted = Float32Array.from(spec).sort();
    if (!n) return;
    const med = sorted[Math.floor(n / 2)];
    floor.current = floor.current === null ? med : floor.current * 0.9 + med * 0.1;
    const lo = floor.current - 5;
    const hi = floor.current + 45;

    const f = fall.current;
    const g = f.getContext("2d")!;
    if (f.width !== n) {
      f.width = n;
      f.height = ROWS;
      g.fillStyle = "rgb(8, 12, 30)";
      g.fillRect(0, 0, n, ROWS);
    }
    g.drawImage(f, 0, 0, n, ROWS - 1, 0, 1, n, ROWS - 1);
    const row = g.createImageData(n, 1);
    for (let i = 0; i < n; i++) {
      const [r, gg, b] = color((spec[i] - lo) / (hi - lo));
      row.data[4 * i] = r;
      row.data[4 * i + 1] = gg;
      row.data[4 * i + 2] = b;
      row.data[4 * i + 3] = 255;
    }
    g.putImageData(row, 0, 0);

    const t = trace.current;
    const w = t.clientWidth * devicePixelRatio;
    const h = t.clientHeight * devicePixelRatio;
    if (t.width !== w || t.height !== h) {
      t.width = w;
      t.height = h;
    }
    const tg = t.getContext("2d")!;
    tg.clearRect(0, 0, w, h);
    const css = getComputedStyle(t);
    tg.strokeStyle = css.getPropertyValue("--trace").trim() || "#6cf";
    tg.lineWidth = devicePixelRatio;
    tg.beginPath();
    for (let i = 0; i < n; i++) {
      const x = (i / (n - 1)) * w;
      const y = h - ((spec[i] - lo) / (hi - lo)) * h * 0.9 - 2;
      if (i) tg.lineTo(x, y);
      else tg.moveTo(x, y);
    }
    tg.stroke();
  }, [radio]);

  const pos = (hz: number) => (radio ? ((hz - radio.centerHz) / radio.rateHz + 0.5) * 100 : -100);
  const inView = (p: number) => p >= 0 && p <= 100;
  const read = radio && hover !== null && radio.bins.length > 1 ? readout(radio.bins, radio.centerHz, radio.rateHz, floor.current ?? 0, hover) : null;

  // The followed sites' names along the top, stacked rather than printed over each other; an adjacent site's
  // line only (its address on hover); the calls on a row of their own below, fading once read.
  const sites = props.ccs.filter((m) => !m.faint && inView(pos(m.hz)));
  const adjacent = props.ccs.filter((m) => m.faint && inView(pos(m.hz)));
  const calls = props.calls.filter((c) => inView(pos(c.freqHz)));
  const callText = (c: CallView) => String(c.alphaTag || c.patched?.find((t) => t.alphaTag)?.alphaTag || c.talkgroup);
  const callTip = (c: CallView) => `${c.systemName} · TG ${c.talkgroup}${c.alphaTag ? ` (${c.alphaTag})` : ""}`;
  const sitePlaces = stackLabels(sites.map((m) => ({ at: pos(m.hz), px: labelPx(m.label) })), width, SITE_ROWS);
  const callPlaces = stackLabels(calls.map((c) => ({ at: pos(c.freqHz), px: labelPx(callText(c)) + (props.multi ? 12 : 0) })), width, CALL_ROWS);
  // What hovering a line says: the readout does (the markers don't take the pointer).
  const hovered = [...[...sites, ...adjacent].map((m) => ({ at: pos(m.hz), tip: m.title })), ...calls.map((c) => ({ at: pos(c.freqHz), tip: callTip(c) }))];
  const near = hover !== null ? nearestMark(hovered.map((x) => x.at), hover, width) : -1;
  const placed = (p: Placed, top: number) => ({ top: `${top + p.row * 18}px`, ...(p.left ? { left: "auto", right: "3px" } : {}) });

  return (
    <div className="waterfall">
      <div
        className="wf-stack"
        ref={measure}
        onPointerMove={(e) => {
          const b = e.currentTarget.getBoundingClientRect();
          setHover(b.width ? (e.clientX - b.left) / b.width : null);
        }}
        onPointerLeave={() => setHover(null)}
      >
        <canvas ref={trace} className="wf-trace" aria-hidden="true" />
        <canvas ref={fall} className="wf-fall" aria-label="Waterfall of the source bandwidth" />
        <div className="wf-markers">
          {sites.map((m, i) => (
            <span key={`${m.label}-${m.hz}`} className="mk mk-cc" style={{ left: `${pos(m.hz)}%`, borderLeftColor: m.color }}>
              <i style={{ color: m.color, ...placed(sitePlaces[i], SITE_TOP) }}>{m.label}</i>
            </span>
          ))}
          {adjacent.map((m) => (
            <span key={`${m.label}-${m.hz}`} className="mk mk-cc faint" style={{ left: `${pos(m.hz)}%`, borderLeftColor: m.color }} />
          ))}
          {calls.map((c, i) => (
            <span key={c.id} className={`mk mk-call mk-${c.state}${c.encrypted ? " mk-enc" : ""}`} style={{ left: `${pos(c.freqHz)}%` }}>
              <i style={placed(callPlaces[i], CALL_TOP)}>
                {props.multi && <SystemDot color={props.colorOf?.(c.systemName) ?? "var(--line)"} />}
                {callText(c)}
              </i>
            </span>
          ))}
        </div>
        {read && (
          <div className="wf-cursor" style={{ left: `${read.x * 100}%` }}>
            <i className={`mono${read.x > 0.75 ? " left" : ""}`}>
              {(read.hz / 1e6).toFixed(4)} MHz{read.overDb !== null && <span className="muted"> · +{read.overDb.toFixed(0)} dB</span>}
              {near >= 0 && <span className="wf-tip">{hovered[near].tip}</span>}
            </i>
          </div>
        )}
      </div>
      {radio && (
        <div className="wf-axis mono">
          <span>{((radio.centerHz - radio.rateHz / 2) / 1e6).toFixed(3)}</span>
          <span className="wf-axis-mid">
            {props.label ? (
              <>
                <Clip text={props.label} title={props.labelTitle} /> ·{" "}
              </>
            ) : (
              ""
            )}
            {(radio.centerHz / 1e6).toFixed(3)} MHz
          </span>
          <span>{((radio.centerHz + radio.rateHz / 2) / 1e6).toFixed(3)}</span>
        </div>
      )}
    </div>
  );
}
