// The messages between the interface and the recorder (crates/trunk-pro/src/
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
  mode: "fm" | "p25" | "dmr";
  name: string;
  talkgroup?: number;
  description?: string;
  tag?: string;
  group?: string;
  squelchDb?: number;
  enabled: boolean;
}

/** A P25 site's identity (NAC, WACN, System ID are hex on the air). Absent = unknown / any. */
export interface SiteIdentity {
  nac?: number | null;
  wacn?: number | null;
  sysId?: number | null;
  rfss?: number | null;
  site?: number | null;
}

/**
 * A trunked system — or one site of a multi-site system: each site recorded
 * from its own control channel is a system, with its own short name (folder).
 */
/** A SmartNet band plan, in Trunk Recorder's config names (Hz; offset a channel number). */
export interface SmartnetBandplan {
  /** "800_standard" | "800_reband" | "800_splinter" | "900" | "400_custom". */
  bandplan: string;
  bandplanBase?: number;
  bandplanSpacing?: number;
  bandplanOffset?: number;
  bandplanHigh?: number;
}

export interface System extends Partial<SmartnetBandplan> {
  shortName: string;
  /** "smartnet": a Motorola SmartNet / SmartZone control channel (voice P25 or analog FM).
   *  "dmr": a trunked DMR site (Capacity Plus, Capacity Max, Connect Plus, Tier III); every
   *  control channel and `channels` frequency is watched. */
  type: "p25" | "smartnet" | "dmr";
  /** DMR: logical channel number → frequency, Hz (Trunk Recorder's lcnTable); the rest is learned. */
  lcnTable?: Record<string, number>;
  /** DMR: voice frequencies to watch besides the control channels. */
  channels?: number[];
  /** DMR: only this colour code. */
  colorCode?: number;
  /** SmartNet: the voice of a talkgroup never heard granted. */
  defaultMode?: "digital" | "analog";
  enabled: boolean;
  controlChannels: number[];
  modulation: "auto" | "fsk4" | "qpsk";
  talkgroupsCsv: string;
  talkgroupsName: string;
  /** Only follow a control channel with this identity (e.g. this site, not a neighbour). */
  expect: SiteIdentity;
  /** Voice channels the survey heard (for placing sources). */
  voiceChannels: number[];
  /** Record talkgroups not in its CSV; absent/null = the Recording setting. */
  recordUnknown?: boolean | null;
}

export interface Config {
  sources: Source[];
  systems: System[];
  /** Energy-detected channels; `squelchDb` is the open threshold above the noise floor. */
  conventional: {
    /** Folder / record name of conventional calls. */
    shortName: string;
    squelchDb: number;
    /** Desktop: a CSV the channels are read from ("" = the list here). Changed with the channelFile message. */
    channelFile?: string;
    channels: Channel[];
    /** How the channel file last read (from the recorder). */
    channelFileStatus?: string;
  };
  recording: {
    captureDir: string;
    prerollS: number;
    maxRecorders: number;
    callTimeoutS: number;
    recordUnknown: boolean;
    recordEncrypted: boolean;
    recordUnitToUnit: boolean;
    keepSilentCalls: boolean;
    /** Save each call's vocoder frames (<call>.frames.jsonl) for diagnosis. */
    captureFrames: boolean;
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

export interface Identity {
  nac: number | null;
  wacn: number | null;
  sysId: number | null;
  rfss: number | null;
  site: number | null;
}

/** `Call.system` / `listen.system` of a conventional channel. */
export const CONVENTIONAL = 65535;

/** One running system (site). `index` is what calls and audio carry as `system`. */
export interface SystemStatus {
  index: number;
  shortName: string;
  nowS: number;
  controlChannelHz: number | null;
  identity: Identity;
  good: number;
  bad: number;
  modulation: string | null;
  activeCalls: number;
  recording: number;
  callsConcluded: number;
  /** The control channel is another system's / site's: why (its grants are not followed). */
  mismatch: string | null;
  /** Neighbouring sites its control channel announces. */
  adjacent: { sysId: number; rfss: number; site: number; freqHz: number }[];
  /** Patches standing now: the supergroup, and the talkgroups patched into it. */
  patches?: { supergroup: TalkgroupName; members: TalkgroupName[] }[];
  /** A trunked DMR site. */
  dmr?: DmrSiteStatus | null;
}

export interface DmrSiteStatus {
  /** "DMR Capacity Plus" | "DMR Capacity Max" | "DMR Connect Plus" | "DMR Tier III" | null (not known yet). */
  variant: string | null;
  colorCode: number | null;
  /** Capacity Plus: the rest channel's logical slot number, and its frequency once known. */
  rest: { lsn: number; freqHz: number | null } | null;
  /** Keyed CRCs (restricted access). */
  keyed: boolean;
  /** Logical channel → frequency: from the config, or learned from the air. */
  channels: { lcn: number; freqHz: number; configured: boolean }[];
  /** Every watched frequency: sending control blocks, and each slot's call now. */
  carriers: { freqHz: number; control: boolean; colorCode: number | null; slots: ({ talkgroup: TalkgroupName; source: number } | null)[] }[];
}

/** A talkgroup and its alpha tag from the system's talkgroup file ("" when not in it). */
export interface TalkgroupName {
  talkgroup: number;
  alphaTag: string;
}

export interface EngineStatus {
  nowS: number;
  activeCalls: number;
  /** Recorders in use (shared by every system). */
  recording: number;
  channelsOpen: number;
  conventionalOpen: number;
  callsConcluded: number;
  systems: SystemStatus[];
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
  /** SystemStatus.index; CONVENTIONAL for a conventional channel. */
  system: number;
  systemName: string;
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
  /** The talkgroups patched with this one during the call. */
  patched?: TalkgroupName[];
}

export interface LogLine {
  timeS: number;
  kind: string;
  text: string;
  /** The system's short name, for its messages. */
  system?: string;
}

/** Trunk Recorder's call JSON (the fields the interface shows). */
export interface CallRecord {
  short_name?: string;
  talkgroup: number;
  talkgroup_tag: string;
  freq: number;
  start_time: number;
  start_time_ms: number;
  call_length: number;
  call_length_ms: number;
  emergency: number;
  encrypted: number;
  /** `tag_ota`: the unit's talker alias when the call was saved. */
  srcList: { src: number; tag_ota?: string }[];
  /** Every talkgroup patched with this one, its own included (only when patched). */
  patched_talkgroups?: number[];
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

// ── first-run survey (crates/trunk-app/src/survey.rs) ────────────────────────

/** A band the survey can scan. */
export interface SurveyBand {
  id: string;
  label: string;
  loHz: number;
  hiHz: number;
  defaultOn: boolean;
}

export interface SurveyIdentity {
  nac: number | null;
  wacn: number | null;
  sysId: number | null;
  rfss: number | null;
  site: number | null;
}

/** A signal the scan found. `freqHz` as heard (uncorrected); `correctedHz` once the ppm is known. */
export interface SurveyCandidate {
  freqHz: number;
  correctedHz: number | null;
  band: string;
  snrDb: number;
  widthHz: number;
  kind: "control" | "smartnet" | "p25" | "dmrControl" | "dmr" | "other";
  frames: number;
  good: number;
  bad: number;
  modulation: string;
  identity: SurveyIdentity;
  /** DMR: the trunking its control blocks are ("DMR Capacity Plus", …) and its colour code. */
  dmr?: { variant: string | null; colorCode: number | null } | null;
}

export interface SurveyMonitor {
  freqHz: number | null;
  heardHz: number;
  identity: SurveyIdentity;
  /** The frequency the control channel announces for itself. */
  advertisedHz: number | null;
  secondary: number[];
  adjacent: { rfss: number; site: number; sysId: number; freqHz: number }[];
  voice: { freqHz: number; grants: number; tdma: boolean }[];
  idens: number;
  good: number;
  bad: number;
  modulation: string;
  snrDb: number | null;
  offsetHz: number | null;
  /** Total correction to set, ppm. */
  ppm: number | null;
  gain: { state: "off" | "waiting" | "running" | "done"; steps: { gainDb: number; snrDb: number; okRatio: number; clipped: number }[]; bestDb: number | null };
  elapsedS: number;
  ready: boolean;
  /** A SmartNet control channel: the band plan learned from where granted channels light up. */
  smartnet: {
    ccChan: number | null;
    altChans: number[];
    channels: number[];
    points: { chan: number; hz: number; riseDb: number }[];
    spacingHz: number | null;
    inliers: number | null;
    bandplan: SmartnetBandplan | null;
  } | null;
}

/** What to put in the config. */
export interface SurveySuggestion {
  type: "p25" | "smartnet";
  /** SmartNet: the learned band plan. */
  bandplan: SmartnetBandplan | null;
  controlChannels: number[];
  ppm: number | null;
  /** As the source takes it (an RTL-SDR: whole ppm). */
  ppmApply: number | null;
  gainDb: number | null;
  centerHz: number;
  voiceCovered: number;
  voiceTotal: number;
  spanHz: number;
  nac: number | null;
  sysId: number | null;
  wacn: number | null;
  rfss: number | null;
  site: number | null;
  voiceChannels: number[];
}

export type SurveyState =
  | { stage: "idle" }
  | {
      stage: "scanning" | "monitoring" | "done";
      source: number;
      bands: string[];
      message: string;
      error: string | null;
      progress: { hop: number; hops: number; band: string; centerHz: number } | null;
      candidates: SurveyCandidate[];
      monitor: SurveyMonitor | null;
      suggest: SurveySuggestion | null;
    };

// ── plugins (desktop app) ─────────────────────────────────────────────────────

/** A plugin's settings, as JSON Schema — the subset the settings form draws (trunk-recorder-plugin's schema.rs). */
export interface PluginSchema {
  type?: "object" | "string" | "integer" | "number" | "boolean" | "array";
  title?: string;
  description?: string;
  default?: unknown;
  properties?: Record<string, PluginSchema>;
  "x-order"?: string[];
  enum?: (string | number)[];
  "x-enum-labels"?: string[];
  format?: string;
  "x-secret"?: boolean;
  "x-multiline"?: boolean;
  minimum?: number;
  maximum?: number;
  items?: PluginSchema;
}

/** What a plugin says it is (`<plugin> --describe`). */
export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  api: number;
  subscribe: string[];
  audio_formats: string[];
  config?: PluginSchema;
  system_config?: PluginSchema;
  homepage?: string;
  repository?: string;
  authors?: string[];
  license?: string;
}

/** How a plugin is doing while recording. */
export interface PluginRuntime {
  state: "off" | "starting" | "ok" | "warning" | "error";
  message: string;
  ok: number;
  skipped: number;
  failed: number;
  lastFailure: string;
  log: { time: number; level: "error" | "warn" | "info"; text: string }[];
}

export type PluginValues = Record<string, unknown>;

export interface PluginInfo {
  id: string;
  enabled: boolean;
  /** Its executable. */
  path: string;
  /** A build of the user's own (not installed in the plugins folder). */
  custom: boolean;
  /** Null when it can't be asked (see `problem`). */
  manifest: PluginManifest | null;
  problem: string | null;
  config: PluginValues | null;
  /** Its settings for each system, by short name. */
  systems: Record<string, PluginValues>;
  runtime: PluginRuntime;
}

export interface PluginsList {
  /** plugins.json */
  file: string;
  problem?: string;
  plugins: PluginInfo[];
  /** Systems' short names, for settings per system. */
  systems: string[];
  audio: { encoder: string; bitrateKbps: number; found: string | null };
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
      /** Each system's radios' talker aliases, as saved: short name → unitTagsOTA CSV. */
      units?: Record<string, string>;
      radios?: Radios;
      surveyBands?: SurveyBand[];
      survey?: { type: "survey" } & SurveyState;
    }
  | { type: "radios"; radios: Radios }
  | ({ type: "state" } & PhaseState)
  | { type: "config"; config: Config }
  | { type: "status"; status: EngineStatus; sources: SourceStatus[]; load: number; calls: CallView[] }
  | ({ type: "spectrum" } & Spectrum)
  | { type: "log"; lines: LogLine[] }
  | { type: "concluded"; entry: CallEntry }
  /** A radio's talker alias, newly heard on system `system` (its short name). */
  | { type: "unitAlias"; system: string; unit: number; alias: string }
  | ({ type: "survey" } & SurveyState)
  | ({ type: "surveySpectrum" } & Spectrum)
  | { type: "devices"; devices: Device[] }
  | { type: "error"; message: string }
  | { type: "notice"; message: string }
  | ({ type: "plugins" } & PluginsList)
  | { type: "pluginRuntime"; id: string; runtime: PluginRuntime }
  | { type: "quit" };

export type ToRecorder =
  | { type: "setConfig"; config: Config }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "findRadios" }
  /** Live audio: on/off, optionally one system's (CONVENTIONAL: conventional channels) and/or one talkgroup's. */
  | { type: "listen"; on: boolean; system: number | null; talkgroup: number | null }
  /** Link the conventional channels to a CSV (created from the list if new), reload it (same path), or unlink (""). */
  | { type: "channelFile"; path: string }
  /** Find a system: scan `bands` with source `source`, then monitor the best control channel. */
  | { type: "surveyStart"; source: number; bands: string[]; findGain: boolean }
  /** Stop scanning and monitor this signal (as heard). */
  | { type: "surveyListen"; freqHz: number }
  | { type: "surveyRescan" }
  | { type: "surveyStop" }
  | { type: "plugins" }
  /** Turn a plugin on or off, or replace its settings. */
  | { type: "setPlugin"; id: string; enabled?: boolean; config?: PluginValues; systems?: Record<string, PluginValues> }
  /** A plugin executable on the recorder's computer (a build of the user's own). */
  | { type: "addPlugin"; path: string }
  /** Forget a plugin; an installed copy is deleted. */
  | { type: "removePlugin"; id: string }
  | { type: "setPluginAudio"; encoder?: string; bitrateKbps?: number }
  | { type: "quit" };

/** Live audio: one 20 ms (or longer) chunk of a call, 8 kHz. */
export interface AudioChunk {
  callId: number;
  system: number;
  talkgroup: number;
  samples: Float32Array;
}

/** Binary audio frame: [2][u16 system][u32 call id][u32 talkgroup][i16 samples…], little-endian (version 1 had no system). */
export function decodeAudioFrame(buf: ArrayBuffer): AudioChunk | null {
  const v = new DataView(buf);
  const ver = buf.byteLength ? v.getUint8(0) : 0;
  const head = ver === 2 ? 11 : ver === 1 ? 9 : 0;
  if (!head || buf.byteLength < head) return null;
  const at = ver === 2 ? 3 : 1;
  const n = (buf.byteLength - head) >> 1;
  const samples = new Float32Array(n);
  for (let i = 0; i < n; i++) samples[i] = v.getInt16(head + 2 * i, true) / 32768;
  return { callId: v.getUint32(at, true), system: ver === 2 ? v.getUint16(1, true) : 0, talkgroup: v.getUint32(at + 4, true), samples };
}
