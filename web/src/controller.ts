// The interface's state and actions. The recorder (desktop app, or later the
// web build's engine worker) owns the truth; this mirrors what it reports and
// sends what the user does. Components read state with useApp().

import { useSyncExternalStore } from "react";
import { LivePlayer } from "./livePlayer.ts";
import type { AudioChunk, CallEntry, CallView, Config, Device, EngineStatus, FromRecorder, LogLine, Phase, SourceStatus, Spectrum } from "./protocol.ts";
import { WsTransport, type Transport } from "./transport.ts";
import type { WorkerTransport } from "./web/workerTransport.ts";

export interface AppState {
  connected: boolean;
  version: string;
  platform: string;
  configPath: string;
  phase: Phase;
  error: string | null;
  notice: string | null;
  ended: boolean;
  config: Config | null;
  devices: Device[];
  status: EngineStatus | null;
  sources: SourceStatus[];
  load: number;
  calls: CallView[];
  spectra: Spectrum[];
  log: LogLine[];
  history: CallEntry[];
  listen: boolean;
  /** Only play this talkgroup live (null = any). */
  listenTalkgroup: number | null;
  nowPlaying: { talkgroup: number; callId: number } | null;
}

let state: AppState = {
  connected: false,
  version: "",
  platform: "",
  configPath: "",
  phase: "idle",
  error: null,
  notice: null,
  ended: false,
  config: null,
  devices: [],
  status: null,
  sources: [],
  load: 0,
  calls: [],
  spectra: [],
  log: [],
  history: [],
  listen: false,
  listenTalkgroup: null,
  nowPlaying: null,
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
        phase: m.phase.phase,
        error: m.phase.error,
        ended: m.phase.ended,
      });
      if (state.listen) transport.send({ type: "listen", on: true, talkgroup: state.listenTalkgroup });
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
    case "devices":
      set({ devices: m.devices });
      break;
    case "error":
      set({ error: m.message });
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

// ── run ──────────────────────────────────────────────────────────────────────

export function start(): void {
  flushConfig();
  set({ error: null, ended: false });
  transport.send({ type: "start" });
  if (state.listen) player.resume();
}

export function stop(): void {
  transport.send({ type: "stop" });
}

// ── live audio ───────────────────────────────────────────────────────────────

export function setListen(on: boolean, talkgroup: number | null = state.listenTalkgroup): void {
  set({ listen: on, listenTalkgroup: talkgroup });
  transport.send({ type: "listen", on, talkgroup });
  if (on) player.resume();
  else {
    player.stop();
    set({ nowPlaying: null });
  }
}

let current: { callId: number; lastMs: number } | null = null;

function onAudio(a: AudioChunk): void {
  if (!state.listen) return;
  if (state.listenTalkgroup !== null && a.talkgroup !== state.listenTalkgroup) return;
  const now = performance.now();
  // Scanner behaviour: stay on the current call until it has been quiet 2 s.
  if (current && current.callId !== a.callId && now - current.lastMs < 2000) return;
  current = { callId: a.callId, lastMs: now };
  if (state.nowPlaying?.callId !== a.callId) set({ nowPlaying: { talkgroup: a.talkgroup, callId: a.callId } });
  player.enqueue(a.samples, 8000);
}
