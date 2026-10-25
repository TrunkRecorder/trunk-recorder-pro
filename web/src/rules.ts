// The config's rules, the recorder's own: crates/trunk-web/src/rules.rs,
// over trunk-app's config.rs, compiled to WebAssembly (src/web/pkg, which
// `npm run wasm` builds). Setup says why Start would refuse as Start says it,
// places sources where Start will, and reads a stored config as the
// recorder reads one. Nothing here is written twice.
//
// The page awaits loadRules() before it renders (main.tsx); the engine worker
// loads the same module itself; under Node (npm test) it is loaded from its
// file on first use. For the store's config — which never changes in place:
// an edit makes a new one — answers are kept until it is replaced
// (configReplaced); for any other (a draft being edited) they are worked out
// each time.

import init, * as wasm from "./web/pkg/trunk_web.js";
import type { Channel, Config, SignalDef, Site, TonePatternKind } from "./protocol.ts";

let ready = false;

/** Load the rules (once; later calls return at once). */
export async function loadRules(): Promise<void> {
  if (!ready) await init();
  ready = true;
}

/** Ready to use: under Node, read from its file now. */
function rules(): typeof wasm {
  if (ready) return wasm;
  const node = (globalThis as { process?: { versions?: { node?: string }; getBuiltinModule?: (m: string) => unknown } }).process;
  if (node?.versions?.node && node.getBuiltinModule) {
    const fs = node.getBuiltinModule("node:fs") as { readFileSync: (u: URL) => Uint8Array };
    wasm.initSync({ module: fs.readFileSync(new URL("./web/pkg/trunk_web_bg.wasm", import.meta.url)) });
    ready = true;
    return wasm;
  }
  // (The worker loads the module itself before it reads a config.)
  ready = true;
  return wasm;
}

/** The talkgroup files an older config's JSON holds, as JSON by short name (migrate.ts). */
export function configTalkgroups(json: string): string {
  return rules().configTalkgroups(json);
}

/** The store's config now, and what has been worked out of it. */
let current: Config | null = null;
let kept = new Map<unknown, unknown>();

/** The store's config is now `c` (controller.ts): what was kept was of the one before. */
export function configReplaced(c: Config | null): void {
  if (c !== current) {
    current = c;
    kept = new Map();
  }
}

/** `f(config)`: for the store's config, worked out once while it lasts. */
function perConfig<T>(f: (json: string) => T): (c: Config) => T {
  const rule = (c: Config): T => {
    if (c !== current) return f(JSON.stringify(c));
    if (!kept.has(rule)) kept.set(rule, f(JSON.stringify(c)));
    return kept.get(rule) as T;
  };
  return rule;
}

/** A source's guard band when it sets none, Hz (the recorder's DEFAULT_GUARD_HZ). */
export const DEFAULT_GUARD_HZ = 75_000;

/** Why the config can't start, or null (the recorder's Config::problem). */
export const startProblem = perConfig((j) => rules().configProblem(j) ?? null);

/** Two systems sharing a short name, or one without, or two sites of a system one name (Config::name_problem), or null. */
export const nameProblem = perConfig((j) => rules().configNameProblem(j) ?? null);

/** Each source's centre: as set, or (0 = auto) where Start places it; null where nothing fits (Config::resolved_centers). */
export const resolvedCenters = perConfig((j) => Array.from(rules().configCenters(j), (c) => c || null));

/** How far from a source's centre a channel may be: half its band less the guard (never under a quarter of the band). */
export function usableHalfWidth(rateHz: number, guardHz: number = DEFAULT_GUARD_HZ): number {
  return rules().usableHalfWidth(rateHz, guardHz);
}

/** A centre that fits every one of `hz` in one source, off the DC spike, or null. */
export function autoCenter(hz: number[], rateHz: number, guardHz: number = DEFAULT_GUARD_HZ): number | null {
  return rules().autoCenter(Float64Array.from(hz), rateHz, guardHz) ?? null;
}

/** A source to place (planCenters): its rate and guard, and its centre if it can't move (a capture file). */
export interface Placing {
  rateHz: number;
  guardHz?: number;
  fixedHz?: number | null;
}

/**
 * Where to tune each radio for a system found on the air (the recorder's
 * plan_centers): `first` at `firstCenter`, the others over the busiest voice
 * channels still out of reach, or alongside the first; each off the DC spike.
 */
export function planCenters(sources: Placing[], first: number, firstCenter: number, ccs: number[], voice: { hz: number; weight: number }[]): (number | null)[] {
  return JSON.parse(rules().planCenters(JSON.stringify({ sources, first, firstCenter, ccs, voice })));
}

/** Why a SmartNet site's band plan can't be used, or null (smartnet::Bandplan::from_config). */
export function bandplanProblem(x: Site): string | null {
  return rules().bandplanProblem(x.bandplan ?? "", x.bandplanBaseHz ?? 0, x.bandplanSpacingHz ?? 0, x.bandplanOffset ?? 0, x.bandplanHighHz ?? 0) ?? null;
}

/**
 * A config as the recorder reads it: every setting it lacks at its default,
 * an older one upgraded (trunk_app::migrate); `notes` say what that changed.
 * A config the recorder couldn't read gives the defaults, and why.
 */
export function readConfig(raw: unknown): { config: Config; notes: string[]; error?: string } {
  const r = JSON.parse(rules().configNormalize(JSON.stringify(raw ?? {}))) as { config?: Config; notes?: string[]; error?: string };
  if (r.config) return { config: r.config, notes: r.notes ?? [] };
  return { ...readConfig({}), error: r.error };
}

/** The recorder's defaults. */
export function defaultConfig(): Config {
  return readConfig({}).config;
}

/** A conventional channel CSV read as the recorder reads it (channels.rs). */
export function parseChannelCsv(text: string): { channels: Channel[]; notes: string[]; error?: string } {
  const r = JSON.parse(rules().channelsParse(text)) as { channels?: Channel[]; notes?: string[]; error?: string };
  return { channels: r.channels ?? [], notes: r.notes ?? [], ...(r.error ? { error: r.error } : {}) };
}

/** The channels as CSV, which parseChannelCsv and the recorder read back unchanged. */
export function channelsToCsv(channels: Channel[]): string {
  return rules().channelsWrite(JSON.stringify(channels));
}

/** What's wrong with a signal (a system's `signals` entry), or null (SignalDef::problem: Start says the same). */
export function signalProblem(d: SignalDef): string | null {
  return rules().signalProblem(JSON.stringify(d)) ?? null;
}

/** What the recorder counts as signal `d`: how near its tones have to be, and the other paging codes that takes in. */
export function signalNote(d: SignalDef): string {
  return rules().signalNote(JSON.stringify(d));
}

/** Signal `d` in words, as the recorder logs a pattern heard ("two-tone 313 → 1642 Hz", "KNOX 0123"); `kind: false`: less its kind ("313 → 1642 Hz"), where that's said already. */
export function signalText(d: Partial<SignalDef>, kind = true): string {
  return rules().signalText(JSON.stringify(d), kind);
}

/** A signal's id made from what people call it ("Norwalk EMS page" → NORWALK_EMS_PAGE). */
export function signalId(text: string): string {
  return rules().signalId(text);
}

/** One step of a tone pattern: these frequencies together (none: silence), for `s` seconds. */
export type Step = { hz: number[]; s: number };

/** What a tone pattern (as the call JSON's `tone_patterns` and the tones log have it) sounds like, step by step. */
export function toneSteps(p: { pattern: TonePatternKind; tones: number[]; lengths: number[]; digits?: string; knox?: boolean; cycles?: number }): Step[] {
  return JSON.parse(rules().toneSteps(JSON.stringify(p)));
}
