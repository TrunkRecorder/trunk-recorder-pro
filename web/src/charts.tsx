// Time charts (uPlot), coloured from the theme, sized to their container.

import { useEffect, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { useTheme } from "./theme.ts";

export interface ChartSeries {
  label: string;
  /** A CSS colour or a theme variable ("--sys-0"). */
  color: string;
  /** The right-hand axis (its own scale). */
  right?: boolean;
  /** Held until the next value (counts), not a line between them. */
  stepped?: boolean;
  /** A bar per value (counts per hour). */
  bars?: boolean;
  format?: (v: number) => string;
}

/** `color` (#rgb or #rrggbb) at opacity `a`. */
function alpha(color: string, a: number): string {
  const h = color.replace("#", "");
  const n =
    h.length === 3
      ? h
          .split("")
          .map((c) => c + c)
          .join("")
      : h;
  if (!/^[0-9a-f]{6}$/i.test(n)) return color;
  const v = parseInt(n, 16);
  return `rgba(${v >> 16}, ${(v >> 8) & 255}, ${v & 255}, ${a})`;
}

const css = (v: string) => (v.startsWith("--") ? getComputedStyle(document.documentElement).getPropertyValue(v).trim() || "#888" : v);

/** `x`: unix seconds; `ys`: one array per series (null: no value); `pad`: seconds of room either side (half a bar). */
export function TimeChart(props: {
  x: number[];
  ys: (number | null)[][];
  series: ChartSeries[];
  height?: number;
  leftLabel?: string;
  rightLabel?: string;
  pad?: number;
}) {
  const box = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const { x, ys, series } = props;
  const height = props.height ?? 200;
  // Colours are read when the chart is made: a new theme makes it again.
  const { epoch } = useTheme();

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const muted = css("--muted");
    const grid = css("--line");
    const axis = (side: 1 | 3, label: string | undefined, scale: string, fmt?: (v: number) => string): uPlot.Axis => ({
      side,
      scale,
      label,
      stroke: muted,
      grid: { stroke: grid, width: 1, show: side === 3 },
      ticks: { stroke: grid, width: 1 },
      size: 52,
      values: fmt ? (_u, vals) => vals.map((v) => (v === null ? "" : fmt(v))) : undefined,
    });
    const rightFmt = series.find((s) => s.right)?.format;
    const leftFmt = series.find((s) => !s.right)?.format;
    const opts: uPlot.Options = {
      width: el.clientWidth || 600,
      height,
      cursor: { drag: { x: false, y: false } },
      legend: { show: true, live: true },
      scales: {
        x: { time: true, range: (_u, min, max) => [min - (props.pad ?? 0), max + (props.pad ?? 0)] },
        y: { range: (_u, _min, max) => [0, max > 0 ? max * 1.1 : 1] },
        r: { range: (_u, _min, max) => [0, max > 0 ? max * 1.1 : 1] },
      },
      axes: [
        { stroke: muted, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid, width: 1 } },
        axis(3, props.leftLabel, "y", leftFmt),
        ...(series.some((s) => s.right) ? [axis(1, props.rightLabel, "r", rightFmt)] : []),
      ],
      series: [
        {},
        ...series.map((s) => ({
          label: s.label,
          stroke: css(s.color),
          width: 1.5,
          scale: s.right ? "r" : "y",
          paths: s.bars ? uPlot.paths.bars!({ size: [0.6, 40] }) : s.stepped ? uPlot.paths.stepped!({ align: 1 }) : undefined,
          fill: s.bars ? alpha(css(s.color), 0.35) : undefined,
          points: s.bars ? { show: false } : undefined,
          value: s.format ? (_u: uPlot, v: number | null) => (v === null ? "—" : s.format!(v)) : undefined,
          spanGaps: false,
        })),
      ],
    };
    const u = new uPlot(opts, [x, ...ys] as uPlot.AlignedData, el);
    plot.current = u;
    const ro = new ResizeObserver(() => u.setSize({ width: el.clientWidth, height }));
    ro.observe(el);
    return () => {
      ro.disconnect();
      u.destroy();
      plot.current = null;
    };
    // The chart is rebuilt when what it shows changes shape; new data alone is set below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [series.map((s) => s.label + s.color + (s.right ?? "")).join("|"), height, props.leftLabel, props.rightLabel, props.pad, epoch]);

  useEffect(() => {
    plot.current?.setData([x, ...ys] as uPlot.AlignedData);
  }, [x, ys]);

  return <div className="chart" ref={box} />;
}
