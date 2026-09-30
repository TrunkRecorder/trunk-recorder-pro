// The messages between the interface and the recorder (crates/trunk-lite/src/
// server.rs). The desktop app carries them over a WebSocket; the web build
// will carry the same messages between the page and its engine worker.

export type SampleFormat = "cu8" | "cs16" | "cf32";

export type Source =
  | { kind: "rtlsdr"; serial: string; centerHz: number; rateHz: number; gainDb: number | null; ppm: number }
  /** USRP through UHD (desktop app, UHD installed). `args` "" = first found. */
  | { kind: "usrp"; args: string; centerHz: number; rateHz: number; gainDb: number; antenna: string; ppm: number }
  /** Airspy R2 / Mini through libairspy (desktop app). `gain`: linearity step 0–21. */
  | { kind: "airspy"; serial: string; centerHz: number; rateHz: number; gain: number; biasTee: boolean; ppm: number }
  | { kind: "file"; path: string; centerHz: number; rateHz: number; realtime: boolean; format?: SampleFormat };

/** An optional driver (desktop app) and what it found; `devices` null = not searched. */
export interface DriverState {
  available: boolean;
  detail: string;
  devices: { args?: string; serial?: string; label: string }[] | null;
}
export interface Radios {
  usrp: DriverState;
  airspy: DriverState;
}

/** A conventional channel. `talkgroup` defaults to the frequency in kHz; `squelchDb` to the section's. */
export interface Channel {
  freqHz: number;
  mode: "fm" | "p25";
  name: string;
  talkgroup?: number;
  description?: string;
  tag?: string;
  group?: string;
  squelchDb?: number;
  enabled: boolean;
}

export interface Config {
  sources: Source[];
  system: {
    shortName: string;
    type: "p25";
    controlChannels: number[];
    modulation: "auto" | "fsk4" | "qpsk";
    talkgroupsCsv: string;
    talkgroupsName: string;
  };
  /** Energy-detected channels; `squelchDb` is the open threshold above the noise floor. */
  conventional: { squelchDb: number; channels: Channel[] };
  recording: {
    captureDir: string;
    prerollS: number;
    maxRecorders: number;
    callTimeoutS: number;
    recordUnknown: boolean;
    recordEncrypted: boolean;
    recordUnitToUnit: boolean;
    keepSilentCalls: boolean;
  };
  server: { bind: string; port: number; autoStart: boolean };
}

export interface Device {
  index: number;
  serial: string;
  product: string;
}

export type Phase = "idle" | "starting" | "running" | "stopping";

export interface PhaseState {
  phase: Phase;
  error: string | null;
  ended: boolean;
}

export interface EngineStatus {
  nowS: number;
  controlChannelHz: number | null;
  identity: { nac: number | null; wacn: number | null; sysId: number | null; rfss: number | null; site: number | null };
  good: number;
  bad: number;
  modulation: string | null;
  activeCalls: number;
  recording: number;
  channelsOpen: number;
  conventionalOpen: number;
  callsConcluded: number;
}

export interface SourceStatus {
  index: number;
  label: string;
  centerHz: number;
  rateHz: number;
  rateMeasured: number;
  dropped: number;
  errors: number;
  lastError: string | null;
  ended: boolean;
}

export interface CallView {
  id: number;
  talkgroup: number;
  alphaTag: string;
  freqHz: number;
  slot: number | null;
  analog: boolean;
  state: "recording" | "monitoring";
  reason: "unknown_tg" | "encrypted" | "no_source" | "no_recorder" | null;
  encrypted: boolean;
  emergency: boolean;
  startS: number;
  sources: number[];
}

export interface LogLine {
  timeS: number;
  kind: string;
  text: string;
}

/** Trunk Recorder's call JSON (the fields the interface shows). */
export interface CallRecord {
  talkgroup: number;
  talkgroup_tag: string;
  freq: number;
  start_time: number;
  start_time_ms: number;
  call_length: number;
  call_length_ms: number;
  emergency: number;
  encrypted: number;
  srcList: { src: number }[];
}

/** A recorded call: `path` (no extension) under the capture folder. */
export interface CallEntry {
  path: string;
  record: CallRecord;
}

export interface Spectrum {
  source: number;
  centerHz: number;
  rateHz: number;
  bins: number[];
}

export type FromRecorder =
  | {
      type: "hello";
      version: string;
      platform: string;
      config: Config;
      configPath: string;
      devices: Device[];
      phase: PhaseState & { type: "state" };
      history: CallEntry[];
      radios?: Radios;
    }
  | { type: "radios"; radios: Radios }
  | ({ type: "state" } & PhaseState)
  | { type: "config"; config: Config }
  | { type: "status"; status: EngineStatus; sources: SourceStatus[]; load: number; calls: CallView[] }
  | ({ type: "spectrum" } & Spectrum)
  | { type: "log"; lines: LogLine[] }
  | { type: "concluded"; entry: CallEntry }
  | { type: "devices"; devices: Device[] }
  | { type: "error"; message: string }
  | { type: "quit" };

export type ToRecorder =
  | { type: "setConfig"; config: Config }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "findRadios" }
  | { type: "listen"; on: boolean; talkgroup: number | null }
  | { type: "quit" };

/** Live audio: one 20 ms (or longer) chunk of a call, 8 kHz. */
export interface AudioChunk {
  callId: number;
  talkgroup: number;
  samples: Float32Array;
}

/** Binary audio frame: [1][u32 call id][u32 talkgroup][i16 samples…], little-endian. */
export function decodeAudioFrame(buf: ArrayBuffer): AudioChunk | null {
  const v = new DataView(buf);
  if (buf.byteLength < 9 || v.getUint8(0) !== 1) return null;
  const n = (buf.byteLength - 9) >> 1;
  const samples = new Float32Array(n);
  for (let i = 0; i < n; i++) samples[i] = v.getInt16(9 + 2 * i, true) / 32768;
  return { callId: v.getUint32(1, true), talkgroup: v.getUint32(5, true), samples };
}
