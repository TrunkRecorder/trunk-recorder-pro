// The interface's state and actions. The recorder (desktop app, or later the
// web build's engine worker) owns the truth; this mirrors what it reports and
// sends what the user does. Components read state with useApp().

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { feedSeries } from "./series.ts";
import { LivePlayer } from "./livePlayer.ts";
import { copyOverride, copyOwn, OVERRIDE, pluginCopies } from "./protocol.ts";
import { renameSystemRefs } from "./pluginSettings.ts";
import { configReplaced } from "./rules.ts";
import type {
  MonitorEvent,
  PlatformInfo,
  RadioResult,
  ToRecorder,
  HeardCode,
  AudioChunk,
  CallEntry,
  CallView,
  Config,
  Conventional,
  Device,
  DirListing,
  EngineStatus,
  FromRecorder,
  LogLine,
  Phase,
  InstallProgress,
  PluginsList,
  InterfaceSetup,
  InterfacesList,
  Store,
  StoreKind,
  PluginValues,
  LockableKeys,
  Radios,
  SourceStatus,
  SiteIdentity,
  SourceProfile,
  Spectrum,
  SurveyBand,
  Capability,
  SurveyState,
  LicenseResultRow,
  CallSignHolder,
  FccLicenses,
  SurveySuggestion,
  Site,
  System,
} from "./protocol.ts";
import {
  type ImportTodo,
  activeSites,
  fccKey,
  fccPrimary,
  newConventional,
  newSiteName,
  newSystem,
  normalizeSite,
  resolvedCenters,
  restartKey,
  sameSystem,
  shortNameFor,
  siteNameOf,
  sourceCovering,
  trpBase,
  usableHalfWidth,
} from "./config.ts";
import { WsTransport, type CallExt, type Transport } from "./transport.ts";
import { parseUnitsCsv, type UnitAliases } from "./units.ts";
import { withIgnore } from "./talkgroups.ts";
import type { WorkerTransport } from "./web/workerTransport.ts";

export interface AppState {
  connected: boolean;
  /** The recorder was told to quit (desktop app). */
  quit: boolean;
  version: string;
  platform: string;
  configPath: string;
  phase: Phase;
  error: string | null;
  notice: string | null;
  ended: boolean;
  config: Config | null;
  /** Each system's talkgroup file (Trunk Recorder's CSV), by short name: kept beside the config, not in it. */
  talkgroups: Record<string, string>;
  devices: Device[];
  /** Optional drivers (desktop app); null in the web build. */
  radios: Radios | null;
  /** Key containers waiting for a password, and which are unlocked. */
  lockable: LockableKeys[];
  /** A USRP search is running. */
  findingRadios: boolean;
  status: EngineStatus | null;
  sources: SourceStatus[];
  load: number;
  calls: CallView[];
  spectra: Spectrum[];
  log: LogLine[];
  history: CallEntry[];
  /** Each system's radios' talker aliases, by short name. */
  units: Record<string, UnitAliases>;
  /** The codes each conventional frequency (Hz, as a string) carried. */
  heard: Record<string, HeardCode[]>;
  listen: boolean;
  /** Only play this system's calls live (its short name; null = any). */
  listenSystem: string | null;
  /** Only play this talkgroup live (null = any). */
  listenTalkgroup: number | null;
  /** The call playing live; `system` is its system's short name. */
  nowPlaying: { system: string; talkgroup: number; callId: number } | null;
  /** The first-run survey, as the recorder last reported it. */
  survey: SurveyState;
  surveyBands: SurveyBand[];
  /** What the recorder can do beyond recording (hello): see [`can`]. */
  capabilities: Capability[];
  surveySpectrum: Spectrum | null;
  /** The licenses looked up for the survey's signals ("Identify surveyed sites"); null: none asked for since it started. */
  surveyLicenses: LicenseLookup | null;
  /** A source's roll-off as last measured (Setup), and the source being measured now (null: none). */
  profile: SourceProfile | null;
  profiling: number | null;
  /** Settings changed since this run started that only apply after a restart. */
  restartNeeded: boolean;
  /** The plugins (desktop app); null until the recorder says. */
  plugins: PluginsList | null;
  /** A plugin this browser just installed: its settings open. */
  pluginJustInstalled: string | null;
  /** The interfaces besides this one (desktop app); null until the recorder says. */
  interfaces: InterfacesList | null;
  /** Each store's list (the plugins' registry, the interfaces'), once asked for. */
  stores: { [K in StoreKind]: Extract<Store, { kind: K }> | null };
  /** Installs under way from each store, by what was asked for (an id, or a repository). */
  installs: Record<StoreKind, Record<string, InstallProgress>>;
  /** Which page is showing, and what in it (`#/rf/0` → page "rf", path ["0"]). */
  view: View;
  path: string[];
  /** The dashboard: each series' value now (the recorder's `stats`, every second while recording). */
  stats: { t: number; values: Record<string, number> } | null;
  /** The computer's and the plugins' series, and the computer's own figures (desktop app, every 2 s). */
  host: { t: number; values: Record<string, number>; platform: PlatformInfo } | null;
  /** Notable events, oldest first (the last 200). */
  events: MonitorEvent[];
  /** The latest `rfDetail` per source and `decodeDetail` per system (only while watched). */
  rfDetail: Record<number, Extract<FromRecorder, { type: "rfDetail" }>>;
  decodeDetail: Record<string, Extract<FromRecorder, { type: "decodeDetail" }>>;
  /** The folder the folder picker last listed (desktop app). */
  dir: DirListing | null;
  /** The Trunk Recorder config last read from the recorder's computer. */
  trConfig: Extract<FromRecorder, { type: "trConfig" }> | null;
  /** The setup guide, when open: a first setup, or bringing over a Trunk Recorder config. */
  guide: "start" | "import" | null;
  /** What an import left to finish (highlighted in setup until done or dismissed). */
  todo: ImportTodo[];
  /** The setup page's tab. */
  setupTab: SetupTab;
}

export type SetupTab = "systems" | "conventional" | "radios" | "recording" | "plugins";

export type View = "overview" | "listen" | "rf" | "decode" | "radio" | "plugins" | "interfaces" | "platform" | "setup";
const VIEWS: View[] = ["overview", "listen", "rf", "decode", "radio", "plugins", "interfaces", "platform", "setup"];

/** `#/radio/dcfd/tg/101` → ["radio", ["dcfd", "tg", "101"]]; the old `#plugins` too. */
function routeFromHash(): { view: View; path: string[] } {
  const parts = location.hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  // (Live, once Calls, is each system's Radio system page now.)
  const v = (parts[0] === "calls" || parts[0] === "live" ? "radio" : parts[0]) as View;
  return VIEWS.includes(v) ? { view: v, path: parts.slice(1) } : { view: "overview", path: [] };
}

// Remembered in this browser (read while the state below is built, so declared first).
const TAB_KEY = "trp.setupTab";
const TODO_KEY = "trp.importTodo";

let state: AppState = {
  connected: false,
  quit: false,
  version: "",
  platform: "",
  configPath: "",
  phase: "idle",
  error: null,
  notice: null,
  ended: false,
  config: null,
  talkgroups: {},
  devices: [],
  radios: null,
  lockable: [],
  findingRadios: false,
  status: null,
  sources: [],
  load: 0,
  calls: [],
  spectra: [],
  log: [],
  history: [],
  units: {},
  heard: {},
  listen: false,
  listenSystem: null,
  listenTalkgroup: null,
  nowPlaying: null,
  survey: { stage: "idle" },
  surveyBands: [],
  capabilities: [],
  surveySpectrum: null,
  surveyLicenses: null,
  profile: null,
  profiling: null,
  restartNeeded: false,
  plugins: null,
  pluginJustInstalled: null,
  interfaces: null,
  stores: { plugin: null, interface: null },
  installs: { plugin: {}, interface: {} },
  ...routeFromHash(),
  stats: null,
  host: null,
  events: [],
  rfDetail: {},
  decodeDetail: {},
  dir: null,
  trConfig: null,
  guide: null,
  todo: loadTodo(),
  setupTab: loadSetupTab(),
};

const listeners = new Set<() => void>();
/** restartKey of the config the current run started with (null: not running). */
let runKey: string | null = null;
/** The desktop recorder takes a radio's gain while recording; the browser build doesn't. */
const liveGain = import.meta.env.MODE !== "web";

function set(patch: Partial<AppState>): void {
  const was = state;
  state = { ...state, ...patch };
  // (Only when the config or phase changed: spectrum and stats come many times a second.)
  if (state.config !== was.config || state.phase !== was.phase) {
    const key = state.phase === "idle" || !state.config ? null : restartKey(state.config, liveGain);
    runKey = key && (runKey ?? key);
    state.restartNeeded = key !== runKey;
  }
  // (Its rules' answers last as long as the config does: rules.ts.)
  if ("config" in patch) configReplaced(state.config);
  for (const l of listeners) l();
}

/** Be told of every change to the state (outside React). */
export function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useApp(): AppState {
  const get = () => state;
  return useSyncExternalStore(subscribe, get, get);
}

/** Shallow equality of two values (arrays and plain objects by their items). */
export function shallowEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || !a || !b) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k) => Object.is((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
}

/**
 * One slice of the state: the component re-renders only when the slice
 * changes (shallowly), not on every message as with useApp().
 */
export function useSelect<T>(pick: (s: AppState) => T, eq: (a: T, b: T) => boolean = shallowEqual): T {
  const last = useRef<{ s: AppState; v: T } | null>(null);
  const get = () => {
    const l = last.current;
    if (l && l.s === state) return l.v;
    const v = pick(state);
    if (l && eq(l.v, v)) {
      last.current = { s: state, v: l.v };
      return l.v;
    }
    last.current = { s: state, v };
    return v;
  };
  return useSyncExternalStore(subscribe, get, get);
}

/** The state now (outside React). */
export function snapshot(): AppState {
  return state;
}

const player = new LivePlayer();
// The web build (`vite build --mode web`) runs the recorder in this page; the
// desktop build leaves the worker and WebAssembly out entirely.
/**
 * Whether the recorder can do `c` (hello.capabilities): what to show. (Not
 * whether this page hosts the engine, the browser build: that's `web`.)
 */
export function can(c: Capability): boolean {
  return state.capabilities.includes(c);
}

export const web: WorkerTransport | null = import.meta.env.MODE === "web" ? new (await import("./web/workerTransport.ts")).WorkerTransport() : null;
export const transport: Transport = web ?? new WsTransport();

/** Save a recorded file (a download from the recorder, or out of browser storage). */
export async function downloadCall(path: string, ext: CallExt): Promise<void> {
  const a = document.createElement("a");
  a.href = await transport.callUrl(path, ext);
  a.download = `${path.split("/").pop()}.${ext}`;
  a.click();
}

const messageHooks = new Set<(m: FromRecorder) => void>();
/** Every message from the recorder, after the state has taken it (the Listen page's calls and older ones). */
export function onRecorderMessage(f: (m: FromRecorder) => void): void {
  messageHooks.add(f);
}

transport.onConnection = (connected) => {
  set({ connected, ...(connected ? {} : { phase: "idle" as Phase }) });
  if (!connected) return;
  // A new connection's greeting says how things are, from nothing: what
  // collects (events, files by system) starts over, an edit awaiting an
  // answer won't get one.
  saving = queued = false;
  set({ events: [], talkgroups: {}, units: {} });
};
transport.onAudio = onAudio;
transport.onMessage = (m: FromRecorder) => {
  take(m);
  for (const f of messageHooks) f(m);
};

function take(m: FromRecorder): void {
  switch (m.type) {
    // The greeting's end: everything before it said how things are now.
    case "hello":
      set({ version: m.version, platform: m.platform, configPath: m.configPath, surveyBands: m.surveyBands, capabilities: m.capabilities });
      if (state.listen) transport.send({ type: "listen", on: true, system: state.listenSystem, talkgroup: state.listenTalkgroup });
      // (A new connection: it watches nothing until told.)
      sendTopics(true);
      break;
    case "state":
      set({ phase: m.phase, error: m.error ?? state.error, ended: m.ended, ...(m.phase === "starting" ? { calls: [], log: [], status: null, sources: [], spectra: [], rfDetail: {}, decodeDetail: {} } : {}) });
      if (m.phase === "idle") set({ nowPlaying: null, stats: null });
      break;
    case "config":
      server = { config: m.config, version: m.version };
      // (Not over an edit of this browser's not saved yet: its answer says.)
      if (!pendingConfig && !saving) {
        set({ config: keepCards(m.config, state.config) });
        basedOn = m.version;
      }
      break;
    case "configSaved":
      configSaved(m.version, m.error);
      break;
    case "talkgroups":
      set({ talkgroups: withFile(state.talkgroups, m.system, m.csv) });
      break;
    case "units":
      set({ units: { ...state.units, [m.system]: parseUnitsCsv(m.csv) } });
      break;
    case "history":
      set({ history: m.entries });
      break;
    case "status":
      set({ status: m.status, sources: m.sources, load: m.load, calls: m.calls });
      break;
    case "spectrum": {
      const spectra = state.spectra.slice();
      spectra[m.source] = m;
      set({ spectra });
      break;
    }
    case "log":
      // (Each system's page shows its own: enough that a quiet one's aren't pushed out by a busy one.)
      set({ log: [...state.log, ...m.lines].slice(-2000) });
      break;
    case "concluded":
      set({ history: [m.entry, ...state.history].slice(0, 500) });
      break;
    case "callFiles":
      set({ history: state.history.map((e) => (e.path === m.path ? { ...e, audio: m.audio, json: m.json } : e)) });
      break;
    case "heard":
      set({ heard: m.heard });
      break;
    case "unitAlias":
      set({ units: { ...state.units, [m.system]: { ...state.units[m.system], [m.unit]: m.alias } } });
      break;
    case "survey":
      set({ survey: m, ...(m.stage === "idle" ? { surveySpectrum: null } : {}) });
      break;
    case "surveySpectrum":
      set({ surveySpectrum: m });
      break;
    case "licenseResult":
      licenseWaiting.get(m.id)?.({ results: m.results, error: m.error });
      licenseWaiting.delete(m.id);
      break;
    case "callSignResult":
      callSignWaiting.get(m.id)?.({ results: m.results, error: m.error });
      callSignWaiting.delete(m.id);
      break;
    case "sourceProfile": {
      const { type: _, ...profile } = m;
      set({ profile, profiling: null });
      break;
    }
    case "devices":
      set({ devices: m.devices });
      break;
    case "radios":
      set({ radios: m.radios, findingRadios: false });
      break;
    case "keys":
      set({ lockable: m.sources });
      break;
    case "trConfig":
      set({ trConfig: m });
      break;
    case "dir": {
      const { type: _, ...dir } = m;
      set({ dir });
      break;
    }
    case "error":
      set({ error: m.message });
      break;
    case "notice":
      set({ notice: m.message });
      break;
    case "plugins": {
      const { type: _, ...list } = m;
      set({ plugins: list });
      break;
    }
    case "stats":
      set({ stats: { t: m.t, values: m.values } });
      feedSeries(m.t, m.values);
      break;
    case "host":
      set({ host: { t: m.t, values: m.values, platform: m.platform } });
      feedSeries(m.t, m.values);
      break;
    case "rfDetail":
      set({ rfDetail: { ...state.rfDetail, [m.source]: m } });
      break;
    case "decodeDetail":
      set({ decodeDetail: { ...state.decodeDetail, [m.system]: m } });
      break;
    case "monitorEvent":
      set({ events: [...state.events, m].slice(-200) });
      break;
    case "subscribed":
      break;
    case "statsResult":
    case "radioResult": {
      const r = pending.get(m.id);
      pending.delete(m.id);
      r?.(m);
      break;
    }
    case "pluginRuntime":
      if (state.plugins) set({ plugins: { ...state.plugins, plugins: state.plugins.plugins.map((p) => (p.id === m.id ? { ...p, runtime: m.runtime } : p)) } });
      break;
    case "store": {
      const { type: _, ...store } = m;
      set({ stores: { ...state.stores, [store.kind]: store } });
      break;
    }
    case "installProgress": {
      const { type: _, ...inst } = m;
      const installs = { ...state.installs[inst.kind] };
      if (inst.stage === "done" || inst.stage === "failed") {
        delete installs[inst.key];
        // Only the browser that asked says how it went.
        if (askedInstalls.delete(`${inst.kind}:${inst.key}`)) {
          // A new plugin's settings open (not an update's: the list still has it).
          const fresh = inst.kind === "plugin" && !!inst.id && !state.plugins?.plugins.some((p) => p.id === inst.id);
          if (inst.stage === "done") set({ notice: inst.message, ...(fresh ? { pluginJustInstalled: inst.id } : {}) });
          else set({ error: inst.message });
        }
      } else installs[inst.key] = inst;
      set({ installs: { ...state.installs, [inst.kind]: installs } });
      break;
    }
    case "interfaces": {
      const { type: _, ...list } = m;
      set({ interfaces: list });
      break;
    }
    case "quit":
      player.stop();
      transport.close?.();
      set({ quit: true, connected: false, phase: "idle" });
      break;
  }
}

// ── what this dashboard watches ──────────────────────────────────────────────
//
// Costly messages (waterfalls, the control channel log, detail views) only
// come while something on screen asks for them: each component that shows one
// holds its topic with useTopic(), and the union is sent as `subscribe`.

const held = new Map<string, number>();
let topicsTimer: ReturnType<typeof setTimeout> | null = null;
let topicsSent = "";

function sendTopics(now = false): void {
  if (topicsTimer) clearTimeout(topicsTimer);
  const go = () => {
    topicsTimer = null;
    const topics = [...held.keys()].sort();
    const key = topics.join(",");
    if (!now && key === topicsSent) return;
    topicsSent = key;
    transport.send({ type: "subscribe", topics });
  };
  if (now) go();
  else topicsTimer = setTimeout(go, 150);
}

/** Watch `topic` while the component is shown (null: nothing). */
export function useTopic(topic: string | null): void {
  useEffect(() => {
    if (!topic) return;
    held.set(topic, (held.get(topic) ?? 0) + 1);
    sendTopics();
    return () => {
      const n = (held.get(topic) ?? 1) - 1;
      if (n <= 0) held.delete(topic);
      else held.set(topic, n);
      sendTopics();
    };
  }, [topic]);
}

// ── queries ──────────────────────────────────────────────────────────────────

let nextId = 1;
const pending = new Map<number, (m: FromRecorder) => void>();

function ask<T extends FromRecorder>(msg: ToRecorder & { id: number }): Promise<T> {
  return new Promise((resolve) => {
    pending.set(msg.id, (m) => resolve(m as T));
    transport.send(msg);
    // Never answered (the connection dropped): let it go.
    setTimeout(() => {
      if (pending.delete(msg.id)) resolve({ type: "error", message: "no answer" } as unknown as T);
    }, 20_000);
  });
}

export type StatsResult = Extract<FromRecorder, { type: "statsResult" }>;
export type StatsRange = "10m" | "1h" | "6h" | "24h" | "7d";

/** The history of `series` (names or `prefix*`) over `range`. */
export function statsQuery(series: string[], range: StatsRange, points?: number): Promise<StatsResult> {
  return ask<StatsResult>({ type: "statsQuery", id: nextId++, series, range, ...(points ? { points } : {}) });
}

/** The history of `series` from `from` (Unix s) to now. */
export function statsSince(series: string[], from: number, points?: number): Promise<StatsResult> {
  return ask<StatsResult>({ type: "statsQuery", id: nextId++, series, from, to: Math.floor(Date.now() / 1000), ...(points ? { points } : {}) });
}

export type RadioQuery = Omit<Extract<ToRecorder, { type: "radioQuery" }>, "type" | "id">;

export function radioQuery(q: RadioQuery): Promise<RadioResult> {
  return ask<RadioResult>({ type: "radioQuery", id: nextId++, ...q });
}

/**
 * A query's answer, asked again every `everyMs` while shown (and when `key`
 * — what it depends on — changes). Null until the first answer.
 */
export function usePolled<T>(key: string, run: () => Promise<T>, everyMs: number): T | null {
  const [v, setV] = useState<{ key: string; v: T } | null>(null);
  useEffect(() => {
    let live = true;
    const go = () => run().then((r) => live && setV({ key, v: r }));
    go();
    const t = everyMs > 0 ? setInterval(go, everyMs) : null;
    return () => {
      live = false;
      if (t) clearInterval(t);
    };
  }, [key, everyMs]);
  return v && v.key === key ? v.v : null;
}

/** Mark talkgroup `tg` of system `system` ignored (or not) in its talkgroup file. */
export function setTalkgroupIgnore(system: string, tg: number, ignore: boolean, alphaTag = ""): void {
  setTalkgroups(system, withIgnore(state.talkgroups[system] ?? "", tg, ignore, alphaTag));
}

// ── talkgroup files ──────────────────────────────────────────────────────────

/** `files` with system `system`'s now `csv` ("" for none). */
function withFile(files: Record<string, string>, system: string, csv: string): Record<string, string> {
  const next = { ...files };
  if (csv.trim()) next[system] = csv;
  else delete next[system];
  return next;
}

/** System `system`'s talkgroup file is now `csv` ("": none): saved on the recorder at once. */
export function setTalkgroups(system: string, csv: string): void {
  set({ talkgroups: withFile(state.talkgroups, system, csv) });
  transport.send({ type: "setTalkgroups", system, csv });
}

/**
 * Rename `sys` (one of `x`'s systems, trunked or conventional) to `to`, in an
 * updateConfig: its talkgroup file moves with it, and plugin settings that
 * name it follow. Setup and the first-run guide both rename this way.
 */
export function renameSystem(x: Config, sys: { shortName: string }, to: string): void {
  if (sys.shortName === to) return;
  moveTalkgroups(sys.shortName, to, [...x.systems, ...x.conventional].some((s) => s !== sys && s.shortName === to));
  renameSystemRefs(x, sys.shortName, to, state.plugins?.plugins ?? []);
  sys.shortName = to;
}

/** System `name` is gone from `x` (removed, or made a site of another), in an updateConfig: its talkgroup file goes too (once the config is saved), unless a system still has the name. */
export function forgetTalkgroups(x: Config, name: string): void {
  if ([...x.systems, ...x.conventional].some((s) => s.shortName === name)) return;
  const csv = state.talkgroups[name];
  if (csv === undefined) return;
  forgottenTalkgroups.set(name, csv);
  set({ talkgroups: withFile(state.talkgroups, name, "") });
}

// Talkgroup files follow the config they are edited with: moved and dropped in
// the store as the edit is made, on the recorder once the edit is saved (a
// refused edit puts them back).

/** Files following a renamed system: its name when the renaming began → its name now, and the name the file sits under in the store. */
const movedTalkgroups = new Map<string, { now: string; at: string }>();
/** Files of systems removed (by name), with their text, in case the edit is refused. */
const forgottenTalkgroups = new Map<string, string>();

/**
 * System `from` is renamed `to` (as its short name is typed): its talkgroup
 * file follows. Not onto "" or a name another system has (`taken`): the file
 * waits under its last name of its own until the new one is.
 */
export function moveTalkgroups(from: string, to: string, taken = false): void {
  if (from === to) return;
  const began = [...movedTalkgroups].find(([, m]) => m.now === from)?.[0] ?? from;
  const move = movedTalkgroups.get(began) ?? { now: from, at: from };
  const csv = state.talkgroups[move.at];
  if (csv === undefined) return;
  move.now = to;
  if (to === began) movedTalkgroups.delete(began);
  else movedTalkgroups.set(began, move);
  if (to === "" || taken) return;
  if (move.at !== to) set({ talkgroups: withFile(withFile(state.talkgroups, move.at, ""), to, csv) });
  move.at = to;
}

/** The config was saved: the recorder's files follow it. */
function sendTalkgroupMoves(): void {
  for (const [began, m] of movedTalkgroups) {
    // (Waiting for a name of its own: the file stays where it was.)
    if (m.at !== m.now) continue;
    transport.send({ type: "setTalkgroups", system: m.now, csv: state.talkgroups[m.now] ?? "" });
    transport.send({ type: "setTalkgroups", system: began, csv: "" });
    movedTalkgroups.delete(began);
  }
  for (const name of forgottenTalkgroups.keys()) transport.send({ type: "setTalkgroups", system: name, csv: "" });
  forgottenTalkgroups.clear();
}

/** The edit was refused: the files are where the recorder has them. */
function undoTalkgroupMoves(): void {
  let files = state.talkgroups;
  for (const [began, m] of movedTalkgroups) {
    const csv = files[m.at];
    if (m.at !== began && csv !== undefined) files = withFile(withFile(files, m.at, ""), began, csv);
  }
  for (const [name, csv] of forgottenTalkgroups) files = withFile(files, name, csv);
  movedTalkgroups.clear();
  forgottenTalkgroups.clear();
  set({ talkgroups: files });
}

// ── config ───────────────────────────────────────────────────────────────────

let pendingConfig: ReturnType<typeof setTimeout> | null = null;
/** The config as the recorder has it, and its version. */
let server: { config: Config | null; version: number } = { config: null, version: 0 };
/** The version the config in the store was taken from: what an edit of it is sent as editing (one made over a newer config is refused). */
let basedOn = 0;
/** An edit sent and not answered yet; and another made meanwhile, to send once it is. */
let saving = false;
let queued = false;

/** Send the config as edited: one edit at a time. */
function sendConfig(): void {
  if (!state.config) return;
  if (saving) {
    queued = true;
    return;
  }
  saving = true;
  transport.send({ type: "setConfig", config: sendable(state.config), version: basedOn });
}

/** The answer to an edit: saved (its version now), or not (the config is the recorder's again, and why). */
function configSaved(version: number, error: string | undefined): void {
  saving = false;
  server.version = Math.max(server.version, version);
  if (error) {
    queued = false;
    if (pendingConfig) clearTimeout(pendingConfig);
    pendingConfig = null;
    undoTalkgroupMoves();
    if (server.config) set({ config: keepCards(server.config, state.config) });
    basedOn = server.version;
    set({ error });
    return;
  }
  basedOn = version;
  sendTalkgroupMoves();
  if (queued) {
    queued = false;
    sendConfig();
  }
}

/** Edit the config; saved on the recorder shortly after the last change. */
export function updateConfig(fn: (c: Config) => void): void {
  if (!state.config) return;
  const c = structuredClone(state.config);
  fn(c);
  set({ config: c });
  if (pendingConfig) clearTimeout(pendingConfig);
  pendingConfig = setTimeout(() => {
    pendingConfig = null;
    sendConfig();
  }, 300);
}

function flushConfig(): void {
  if (pendingConfig && state.config) {
    clearTimeout(pendingConfig);
    pendingConfig = null;
    sendConfig();
  }
}

/** Link conventional system `index`'s channels to a CSV file on the recorder's computer, reload it, or unlink (""). */
export function setChannelFile(index: number, path: string): void {
  flushConfig();
  transport.send({ type: "channelFile", index, path });
}

/** A system: a trunked one by its place in `systems`, or conventional system `conv`. */
export type SystemRef = number | { conv: number };

/** The system a ref names. */
export function systemAt(c: Config, at: SystemRef): System | Conventional | undefined {
  return typeof at === "number" ? c.systems[at] : c.conventional[at.conv];
}

/** Save text as a file through the browser. */
export function downloadText(name: string, text: string, type = "text/csv"): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

// ── card keys ────────────────────────────────────────────────────────────────

/** Where a config entry keeps its card key: on it, in this browser only (never sent). */
const CARD = "_card";

/**
 * A config entry's card key (a source, system, site or conventional system,
 * one of `among`): Setup's card for it, open or not, stays its own as other
 * entries come and go and as it is renamed. Taken from `seed` (what it is
 * now: a system's short name, a source's position) the first time it's
 * asked for, so a reload finds the same cards open; kept on the entry,
 * which edits copy.
 */
export function cardKey(x: object, seed: string, among: readonly object[]): string {
  const o = x as Record<string, unknown>;
  if (typeof o[CARD] !== "string") {
    const taken = (k: string) => among.some((y) => y !== x && (y as Record<string, unknown>)[CARD] === k);
    let key = seed;
    for (let n = 2; taken(key); n++) key = `${seed}#${n}`;
    o[CARD] = key;
  }
  return o[CARD] as string;
}

/**
 * `next` (the recorder's copy of the config, which has no card keys) with
 * the card keys of the entries it replaces in `prev`: a card stays open,
 * or closed, when an edit comes back from the recorder — a system renamed
 * as its short name is typed keeps its card. Entries are matched by short
 * name (sites by name), else by place.
 */
export function keepCards(next: Config, prev: Config | null): Config {
  if (!prev) return next;
  const carry = <T extends object>(to: T[], from: T[], id?: (x: T) => string) => {
    const used = new Set<string>();
    const keyOf = (y: T | undefined) => (y as Record<string, unknown> | undefined)?.[CARD];
    to.forEach((x, i) => {
      const was = [id && from.find((y) => id(y) === id(x)), from[i]].map((y) => keyOf(y || undefined)).find((k): k is string => typeof k === "string" && !used.has(k));
      if (was === undefined) return;
      used.add(was);
      (x as Record<string, unknown>)[CARD] = was;
    });
  };
  carry(next.sources, prev.sources);
  carry(next.systems, prev.systems, (s) => s.shortName);
  next.systems.forEach((s, i) => {
    const was = prev.systems.find((p) => p.shortName === s.shortName) ?? prev.systems[i];
    if (was) carry(s.sites, was.sites, (x) => x.name ?? "");
  });
  carry(next.conventional, prev.conventional, (v) => v.shortName);
  return next;
}

/** `c` as the recorder takes it: without card keys. */
function sendable(c: Config): Config {
  return JSON.parse(JSON.stringify(c, (k, v) => (k === CARD ? undefined : v)));
}

// ── pages ────────────────────────────────────────────────────────────────────

/** The page showing (outside React). */
export function currentView(): View {
  return state.view;
}

/** Show a page, and what in it (the address's #fragment follows, so back/forward and reloads work). */
export function setView(view: View, ...path: (string | number)[]): void {
  const p = path.map(String);
  const hash = `#/${[view, ...p].map(encodeURIComponent).join("/")}`;
  if (location.hash !== hash) history.pushState(null, "", hash);
  set({ view, path: p });
  scrollTo({ top: 0 });
}
addEventListener("popstate", () => set(routeFromHash()));

// ── plugins ──────────────────────────────────────────────────────────────────

// Plugins are set up in the config: on or off and their settings for the
// whole recorder in `plugins`, their settings for each system in the system.

/** Whether plugin `id` is on (in the config). */
export function pluginOn(c: Config | null, id: string): boolean {
  return !!c?.plugins?.[id]?.enabled;
}

/** Settings with nothing in them: nothing to keep. */
function blank(v: PluginValues | undefined): boolean {
  return !v || Object.values(v).every((x) => x === undefined || x === null || x === "" || (Array.isArray(x) && x.length === 0));
}

export function setPluginEnabled(id: string, enabled: boolean): void {
  updateConfig((x) => {
    const all = (x.plugins ??= {});
    all[id] = { ...(all[id] ?? { enabled: false }), enabled };
  });
}
/** Whether a plugin is given calls decrypted with the recorder's keys. */
export function setPluginShareDecrypted(id: string, share: boolean): void {
  updateConfig((x) => {
    const all = (x.plugins ??= {});
    const p = { ...(all[id] ?? { enabled: false }) };
    if (share) p.shareDecrypted = true;
    else delete p.shareDecrypted;
    all[id] = p;
  });
}
/** A plugin's settings for the whole recorder. */
export function setPluginSettings(id: string, settings: PluginValues): void {
  updateConfig((x) => {
    const all = (x.plugins ??= {});
    const p = { ...(all[id] ?? { enabled: false }) };
    if (blank(settings)) delete p.settings;
    else p.settings = settings;
    all[id] = p;
  });
}
/** Change a system's copies of plugin `id` (trunked or conventional): one is
 * kept as an object, several as a list; a lone copy with nothing set, not at all. */
function editSystemPlugin(system: SystemRef, id: string, f: (copies: PluginValues[]) => PluginValues[]): void {
  updateConfig((x) => {
    const sys = systemAt(x, system);
    if (!sys) return;
    const all = { ...(sys.plugins ?? {}) };
    const copies = f([...pluginCopies(all[id])]);
    if (copies.length === 0 || (copies.length === 1 && blank(copies[0]))) delete all[id];
    else all[id] = copies.length === 1 ? copies[0] : copies;
    if (Object.keys(all).length) sys.plugins = all;
    else delete sys.plugins;
  });
}
/** A plugin's settings for one system: copy `copy` of it (0: the first). */
export function setSystemPluginSettings(system: SystemRef, id: string, values: PluginValues, copy = 0): void {
  editSystemPlugin(system, id, (c) => {
    while (c.length <= copy) c.push({});
    const over = copyOverride(c[copy]);
    c[copy] = blank(over) ? copyOwn(values) : { ...copyOwn(values), [OVERRIDE]: over };
    return c;
  });
}
/** The plugin's own settings that a system's copy of it overrides (another server, say); empty: none. */
export function setSystemPluginOverride(system: SystemRef, id: string, over: PluginValues, copy = 0): void {
  editSystemPlugin(system, id, (c) => {
    while (c.length <= copy) c.push({});
    const own = copyOwn(c[copy]);
    c[copy] = blank(over) ? own : { ...own, [OVERRIDE]: over };
    return c;
  });
}
/** Another copy of a plugin for one system, to set up: it runs again, with these settings, for this system alone. */
export function addSystemPluginCopy(system: SystemRef, id: string): void {
  editSystemPlugin(system, id, (c) => [...(c.length ? c : [{}]), {}]);
}
export function removeSystemPluginCopy(system: SystemRef, id: string, copy: number): void {
  editSystemPlugin(system, id, (c) => c.filter((_, i) => i !== copy));
}
export function addPlugin(path: string): void {
  transport.send({ type: "addPlugin", path });
}
export function removePlugin(id: string): void {
  transport.send({ type: "removePlugin", id });
}
/** M4A for the plugins that upload it. */
export function setM4a(patch: { encoder?: string; bitrateKbps?: number }): void {
  updateConfig((x) => void (x.recording.m4a = { ...(x.recording.m4a ?? { encoder: "auto", bitrateKbps: 32 }), ...patch }));
}
// ── the stores ───────────────────────────────────────────────────────────────
// The plugins' registry and the interfaces' store: the same lists, installs
// and progress, by kind.

/** A store's list (fetched again now with `refresh`). */
export function fetchStore(kind: StoreKind, refresh = false): void {
  transport.send({ type: "store", kind, refresh });
}
/** Installs this browser asked for (`kind:key`): it says how they went. */
const askedInstalls = new Set<string>();
function installing(kind: StoreKind, key: string, id: string): void {
  askedInstalls.add(`${kind}:${key}`);
  set({ installs: { ...state.installs, [kind]: { ...state.installs[kind], [key]: { kind, key, id, stage: "finding", message: null } } } });
}
/** Install (or update) a plugin or interface from its store. */
export function installFromStore(kind: StoreKind, id: string): void {
  installing(kind, id, id);
  transport.send({ type: "install", kind, id });
}
/** Install from a GitHub release that isn't in the store (its latest, or `tag`). */
export function installFromGitHub(kind: StoreKind, repository: string, tag?: string): void {
  const key = repository.trim();
  installing(kind, key, "");
  transport.send({ type: "install", kind, repository: key, ...(tag?.trim() ? { tag: tag.trim() } : {}) });
}
// ── interfaces ───────────────────────────────────────────────────────────────
// The other interfaces (docs/interfaces): each served on a port of its own,
// set up in the config's server.interfaces (port, on or off, access).

/** Serve a folder of your own as an interface. */
export function addInterface(path: string): void {
  transport.send({ type: "addInterface", path });
}
export function removeInterface(id: string): void {
  transport.send({ type: "removeInterface", id });
}
/** Change an interface's port, on/off or access: served so as soon as it's saved. */
export function setInterface(id: string, patch: Partial<InterfaceSetup>): void {
  updateConfig((x) => {
    const i = x.server.interfaces?.[id];
    if (i) x.server.interfaces![id] = { ...i, ...patch };
  });
}

export function settingsOpened(): void {
  set({ pluginJustInstalled: null });
}

export function dismissError(): void {
  set({ error: null });
}
export function setNotice(notice: string | null): void {
  set({ notice });
}
export function forgetHistory(): void {
  set({ history: [] });
}
export function refreshDevices(): void {
  transport.send({ type: "devices" });
}
/** Search for USRPs and SoapySDR devices (and re-list Airspys and SoapySDR modules); a search can take seconds. */
export function findRadios(): void {
  // (A recorder without the drivers — the browser version — has none to find.)
  if (!can("drivers")) return;
  set({ findingRadios: true });
  transport.send({ type: "findRadios" });
}

/** Read a Trunk Recorder config.json (or a folder with one) and the files it names; the answer lands in `trConfig`. */
export function readTrConfig(path: string): void {
  set({ trConfig: null });
  transport.send({ type: "readTrConfig", path });
}

// ── the setup guide and what an import left to do ────────────────────────────

export function openGuide(mode: "start" | "import"): void {
  set({ guide: mode, trConfig: null });
}
export function closeGuide(): void {
  set({ guide: null });
}

function loadSetupTab(): SetupTab {
  try {
    const t = localStorage.getItem(TAB_KEY);
    if (t === "systems" || t === "conventional" || t === "radios" || t === "recording" || t === "plugins") return t;
  } catch {
    // Not remembered.
  }
  return "systems";
}
/** Show a tab of the setup page (remembered in this browser). */
export function setSetupTab(setupTab: SetupTab): void {
  set({ setupTab });
  try {
    localStorage.setItem(TAB_KEY, setupTab);
  } catch {
    // Not remembered.
  }
}

function loadTodo(): ImportTodo[] {
  try {
    return JSON.parse(localStorage.getItem(TODO_KEY) ?? "[]") as ImportTodo[];
  } catch {
    return [];
  }
}
/** Replace what's left to do (an import), or drop one item (done, or dismissed). */
export function setTodo(todo: ImportTodo[]): void {
  set({ todo });
  try {
    localStorage.setItem(TODO_KEY, JSON.stringify(todo));
  } catch {
    // Kept for this session only.
  }
}
export function dismissTodo(item: ImportTodo): void {
  setTodo(state.todo.filter((t) => t !== item));
}

/** List a folder on the recorder's computer ("" = the home folder); the answer lands in `dir`. */
export function listDir(path: string): void {
  transport.send({ type: "listDir", path });
}

// ── first-run survey ─────────────────────────────────────────────────────────

export function startSurvey(source: number, bands: string[], findGain: boolean): void {
  flushConfig();
  set({ error: null, surveySpectrum: null, surveyLicenses: null });
  transport.send({ type: "surveyStart", source, bands, findGain });
}
/** Measure source `source`'s roll-off, tuned to `centerHz` (a capture keeps its own); the answer lands in `profile`. */
export function profileSource(source: number, centerHz: number): void {
  flushConfig();
  set({ profile: null, profiling: source });
  transport.send({ type: "profileSource", source, centerHz });
}
export function surveyListen(freqHz: number): void {
  transport.send({ type: "surveyListen", freqHz });
}
export function surveyRescan(): void {
  transport.send({ type: "surveyRescan" });
}
export function stopSurvey(): void {
  transport.send({ type: "surveyStop" });
}
// ── FCC license lookups ──────────────────────────────────────────────────────

/** A license lookup as it goes: where it was asked for, and what came back. */
export interface LicenseLookup {
  stage: "running" | "done" | "error";
  lat: number;
  lon: number;
  radiusM: number;
  results: LicenseResultRow[];
  error: string | null;
}
/** A frequency to look up, and how wide its signal is. */
export type LicenseSignal = { freqHz: number; widthHz?: number };

let licenseIds = 0;
const licenseWaiting = new Map<number, (r: { results: LicenseResultRow[]; error: string | null }) => void>();

/** Who is licensed on `signals` within `radiusM` of (lat, lon): asked of trunkrecorder.pro, by the recorder. */
export function lookupLicenses(lat: number, lon: number, radiusM: number, signals: LicenseSignal[]): Promise<{ results: LicenseResultRow[]; error: string | null }> {
  const id = ++licenseIds;
  return new Promise((resolve) => {
    licenseWaiting.set(id, resolve);
    // No answer (the recorder went away): say so rather than wait for ever.
    setTimeout(() => licenseWaiting.get(id)?.({ results: [], error: "No answer from the license lookup." }), 30_000);
    transport.send({ type: "licenseQuery", id, lat, lon, radiusM, signals });
  });
}

const callSignWaiting = new Map<number, (r: { results: CallSignHolder[]; error: string | null }) => void>();

/** A place the directory's place search found: a city, or a ZIP code's. */
export type Place = { name: string; state: string; lat: number; lon: number };

/** Places by name ("Arlington, VA") or ZIP code: the directory's place search, asked by the browser (as its map is). None when it couldn't be reached. */
export async function findPlaces(q: string): Promise<Place[]> {
  try {
    const r = (await (await fetch(`${trpBase(state.config)}/directory/api/v1/search?q=${encodeURIComponent(q)}`)).json()) as { places?: { kind: string; name: string; state: string; lat: number | null; lon: number | null }[] };
    return (r.places ?? []).filter((x): x is typeof x & Place => x.kind !== "state" && x.lat != null && x.lon != null).map(({ name, state, lat, lon }) => ({ name, state, lat, lon }));
  } catch {
    return [];
  }
}

/** Who holds these call signs (heard on the air): asked of trunkrecorder.pro, by the recorder; no location. */
export function lookupCallSigns(callSigns: string[]): Promise<{ results: CallSignHolder[]; error: string | null }> {
  const id = ++licenseIds;
  return new Promise((resolve) => {
    callSignWaiting.set(id, resolve);
    setTimeout(() => callSignWaiting.get(id)?.({ results: [], error: "No answer from the license lookup." }), 30_000);
    transport.send({ type: "callSignQuery", id, callSigns });
  });
}

/** What the survey heard, to look licenses up for: the monitored system's control channels first, then the signals found, strongest first (not the weak unknown ones). */
export function surveySignals(sv: SurveyState): LicenseSignal[] {
  if (sv.stage === "idle") return [];
  const ccs = sv.monitor ? [sv.monitor.advertisedHz, ...sv.monitor.secondary].filter((f): f is number => !!f).map((freqHz) => ({ freqHz })) : [];
  const found = sv.candidates
    .filter((x) => x.kind !== "other" || x.snrDb >= 10)
    .sort((a, b) => Number(a.kind === "other") - Number(b.kind === "other") || b.snrDb - a.snrDb)
    .map((x) => ({ freqHz: x.correctedHz ?? x.freqHz, widthHz: Math.min(25_000, Math.max(6_250, x.widthHz)) }));
  const out: LicenseSignal[] = [];
  for (const x of [...ccs, ...found]) {
    const f = Math.round(x.freqHz / 6250) * 6250;
    if (out.length < 64 && !out.some((y) => y.freqHz === f)) out.push({ ...x, freqHz: f });
  }
  return out;
}

/** "Identify surveyed sites": look up the survey's signals near (lat, lon); the answer lands in `surveyLicenses`. */
export function identifySurvey(lat: number, lon: number, radiusM: number): void {
  const signals = surveySignals(state.survey);
  if (!signals.length) return;
  const asked: LicenseLookup = { stage: "running", lat, lon, radiusM, results: [], error: null };
  set({ surveyLicenses: asked });
  void lookupLicenses(lat, lon, radiusM, signals).then((r) => {
    if (state.surveyLicenses === asked) set({ surveyLicenses: { ...asked, stage: r.error ? "error" : "done", ...r } });
  });
}

/** "DISTRICT OF COLUMBIA" → "District of Columbia". */
export function licenseeName(s: string): string {
  const small = new Set(["of", "the", "and", "for", "in", "on", "at"]);
  return s.toLowerCase().replace(/[a-z][a-z'.&-]*/g, (w, i: number) => (i > 0 && small.has(w) ? w : w[0].toUpperCase() + w.slice(1)));
}

/** A licensee found on some frequencies: how many of them, its call sign there, and how near. */
export interface Licensee {
  licensee: string;
  callSign: string;
  /** How many of the frequencies looked up it is licensed on. */
  frequencies: number;
  distanceM: number | null;
  bearingCardinal: string | null;
  service: string | null;
  /** None of its licenses there is active. */
  lapsed: boolean;
}

/** Who is licensed on `freqs` (within half a 6.25 kHz channel), by license: the most frequencies first, then the nearest (active before lapsed). */
export function licensees(results: LicenseResultRow[], freqs: number[]): Licensee[] {
  const by = new Map<string, Licensee & { seen: Set<number> }>();
  for (const r of results) {
    if (!freqs.some((f) => Math.abs(r.freqHz - f) <= 3125)) continue;
    for (const x of r.licenses) {
      if (!x.licensee || !x.callSign) continue;
      const k = `${x.licensee}|${x.callSign}`;
      const had = by.get(k);
      const near = (x.distanceM ?? Infinity) < (had?.distanceM ?? Infinity);
      const e = had ?? { licensee: x.licensee, callSign: x.callSign, frequencies: 0, distanceM: x.distanceM, bearingCardinal: x.bearingCardinal, service: x.serviceDesc, lapsed: r.historical, seen: new Set<number>() };
      if (near) Object.assign(e, { distanceM: x.distanceM, bearingCardinal: x.bearingCardinal });
      e.lapsed &&= r.historical;
      e.seen.add(r.freqHz);
      e.frequencies = e.seen.size;
      by.set(k, e);
    }
  }
  return [...by.values()]
    .map(({ seen: _, ...e }) => e)
    .sort((a, b) => b.frequencies - a.frequencies || Number(a.lapsed) - Number(b.lapsed) || (a.distanceM ?? Infinity) - (b.distanceM ?? Infinity));
}

/** What a location lookup found for each frequency, as FCC licenses: the nearest licensee on it (active before lapsed). */
export function lookupLicensesByFreq(results: LicenseResultRow[]): FccLicenses {
  const out: FccLicenses = {};
  for (const r of results) {
    const x = r.licenses.find((l) => l.callSign && l.licensee);
    if (x) out[fccKey(r.freqHz)] = { callSign: x.callSign!.toUpperCase(), name: x.licensee!, from: "lookup" };
  }
  return out;
}

/**
 * A lookup's licenses into `fcc`: on a frequency with none, or another
 * lookup's; on one with the same call sign, its name if it has none. Never
 * in place of the user's, or a call sign heard on the air. How many changed.
 */
export function mergeLookup(fcc: FccLicenses, found: FccLicenses): number {
  let n = 0;
  for (const [k, l] of Object.entries(found)) {
    const had = fcc[k];
    if (!had || (had.from === "lookup" && had.callSign !== l.callSign)) {
      fcc[k] = l;
      n++;
    } else if (had.callSign === l.callSign && !had.name && l.name) {
      fcc[k] = { ...had, name: l.name };
      n++;
    }
  }
  return n;
}

/** Names for the call signs `fcc` has without one, from a call sign lookup. How many it named. */
export function nameCallSigns(fcc: FccLicenses, holders: CallSignHolder[]): number {
  let n = 0;
  for (const [k, l] of Object.entries(fcc)) {
    const h = holders.find((x) => x.callSign === l.callSign && x.licensee);
    if (h && !l.name) {
      fcc[k] = { ...l, name: h.licensee! };
      n++;
    }
  }
  return n;
}

/** A system's or a conventional system's licenses, by its place in the config. */
export type FccTarget = { kind: "system" | "conventional"; index: number };

/** Change a system's FCC licenses with `f` (absent again when it leaves none). */
export function editFcc(target: FccTarget, f: (fcc: FccLicenses) => void): void {
  updateConfig((c) => {
    const x = target.kind === "system" ? c.systems[target.index] : c.conventional[target.index];
    if (!x) return;
    const fcc = { ...(x.fcc ?? {}) };
    f(fcc);
    if (Object.keys(fcc).length) x.fcc = fcc;
    else delete x.fcc;
  });
}

/** What the survey's license lookup found, into a system just made from it or added to: the licensee on each of the site's control and voice channels, and the system's name (if it has none). */
function withLicenses(sys: System, site: Site, isNew: boolean): void {
  const l = state.surveyLicenses;
  if (l?.stage !== "done") return;
  const freqs = [...site.controlChannelsHz, ...site.voiceChannelsHz].map(fccKey);
  const found = Object.fromEntries(Object.entries(lookupLicensesByFreq(l.results)).filter(([k]) => freqs.includes(k)));
  if (!Object.keys(found).length) return;
  const fcc = { ...(sys.fcc ?? {}) };
  mergeLookup(fcc, found);
  sys.fcc = fcc;
  const top = fccPrimary(fcc, site.controlChannelsHz);
  if (isNew && !sys.name && top?.name) sys.name = licenseeName(top.name);
}

/** The frequencies of some sites (or a conventional system's channels) worth looking licenses up for: control, voice and repeater channels. */
export function siteFrequencies(sites: Site[]): LicenseSignal[] {
  const all = sites.flatMap((x) => [...x.controlChannelsHz, ...x.voiceChannelsHz, ...(x.dmrChannelsHz ?? []), ...(x.nxdnChannelsHz ?? []), ...Object.values(x.lcnTableHz ?? {})]);
  return [...new Set(all.filter((f) => f > 0).map((f) => Math.round(f / 6250) * 6250))].slice(0, 64).map((freqHz) => ({ freqHz }));
}

/** What applying a survey result did to source `i`. */
export interface SurveyApplied {
  /** What it went into: "dcfd", or "dcfd-1-3, a site of dcfd". */
  system: string;
  /** The source's centre moved to cover it (false: other systems rely on where it is). */
  centered: boolean;
}

/** Where a survey result goes: a new system, or system `system`'s site `site` (replaced) or a new site of it. */
export type SurveyTarget = "new" | { system: number; site: number | "new" };

/**
 * The system (index) a site with this identity is of, already set up: one
 * of its sites is locked to the same WACN and System ID, or the running
 * recorder hears one say so. -1: none.
 */
export function systemOfSite(c: Config, id: SiteIdentity): number {
  const live = state.status?.systems ?? [];
  return c.systems.findIndex(
    (sys) => sys.sites.some((x) => sameSystem(x.expect, id)) || !!live.find((y) => y.shortName === sys.shortName)?.sites.some((x) => sameSystem(x.identity, id)),
  );
}

/** A new site of `sys` (not yet in it): `patch`, with its first site's modulation, band plan and NXDN rate. */
export function newSiteOf(sys: System, patch: Partial<Site>): Site {
  const first = sys.sites[0];
  const like: Partial<Site> = first
    ? {
        modulation: first.modulation,
        ...(first.bandplan ? { bandplan: first.bandplan } : {}),
        ...(first.bandplanBaseHz ? { bandplanBaseHz: first.bandplanBaseHz } : {}),
        ...(first.bandplanSpacingHz ? { bandplanSpacingHz: first.bandplanSpacingHz } : {}),
        ...(first.bandplanOffset ? { bandplanOffset: first.bandplanOffset } : {}),
        ...(first.bandplanHighHz ? { bandplanHighHz: first.bandplanHighHz } : {}),
        ...(first.nxdnRate ? { nxdnRate: first.nxdnRate } : {}),
      }
    : {};
  return normalizeSite({ ...like, name: newSiteName(sys), ...patch }, sys);
}

/** "dcfd-1-3, a site of dcfd"; a system's only site, the system. */
const siteLabel = (sys: System, site: Site) => (sys.sites.length > 1 ? `${siteNameOf(sys, site)}, a site of ${sys.shortName}` : sys.shortName);

/**
 * Put what the survey found into the config: a site (of a new system, a new
 * site of `target`'s system, or `target`'s site replaced) with its control
 * channels, site identity and voice channels; and source `i`'s ppm and gain,
 * and its centre — unless another site is covered only where that source is now.
 */
export function applySurvey(i: number, sug: SurveySuggestion, target: SurveyTarget): SurveyApplied {
  let applied: SurveyApplied = { system: "", centered: false };
  updateConfig((c) => {
    const expect: SiteIdentity = { ...sug.identity };
    const b = sug.type === "smartnet" ? sug.bandplan : null;
    const type = sug.type === "smartnet" ? "smartnet" : "p25";
    // (In the config's own terms.)
    const band: Partial<Site> = b ?? {};
    const fields: Partial<Site> = { controlChannelsHz: sug.controlChannels, expect, voiceChannelsHz: sug.voiceChannels, enabled: true, ...band };
    let sys = target === "new" ? undefined : c.systems[target.system];
    let site: Site;
    if (!sys || target === "new") {
      sys = newSystem(c, { shortName: shortNameFor(c, expect, type === "smartnet"), type, ...fields });
      c.systems.push(sys);
      site = sys.sites[0];
      withLicenses(sys, site, true);
    } else if (target.site !== "new" && sys.sites[target.site]) {
      // Replaced: its name and modulation stay.
      sys.type = type;
      site = normalizeSite({ ...sys.sites[target.site], ...fields }, sys);
      sys.sites[target.site] = site;
      withLicenses(sys, site, false);
    } else {
      site = newSiteOf(sys, { name: newSiteName(sys, expect), ...fields });
      sys.sites.push(site);
      withLicenses(sys, site, false);
    }
    sys.enabled = true;
    const src = c.sources[i];
    let centered = false;
    if (src && src.type !== "file") {
      if (sug.ppmApply !== null) src.ppm = sug.ppmApply;
      if (src.type === "rtlsdr" && sug.gainDb !== null) {
        src.gainDb = sug.gainDb;
        src.agc = false;
      }
      // Don't strand another site that only this source covers now.
      const centers = resolvedCenters(c);
      const within = (f: number) => Math.abs(f - sug.centerHz) <= usableHalfWidth(src.rateHz, src.guardHz);
      const stranded = activeSites(c).some(({ site: x }) => {
        if (x === site) return false;
        const on = x.controlChannelsHz.map((f) => sourceCovering(c, centers, f));
        const onlyHere = on.includes(i) && !on.some((k) => k >= 0 && k !== i);
        return onlyHere && !x.controlChannelsHz.some(within);
      });
      if (!stranded) {
        src.centerHz = sug.centerHz;
        centered = true;
      }
    }
    applied = { system: siteLabel(sys, site), centered };
  });
  return applied;
}

/**
 * Add a site (a neighbour the control channel announced, or a scanned
 * control channel): to the system it is of, if that is set up, else as a new
 * system. What it went into ("dcfd-1-3, a site of dcfd").
 */
export function addSite(controlChannels: number[], expect: SiteIdentity): string {
  let name = "";
  updateConfig((c) => {
    const k = systemOfSite(c, expect);
    if (k >= 0) {
      const sys = c.systems[k];
      const site = newSiteOf(sys, { name: newSiteName(sys, expect), controlChannelsHz: controlChannels, expect });
      sys.sites.push(site);
      withLicenses(sys, site, false);
      name = siteLabel(sys, site);
      return;
    }
    const sys = newSystem(c, { shortName: shortNameFor(c, expect), controlChannelsHz: controlChannels, expect });
    withLicenses(sys, sys.sites[0], true);
    c.systems.push(sys);
    name = sys.shortName;
  });
  return name;
}

/** A trunked DMR site the scan found: its control / rest channel and colour code. */
export function addDmrSite(freqHz: number, colorCode: number | null): string {
  let name = "";
  updateConfig((c) => {
    const sys = newSystem(c, {
      shortName: colorCode === null ? "dmr" : `dmr-cc${colorCode}`,
      type: "dmr",
      controlChannelsHz: [freqHz],
      ...(colorCode === null ? {} : { colorCode }),
    });
    withLicenses(sys, sys.sites[0], true);
    c.systems.push(sys);
    name = sys.shortName;
  });
  return name;
}

/** An NXDN Type-C site the scan found: its control channel, rate and RAN. */
export function addNxdnSite(freqHz: number, rate: "nxdn48" | "nxdn96", ran: number | null): string {
  let name = "";
  updateConfig((c) => {
    const sys = newSystem(c, {
      shortName: ran === null ? "nxdn" : `nxdn-ran${ran}`,
      type: "nxdn",
      controlChannelsHz: [freqHz],
      ...(rate === "nxdn96" ? { nxdnRate: rate } : {}),
      ...(ran === null ? {} : { ran }),
    });
    withLicenses(sys, sys.sites[0], true);
    c.systems.push(sys);
    name = sys.shortName;
  });
  return name;
}

/** A conventional NXDN channel the scan found, with its RAN: into the first conventional system (or a new one). Its name. */
export function addNxdnChannel(freqHz: number, rate: "nxdn48" | "nxdn96", ran: number | null): string {
  let name = "";
  updateConfig((c) => {
    let conv = c.conventional[0];
    if (!conv) {
      conv = newConventional(c);
      c.conventional.push(conv);
    }
    conv.channels.push({ freqHz, mode: rate, name: "", enabled: true, ...(ran ? { code: String(ran) } : {}) });
    name = conv.shortName;
  });
  return name;
}

/** Open key container `source` with its password, or (no password) lock it again. */
export function unlockKeys(source: string, password: string | null): void {
  transport.send(password === null ? { type: "lockKeys", source } : { type: "unlockKeys", source, password });
}

// ── run ──────────────────────────────────────────────────────────────────────

export function start(): void {
  flushConfig();
  // The recorder reads the config now: what it runs with from here.
  runKey = state.config ? restartKey(state.config, liveGain) : null;
  set({ error: null, ended: false, restartNeeded: false });
  transport.send({ type: "start" });
  if (state.listen) player.resume();
}

/** Stop recording and end the desktop app (the web build has nothing to quit). */
export function quitApp(): void {
  if ((state.phase === "running" || state.phase === "starting") && !confirm("Recording is running. Quit anyway? Calls in progress are saved.")) return;
  flushConfig();
  transport.send({ type: "quit" });
}

export function stop(): void {
  transport.send({ type: "stop" });
}

// ── live audio ───────────────────────────────────────────────────────────────

/** The short name of the system a call or audio frame names by number (its number this run); "" if unknown. */
export function systemNameOf(s: AppState, system: number): string {
  return s.status?.systemNames?.[system] ?? "";
}

/** Live audio on/off; optionally only one system's (its short name; null = any) and/or one talkgroup's. */
export function setListen(on: boolean, system: string | null = state.listenSystem, talkgroup: number | null = state.listenTalkgroup): void {
  set({ listen: on, listenSystem: system, listenTalkgroup: talkgroup });
  transport.send({ type: "listen", on, system, talkgroup });
  if (on) player.resume();
  else {
    player.stop();
    set({ nowPlaying: null });
  }
}

let current: { callId: number; lastMs: number } | null = null;

function onAudio(a: AudioChunk): void {
  if (!state.listen) return;
  const name = systemNameOf(state, a.system);
  if (state.listenSystem !== null && name !== state.listenSystem) return;
  if (state.listenTalkgroup !== null && a.talkgroup !== state.listenTalkgroup) return;
  const now = performance.now();
  // Scanner behaviour: stay on the current call until it has been quiet 2 s.
  if (current && current.callId !== a.callId && now - current.lastMs < 2000) return;
  current = { callId: a.callId, lastMs: now };
  if (state.nowPlaying?.callId !== a.callId) set({ nowPlaying: { system: name, talkgroup: a.talkgroup, callId: a.callId } });
  player.enqueue(a.samples, 8000);
}
