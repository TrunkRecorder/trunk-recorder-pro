// Numbers as the dashboard shows them.

/** 1,284 · 12.9K · 4.2M. */
export function compact(v: number | null | undefined, digits = 1): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toFixed(digits)}B`;
  if (a >= 1e6) return `${(v / 1e6).toFixed(digits)}M`;
  if (a >= 1e4) return `${(v / 1e3).toFixed(digits)}K`;
  if (a >= 100 || Number.isInteger(v)) return Math.round(v).toLocaleString();
  return v.toFixed(a >= 10 ? 1 : digits + 1).replace(/\.0+$/, "");
}

/** A number with `d` decimals, or "—". */
export function num(v: number | null | undefined, d = 1): string {
  return v === null || v === undefined || !Number.isFinite(v) ? "—" : v.toFixed(d);
}

export function pct(v: number | null | undefined, d = 0): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  // A small share keeps a decimal (0.3%, not 0%).
  return `${v.toFixed(d === 0 && Math.abs(v) < 10 && v !== 0 ? 1 : d)}%`;
}

/** 812 B · 3.4 MB · 1.21 GB. */
export function bytes(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let x = v;
  while (Math.abs(x) >= 1000 && i < u.length - 1) {
    x /= 1000;
    i++;
  }
  return `${x.toFixed(x >= 100 || i === 0 ? 0 : x >= 10 ? 1 : 2)} ${u[i]}`;
}

/** 45 s · 12 m · 3 h 20 m · 2 d 4 h. */
export function dur(s: number | null | undefined): string {
  if (s === null || s === undefined || !Number.isFinite(s)) return "—";
  s = Math.max(0, s);
  if (s < 60) return `${Math.round(s)} s`;
  if (s < 3600) return `${Math.round(s / 60)} m`;
  if (s < 86400) {
    const h = Math.floor(s / 3600);
    const m = Math.round((s % 3600) / 60);
    return m ? `${h} h ${m} m` : `${h} h`;
  }
  const d = Math.floor(s / 86400);
  const h = Math.round((s % 86400) / 3600);
  return h ? `${d} d ${h} h` : `${d} d`;
}

/** h:mm for an amount of airtime. */
export function hmm(s: number | null | undefined): string {
  if (s === null || s === undefined || !Number.isFinite(s)) return "—";
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return `${h}:${String(m).padStart(2, "0")}`;
}

/** "just now", "3 m ago", "2 h ago" from Unix seconds. */
export function ago(t: number | null | undefined, now = Date.now() / 1000): string {
  if (!t) return "never";
  const d = now - t;
  if (d < 10) return "just now";
  return `${dur(d)} ago`;
}

/** 851.0125 (MHz). */
export function mhz(hz: number | null | undefined, d = 4): string {
  return hz ? (hz / 1e6).toFixed(d) : "—";
}

/** +1.25 (a signed number). */
export function signed(v: number | null | undefined, d = 1): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  return `${v >= 0 ? "+" : "−"}${Math.abs(v).toFixed(d)}`;
}

/** A clock time from Unix seconds: 14:05 (with the day when not today). */
export function clockAt(t: number, withSeconds = false): string {
  const d = new Date(t * 1000);
  const today = new Date().toDateString() === d.toDateString();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", ...(withSeconds ? { second: "2-digit" } : {}) });
  return today ? time : `${d.toLocaleDateString([], { weekday: "short" })} ${time}`;
}

/** Round "nice" tick values covering [lo, hi]. */
export function ticks(lo: number, hi: number, n = 4): number[] {
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) return [];
  if (hi === lo) return [lo];
  const span = hi - lo;
  const step0 = span / n;
  const mag = 10 ** Math.floor(Math.log10(step0));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => span / s <= n) ?? 10 * mag;
  const out: number[] = [];
  for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step) out.push(Math.round(v / step) * step);
  return out;
}

/** A series key part: as the recorder makes it (crates/trunk-core/src/metrics.rs key_part). */
export function keyPart(s: string): string {
  return s.trim().replaceAll("/", "_");
}
