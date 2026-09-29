// The page's side of the engine: owns the two workers, the app state, config
// persistence, the USB grant, the wake lock and live audio. React components
// read state through `useApp()` and call the actions below.

import { useSyncExternalStore } from "react";
import { autoCenter, DEFAULT_CONFIG, type LiteConfig } from "../config.ts";
import { channelizerOutputRate } from "../engine/channelizer.ts";
import { driverFor } from "../protocols/registry.ts";
import { listCalls, type StoredCall } from "../recording/opfsStore.ts";
import { RTL_USB_FILTERS } from "../sources/WebRtlSource.ts";
import type { EngineStatus } from "../trunking/trunkEngine.ts";
import type { CallView, FromRadio, FromTrunk, LogLine, RadioStats, ToRadio, ToTrunk } from "../worker/messages.ts";
import { LivePlayer } from "./livePlayer.ts";

export type Phase = "idle" | "starting" | "running" | "stopping";

export interface AppState {
  phase: Phase;
  error: string | null;
  notice: string | null;
  config: LiteConfig;
  sourceKind: "usb" | "file";
  file: File | null;
  realtime: boolean;
  devices: { serial: string; product: string }[];
  radio: RadioStats | null;
  status: EngineStatus | null;
  calls: CallView[];
  backlogS: number;
  log: LogLine[];
  history: StoredCall[];
  listen: boolean;
  /** Only play this talkgroup live (null = any). */
  listenTalkgroup: number | null;
  nowPlaying: { talkgroup: number; callId: number } | null;
  startedAtMs: number | null;
  ended: boolean;
}

const CONFIG_KEY = "trl.config.v1";

function loadConfig(): LiteConfig {
  try {
    const raw = localStorage.getItem(CONFIG_KEY);
    if (raw) {
      const c = JSON.parse(raw) as Partial<LiteConfig>;
      return {
        source: { ...DEFAULT_CONFIG.source, ...c.source },
        system: { ...DEFAULT_CONFIG.system, ...c.system },
        recording: { ...DEFAULT_CONFIG.recording, ...c.recording },
      };
    }
  } catch {
    /* storage blocked or corrupt: defaults */
  }
  return structuredClone(DEFAULT_CONFIG);
}

let state: AppState = {
  phase: "idle",
  error: null,
  notice: null,
  config: loadConfig(),
  sourceKind: "usb",
  file: null,
  realtime: true,
  devices: [],
  radio: null,
  status: null,
  calls: [],
  backlogS: 0,
  log: [],
  history: [],
  listen: false,
  listenTalkgroup: null,
  nowPlaying: null,
  startedAtMs: null,
  ended: false,
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

export function getState(): AppState {
  return state;
}

// ── config ───────────────────────────────────────────────────────────────────

export function updateConfig(fn: (c: LiteConfig) => void): void {
  const c = structuredClone(state.config);
  fn(c);
  try {
    localStorage.setItem(CONFIG_KEY, JSON.stringify(c));
  } catch {
    /* not persisted; still used for this session */
  }
  set({ config: c });
}

export function setSource(kind: "usb" | "file", file?: File | null, realtime?: boolean): void {
  set({ sourceKind: kind, file: file === undefined ? state.file : file, realtime: realtime ?? state.realtime });
}

export function dismissError(): void {
  set({ error: null });
}

export function setNotice(notice: string | null): void {
  set({ notice });
}

export async function refreshDevices(): Promise<void> {
  if (!("usb" in navigator)) return;
  const ds = (await navigator.usb.getDevices()).filter((d) => RTL_USB_FILTERS.some((f) => f.vendorId === d.vendorId && f.productId === d.productId));
  set({ devices: ds.map((d) => ({ serial: d.serialNumber ?? "", product: d.productName ?? "RTL-SDR" })) });
}

/** Must be called straight from a click (WebUSB needs a user gesture). */
export async function grantDevice(): Promise<boolean> {
  try {
    await navigator.usb.requestDevice({ filters: RTL_USB_FILTERS });
    await refreshDevices();
    return true;
  } catch (e) {
    if (e instanceof DOMException && e.name === "NotFoundError") return false; // chooser cancelled
    set({ error: `Couldn't get access to the dongle: ${e instanceof Error ? e.message : String(e)}` });
    return false;
  }
}

export async function refreshHistory(): Promise<void> {
  set({ history: await listCalls(500) });
}

// ── run ──────────────────────────────────────────────────────────────────────

let radio: Worker | null = null;
let trunk: Worker | null = null;
let wakeLock: { release(): Promise<void> } | null = null;
const player = new LivePlayer();

/** Why the current config can't start, or null. */
export function startProblem(s: AppState = state): string | null {
  if (!globalThis.crossOriginIsolated) return "This page isn't cross-origin isolated, so the sample rings can't be shared between workers. Serve it with the COOP/COEP headers (see README).";
  const c = s.config;
  if (!c.system.controlChannels.length) return "Add at least one control channel.";
  const center = c.source.centerHz || autoCenter(c.system.controlChannels, c.source.rateHz);
  if (!center) return "The control channels span more than one dongle can see — pick a center frequency by hand, or drop the far ones.";
  if (s.sourceKind === "file" && !s.file) return "Choose a capture file to replay.";
  if (s.sourceKind === "usb" && !("usb" in navigator)) return "This browser has no WebUSB. Use Chrome or Edge on desktop or Android — or replay a capture file.";
  return null;
}

export async function start(): Promise<void> {
  const problem = startProblem();
  if (problem) {
    set({ error: problem });
    return;
  }
  if (state.sourceKind === "usb") {
    await refreshDevices();
    if (!state.devices.length && !(await grantDevice())) return;
  }
  const cfg = structuredClone(state.config);
  if (!cfg.source.centerHz) cfg.source.centerHz = autoCenter(cfg.system.controlChannels, cfg.source.rateHz)!;
  const driver = driverFor(cfg.system.type, { modulation: cfg.system.modulation });
  const channelRate = channelizerOutputRate(cfg.source.rateHz, driver.minChannelRate);

  set({ phase: "starting", error: null, radio: null, status: null, calls: [], log: [], backlogS: 0, ended: false, startedAtMs: Date.now() });
  radio = new Worker(new URL("../worker/radio.worker.ts", import.meta.url), { type: "module" });
  trunk = new Worker(new URL("../worker/trunk.worker.ts", import.meta.url), { type: "module" });
  const link = new MessageChannel();

  radio.onmessage = (ev: MessageEvent<FromRadio>) => {
    const m = ev.data;
    if (m.type === "started") set({ phase: "running" });
    else if (m.type === "stats") set({ radio: m.stats });
    else if (m.type === "ended") {
      set({ ended: true });
      void stop();
    } else if (m.type === "error") {
      set({ error: m.message });
      void stop();
    }
  };
  trunk.onmessage = (ev: MessageEvent<FromTrunk>) => {
    const m = ev.data;
    if (m.type === "status") set({ status: m.status, calls: m.calls, backlogS: m.backlogS });
    else if (m.type === "log") set({ log: [...state.log, ...m.lines].slice(-400) });
    else if (m.type === "concluded") set({ history: [m.entry, ...state.history].slice(0, 500) });
    else if (m.type === "audio") onAudio(m.callId, m.talkgroup, m.samples);
    else if (m.type === "error") set({ error: m.message });
  };
  radio.onerror = (e) => set({ error: `Radio worker crashed: ${e.message}` });
  trunk.onerror = (e) => set({ error: `Trunk worker crashed: ${e.message}` });

  const toRadio: ToRadio = {
    type: "start",
    source: state.sourceKind === "usb" ? { kind: "usb" } : { kind: "file", file: state.file!, realtime: state.realtime },
    settings: { ...cfg.source },
    minOutputRate: driver.minChannelRate,
    historyS: Math.max(0.1, cfg.recording.prerollS),
    trunkPort: link.port1,
  };
  radio.postMessage(toRadio, [link.port1]);
  const toTrunk: ToTrunk = { type: "start", config: cfg, channelRate, epochMsAtZero: Date.now(), radioPort: link.port2 };
  trunk.postMessage(toTrunk, [link.port2]);

  try {
    wakeLock = await (navigator as Navigator & { wakeLock?: { request(t: "screen"): Promise<{ release(): Promise<void> }> } }).wakeLock?.request("screen") ?? null;
  } catch {
    wakeLock = null;
  }
  if (state.listen) player.resume();
}

export async function stop(): Promise<void> {
  if (!radio && !trunk) return;
  set({ phase: "stopping" });
  radio?.postMessage({ type: "stop" } satisfies ToRadio);
  trunk?.postMessage({ type: "stop" } satisfies ToTrunk);
  const r = radio;
  const t = trunk;
  radio = null;
  trunk = null;
  // Give the trunk worker a moment to conclude and save in-progress calls.
  await new Promise((res) => setTimeout(res, 1500));
  r?.terminate();
  t?.terminate();
  await wakeLock?.release().catch(() => {});
  wakeLock = null;
  set({ phase: "idle", nowPlaying: null });
  void refreshHistory();
}

// ── live audio ───────────────────────────────────────────────────────────────

export function setListen(on: boolean, talkgroup: number | null = state.listenTalkgroup): void {
  set({ listen: on, listenTalkgroup: talkgroup });
  if (on) player.resume();
  else {
    player.stop();
    set({ nowPlaying: null });
  }
}

let current: { callId: number; lastMs: number } | null = null;

function onAudio(callId: number, talkgroup: number, samples: Float32Array): void {
  if (!state.listen) return;
  if (state.listenTalkgroup !== null && talkgroup !== state.listenTalkgroup) return;
  const now = performance.now();
  // Scanner behaviour: stay on the current call until it has been quiet 2 s.
  if (current && current.callId !== callId && now - current.lastMs < 2000) return;
  current = { callId, lastMs: now };
  if (state.nowPlaying?.callId !== callId) set({ nowPlaying: { talkgroup, callId } });
  player.enqueue(samples, 8000);
}
