// The interface's state and actions. The recorder (desktop app, or later the
// web build's engine worker) owns the truth; this mirrors what it reports and
// sends what the user does. Components read state with useApp().

import { useSyncExternalStore } from "react";
import { LivePlayer } from "./livePlayer.ts";
import type {
  AudioChunk,
  CallEntry,
  CallView,
  Config,
  Device,
  EngineStatus,
  FromRecorder,
  LogLine,
  Phase,
  PluginsList,
  PluginValues,
  Radios,
  SourceStatus,
  SiteIdentity,
  Spectrum,
  SurveyBand,
  SurveyState,
  SurveySuggestion,
  System,
} from "./protocol.ts";
import { activeSystems, newSystem, resolvedCenters, sameSystem, siteName, sourceCovering, usableHalfWidth } from "./config.ts";
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
  /** Which page is showing. */
  view: View;
}

export type View = "recorder" | "plugins";

function viewFromHash(): View {
  return location.hash === "#plugins" ? "plugins" : "recorder";
}

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
  listen: false,
  listenSystem: null,
  listenTalkgroup: null,
  nowPlaying: null,
  survey: { stage: "idle" },
  surveyBands: [],
  surveySpectrum: null,
  configEpoch: 0,
  plugins: null,
  view: viewFromHash(),
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
        units: Object.fromEntries(Object.entries(m.units ?? {}).map(([name, csv]) => [name, parseUnitsCsv(csv)])),
        radios: m.radios ?? null,
        phase: m.phase.phase,
        error: m.phase.error,
        ended: m.phase.ended,
        surveyBands: m.surveyBands ?? [],
        survey: m.survey ?? { stage: "idle" },
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
    case "quit":
      player.stop();
      transport.close?.();
      set({ quit: true, connected: false, phase: "idle" });
      break;
  }
};

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

/** Link the conventional channels to a CSV file on the recorder's computer, reload it, or unlink (""). */
export function setChannelFile(path: string): void {
  flushConfig();
  transport.send({ type: "channelFile", path });
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
export function setView(view: View): void {
  const hash = view === "plugins" ? "#plugins" : "";
  if (location.hash !== hash) history.pushState(null, "", hash || location.pathname + location.search);
  set({ view });
}
addEventListener("popstate", () => set({ view: viewFromHash() }));

// ── plugins ──────────────────────────────────────────────────────────────────

export function setPluginEnabled(id: string, enabled: boolean): void {
  // Show it at once; the recorder's list follows.
  if (state.plugins) set({ plugins: { ...state.plugins, plugins: state.plugins.plugins.map((p) => (p.id === id ? { ...p, enabled } : p)) } });
  transport.send({ type: "setPlugin", id, enabled });
}
/** Replace a plugin's settings (its own, and for each system by short name). */
export function savePluginSettings(id: string, config: PluginValues, systems: Record<string, PluginValues>): void {
  transport.send({ type: "setPlugin", id, config, systems });
}
export function addPlugin(path: string): void {
  transport.send({ type: "addPlugin", path });
}
export function removePlugin(id: string): void {
  transport.send({ type: "removePlugin", id });
}
export function setPluginAudio(patch: { encoder?: string; bitrateKbps?: number }): void {
  transport.send({ type: "setPluginAudio", ...patch });
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
/** Search for USRPs (and re-list Airspys); a USRP search can take seconds. */
export function findRadios(): void {
  set({ findingRadios: true });
  transport.send({ type: "findRadios" });
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
      if (src.kind === "rtlsdr" && sug.gainDb !== null) src.gainDb = sug.gainDb;
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
