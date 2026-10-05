// The interface's state and actions. The recorder (desktop app, or later the
// web build's engine worker) owns the truth; this mirrors what it reports and
// sends what the user does. Components read state with useApp().

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { feedSeries } from "./series.ts";
import { LivePlayer } from "./livePlayer.ts";
import { conventionalIndex } from "./protocol.ts";
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
  PluginInstall,
  PluginsList,
  PluginStore,
  PluginValues,
  Radios,
  SourceStatus,
  SiteIdentity,
  SourceProfile,
  Spectrum,
  SurveyBand,
  SurveyState,
  SurveySuggestion,
  System,
} from "./protocol.ts";
import { type ImportTodo, activeSystems, newConventional, newSystem, resolvedCenters, sameSystem, siteName, sourceCovering, usableHalfWidth } from "./config.ts";
import { WsTransport, type Transport } from "./transport.ts";
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
  devices: Device[];
  /** Optional drivers (desktop app); null in the web build. */
  radios: Radios | null;
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
  surveySpectrum: Spectrum | null;
  /** A source's roll-off as last measured (Setup), and the source being measured now (null: none). */
  profile: SourceProfile | null;
  profiling: number | null;
  /** Bumped when the config is replaced from outside the form (fields re-read it). */
  configEpoch: number;
  /** The plugins (desktop app); null until the recorder says. */
  plugins: PluginsList | null;
  /** The plugin registry's list; null until asked for. */
  pluginStore: PluginStore | null;
  /** Installs under way, by what was asked for (an id, or a repository). */
  pluginInstalls: Record<string, PluginInstall>;
  /** A plugin this browser just installed: its settings open. */
  pluginJustInstalled: string | null;
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

export type View = "overview" | "rf" | "decode" | "radio" | "calls" | "plugins" | "platform" | "setup";
const VIEWS: View[] = ["overview", "rf", "decode", "radio", "calls", "plugins", "platform", "setup"];

/** `#/radio/dcfd/tg/101` → ["radio", ["dcfd", "tg", "101"]]; the old `#plugins` too. */
function routeFromHash(): { view: View; path: string[] } {
  const parts = location.hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  const v = parts[0] as View;
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
  devices: [],
  radios: null,
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
  surveySpectrum: null,
  profile: null,
  profiling: null,
  configEpoch: 0,
  plugins: null,
  pluginStore: null,
  pluginInstalls: {},
  pluginJustInstalled: null,
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
function set(patch: Partial<AppState>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useApp(): AppState {
  return useSyncExternalStore(subscribe, () => state);
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
  return useSyncExternalStore(subscribe, get);
}

/** The state now (outside React). */
export function snapshot(): AppState {
  return state;
}

const player = new LivePlayer();
// The web build (`vite build --mode web`) runs the recorder in this page; the
// desktop build leaves the worker and WebAssembly out entirely.
export const web: WorkerTransport | null = import.meta.env.MODE === "web" ? new (await import("./web/workerTransport.ts")).WorkerTransport() : null;
export const transport: Transport = web ?? new WsTransport();

/** Save a recorded file (a download from the recorder, or out of browser storage). */
export async function downloadCall(path: string, ext: "wav" | "json"): Promise<void> {
  const a = document.createElement("a");
  a.href = await transport.callUrl(path, ext);
  a.download = `${path.split("/").pop()}.${ext}`;
  a.click();
}

transport.onConnection = (connected) => set({ connected, ...(connected ? {} : { phase: "idle" as Phase }) });
transport.onAudio = onAudio;
transport.onMessage = (m: FromRecorder) => {
  switch (m.type) {
    case "hello":
      set({
        version: m.version,
        platform: m.platform,
        configPath: m.configPath,
        config: m.config,
        devices: m.devices,
        history: m.history,
        heard: m.heard ?? {},
        units: Object.fromEntries(Object.entries(m.units ?? {}).map(([name, csv]) => [name, parseUnitsCsv(csv)])),
        radios: m.radios ?? null,
        phase: m.phase.phase,
        error: m.phase.error,
        ended: m.phase.ended,
        surveyBands: m.surveyBands ?? [],
        survey: m.survey ?? { stage: "idle" },
        events: m.events ?? [],
        host: m.host ?? state.host,
      });
      if (m.host) feedSeries(m.host.t, m.host.values);
      if (m.pluginRuntime) pendingRuntime = m.pluginRuntime;
      if (state.listen) transport.send({ type: "listen", on: true, system: state.listenSystem, talkgroup: state.listenTalkgroup });
      // (A new connection: it watches nothing until told.)
      sendTopics(true);
      break;
    case "state":
      set({ phase: m.phase, error: m.error ?? state.error, ended: m.ended, ...(m.phase === "starting" ? { calls: [], log: [], status: null, sources: [], spectra: [], rfDetail: {}, decodeDetail: {} } : {}) });
      if (m.phase === "idle") set({ nowPlaying: null, stats: null });
      break;
    case "config":
      if (!pendingConfig) set({ config: m.config });
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
      set({ log: [...state.log, ...m.lines].slice(-400) });
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
      // The runtimes hello carried (the list only names plugins), merged in once.
      if (pendingRuntime) {
        const rt = pendingRuntime;
        list.plugins = list.plugins.map((p) => (rt[p.id] ? { ...p, runtime: rt[p.id] } : p));
        pendingRuntime = null;
      }
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
    case "pluginStore": {
      const { type: _, ...store } = m;
      set({ pluginStore: store });
      break;
    }
    case "pluginInstall": {
      const { type: _, ...inst } = m;
      const installs = { ...state.pluginInstalls };
      if (inst.stage === "done" || inst.stage === "failed") {
        delete installs[inst.key];
        // Only the browser that asked says how it went.
        if (askedInstalls.delete(inst.key)) {
          // A new plugin's settings open (not an update's: the list still has it).
          const fresh = !!inst.id && !state.plugins?.plugins.some((p) => p.id === inst.id);
          if (inst.stage === "done") set({ notice: inst.message, pluginJustInstalled: fresh ? inst.id : null });
          else set({ error: inst.message });
        }
      } else installs[inst.key] = inst;
      set({ pluginInstalls: installs });
      break;
    }
    case "quit":
      player.stop();
      transport.close?.();
      set({ quit: true, connected: false, phase: "idle" });
      break;
  }
};

/** Plugin runtimes from hello, waiting for the plugin list. */
let pendingRuntime: Record<string, import("./protocol.ts").PluginRuntime> | null = null;

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
  updateConfig((c) => {
    const sys = c.systems.find((x) => x.shortName === system);
    if (sys) sys.talkgroupsCsv = withIgnore(sys.talkgroupsCsv, tg, ignore, alphaTag);
  });
}

// ── config ───────────────────────────────────────────────────────────────────

let pendingConfig: ReturnType<typeof setTimeout> | null = null;

/** Edit the config; saved on the recorder shortly after the last change. */
export function updateConfig(fn: (c: Config) => void): void {
  if (!state.config) return;
  const c = structuredClone(state.config);
  fn(c);
  set({ config: c });
  if (pendingConfig) clearTimeout(pendingConfig);
  pendingConfig = setTimeout(() => {
    pendingConfig = null;
    if (state.config) transport.send({ type: "setConfig", config: state.config });
  }, 300);
}

function flushConfig(): void {
  if (pendingConfig && state.config) {
    clearTimeout(pendingConfig);
    pendingConfig = null;
    transport.send({ type: "setConfig", config: state.config });
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

/** Make the setup form re-read the config (after it was replaced from outside a field). */
export function bumpEpoch(): void {
  set({ configEpoch: state.configEpoch + 1 });
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
/** A plugin's settings for one system, trunked or conventional. */
export function setSystemPluginSettings(system: SystemRef, id: string, values: PluginValues): void {
  updateConfig((x) => {
    const sys = systemAt(x, system);
    if (!sys) return;
    const all = { ...(sys.plugins ?? {}) };
    if (blank(values)) delete all[id];
    else all[id] = values;
    if (Object.keys(all).length) sys.plugins = all;
    else delete sys.plugins;
  });
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
/** The registry's list (fetched again now with `refresh`). */
export function fetchPluginStore(refresh = false): void {
  transport.send({ type: "pluginStore", refresh });
}
/** Installs this browser asked for: it says how they went. */
const askedInstalls = new Set<string>();
/** Install (or update) a plugin from the registry. */
export function installPlugin(id: string): void {
  askedInstalls.add(id);
  set({ pluginInstalls: { ...state.pluginInstalls, [id]: { key: id, id, stage: "finding", message: null } } });
  transport.send({ type: "installPlugin", id });
}
/** Install a plugin from a GitHub release that isn't in the registry. */
export function installPluginFrom(repository: string, tag?: string): void {
  const key = repository.trim();
  askedInstalls.add(key);
  set({ pluginInstalls: { ...state.pluginInstalls, [key]: { key, id: "", stage: "finding", message: null } } });
  transport.send({ type: "installPlugin", repository: key, ...(tag?.trim() ? { tag: tag.trim() } : {}) });
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
  // The browser has no USRP, Airspy or SoapySDR (its worker drops findRadios: nothing would answer).
  if (web) return;
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
  set({ error: null, surveySpectrum: null });
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

/** What applying a survey result did to source `i`. */
export interface SurveyApplied {
  system: string;
  /** The source's centre moved to cover it (false: other systems rely on where it is). */
  centered: boolean;
}

/**
 * Put what the survey found into the config: a system (new, or `target`
 * replaced) with its control channels, site identity and voice channels; and
 * source `i`'s ppm and gain, and its centre — unless another system is
 * covered only where that source is now.
 */
export function applySurvey(i: number, sug: SurveySuggestion, target: number | "new"): SurveyApplied {
  let applied: SurveyApplied = { system: "", centered: false };
  updateConfig((c) => {
    const expect: SiteIdentity = { nac: sug.nac, wacn: sug.wacn, sysId: sug.sysId, rfss: sug.rfss, site: sug.site };
    const b = sug.type === "smartnet" ? sug.bandplan : null;
    const smartnet: Partial<System> = b
      ? { type: "smartnet", bandplan: b.bandplan, bandplanBaseHz: b.bandplanBase, bandplanSpacingHz: b.bandplanSpacing, bandplanOffset: b.bandplanOffset, bandplanHighHz: b.bandplanHigh }
      : { type: "p25" };
    const fields: Partial<System> = { controlChannelsHz: sug.controlChannels, expect, voiceChannelsHz: sug.voiceChannels, enabled: true, ...smartnet };
    let sys: System;
    if (target === "new" || !c.systems[target]) {
      // Another site of a system already here: same talkgroups.
      const sibling = c.systems.find((x) => sameSystem(x.expect, expect) && x.talkgroupsCsv);
      sys = newSystem(c, { shortName: siteName(c, expect, sug.type === "smartnet"), ...fields, talkgroupsCsv: sibling?.talkgroupsCsv ?? "", talkgroupsName: sibling?.talkgroupsName ?? "" });
      c.systems.push(sys);
    } else {
      sys = c.systems[target];
      Object.assign(sys, fields);
    }
    const src = c.sources[i];
    let centered = false;
    if (src && src.type !== "file") {
      if (sug.ppmApply !== null) src.ppm = sug.ppmApply;
      if (src.type === "rtlsdr" && sug.gainDb !== null) {
        src.gainDb = sug.gainDb;
        src.agc = false;
      }
      // Don't strand another system that only this source covers now.
      const centers = resolvedCenters(c);
      const within = (f: number) => Math.abs(f - sug.centerHz) <= usableHalfWidth(src.rateHz, src.guardHz);
      const stranded = activeSystems(c).some((x) => {
        if (x === sys) return false;
        const on = x.controlChannelsHz.map((f) => sourceCovering(c, centers, f));
        const onlyHere = on.includes(i) && !on.some((k) => k >= 0 && k !== i);
        return onlyHere && !x.controlChannelsHz.some(within);
      });
      if (!stranded) {
        src.centerHz = sug.centerHz;
        centered = true;
      }
    }
    applied = { system: sys.shortName, centered };
  });
  set({ configEpoch: state.configEpoch + 1 });
  return applied;
}

/** Add a site (a neighbour the control channel announced, or a scanned control channel) as a system. */
export function addSite(controlChannels: number[], expect: SiteIdentity): string {
  let name = "";
  updateConfig((c) => {
    const sibling = c.systems.find((x) => sameSystem(x.expect, expect) || (expect.sysId != null && x.expect.sysId === expect.sysId && x.talkgroupsCsv));
    const sys = newSystem(c, {
      shortName: siteName(c, expect),
      controlChannelsHz: controlChannels,
      expect,
      modulation: sibling?.modulation ?? "auto",
      talkgroupsCsv: sibling?.talkgroupsCsv ?? "",
      talkgroupsName: sibling?.talkgroupsName ?? "",
    });
    c.systems.push(sys);
    name = sys.shortName;
  });
  set({ configEpoch: state.configEpoch + 1 });
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
    c.systems.push(sys);
    name = sys.shortName;
  });
  set({ configEpoch: state.configEpoch + 1 });
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
    c.systems.push(sys);
    name = sys.shortName;
  });
  set({ configEpoch: state.configEpoch + 1 });
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
    conv.channels.push({ freqHz, mode: rate, name: "", enabled: true, ...(ran ? { tone: `RAN ${ran}` } : {}) });
    name = conv.shortName;
  });
  set({ configEpoch: state.configEpoch + 1 });
  return name;
}

// ── run ──────────────────────────────────────────────────────────────────────

export function start(): void {
  flushConfig();
  set({ error: null, ended: false });
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

/**
 * The short name of the system a call or audio frame names by number (its
 * number this run: SystemStatus.index, or conventionalSystem(k)); "" if unknown.
 */
export function systemNameOf(s: AppState, system: number): string {
  const k = conventionalIndex(system);
  if (k !== null) return s.config?.conventional[k]?.shortName ?? "";
  return s.status?.systems.find((x) => x.index === system)?.shortName ?? "";
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
