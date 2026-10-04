// The last ten minutes of every series, as the `stats` and `host` messages
// bring them (one value a second, or every 2 s), in fixed rings outside
// React's state: charts read them directly, and are told at most once a frame.
// Longer spans come from the recorder's history (statsQuery).

import { useSyncExternalStore } from "react";

/** Points kept per series: ten minutes of seconds. */
const CAP = 600;

interface Ring {
  t: Float64Array;
  v: Float32Array;
  head: number;
  n: number;
}

const rings = new Map<string, Ring>();
let version = 0;
const listeners = new Set<() => void>();
let scheduled = false;

function notify(): void {
  if (scheduled) return;
  scheduled = true;
  const run = () => {
    scheduled = false;
    version++;
    for (const l of listeners) l();
  };
  if (typeof requestAnimationFrame === "function" && document.visibilityState === "visible") requestAnimationFrame(run);
  else setTimeout(run, 0);
}

/** Add one tick's values (Unix seconds `t`). */
export function feedSeries(t: number, values: Record<string, number>): void {
  for (const k in values) {
    let r = rings.get(k);
    if (!r) {
      r = { t: new Float64Array(CAP), v: new Float32Array(CAP), head: 0, n: 0 };
      rings.set(k, r);
    }
    r.t[r.head] = t;
    r.v[r.head] = values[k];
    r.head = (r.head + 1) % CAP;
    r.n = Math.min(CAP, r.n + 1);
  }
  notify();
}

/** A series' points since `sinceS` ago (Unix seconds `t`, values `v`), oldest first. */
export function seriesPoints(key: string, sinceS = CAP * 2): { t: number[]; v: number[] } {
  const r = rings.get(key);
  const t: number[] = [];
  const v: number[] = [];
  if (!r || !r.n) return { t, v };
  const newest = r.t[(r.head - 1 + CAP) % CAP];
  for (let i = 0; i < r.n; i++) {
    const j = (r.head - r.n + i + CAP) % CAP;
    if (newest - r.t[j] <= sinceS) {
      t.push(r.t[j]);
      v.push(r.v[j]);
    }
  }
  return { t, v };
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Re-render when new values arrive (at most once a frame); returns a counter. */
export function useSeriesTick(): number {
  return useSyncExternalStore(subscribe, () => version);
}

/** A series' recent points; re-renders as they come. */
export function useSeries(key: string, sinceS?: number): { t: number[]; v: number[] } {
  useSeriesTick();
  return seriesPoints(key, sinceS);
}
