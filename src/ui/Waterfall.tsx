// Spectrum trace + waterfall of the whole source bandwidth, drawn from the
// channelizer's own FFT (no extra DSP). Markers: the control channel, and every
// active call colored by state.

import { useEffect, useRef } from "react";
import type { RadioStats, CallView } from "../worker/messages.ts";

const ROWS = 160;

function color(t: number): [number, number, number] {
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

export function Waterfall(props: { radio: RadioStats | null; ccHz: number | null; calls: CallView[] }) {
  const trace = useRef<HTMLCanvasElement>(null);
  const fall = useRef<HTMLCanvasElement>(null);
  const floor = useRef<number | null>(null);
  const { radio } = props;

  useEffect(() => {
    if (!radio || !fall.current || !trace.current) return;
    const spec = radio.spectrum;
    const n = spec.length;
    // Track the noise floor (median) so the colors follow the gain.
    const sorted = Float32Array.from(spec).sort();
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

  return (
    <div className="waterfall">
      <div className="wf-stack">
        <canvas ref={trace} className="wf-trace" aria-hidden="true" />
        <canvas ref={fall} className="wf-fall" aria-label="Waterfall of the source bandwidth" />
        <div className="wf-markers">
          {props.ccHz && inView(pos(props.ccHz)) && (
            <span className="mk mk-cc" style={{ left: `${pos(props.ccHz)}%` }} title="Control channel">
              <i>CC</i>
            </span>
          )}
          {props.calls
            .filter((c) => inView(pos(c.freqHz)))
            .map((c) => (
              <span key={c.id} className={`mk mk-${c.state}${c.encrypted ? " mk-enc" : ""}`} style={{ left: `${pos(c.freqHz)}%` }} title={`TG ${c.talkgroup}`}>
                <i>{c.alphaTag || c.talkgroup}</i>
              </span>
            ))}
        </div>
      </div>
      {radio && (
        <div className="wf-axis mono">
          <span>{((radio.centerHz - radio.rateHz / 2) / 1e6).toFixed(3)}</span>
          <span>{(radio.centerHz / 1e6).toFixed(3)} MHz</span>
          <span>{((radio.centerHz + radio.rateHz / 2) / 1e6).toFixed(3)}</span>
        </div>
      )}
    </div>
  );
}
