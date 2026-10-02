// The interface's state and actions. The recorder (desktop app, or later the
// web build's engine worker) owns the truth; this mirrors what it reports and
// sends what the user does. Components read state with useApp().

import { useSyncExternalStore } from "react";
import { LivePlayer } from "./livePlayer.ts";
import type {
  Access,
  Account,
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
  Role,
  SourceStatus,
  SiteIdentity,
  Spectrum,
  SurveyBand,
  SurveyState,
  SurveySuggestion,
  System,
} from "./protocol.ts";
import { type ImportTodo, activeSystems, newSystem, resolvedCenters, sameSystem, siteName, sourceCovering, usableHalfWidth } from "./config.ts";
import { WsTransport, type Transport } from "./transport.ts";
import { parseUnitsCsv, type UnitAliases } from "./units.ts";
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
  /** Only play this system's calls live (SystemStatus.index, CONVENTIONAL; null = any). */
  listenSystem: number | null;
  /** Only play this talkgroup live (null = any). */
  listenTalkgroup: number | null;
  nowPlaying: { system: number; talkgroup: number; callId: number } | null;
  /** The first-run survey, as the recorder last reported it. */
  survey: SurveyState;
  surveyBands: SurveyBand[];
  surveySpectrum: Spectrum | null;
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
  /** Which page is showing. */
  view: View;
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
  /** Who this is (desktop app); null in the web build. */
  access: Access | null;
  /** The accounts, when an admin has asked. */
  accounts: Account[] | null;
}

export type SetupTab = "systems" | "conventional" | "radios" | "recording" | "plugins" | "accounts";

export type View = "recorder" | "plugins";

function viewFromHash(): View {
  return location.hash === "#plugins" ? "plugins" : "recorder";
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
  configEpoch: 0,
  plugins: null,
  pluginStore: null,
  pluginInstalls: {},
  pluginJustInstalled: null,
  view: viewFromHash(),
  dir: null,
  trConfig: null,
  guide: null,
  todo: loadTodo(),
  setupTab: loadSetupTab(),
  access: null,
  accounts: null,
};

const listeners = new Set<() => void>();
function set(patch: Partial<AppState>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function useApp(): AppState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
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
        access: m.access ?? null,
      });
      if (state.listen) transport.send({ type: "listen", on: true, system: state.listenSystem, talkgroup: state.listenTalkgroup });
      break;
    case "state":
      set({ phase: m.phase, error: m.error ?? state.error, ended: m.ended, ...(m.phase === "starting" ? { calls: [], log: [], status: null, sources: [], spectra: [] } : {}) });
      if (m.phase === "idle") set({ nowPlaying: null });
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
      set({ plugins: list });
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
    case "accounts":
      set({ accounts: m.accounts });
      break;
    case "loggedOut":
      transport.close?.();
      location.reload();
      break;
    case "quit":
      player.stop();
      transport.close?.();
      set({ quit: true, connected: false, phase: "idle" });
      break;
  }
};

// ── accounts ─────────────────────────────────────────────────────────────────

/** Only watching: no setup, plugins, start or stop. */
export function readOnly(s: AppState): boolean {
  return s.access?.role === "viewer";
}

export function fetchAccounts(): void {
  transport.send({ type: "accounts" });
}
export function addAccount(name: string, role: Role, password: string): void {
  transport.send({ type: "addAccount", name, role, password });
}
export function removeAccount(name: string): void {
  transport.send({ type: "removeAccount", name });
}
export function setAccountRole(name: string, role: Role): void {
  transport.send({ type: "setAccountRole", name, role });
}
export function setAccountPassword(name: string, password: string): void {
  transport.send({ type: "setAccountPassword", name, password });
}
export function changePassword(old: string, password: string): void {
  transport.send({ type: "changePassword", old, password });
}
export async function logOut(): Promise<void> {
  await fetch("/api/logout", { method: "POST" }).catch(() => {});
  transport.close?.();
  location.reload();
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

/** Show a page (the address's #fragment follows, so back/forward and reloads work). */
/** The page showing (outside React). */
export function currentView(): View {
  return state.view;
}

export function setView(view: View): void {
  const hash = view === "plugins" ? "#plugins" : "";
  if (location.hash !== hash) history.pushState(null, "", hash || location.pathname + location.search);
  set({ view });
}
addEventListener("popstate", () => set({ view: viewFromHash() }));

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
    if (t === "systems" || t === "conventional" || t === "radios" || t === "recording" || t === "plugins" || t === "accounts") return t;
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
    const smartnet = sug.type === "smartnet" && sug.bandplan ? { type: "smartnet" as const, ...sug.bandplan } : { type: "p25" as const };
    const fields = { controlChannels: sug.controlChannels, expect, voiceChannels: sug.voiceChannels, enabled: true, ...smartnet };
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
    if (src && src.kind !== "file") {
      if (sug.ppmApply !== null) src.ppm = sug.ppmApply;
      if (src.kind === "rtlsdr" && sug.gainDb !== null) {
        src.gainDb = sug.gainDb;
        src.agc = false;
      }
      // Don't strand another system that only this source covers now.
      const centers = resolvedCenters(c);
      const within = (f: number) => Math.abs(f - sug.centerHz) <= usableHalfWidth(src.rateHz);
      const stranded = activeSystems(c).some((x) => {
        if (x === sys) return false;
        const on = x.controlChannels.map((f) => sourceCovering(c, centers, f));
        const onlyHere = on.includes(i) && !on.some((k) => k >= 0 && k !== i);
        return onlyHere && !x.controlChannels.some(within);
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
      controlChannels,
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
      controlChannels: [freqHz],
      ...(colorCode === null ? {} : { colorCode }),
    });
    c.systems.push(sys);
    name = sys.shortName;
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

/** Live audio on/off; optionally only one system's (null = any) and/or one talkgroup's. */
export function setListen(on: boolean, system: number | null = state.listenSystem, talkgroup: number | null = state.listenTalkgroup): void {
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
  if (state.listenSystem !== null && a.system !== state.listenSystem) return;
  if (state.listenTalkgroup !== null && a.talkgroup !== state.listenTalkgroup) return;
  const now = performance.now();
  // Scanner behaviour: stay on the current call until it has been quiet 2 s.
  if (current && current.callId !== a.callId && now - current.lastMs < 2000) return;
  current = { callId: a.callId, lastMs: now };
  if (state.nowPlaying?.callId !== a.callId) set({ nowPlaying: { system: a.system, talkgroup: a.talkgroup, callId: a.callId } });
  player.enqueue(a.samples, 8000);
}
