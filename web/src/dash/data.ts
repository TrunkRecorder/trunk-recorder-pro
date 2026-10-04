// What the dashboard pages share: the series' names, the systems and
// sources as one list, the history and "the usual" for a series, and the
// health rules — plain functions returning a level and why, the why worded as
// a hint (what it means, what to try).

import { activeSystems } from "../config.ts";
import { snapshot, statsQuery, usePolled, type AppState, type StatsRange } from "../controller.ts";
import { keyPart, num, pct } from "../fmt.ts";
import { seriesPoints } from "../series.ts";
import type { Level } from "../charts.tsx";
import type { PluginInfo, SeriesData, SourceStatus, SystemStatus } from "../protocol.ts";

// ── series names (crates/trunk-app/src/stats) ────────────────────────────────

export const K = {
  sys: (short: string, k: string) => `sys/${keyPart(short)}/${k}`,
  src: (label: string, k: string) => `src/${keyPart(label)}/${k}`,
  plg: (id: string, k: string) => `plg/${keyPart(id)}/${k}`,
};

/** A value now, from the latest stats / host message. */
export function val(s: AppState, key: string): number | null {
  const v = s.stats?.values[key] ?? s.host?.values[key];
  return v === undefined ? null : v;
}

/** The average of a live series over the last `sinceS` seconds. */
export function recentAvg(key: string, sinceS: number): number | null {
  const { v } = seriesPoints(key, sinceS);
  return v.length ? v.reduce((a, b) => a + b, 0) / v.length : null;
}

// ── systems and sources ──────────────────────────────────────────────────────

export interface DashSystem {
  name: string;
  kind: "p25" | "smartnet" | "dmr" | "conventional";
  /** The running system's status (trunked, once its control channel is open). */
  status: SystemStatus | null;
}

/** Every system set up (trunked, then conventional with channels), with its status while running. */
export function dashSystems(s: AppState): DashSystem[] {
  if (!s.config) return [];
  const out: DashSystem[] = activeSystems(s.config).map((x) => ({
    name: x.shortName,
    kind: (x.type ?? "p25") as DashSystem["kind"],
    status: s.status?.systems.find((y) => y.shortName === x.shortName) ?? null,
  }));
  for (const c of s.config.conventional) if (c.enabled !== false && c.channels.some((ch) => ch.enabled)) out.push({ name: c.shortName, kind: "conventional", status: null });
  return out;
}

export const KIND_LABEL: Record<DashSystem["kind"], string> = { p25: "P25", smartnet: "SmartNet", dmr: "DMR", conventional: "Conventional" };

export const running = (s: AppState) => s.phase === "running" || s.phase === "starting";

// ── history ──────────────────────────────────────────────────────────────────

/** `series` over `range`, asked again while shown (every minute for an hour's span, else every 5). */
export function useHistory(series: string[], range: StatsRange, points?: number): Record<string, SeriesData> | null {
  const key = `${series.join("|")}@${range}@${points ?? ""}`;
  const r = usePolled(key, () => statsQuery(series, range, points), range === "10m" ? 20_000 : range === "1h" ? 60_000 : 300_000);
  return r?.series ?? null;
}

/** The usual of each series: its average (and lowest and highest step) over the last day. */
export interface Usual {
  avg: number;
  lo: number;
  hi: number;
}

export function usualOf(d: SeriesData | undefined): Usual | null {
  if (!d) return null;
  const v = d.v.filter((x): x is number => x !== null);
  if (v.length < 6) return null;
  const sorted = [...v].sort((a, b) => a - b);
  return { avg: v.reduce((a, b) => a + b, 0) / v.length, lo: sorted[Math.floor(sorted.length * 0.1)], hi: sorted[Math.floor(sorted.length * 0.9)] };
}

/** "The usual" for these series (last 24 h, refreshed every 10 minutes). */
export function useUsual(series: string[]): Record<string, Usual | null> {
  const key = series.join("|");
  const r = usePolled(`usual:${key}`, () => statsQuery(series, "24h", 96), 600_000);
  const out: Record<string, Usual | null> = {};
  for (const k of series) out[k] = usualOf(r?.series[k]);
  return out;
}

/** The sum over the span of a per-second rate series: an amount (calls, bytes). */
export function total(d: SeriesData | undefined): number | null {
  if (!d) return null;
  let sum = 0;
  let any = false;
  d.v.forEach((v, i) => {
    if (v !== null) {
      sum += v * (d.n?.[i] ?? d.stepS / 60) * 60;
      any = true;
    }
  });
  return any ? sum : null;
}

/** A series' rate per second over a span of history (the minutes measured only); null with under 3 minutes. */
export function rateOver(d: SeriesData | undefined): number | null {
  const mins = d?.n?.reduce((a, b) => a + b, 0) ?? 0;
  const t = total(d);
  return t === null || mins < 3 ? null : t / (mins * 60);
}

/** Change of `now` against `usual`, as a fraction (+0.2: 20% up). */
export function change(now: number | null, usual: Usual | null | undefined): number | null {
  if (now === null || !usual || !(Math.abs(usual.avg) > 1e-9)) return null;
  return (now - usual.avg) / Math.abs(usual.avg);
}

// ── health ───────────────────────────────────────────────────────────────────

export interface Health {
  level: Level;
  why: string;
}

const OK = (why: string): Health => ({ level: "ok", why });
const worst = (hs: Health[]): Health => {
  const order: Level[] = ["bad", "warn", "ok", "idle"];
  for (const l of order) {
    const h = hs.find((x) => x.level === l);
    if (h) return h;
  }
  return { level: "idle", why: "" };
};

/** A trunked system's control channel, judged against its own usual rate. */
export function systemHealth(s: AppState, x: DashSystem, usualRate?: Usual | null): Health {
  if (!running(s)) return { level: "idle", why: "Not recording" };
  if (x.kind === "conventional") return OK("Listening on its channels");
  const st = x.status;
  if (!st) return { level: "bad", why: "No control channel open — is one inside a source's band?" };
  if (st.mismatch) return { level: "bad", why: `The control channel is another system's: ${st.mismatch}` };
  const locked = val(s, K.sys(x.name, "cc/locked"));
  const rate = recentAvg(K.sys(x.name, "cc/good"), 30);
  const bad = recentAvg(K.sys(x.name, "cc/bad"), 60) ?? 0;
  const good = recentAvg(K.sys(x.name, "cc/good"), 60) ?? 0;
  const badPct = good + bad > 0 ? (100 * bad) / (good + bad) : 0;
  if (locked === 0) return { level: "bad", why: "Not decoding its control channel — hunting for another. Check the antenna, the gain, and that the frequency is right." };
  if (rate === null) return { level: "ok", why: "Starting…" };
  if (usualRate && usualRate.avg > 5 && rate < usualRate.avg * 0.6)
    return { level: "warn", why: `Decoding ${num(rate)} msg/s, ${Math.round((1 - rate / usualRate.avg) * 100)}% under its usual ${num(usualRate.avg)}. Fewer messages with more lost ones points at reception, not traffic.` };
  if (badPct > 30) return { level: "bad", why: `${Math.round(badPct)}% of control messages lost — the signal is weak or distorted here.` };
  if (badPct > 10) return { level: "warn", why: `${Math.round(badPct)}% of control messages lost. A few percent is normal; more suggests multipath or a marginal signal.` };
  return OK(`${num(rate)} msg/s, ${Math.round(100 - badPct)}% decoded`);
}

/** A source: drops, clipping, frequency error, sample rate. */
export function sourceHealth(s: AppState, src: SourceStatus, autoTune: boolean): Health {
  if (!running(s)) return { level: "idle", why: "Not recording" };
  if (src.ended) return { level: "idle", why: "Ended" };
  const drops = recentAvg(K.src(src.label, "dropped"), 10) ?? 0;
  const clip = recentAvg(K.src(src.label, "clipPct"), 10) ?? 0;
  const peak = val(s, K.src(src.label, "peak"));
  if (drops > 0) return { level: "bad", why: "Dropping samples: the computer or USB bus can't keep up. Calls on it lose audio." };
  if (src.lastError && src.lastErrorS && Date.now() / 1000 - src.lastErrorS < 60) return { level: "bad", why: src.lastError };
  if (clip > 0.5) return { level: "warn", why: `${num(clip, 1)}% of samples at full scale: the gain is too high (or a strong signal nearby). Lower it a few dB.` };
  if (peak !== null && peak > -1) return { level: "warn", why: "Peaks reach full scale: little headroom left. Lower the gain a little." };
  if (src.errorPpm != null && !autoTune && Math.abs(src.errorPpm - (src.tunePpm ?? 0)) > 2)
    return { level: "warn", why: `Its frequency is off by ${num(src.errorPpm, 1)} ppm. Set the source's ppm, or turn on AutoTune.` };
  if (src.rateMeasured > 0 && Math.abs(src.rateMeasured / src.rateHz - 1) > 0.05) return { level: "warn", why: `Delivering ${num(src.rateMeasured / 1e6, 2)} MS/s of ${num(src.rateHz / 1e6, 2)}.` };
  return OK("Clean");
}

export function pluginHealth(p: PluginInfo, on: boolean): Health {
  if (!on) return { level: "idle", why: "Off" };
  if (p.problem) return { level: "bad", why: p.problem };
  const r = p.runtime;
  const down = r.metrics?.endpoints?.find((e) => e.state === "down");
  if (down) return { level: "bad", why: `${down.name} isn't answering${down.lastError ? `: ${down.lastError}` : ""}` };
  if (r.state === "error") return { level: "bad", why: r.message || "Stopped with an error" };
  if (r.state === "warning") return { level: "warn", why: r.message || "Needs attention" };
  if (r.state === "off") return { level: "idle", why: "Starts with recording" };
  const tries = r.ok + r.failed;
  if (tries >= 10 && r.failed / tries > 0.05) return { level: "warn", why: `${pct((100 * r.failed) / tries)} of calls failed` };
  return OK(r.message || "Working");
}

export function platformHealth(s: AppState): Health {
  const h = s.host;
  if (!h) return { level: "idle", why: "Waiting for the first sample" };
  const v = h.values;
  const hs: Health[] = [];
  for (const d of h.platform.disks) {
    const free = (100 * d.freeBytes) / Math.max(1, d.totalBytes);
    if (d.name === "spool") {
      // It fills when uploads fall behind; full, calls go to the disk.
      if (free < 10) hs.push({ level: "bad", why: `The RAM spool is ${pct(100 - free)} full: calls will soon go to the disk` });
      else if (free < 25) hs.push({ level: "warn", why: `The RAM spool is ${pct(100 - free)} full: uploads aren't keeping up` });
      continue;
    }
    if (free < 5) hs.push({ level: "bad", why: `The ${d.name} disk is ${pct(100 - free)} full` });
    else if (free < 10) hs.push({ level: "warn", why: `The ${d.name} disk is ${pct(100 - free)} full` });
  }
  if ((v["plat/memLevel"] ?? 0) >= 2 || (v["plat/mem"] ?? 0) > 97) hs.push({ level: "bad", why: "Memory is critically short" });
  else if ((v["plat/memLevel"] ?? 0) >= 1 || (v["plat/memPsi"] ?? 0) > 10 || (v["plat/mem"] ?? 0) > 90) hs.push({ level: "warn", why: "Memory is under pressure" });
  if ((v["plat/cpu"] ?? 0) > 90) hs.push({ level: "warn", why: `CPU at ${pct(v["plat/cpu"])}: decoding may fall behind` });
  if ((v["plat/piThrottled"] ?? 0) > 0) hs.push({ level: "warn", why: "The Pi is throttled (under-voltage or heat)" });
  if (v["net/up"] !== undefined && v["net/up"] < 1) hs.push({ level: v["net/up"] === 0 ? "bad" : "warn", why: v["net/up"] === 0 ? "The internet isn't answering" : "Some connectivity checks fail" });
  if ((v["plat/temp"] ?? 0) > 80) hs.push({ level: "warn", why: `Running hot: ${num(v["plat/temp"], 0)} °C` });
  return hs.length ? worst(hs) : OK("All normal");
}

export function rfHealth(s: AppState): Health {
  if (!running(s)) return { level: "idle", why: "Not recording" };
  const auto = (i: number) => {
    const c = s.config?.sources[i];
    return !!c && "autoTune" in c && !!c.autoTune;
  };
  return worst(s.sources.map((x, i) => sourceHealth(s, x, auto(i))));
}

export function decodeHealth(s: AppState): Health {
  if (!running(s)) return { level: "idle", why: "Not recording" };
  return worst(dashSystems(s).map((x) => systemHealth(s, x)));
}

export function pluginsHealth(s: AppState): Health {
  const ps = s.plugins?.plugins ?? [];
  const on = (id: string) => !!s.config?.plugins?.[id]?.enabled;
  const hs = ps.filter((p) => on(p.id)).map((p) => pluginHealth(p, true));
  return hs.length ? worst(hs) : { level: "idle", why: "No plugins on" };
}

/** Calls lost for want of a recorder or a source, last few minutes. */
export function radioHealth(s: AppState): Health {
  if (!running(s)) return { level: "idle", why: "Not recording" };
  const missed = dashSystems(s).reduce((a, x) => a + (recentAvg(K.sys(x.name, "why/no_recorder"), 300) ?? 0) + (recentAvg(K.sys(x.name, "why/no_source"), 300) ?? 0), 0);
  if (missed > 0) return { level: "warn", why: "Calls are being missed: no free recorder, or outside every source's band" };
  return OK("Recording what it hears");
}

/** The health of a page, for the navigation's lights. */
export function pageHealth(s: AppState): Record<string, Health> {
  return { rf: rfHealth(s), decode: decodeHealth(s), radio: radioHealth(s), plugins: pluginsHealth(s), platform: platformHealth(s) };
}

/** The latest state outside React (for non-hook helpers). */
export const now = () => snapshot();
