// The messages between the interface and the recorder (crates/trunk-pro/src/
// server.rs). The desktop app carries them over a WebSocket; the web build
// will carry the same messages between the page and its engine worker.

export type SampleFormat = "cu8" | "cs16" | "cf32";

/**
 * A radio, or a capture to replay. Every kind may have `autoTune`: correct its
 * channels for the frequency error measured on the control channels (Trunk
 * Recorder's autoTune); the error is measured and shown either way.
 */
export type Source = (
  /** `agc`: the tuner's AGC instead of `gainDb`. */
  | { kind: "rtlsdr"; serial: string; centerHz: number; rateHz: number; gainDb: number; agc: boolean; ppm: number }
  /** USRP through UHD (desktop app, UHD installed). `args` "" = first found; `agc` the device's AGC (B200/B210). */
  | { kind: "usrp"; args: string; centerHz: number; rateHz: number; gainDb: number; agc: boolean; antenna: string; ppm: number }
  /**
   * Airspy R2 / Mini through libairspy (desktop app). `gain`: a 0–21 step of the linearity or sensitivity
   * table (`gainMode`); "manual": each stage, LNA 0–14, mixer 0–15, VGA 0–15, with `agc` the Airspy's own on LNA and mixer.
   */
  | {
      kind: "airspy";
      serial: string;
      centerHz: number;
      rateHz: number;
      gainMode: AirspyGainMode;
      gain: number;
      lnaGain: number;
      mixerGain: number;
      vgaGain: number;
      agc: boolean;
      biasTee: boolean;
      ppm: number;
    }
  /**
   * Any SDR with a SoapySDR module (desktop app). `args` "" = first found ("driver=hackrf,serial=…");
   * `agc` the device's AGC; else `gainDb` overall (null: left as it is), then each stage in `gains`
   * (HackRF LNA / VGA / AMP …); `settings` device settings ("biastee=true").
   */
  | { kind: "soapy"; args: string; centerHz: number; rateHz: number; agc: boolean; gainDb: number | null; gains: Record<string, number>; antenna: string; settings: string; ppm: number }
  | { kind: "file"; path: string; centerHz: number; rateHz: number; realtime: boolean; format?: SampleFormat }
) & { autoTune?: boolean };

export type AirspyGainMode = "linearity" | "sensitivity" | "manual";

/** An optional driver (desktop app) and what it found; `devices` null = not searched. */
export interface DriverState {
  available: boolean;
  detail: string;
  devices: { args?: string; serial?: string; label: string }[] | null;
}
/** A SoapySDR module (one per device family): the drivers it registered, or why it didn't load. */
export interface SoapyModule {
  name: string;
  path: string;
  version: string;
  drivers: string[];
  error: string;
}
/** SoapySDR and its modules; `modules` null = this SoapySDR (0.7) can't list them. */
export interface SoapyState extends DriverState {
  modules?: SoapyModule[] | null;
  searchPaths?: string[];
  devices: { args: string; label: string; driver: string }[] | null;
}
export interface Radios {
  usrp: DriverState;
  airspy: DriverState;
  soapy?: SoapyState;
}

/** A conventional channel. `talkgroup` defaults to the frequency in kHz; `squelchDb` to the section's. */
export interface Channel {
  freqHz: number;
  mode: "fm" | "p25" | "dmr";
  name: string;
  talkgroup?: number;
  /** FM: the CTCSS tone or DCS code it records, Trunk Recorder's form ("151.4", "D023N"); absent / "": any. */
  tone?: string;
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
  /** What people call it ("County Public Safety"); the short name is its folder. */
  name?: string;
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
  /** Its own recording rules; each left out is the Recording tab's. */
  recording?: RecordingOverride;
  /** Names for its radios (Trunk Recorder's unitTagsFile and unitTagsMode). */
  unitNames?: UnitNames;
  /** Multi-site: sites with one group are one system (a call on several is saved once).
   *  Absent: grouped by what the control channels say (P25 WACN + System ID, SmartNet System ID). */
  siteGroup?: string;
  /** Plugins' settings for this system, by plugin id (each plugin's `system_config` schema). */
  plugins?: Record<string, PluginValues>;
}

/** A plugin, as the config has it: on or off, and its settings for the whole recorder. */
export interface PluginSetup {
  enabled: boolean;
  /** As its `config` schema describes them. */
  settings?: PluginValues;
  /** Run this executable instead of the installed plugin (a build of the user's own). */
  path?: string;
}

/** The settings each system can set again for itself (crates/trunk-app/src/config.rs RecordingOverride). */
export interface RecordingRules {
  /** A call ends this long after its last grant or audio, s. */
  callTimeoutS: number;
  /** Talkgroups not in the talkgroup file. */
  recordUnknown: boolean;
  recordEncrypted: boolean;
  recordUnitToUnit: boolean;
  /** Keep calls with no audio (encrypted, nothing decoded). */
  keepSilentCalls: boolean;
  /** Drop calls with less audio than this, s; 0 = keep all (Trunk Recorder's minDuration). */
  minCallS: number;
  /** Save a call this long and carry on in a new one, s; 0 = no limit (maxDuration). */
  maxCallS: number;
  /** Leave out transmissions shorter than this, s; 0 = keep all (minTransmissionDuration). */
  minTransmissionS: number;
  /** Bring every call's speech to the same loudness. */
  normalizeAudio: boolean;
  /** Then raise or lower digital / analog calls, dB (digitalLevels / analogLevels). */
  digitalLevelDb: number;
  analogLevelDb: number;
  /** Desktop: also keep an .m4a of every call (compressWav). */
  compressWav: boolean;
  /** Desktop: keep the audio once every upload plugin has handled the call (audioArchive). */
  audioArchive: boolean;
  /** Desktop: keep the call's JSON then (callLog). */
  callLog: boolean;
  /** Desktop: when an upload failed, keep the files anyway (archiveFilesOnFailure). */
  archiveFilesOnFailure: boolean;
  /** Where calls go under the recordings folder ("" = <short name>/<year>/<month>/<day>/). */
  filenameFormat: string;
}

export type RecordingOverride = Partial<RecordingRules>;

/**
 * Names for a system's radios: Trunk Recorder's unitTagsFile, kept as text
 * (headerless `unit,name`; a unit between slashes is a regular expression,
 * `/^1(\d{3})$/,Engine $1`), and unitTagsMode: "user" (these first, then the
 * aliases heard; the default), "ota" (the aliases first), "user_only", "none".
 */
export interface UnitNames {
  csv?: string;
  name?: string;
  mode?: "" | "user" | "ota" | "user_only" | "none";
}

/** The log (desktop app), with Trunk Recorder's options (crates/trunk-app/src/config.rs LogSettings). */
export interface LogSettings {
  level: "trace" | "debug" | "info" | "warning" | "error" | "fatal";
  console: boolean;
  file: boolean;
  /** "" = logs/ beside the config. */
  dir: string;
  syslogFriendly: boolean;
  syslog: boolean;
  /** "" (colour on a terminal) | "console" | "logfile" | "all" | "none". */
  color: string;
  frequencyFormat: "exp" | "mhz" | "hz";
  talkgroupDisplayFormat: "id" | "id_tag" | "tag_id";
  statusAsString: boolean;
  controlWarnRate: number;
}

export interface Recording extends RecordingRules {
  captureDir: string;
  prerollS: number;
  maxRecorders: number;
  /** Save each call's vocoder frames (<call>.frames.jsonl) for diagnosis. */
  captureFrames: boolean;
  /** A call heard on several sites of one system: save the best copy only. */
  dropDuplicateCalls: boolean;
  /** IMBE vocoder for P25 Phase 1 voice. */
  vocoder: "fixed" | "enhanced" | "mbelib";
  /** M4A for the plugins that upload it, and for compressWav (desktop app). */
  m4a?: { encoder: string; bitrateKbps: number };
}

/**
 * A conventional system (Trunk Recorder's conventional / conventionalP25 /
 * conventionalDMR): energy-detected channels with their own short name,
 * squelch, channel file, recording rules, unit names and upload settings.
 * A frequency belongs to one system.
 */
export interface Conventional {
  /** Folder / record name of its calls. */
  shortName: string;
  /** What people call it; the short name is its folder. */
  name?: string;
  enabled: boolean;
  /** Open threshold above the noise floor, dB (each channel may have its own). */
  squelchDb: number;
  /** Desktop: a CSV the channels are read from ("" = the list here). Changed with the channelFile message. */
  channelFile?: string;
  channels: Channel[];
  /** How the channel file last read (from the recorder). */
  channelFileStatus?: string;
  /** Plugins' settings for it (one more system to them), by plugin id. */
  plugins?: Record<string, PluginValues>;
  /** Its own recording rules; each left out is the Recording tab's. */
  recording?: RecordingOverride;
  unitNames?: UnitNames;
}

export interface Config {
  sources: Source[];
  systems: System[];
  /** The conventional systems; the k-th's calls carry `system` conventionalSystem(k). */
  conventional: Conventional[];
  recording: Recording;
  server: { bind: string; port: number; autoStart: boolean };
  /** The plugins, by id (desktop app). Their settings for each system are in the system. */
  plugins?: Record<string, PluginSetup>;
  /** The log (desktop app). */
  log?: LogSettings;
}

export interface Device {
  index: number;
  serial: string;
  product: string;
  /** Why it can't be opened now (another program has it); absent / null = free. */
  busy?: string | null;
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

/** `Call.system` / `listen.system` of the first conventional system; the next count down from it. */
export const CONVENTIONAL = 65535;
/** The `system` of conventional system `k`. */
export const conventionalSystem = (k: number) => CONVENTIONAL - k;
/** Which conventional system a `system` is, or null for a trunked one. */
export const conventionalIndex = (system: number): number | null => (system > CONVENTIONAL - 256 && system <= CONVENTIONAL ? CONVENTIONAL - system : null);

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
  /** Multi-site: the system it is a site of ("p25:bee00.1a2", "group:<name>"), once known. */
  siteGroup?: string | null;
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
  /** Its frequency error as measured on the control channels, ppm (+: signals come in high); null until measured. */
  errorPpm?: number | null;
  /** The correction autoTune applies now, ppm. */
  tunePpm?: number;
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
  /** Conventional FM: the CTCSS tone ("151.4 Hz") or DCS code ("D023N") heard. */
  tone?: string | null;
  state: "recording" | "monitoring";
  reason: "ignored" | "unknown_tg" | "encrypted" | "no_source" | "no_recorder" | null;
  encrypted: boolean;
  emergency: boolean;
  startS: number;
  sources: number[];
  /** The talkgroups patched with this one during the call. */
  patched?: TalkgroupName[];
  /** Multi-site: the other sites carrying this call (one copy is saved). */
  alsoOn?: string[];
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
  /** `tag`: the unit's name (the unit names file or the alias heard, by the system's mode); `tag_ota`: the alias heard. */
  srcList: { src: number; tag?: string; tag_ota?: string }[];
  /** Reception: the channel's level and the noise under it (dBFS), their difference (dB), and the share of voice frames decoded cleanly (digital). */
  signal?: number | null;
  noise?: number | null;
  snr?: number | null;
  clean_voice_pct?: number | null;
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
  /** Fields that have to be filled in (of an object). */
  required?: string[];
  /** This field has to be filled in (as the plugin marked it; SDK 0.1.0 leaves it on the field). */
  "x-required"?: boolean;
  /** A system's short name: drawn as a menu of the systems. */
  "x-system"?: boolean;
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

/** An installed plugin (or one the config names), and what it is. Whether it's on, and its settings, are in the config. */
export interface PluginInfo {
  id: string;
  /** Its executable. */
  path: string;
  /** A build of the user's own (not installed in the plugins folder). */
  custom: boolean;
  /** The GitHub repository it was installed from, when that wasn't the registry (nobody reviewed it). */
  unlistedFrom?: string;
  /** Null when it can't be asked (see `problem`). */
  manifest: PluginManifest | null;
  problem: string | null;
  runtime: PluginRuntime;
}

export interface PluginsList {
  plugins: PluginInfo[];
  /** The M4A encoder found for the config's choice (null: none). */
  encoderFound: string | null;
}

/** A plugin in the registry (crates/trunk-pro/src/plugins/store.rs): one release of it, pinned. */
export interface StoreListing {
  id: string;
  name: string;
  description: string;
  repository: string;
  homepage?: string;
  license: string;
  tier: "official" | "community" | "unlisted";
  version: string;
  api: number;
  tag: string;
  commit: string;
  /** Why this recorder can't install it (no build for this computer, a newer plugin API); null when it can. */
  unavailable: string | null;
}

/** The registry's list, and where it came from. */
export interface PluginStore {
  /** "registry": just fetched; "saved": fetched before (the registry couldn't be reached); "built-in": this version's copy. */
  source: "registry" | "saved" | "built-in";
  /** When it was fetched, Unix seconds. */
  fetched: number | null;
  /** Why the registry couldn't be reached. */
  problem: string | null;
  /** This computer's release target, e.g. universal-apple-darwin. */
  target: string | null;
  plugins: StoreListing[];
}

/** An install as it goes. `key` is what was asked for: an id, or a repository. */
export interface PluginInstall {
  key: string;
  /** The plugin's id, once known. */
  id: string;
  stage: "finding" | "downloading" | "checking" | "installing" | "done" | "failed";
  message: string | null;
}

/** A code a conventional frequency carried (crates/trunk-app/src/heard.rs), in a row's Tone form ("" = none). */
export interface HeardCode {
  code: string;
  /** Calls recorded with it. */
  calls: number;
  /** Transmissions no row took. */
  skipped: number;
  lastMs: number;
}

/** A folder on the recorder's computer and its subfolders; `parent` null at the top. */
export interface DirListing {
  path: string;
  parent: string | null;
  dirs: string[];
  /** Its .json files. */
  files?: string[];
  home: string;
  sep: string;
  error: string | null;
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
      /** The codes each conventional frequency (Hz, as a string) carried, as saved. */
      heard?: Record<string, HeardCode[]>;
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
  | { type: "heard"; heard: Record<string, HeardCode[]> }
  | ({ type: "survey" } & SurveyState)
  | ({ type: "surveySpectrum" } & Spectrum)
  | { type: "devices"; devices: Device[] }
  /** A folder on the recorder's computer (answers listDir). */
  | ({ type: "dir" } & DirListing)
  /** A Trunk Recorder config.json and the talkgroup / channel files it names, by the name it gives (answers readTrConfig). */
  | { type: "trConfig"; path: string; text?: string; files?: Record<string, string>; error: string | null }
  | { type: "error"; message: string }
  | { type: "notice"; message: string }
  | ({ type: "plugins" } & PluginsList)
  | { type: "pluginRuntime"; id: string; runtime: PluginRuntime }
  | ({ type: "pluginStore" } & PluginStore)
  | ({ type: "pluginInstall" } & PluginInstall)
  | { type: "quit" };

export type ToRecorder =
  | { type: "setConfig"; config: Config }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "findRadios" }
  /** A folder's subfolders, for the folder picker ("" = the home folder). */
  | { type: "listDir"; path: string }
  /** A Trunk Recorder config.json (or a folder with one), with the files it names. */
  | { type: "readTrConfig"; path: string }
  /** Live audio: on/off, optionally one system's (CONVENTIONAL: conventional channels) and/or one talkgroup's. */
  | { type: "listen"; on: boolean; system: number | null; talkgroup: number | null }
  /** Link the conventional channels to a CSV (created from the list if new), reload it (same path), or unlink (""). */
  | { type: "channelFile"; index: number; path: string }
  /** Find a system: scan `bands` with source `source`, then monitor the best control channel. */
  | { type: "surveyStart"; source: number; bands: string[]; findGain: boolean }
  /** Stop scanning and monitor this signal (as heard). */
  | { type: "surveyListen"; freqHz: number }
  | { type: "surveyRescan" }
  | { type: "surveyStop" }
  | { type: "plugins" }
  /** A plugin executable on the recorder's computer (a build of the user's own); it's added to the config. */
  | { type: "addPlugin"; path: string }
  /** Forget a plugin (in the config too); an installed copy is deleted. */
  | { type: "removePlugin"; id: string }
  /** The registry's list; `refresh` fetches it again now. */
  | { type: "pluginStore"; refresh?: boolean }
  /** Install (or update) a plugin from the registry. */
  | { type: "installPlugin"; id: string }
  /** Install a plugin from a GitHub release that isn't in the registry (its latest, or `tag`). */
  | { type: "installPlugin"; repository: string; tag?: string }
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
