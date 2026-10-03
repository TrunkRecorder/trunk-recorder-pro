// The dashboard's chart parts: small, hand-drawn SVG (canvas for the big
// heatmap), coloured from the theme's tokens (styles.css) so they follow
// light and dark. Thin marks, recessive axes, a tooltip on hover.

import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { clockAt, ticks } from "./fmt.ts";
import { useSeries } from "./series.ts";
import type { SeriesData } from "./protocol.ts";

export type Level = "ok" | "warn" | "bad" | "idle";

/** A value's place between lo and hi, clamped to [0, 1]. */
const unit = (v: number, lo: number, hi: number) => (hi > lo ? Math.min(1, Math.max(0, (v - lo) / (hi - lo))) : 0.5);

// ── status ───────────────────────────────────────────────────────────────────

const LEVEL_TEXT: Record<Level, string> = { ok: "OK", warn: "Needs attention", bad: "Problem", idle: "Idle" };

/** A status light, with its meaning for screen readers and on hover. */
export function Light({ level, title }: { level: Level; title?: string }) {
  return <span className={`light light-${level}`} role="img" aria-label={title ?? LEVEL_TEXT[level]} title={title ?? LEVEL_TEXT[level]} />;
}

/** ▲ / ▼ and how much, coloured by whether that direction is good. */
export function Trend({ delta, goodUp = true, text, title }: { delta: number | null; goodUp?: boolean; text?: string; title?: string }) {
  if (delta === null || !Number.isFinite(delta)) return null;
  const flat = Math.abs(delta) < 0.05;
  const good = flat ? null : delta > 0 === goodUp;
  return (
    <span className={`trend ${flat ? "flat" : good ? "good" : "bad"}`} title={title}>
      {flat ? "▶" : delta > 0 ? "▲" : "▼"} {text ?? `${Math.round(Math.abs(delta) * 100)}%`}
    </span>
  );
}

// ── figures ──────────────────────────────────────────────────────────────────

/** A big number: label, value with its unit, a line under it, optionally a trend and a sparkline. */
export function Stat(props: {
  label: string;
  value: ReactNode;
  unit?: string;
  sub?: ReactNode;
  trend?: ReactNode;
  spark?: ReactNode;
  level?: Level;
  hint?: string;
  size?: "hero" | "kpi" | "small";
  onClick?: () => void;
}) {
  const Tag = props.onClick ? "button" : "div";
  return (
    <Tag className={`stat stat-${props.size ?? "kpi"}${props.level ? ` stat-${props.level}` : ""}${props.onClick ? " clickable" : ""}`} onClick={props.onClick} title={props.hint}>
      <div className="stat-label">
        {props.level && props.level !== "ok" && props.level !== "idle" ? <Light level={props.level} /> : null}
        {props.label}
      </div>
      <div className="stat-value">
        {props.value}
        {props.unit && <span className="stat-unit">{props.unit}</span>}
        {props.trend}
      </div>
      {props.spark && <div className="stat-spark">{props.spark}</div>}
      {props.sub && <div className="stat-sub">{props.sub}</div>}
    </Tag>
  );
}

/**
 * A bar from 0 to `max`, filled to `value`, coloured by `zones` ([warn,
 * bad] thresholds; `invert`: low is bad). The track shows the zones faintly.
 */
export function Meter(props: { value: number | null; max: number; zones?: [number, number]; invert?: boolean; label?: string; title?: string }) {
  const v = props.value ?? 0;
  const f = unit(v, 0, props.max);
  const z = props.zones;
  const level: Level = !z || props.value === null ? "ok" : props.invert ? (v <= z[1] ? "bad" : v <= z[0] ? "warn" : "ok") : v >= z[1] ? "bad" : v >= z[0] ? "warn" : "ok";
  return (
    <div className="meter" title={props.title} role="meter" aria-valuenow={props.value ?? undefined} aria-valuemin={0} aria-valuemax={props.max} aria-label={props.label}>
      <div className={`meter-fill fill-${level}`} style={{ width: `${(f * 100).toFixed(1)}%` }} />
      {z && <div className="meter-zone" style={{ left: `${unit(z[0], 0, props.max) * 100}%` }} />}
    </div>
  );
}

// ── lines ────────────────────────────────────────────────────────────────────

/** Points with gaps (null) as SVG path data. */
function pathOf(xs: number[], ys: (number | null)[]): string {
  let d = "";
  let pen = false;
  for (let i = 0; i < ys.length; i++) {
    const y = ys[i];
    if (y === null || !Number.isFinite(y)) {
      pen = false;
      continue;
    }
    d += `${pen ? "L" : "M"}${xs[i].toFixed(1)},${y.toFixed(1)}`;
    pen = true;
  }
  return d;
}

/**
 * A small line: no axes, the last value dotted. `band`: a range drawn
 * behind it (what's usual), `lo` / `hi`: the scale (defaults: the data's).
 */
export function Sparkline(props: {
  v: (number | null)[];
  w?: number;
  h?: number;
  color?: string;
  band?: [number, number] | null;
  lo?: number;
  hi?: number;
  /** The smallest span the scale shows (a 0.01 ppm wobble stays flat). */
  minSpan?: number;
  area?: boolean;
  title?: string;
}) {
  const w = props.w ?? 160;
  const h = props.h ?? 36;
  const vals = props.v.filter((x): x is number => x !== null && Number.isFinite(x));
  if (vals.length < 2) return <svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden="true" />;
  let lo = props.lo ?? Math.min(...vals, ...(props.band ? [props.band[0]] : []));
  let hi = props.hi ?? Math.max(...vals, ...(props.band ? [props.band[1]] : []));
  const span = Math.max(props.minSpan ?? 0, 1e-9);
  if (hi - lo < span) {
    const mid = (hi + lo) / 2;
    lo = props.lo ?? mid - span / 2;
    hi = props.hi ?? lo + span;
  }
  const pad = 3;
  const xs = props.v.map((_, i) => (i / Math.max(1, props.v.length - 1)) * (w - pad * 2) + pad);
  const y = (x: number) => h - pad - unit(x, lo, hi) * (h - pad * 2);
  const ys = props.v.map((x) => (x === null || !Number.isFinite(x) ? null : y(x)));
  const color = props.color ?? "var(--series-1)";
  const lastI = ys.findLastIndex((x) => x !== null);
  const line = pathOf(xs, ys);
  const first = ys.findIndex((x) => x !== null);
  const area = props.area !== false && first >= 0 ? `${line}L${xs[lastI].toFixed(1)},${h - pad}L${xs[first].toFixed(1)},${h - pad}Z` : "";
  return (
    <svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label={props.title ?? "trend"}>
      {props.title && <title>{props.title}</title>}
      {props.band && <rect className="spark-band" x={0} width={w} y={y(props.band[1])} height={Math.max(1, y(props.band[0]) - y(props.band[1]))} />}
      {area && <path d={area} fill={color} opacity={0.12} stroke="none" />}
      <path d={line} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
      {lastI >= 0 && <circle cx={xs[lastI]} cy={ys[lastI]!} r={3} fill={color} className="spark-dot" />}
    </svg>
  );
}

/** A sparkline of a live series (the last `sinceS` seconds), optionally scaled (`mul`: per second → per minute). */
export function LiveSpark(props: { k: string; sinceS?: number; mul?: number; color?: string; band?: [number, number] | null; lo?: number; hi?: number; minSpan?: number; h?: number; title?: string }) {
  const { v } = useSeries(props.k, props.sinceS ?? 600);
  const m = props.mul ?? 1;
  return <Sparkline v={m === 1 ? v : v.map((x) => x * m)} color={props.color} band={props.band} lo={props.lo} hi={props.hi} minSpan={props.minSpan} h={props.h} title={props.title} />;
}

export interface Line {
  label: string;
  color: string;
  /** From the history (`t0`, `stepS`), or live points (`t`). */
  data: SeriesData | { t: number[]; v: number[] } | null;
  /** Draw the lowest–highest band of each step (history only). */
  band?: boolean;
  dashed?: boolean;
  mul?: number;
}

/** (t, v) points of a line, scaled. */
function points(l: Line): { t: number[]; v: (number | null)[]; lo: (number | null)[]; hi: (number | null)[] } {
  const m = l.mul ?? 1;
  const s = (x: number | null) => (x === null ? null : x * m);
  if (!l.data) return { t: [], v: [], lo: [], hi: [] };
  if ("t0" in l.data) {
    const d = l.data;
    return { t: d.v.map((_, i) => d.t0 + i * d.stepS + d.stepS / 2), v: d.v.map(s), lo: d.lo.map(s), hi: d.hi.map(s) };
  }
  return { t: l.data.t, v: l.data.v.map(s), lo: [], hi: [] };
}

/**
 * A time chart: one y axis, a hairline grid, a crosshair and the values
 * under it on hover. `zero`: start the axis at 0. `fmt`: how values read.
 * `marks`: horizontal reference lines (a threshold, the usual).
 */
export function TimeSeries(props: { lines: Line[]; height?: number; fmt?: (v: number) => string; zero?: boolean; lo?: number; hi?: number; marks?: { v: number; label: string }[]; empty?: string }) {
  const H = props.height ?? 160;
  const W = 640;
  const L = 44;
  const R = 8;
  const T = 8;
  const B = 20;
  const fmt = props.fmt ?? ((v: number) => (Math.abs(v) >= 100 ? v.toFixed(0) : v.toFixed(1)));
  const pts = useMemo(() => props.lines.map(points), [props.lines]);
  const [hover, setHover] = useState<number | null>(null);
  const ref = useRef<SVGSVGElement>(null);
  const all = pts.flatMap((p) => [...p.v, ...p.lo, ...p.hi]).filter((x): x is number => x !== null && Number.isFinite(x));
  // The span with data (a week's chart of a new install shows its hours, not six empty days).
  const ts = pts.flatMap((p) => p.t.filter((_, i) => p.v[i] !== null));
  if (!all.length || ts.length < 1) return <div className="chart-empty" style={{ height: H }}>{props.empty ?? "Collecting — the chart fills in as data arrives."}</div>;
  let lo = props.lo ?? Math.min(...all, ...(props.marks ?? []).map((m) => m.v));
  let hi = props.hi ?? Math.max(...all, ...(props.marks ?? []).map((m) => m.v));
  if (props.zero) lo = Math.min(0, lo);
  if (hi === lo) {
    hi += 1;
    lo -= props.zero ? 0 : 1;
  }
  const yt = ticks(lo, hi, 4);
  lo = Math.min(lo, yt[0] ?? lo);
  hi = Math.max(hi, yt[yt.length - 1] ?? hi);
  let t0 = Math.min(...ts);
  let t1 = Math.max(...ts);
  if (t1 - t0 < 120) {
    // One or two points: a few minutes either side, so they sit in the middle.
    t0 -= 120;
    t1 += 120;
  }
  const x = (t: number) => L + unit(t, t0, t1) * (W - L - R);
  const y = (v: number) => T + (1 - unit(v, lo, hi)) * (H - T - B);
  const span = t1 - t0;
  const xt = [0, 0.25, 0.5, 0.75, 1].map((f) => t0 + f * span);
  const onMove = (e: React.MouseEvent) => {
    const r = ref.current?.getBoundingClientRect();
    if (!r) return;
    const fx = ((e.clientX - r.left) / r.width) * W;
    setHover(t0 + unit(fx, L, W - R) * span);
  };
  // The nearest point of each line to the hovered time.
  const near = hover === null ? null : pts.map((p) => {
    let best = -1;
    for (let i = 0; i < p.t.length; i++) if (p.v[i] !== null && (best < 0 || Math.abs(p.t[i] - hover) < Math.abs(p.t[best] - hover))) best = i;
    return best;
  });
  const ht = near && near[0] >= 0 ? pts[0].t[near[0]] : hover;
  return (
    <div className="ts">
      {props.lines.length > 1 && (
        <div className="legend">
          {props.lines.map((l) => (
            <span key={l.label}>
              <i style={{ background: l.color }} className={l.dashed ? "dashed" : ""} />
              {l.label}
            </span>
          ))}
        </div>
      )}
      <svg ref={ref} viewBox={`0 0 ${W} ${H}`} className="ts-svg" style={{ height: H }} preserveAspectRatio="none" onMouseMove={onMove} onMouseLeave={() => setHover(null)} role="img" aria-label={props.lines.map((l) => l.label).join(", ")}>
        {yt.map((v) => (
          <g key={v}>
            <line x1={L} x2={W - R} y1={y(v)} y2={y(v)} className="grid" vectorEffect="non-scaling-stroke" />
            <text x={L - 6} y={y(v) + 4} textAnchor="end" className="axis">
              {fmt(v)}
            </text>
          </g>
        ))}
        {xt.map((t, i) => (
          <text key={i} x={x(t)} y={H - 4} textAnchor={i === 0 ? "start" : i === 4 ? "end" : "middle"} className="axis">
            {clockAt(t)}
          </text>
        ))}
        {(props.marks ?? []).map((m) => (
          <g key={m.label}>
            <line x1={L} x2={W - R} y1={y(m.v)} y2={y(m.v)} className="mark-line" vectorEffect="non-scaling-stroke" />
            <text x={W - R - 2} y={y(m.v) - 4} textAnchor="end" className="axis">
              {m.label}
            </text>
          </g>
        ))}
        {pts.map((p, k) => {
          const l = props.lines[k];
          const xs = p.t.map(x);
          const bandD =
            l.band && p.lo.length
              ? (() => {
                  // A band per run of points (gaps break it).
                  let d = "";
                  let run: number[] = [];
                  const flush = () => {
                    if (run.length > 1) d += `M${run.map((i) => `${xs[i].toFixed(1)},${y(p.hi[i]!).toFixed(1)}`).join("L")}L${[...run].reverse().map((i) => `${xs[i].toFixed(1)},${y(p.lo[i]!).toFixed(1)}`).join("L")}Z`;
                    run = [];
                  };
                  for (let i = 0; i < p.t.length; i++) {
                    if (p.lo[i] === null || p.hi[i] === null) flush();
                    else run.push(i);
                  }
                  flush();
                  return d;
                })()
              : "";
          return (
            <g key={l.label}>
              {bandD && <path d={bandD} fill={l.color} opacity={0.12} />}
              <path d={pathOf(xs, p.v.map((v) => (v === null ? null : y(v))))} fill="none" stroke={l.color} strokeWidth={2} strokeDasharray={l.dashed ? "4 3" : undefined} strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
              {/* A point with no neighbour draws no line: a dot instead. */}
              {p.v.map((v, i) =>
                v !== null && (i === 0 || p.v[i - 1] === null) && (i === p.v.length - 1 || p.v[i + 1] === null) ? <circle key={i} cx={xs[i]} cy={y(v)} r={3} fill={l.color} /> : null,
              )}
            </g>
          );
        })}
        {ht !== null && (
          <line x1={x(ht)} x2={x(ht)} y1={T} y2={H - B} className="crosshair" vectorEffect="non-scaling-stroke" />
        )}
        {near &&
          near.map((i, k) =>
            i >= 0 && pts[k].v[i] !== null ? <circle key={k} cx={x(pts[k].t[i])} cy={y(pts[k].v[i]!)} r={4} fill={props.lines[k].color} className="ts-dot" /> : null,
          )}
      </svg>
      {near && ht !== null && (
        <div className="ts-tip" style={{ left: `${(unit(x(ht), 0, W) * 100).toFixed(1)}%` }}>
          <b>{clockAt(ht, span < 3600)}</b>
          {near.map((i, k) =>
            i >= 0 && pts[k].v[i] !== null ? (
              <div key={k}>
                <i style={{ background: props.lines[k].color }} />
                {props.lines[k].label}: {fmt(pts[k].v[i]!)}
                {pts[k].lo[i] != null && pts[k].hi[i] != null && pts[k].lo[i] !== pts[k].hi[i] ? (
                  <span className="muted">
                    {" "}
                    ({fmt(pts[k].lo[i]!)}–{fmt(pts[k].hi[i]!)})
                  </span>
                ) : null}
              </div>
            ) : null,
          )}
        </div>
      )}
    </div>
  );
}

// ── cells ────────────────────────────────────────────────────────────────────

/** Steps of the sequential ramps in styles.css (--seq-*, --hot-*). */
const STEPS = 6;

/** The ramp step for `v` in [0, max] (0 is the faintest that still shows). */
export function stepOf(v: number, max: number): number {
  if (!(v > 0) || !(max > 0)) return 0;
  return Math.min(STEPS - 1, 1 + Math.floor(unit(v, 0, max) * (STEPS - 1.0001)));
}

/**
 * One row of cells, each coloured by its value on a one-hue ramp ("seq":
 * activity, blue; "hot": trouble, orange→red). Null: nothing measured.
 */
export function HeatStrip(props: { v: (number | null)[]; max?: number; ramp?: "seq" | "hot"; titles?: string[]; h?: number; onCell?: (i: number) => void; label?: string }) {
  const vals = props.v.filter((x): x is number => x !== null);
  const max = props.max ?? Math.max(0, ...vals);
  const ramp = props.ramp ?? "seq";
  return (
    <div className="heatstrip" style={{ height: props.h ?? 14 }} role="img" aria-label={props.label ?? "activity"}>
      {props.v.map((x, i) => (
        <span
          key={i}
          className={x === null ? "cell empty" : `cell ${ramp}-${stepOf(x, max)}`}
          title={props.titles?.[i]}
          onClick={props.onCell ? () => props.onCell!(i) : undefined}
          style={props.onCell ? { cursor: "pointer" } : undefined}
        />
      ))}
    </div>
  );
}

/** A state over time, a cell each: ok / warn / bad / unknown (null). */
export function Ribbon(props: { cells: (Level | null)[]; titles?: string[]; h?: number; label?: string }) {
  return (
    <div className="ribbon" style={{ height: props.h ?? 12 }} role="img" aria-label={props.label ?? "history"}>
      {props.cells.map((c, i) => (
        <span key={i} className={`cell ${c ?? "none"}`} title={props.titles?.[i]} />
      ))}
    </div>
  );
}

/** Columns of stacked parts (`parts[i]`: each column's values, in `colors` order), with a 2px gap between parts. */
export function StackedBars(props: { parts: number[][]; colors: string[]; labels: string[]; h?: number; titles?: string[]; max?: number }) {
  const H = props.h ?? 60;
  const n = props.parts.length;
  const max = props.max ?? Math.max(1, ...props.parts.map((p) => p.reduce((a, b) => a + b, 0)));
  const W = Math.max(n * 6, 120);
  const bw = Math.min(24, (W / Math.max(1, n)) * 0.75);
  return (
    <div>
      <svg viewBox={`0 0 ${W} ${H}`} className="bars" preserveAspectRatio="none" style={{ height: H }} role="img" aria-label={props.labels.join(", ")}>
        <line x1={0} x2={W} y1={H - 0.5} y2={H - 0.5} className="baseline" vectorEffect="non-scaling-stroke" />
        {props.parts.map((p, i) => {
          let y0 = H;
          const cx = (i + 0.5) * (W / Math.max(1, n));
          return (
            <g key={i}>
              {props.titles && <title>{props.titles[i]}</title>}
              {p.map((v, k) => {
                if (!(v > 0)) return null;
                const hgt = (v / max) * (H - 2);
                y0 -= hgt;
                const r = <rect key={k} x={cx - bw / 2} y={y0} width={bw} height={Math.max(0.5, hgt - (y0 > 0.5 ? 1 : 0))} fill={props.colors[k]} rx={1} />;
                return r;
              })}
            </g>
          );
        })}
      </svg>
      <div className="legend small">
        {props.labels.map((l, k) => (
          <span key={l}>
            <i style={{ background: props.colors[k] }} />
            {l}
          </span>
        ))}
      </div>
    </div>
  );
}

/** Counts per bin, with the bins' labels under them. */
export function Histogram(props: { counts: number[]; labels: string[]; color?: string; h?: number; title?: string }) {
  const max = Math.max(1, ...props.counts);
  const total = props.counts.reduce((a, b) => a + b, 0);
  return (
    <div className="hist" role="img" aria-label={props.title ?? "histogram"}>
      <div className="hist-bars" style={{ height: props.h ?? 90 }}>
        {props.counts.map((c, i) => (
          <div key={i} className="hist-col" title={`${props.labels[i]}: ${c} (${total ? Math.round((100 * c) / total) : 0}%)`}>
            <span className="hist-n">{c ? c : ""}</span>
            <span className="hist-bar" style={{ height: `${(c / max) * 100}%`, background: props.color ?? "var(--series-1)" }} />
          </div>
        ))}
      </div>
      <div className="hist-labels">
        {props.labels.map((l, i) => (
          <span key={i}>{l}</span>
        ))}
      </div>
    </div>
  );
}

// ── a heatmap (canvas: up to 40 × 168 cells) ─────────────────────────────────

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
}

/**
 * Rows × columns of counts, a cell each, on the activity ramp. Hover says
 * the row, the column and the value; a click picks the row.
 */
export function Heatmap(props: { rows: { label: string; sub?: string; v: number[] }[]; colLabel: (i: number) => string; valueText: (v: number) => string; onRow?: (i: number) => void; rowH?: number }) {
  const rowH = props.rowH ?? 16;
  const cols = props.rows[0]?.v.length ?? 0;
  const canvas = useRef<HTMLCanvasElement>(null);
  const [tip, setTip] = useState<{ r: number; c: number; x: number; y: number } | null>(null);
  const max = Math.max(1, ...props.rows.flatMap((r) => r.v));
  useEffect(() => {
    const cv = canvas.current;
    if (!cv) return;
    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = cv.clientWidth;
      const h = props.rows.length * rowH;
      cv.width = Math.round(w * dpr);
      cv.height = Math.round(h * dpr);
      const g = cv.getContext("2d");
      if (!g) return;
      g.scale(dpr, dpr);
      const ramp = Array.from({ length: STEPS }, (_, i) => cssVar(`--seq-${i}`));
      const empty = cssVar("--cell-empty");
      const cw = w / Math.max(1, cols);
      props.rows.forEach((r, ri) => {
        r.v.forEach((v, ci) => {
          g.fillStyle = v > 0 ? ramp[stepOf(v, max)] : empty;
          g.fillRect(ci * cw + 0.5, ri * rowH + 1, Math.max(1, cw - 1), rowH - 2);
        });
      });
    };
    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(cv);
    const mq = matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", draw);
    return () => {
      ro.disconnect();
      mq.removeEventListener("change", draw);
    };
  }, [props.rows, rowH, cols, max]);
  const onMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const c = Math.floor(((e.clientX - r.left) / r.width) * cols);
    const ri = Math.floor((e.clientY - r.top) / rowH);
    if (ri >= 0 && ri < props.rows.length && c >= 0 && c < cols) setTip({ r: ri, c, x: e.clientX - r.left, y: e.clientY - r.top });
    else setTip(null);
  };
  return (
    <div className="heatmap">
      <div className="heatmap-labels">
        {props.rows.map((r, i) => (
          <button key={i} className="heatmap-row" style={{ height: rowH }} onClick={props.onRow ? () => props.onRow!(i) : undefined} title={r.sub ? `${r.label} — ${r.sub}` : r.label}>
            {r.label}
          </button>
        ))}
      </div>
      <div className="heatmap-plot">
        <canvas ref={canvas} style={{ width: "100%", height: props.rows.length * rowH }} onMouseMove={onMove} onMouseLeave={() => setTip(null)} onClick={() => tip && props.onRow?.(tip.r)} role="img" aria-label="activity by hour" />
        {tip && (
          <div className="tip" style={{ left: tip.x + 12, top: tip.y + 8 }}>
            <b>{props.rows[tip.r].label}</b>
            <div>{props.colLabel(tip.c)}</div>
            <div>{props.valueText(props.rows[tip.r].v[tip.c])}</div>
          </div>
        )}
      </div>
    </div>
  );
}

// ── who's connected with whom ────────────────────────────────────────────────

export interface Node {
  id: string;
  label: string;
  sub?: string;
  weight: number;
  color?: string;
}

/**
 * Two columns of nodes and weighted links between them (radios ↔
 * talkgroups). Hover a node: its links stand out, the rest fade.
 */
export function Bipartite(props: { left: Node[]; right: Node[]; edges: { a: string; b: string; w: number }[]; onClick?: (n: Node, side: "left" | "right") => void; leftTitle?: string; rightTitle?: string }) {
  const [hot, setHot] = useState<string | null>(null);
  const rowH = 22;
  const H = Math.max(props.left.length, props.right.length) * rowH + 8;
  const W = 640;
  const xl = 200;
  const xr = W - 200;
  const yl = (i: number) => 4 + rowH * i + rowH / 2 + ((Math.max(props.left.length, props.right.length) - props.left.length) * rowH) / 2;
  const yr = (i: number) => 4 + rowH * i + rowH / 2 + ((Math.max(props.left.length, props.right.length) - props.right.length) * rowH) / 2;
  const li = new Map(props.left.map((n, i) => [n.id, i]));
  const ri = new Map(props.right.map((n, i) => [n.id, i]));
  const maxW = Math.max(1, ...props.edges.map((e) => e.w));
  const linked = (id: string) => !hot || hot === id || props.edges.some((e) => (e.a === hot && e.b === id) || (e.b === hot && e.a === id));
  const gid = useId();
  return (
    <div className="bip">
      <div className="bip-titles small muted">
        <span>{props.leftTitle}</span>
        <span>{props.rightTitle}</span>
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: H }} role="img" aria-label="who talks on what">
        <defs>
          <linearGradient id={`${gid}g`} x1="0" x2="1">
            <stop offset="0" stopColor="var(--series-1)" />
            <stop offset="1" stopColor="var(--series-3)" />
          </linearGradient>
        </defs>
        {props.edges.map((e, k) => {
          const a = li.get(e.a);
          const b = ri.get(e.b);
          if (a === undefined || b === undefined) return null;
          const on = !hot || e.a === hot || e.b === hot;
          const y1 = yl(a);
          const y2 = yr(b);
          return (
            <path
              key={k}
              d={`M${xl},${y1} C${(xl + xr) / 2},${y1} ${(xl + xr) / 2},${y2} ${xr},${y2}`}
              fill="none"
              stroke={`url(#${gid}g)`}
              strokeWidth={1 + (e.w / maxW) * 6}
              opacity={on ? 0.55 : 0.06}
            >
              <title>{`${props.left[a].label} ↔ ${props.right[b].label}: ${e.w}`}</title>
            </path>
          );
        })}
        {props.left.map((n, i) => (
          <g key={n.id} className={`bip-node${linked(n.id) ? "" : " faded"}`} onMouseEnter={() => setHot(n.id)} onMouseLeave={() => setHot(null)} onClick={() => props.onClick?.(n, "left")}>
            <rect x={0} y={yl(i) - rowH / 2 + 2} width={xl - 8} height={rowH - 4} rx={4} className="bip-hit" />
            <text x={xl - 14} y={yl(i) + 4} textAnchor="end" className="bip-label">
              {n.label}
            </text>
            <circle cx={xl - 4} cy={yl(i)} r={4 + Math.min(4, Math.sqrt(n.weight) / 2)} fill={n.color ?? "var(--series-1)"} className="ring" />
          </g>
        ))}
        {props.right.map((n, i) => (
          <g key={n.id} className={`bip-node${linked(n.id) ? "" : " faded"}`} onMouseEnter={() => setHot(n.id)} onMouseLeave={() => setHot(null)} onClick={() => props.onClick?.(n, "right")}>
            <rect x={xr + 8} y={yr(i) - rowH / 2 + 2} width={W - xr - 8} height={rowH - 4} rx={4} className="bip-hit" />
            <circle cx={xr + 4} cy={yr(i)} r={4 + Math.min(4, Math.sqrt(n.weight) / 2)} fill={n.color ?? "var(--series-3)"} className="ring" />
            <text x={xr + 14} y={yr(i) + 4} className="bip-label">
              {n.label}
            </text>
          </g>
        ))}
      </svg>
    </div>
  );
}

/**
 * One radio at the centre, the talkgroups it uses on the inner ring and
 * the radios it's heard with on the outer, each sized by how often.
 */
export function RadialEgo(props: { center: { label: string; sub?: string }; inner: Node[]; outer: Node[]; onClick?: (n: Node, ring: "inner" | "outer") => void }) {
  const [hot, setHot] = useState<string | null>(null);
  const S = 420;
  const c = S / 2;
  const r1 = 95;
  const r2 = 175;
  const place = (ns: Node[], r: number, off: number) =>
    ns.map((n, i) => {
      const a = off + (i / Math.max(1, ns.length)) * Math.PI * 2 - Math.PI / 2;
      return { n, x: c + r * Math.cos(a), y: c + r * Math.sin(a), a };
    });
  const inner = place(props.inner, r1, 0);
  const outer = place(props.outer, r2, Math.PI / Math.max(2, props.outer.length));
  const maxI = Math.max(1, ...props.inner.map((n) => n.weight));
  const maxO = Math.max(1, ...props.outer.map((n) => n.weight));
  const node = (p: { n: Node; x: number; y: number; a: number }, ring: "inner" | "outer", max: number) => {
    const rad = 5 + (p.n.weight / max) * (ring === "inner" ? 11 : 8);
    const right = Math.cos(p.a) >= 0;
    return (
      <g key={`${ring}${p.n.id}`} className={`ego-node${hot && hot !== p.n.id ? " faded" : ""}`} onMouseEnter={() => setHot(p.n.id)} onMouseLeave={() => setHot(null)} onClick={() => props.onClick?.(p.n, ring)}>
        <title>{`${p.n.label}${p.n.sub ? ` — ${p.n.sub}` : ""}: ${p.n.weight}`}</title>
        <line x1={c} y1={c} x2={p.x} y2={p.y} className="ego-link" strokeWidth={1 + (p.n.weight / max) * 4} opacity={hot === p.n.id ? 0.9 : 0.35} stroke={ring === "inner" ? "var(--series-1)" : "var(--series-3)"} />
        <circle cx={p.x} cy={p.y} r={rad} fill={ring === "inner" ? "var(--series-1)" : "var(--series-3)"} className="ring" />
        <text x={p.x + (right ? rad + 4 : -rad - 4)} y={p.y + 4} textAnchor={right ? "start" : "end"} className="ego-label">
          {p.n.label}
        </text>
      </g>
    );
  };
  return (
    <svg viewBox={`-90 0 ${S + 180} ${S}`} className="ego" role="img" aria-label={`${props.center.label}: talkgroups and radios`}>
      <circle cx={c} cy={c} r={r1} className="ego-orbit" />
      <circle cx={c} cy={c} r={r2} className="ego-orbit" />
      {outer.map((p) => node(p, "outer", maxO))}
      {inner.map((p) => node(p, "inner", maxI))}
      <circle cx={c} cy={c} r={30} className="ego-center" />
      <text x={c} y={c + 2} textAnchor="middle" className="ego-center-label">
        {props.center.label}
      </text>
      {props.center.sub && (
        <text x={c} y={c + 16} textAnchor="middle" className="ego-center-sub">
          {props.center.sub}
        </text>
      )}
    </svg>
  );
}

// ── layout ───────────────────────────────────────────────────────────────────

/**
 * A card: title, a status light and why, actions, and a body. `open` /
 * `onToggle`: a part that expands for detail (remembered per `id`).
 */
export function Card(props: { title: ReactNode; level?: Level; why?: string; actions?: ReactNode; children?: ReactNode; className?: string; onTitle?: () => void; detail?: ReactNode; id?: string }) {
  const key = props.id ? `trp.card.${props.id}` : null;
  const [open, setOpen] = useState(() => {
    try {
      return key ? localStorage.getItem(key) === "1" : false;
    } catch {
      return false;
    }
  });
  const toggle = () => {
    setOpen(!open);
    try {
      if (key) localStorage.setItem(key, open ? "0" : "1");
    } catch {
      // Not remembered.
    }
  };
  return (
    <section className={`card${props.level ? ` card-${props.level}` : ""}${props.className ? ` ${props.className}` : ""}`}>
      <header className="card-head">
        <div className="card-title">
          {props.level && props.level !== "idle" && <Light level={props.level} title={props.why} />}
          {props.onTitle ? (
            <button className="linkish" onClick={props.onTitle}>
              <h3>{props.title}</h3>
            </button>
          ) : (
            <h3>{props.title}</h3>
          )}
        </div>
        <div className="row">{props.actions}</div>
      </header>
      {props.why && props.level && props.level !== "ok" && props.level !== "idle" && <p className={`why why-${props.level}`}>{props.why}</p>}
      {props.children}
      {props.detail && (
        <>
          <button className="card-more" onClick={toggle} aria-expanded={open}>
            {open ? "Less ▴" : "More ▾"}
          </button>
          {open && <div className="card-detail">{props.detail}</div>}
        </>
      )}
    </section>
  );
}

/** A row of choices (a time span, a system). */
export function Choice<T extends string>(props: { value: T; options: { v: T; label: string }[]; onChange: (v: T) => void; label?: string }) {
  return (
    <div className="choice" role="radiogroup" aria-label={props.label}>
      {props.options.map((o) => (
        <button key={o.v} role="radio" aria-checked={props.value === o.v} className={props.value === o.v ? "on" : ""} onClick={() => props.onChange(o.v)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** A hint that grows intuition: what a figure means, what's normal. */
export function Hint({ children }: { children: ReactNode }) {
  return <p className="hint">{children}</p>;
}
