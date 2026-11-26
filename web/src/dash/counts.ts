// Reset counts: the recorder's since-start totals (a radio's dropped samples
// and errors, calls, a plugin's sent and failed, the RAM spool's overflows),
// counted instead from when this browser's Reset counts was pressed. The
// recorder isn't told: what it sends, and everyone else's view, stay as they
// are. Each total is kept by a key of its own (below) as it stood at the reset,
// and shown less that. A total lower than it was has counted again from
// nothing (a new run, a plugin restarted), so all of it is since the reset.

import type { AppState } from "../controller.ts";
import { clockAt } from "../fmt.ts";
import type { PlatformInfo, PluginInfo, PluginRuntime, SourceStatus, SystemStatus } from "../protocol.ts";

type Disk = PlatformInfo["disks"][number];

const K = {
  src: (label: string, what: "dropped" | "errors") => `src/${label}/${what}`,
  calls: "calls",
  sys: (shortName: string) => `sys/${shortName}/calls`,
  plg: (id: string, what: string) => `plg/${id}/${what}`,
  spool: (what: "overflowed" | "removed") => `spool/${what}`,
};
const PLUGIN_COUNTS = ["ok", "skipped", "failed", "dropped", "restarts"] as const;

/** Counted by a run (Start begins them again): all but the RAM spool's, which last as long as the app. */
export function runCounted(key: string): boolean {
  return !key.startsWith("spool/");
}

/** Every since-start total the recorder has sent, by its key: what a reset keeps. */
export function countTotals(s: AppState): Record<string, number> {
  const t: Record<string, number> = {};
  for (const src of s.sources) {
    t[K.src(src.label, "dropped")] = src.dropped;
    t[K.src(src.label, "errors")] = src.errors;
  }
  if (s.status) {
    t[K.calls] = s.status.callsConcluded;
    for (const v of s.status.systems) t[K.sys(v.shortName)] = v.callsConcluded;
  }
  for (const p of s.plugins?.plugins ?? []) {
    const r = p.runtime;
    for (const c of PLUGIN_COUNTS) t[K.plg(p.id, c)] = r[c] ?? 0;
    for (const x of r.reasons ?? []) t[K.plg(p.id, `why/${x.outcome}/${x.reason}`)] = x.calls;
  }
  const spool = s.host?.platform.disks.find((d) => d.name === "spool");
  if (spool) {
    t[K.spool("overflowed")] = spool.overflowed ?? 0;
    t[K.spool("removed")] = spool.removed ?? 0;
  }
  return t;
}

/** A total since the reset (none: since start). */
export function since(s: AppState, key: string, total: number): number {
  const b = s.countsReset?.base[key];
  return b === undefined || total < b ? total : total - b;
}

/** What the totals are counted from: "Since start", or "Since 14:05". */
export function sinceLabel(s: AppState): string {
  return s.countsReset ? `Since ${clockAt(s.countsReset.at)}` : "Since start";
}

/** A radio's dropped samples and errors since the reset. */
export function sourceCounts(s: AppState, src: SourceStatus): { dropped: number; errors: number } {
  return { dropped: since(s, K.src(src.label, "dropped"), src.dropped), errors: since(s, K.src(src.label, "errors"), src.errors) };
}

/** The calls concluded since the reset: every system's, or one's. */
export function callsCounted(s: AppState, sys?: SystemStatus): number {
  if (sys) return since(s, K.sys(sys.shortName), sys.callsConcluded);
  return s.status ? since(s, K.calls, s.status.callsConcluded) : 0;
}

/** A plugin with its results (sent, skipped, failed, why, restarts, fell behind) since the reset. */
export function pluginCounted(s: AppState, p: PluginInfo): PluginInfo {
  if (!s.countsReset) return p;
  const r = p.runtime;
  const runtime: PluginRuntime = { ...r, reasons: r.reasons?.map((x) => ({ ...x, calls: since(s, K.plg(p.id, `why/${x.outcome}/${x.reason}`), x.calls) })).filter((x) => x.calls > 0) };
  for (const c of PLUGIN_COUNTS) runtime[c] = since(s, K.plg(p.id, c), r[c] ?? 0);
  return { ...p, runtime };
}

/** A disk with the RAM spool's overflows and deletions since the reset. */
export function diskCounted(s: AppState, d: Disk): Disk {
  if (!s.countsReset || d.name !== "spool") return d;
  return { ...d, overflowed: since(s, K.spool("overflowed"), d.overflowed ?? 0), removed: since(s, K.spool("removed"), d.removed ?? 0) };
}
