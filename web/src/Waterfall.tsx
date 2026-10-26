// Spectrum trace + waterfall of one source's whole bandwidth, drawn from the
// channelizer's own FFT (no extra DSP). Markers: each system's control channel,
// and every active call colored by state (with several systems, tagged with
// its system's color). The pointer reads the frequency under it, snapped to
// a signal's peak when one is near: on the waterfall's history too, for a
// conventional channel that's already gone quiet.

import { useEffect, useRef, useState } from "react";
import { SystemDot } from "./charts.tsx";
import type { CallView, Spectrum } from "./protocol.ts";

/** A control channel marker. */
export interface CcMark {
  hz: number;
  label: string;
  color: string;
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

/** `colorOf`: a system's colour, by short name (for calls' markers). */
export function Waterfall(props: { radio: Spectrum | undefined; label?: string; ccs: CcMark[]; calls: CallView[]; multi?: boolean; colorOf?: (shortName: string) => string }) {
  const trace = useRef<HTMLCanvasElement>(null);
  const fall = useRef<HTMLCanvasElement>(null);
  const floor = useRef<number | null>(null);
  const [hover, setHover] = useState<number | null>(null);
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

  return (
    <div className="waterfall">
      <div
        className="wf-stack"
        onPointerMove={(e) => {
          const b = e.currentTarget.getBoundingClientRect();
          setHover(b.width ? (e.clientX - b.left) / b.width : null);
        }}
        onPointerLeave={() => setHover(null)}
      >
        <canvas ref={trace} className="wf-trace" aria-hidden="true" />
        <canvas ref={fall} className="wf-fall" aria-label="Waterfall of the source bandwidth" />
        <div className="wf-markers">
          {props.ccs
            .filter((m) => inView(pos(m.hz)))
            .map((m) => (
              <span key={`${m.label}-${m.hz}`} className="mk mk-cc" style={{ left: `${pos(m.hz)}%`, borderLeftColor: m.color }} title={`Control channel ${m.label}`}>
                <i style={{ color: m.color }}>{m.label}</i>
              </span>
            ))}
          {props.calls
            .filter((c) => inView(pos(c.freqHz)))
            .map((c) => (
              <span key={c.id} className={`mk mk-${c.state}${c.encrypted ? " mk-enc" : ""}`} style={{ left: `${pos(c.freqHz)}%` }} title={`${c.systemName} · TG ${c.talkgroup}`}>
                <i>
                  {props.multi && <SystemDot color={props.colorOf?.(c.systemName) ?? "var(--line)"} />}
                  {c.alphaTag || c.patched?.find((t) => t.alphaTag)?.alphaTag || c.talkgroup}
                </i>
              </span>
            ))}
        </div>
        {read && (
          <div className="wf-cursor" style={{ left: `${read.x * 100}%` }}>
            <i className={`mono${read.x > 0.75 ? " left" : ""}`}>
              {(read.hz / 1e6).toFixed(4)} MHz{read.overDb !== null && <span className="muted"> · +{read.overDb.toFixed(0)} dB</span>}
            </i>
          </div>
        )}
      </div>
      {radio && (
        <div className="wf-axis mono">
          <span>{((radio.centerHz - radio.rateHz / 2) / 1e6).toFixed(3)}</span>
          <span>
            {props.label ? `${props.label} · ` : ""}
            {(radio.centerHz / 1e6).toFixed(3)} MHz
          </span>
          <span>{((radio.centerHz + radio.rateHz / 2) / 1e6).toFixed(3)}</span>
        </div>
      )}
    </div>
  );
}
