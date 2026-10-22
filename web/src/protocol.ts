// The messages between the interface and the recorder (crates/trunk-pro/src/
// server.rs). The desktop app carries them over a WebSocket; the web build
// carries the same messages between the page and its engine worker.
//
// This file is the protocol's definition for other interfaces too
// (docs/api/README.md). After changing it, run `npm run schema` to update
// docs/api/protocol.schema.json; trunk-pro's tests check what the recorder
// sends against that.

export type SampleFormat = "cu8" | "cs16" | "cf32";

/**
 * A radio, or a capture to replay (`type`). Every type may have `autoTune`: correct its
 * channels for the frequency error measured on the control channels (Trunk
 * Recorder's autoTune); the error is measured and shown either way. And
 * `guardHz`: left unused at each edge of its band, where the anti-alias filter
 * rolls off (default 75 000; Setup can measure it — `profileSource`).
 */
export type Source = (
  /**
   * `agc`: the tuner's AGC instead of `gainDb`. `bufLen`: bytes per USB transfer (Trunk Recorder's
   * buflen), a multiple of 512, 0 = the driver's 262144; several dongles on one USB 2.0 bus want 65536.
   * `buffers`: transfers in flight, 0 = enough for about 2 MiB. Desktop app only.
   */
  | { type: "rtlsdr"; serial: string; centerHz: number; rateHz: number; gainDb: number; agc: boolean; ppm: number; bufLen?: number; buffers?: number }
  /** USRP through UHD (desktop app, UHD installed). `args` "" = first found; `agc` the device's AGC (B200/B210). */
  | { type: "usrp"; args: string; centerHz: number; rateHz: number; gainDb: number; agc: boolean; antenna: string; ppm: number }
  /**
   * Airspy R2 / Mini through libairspy (desktop app). `gainStep`: a 0–21 step of the linearity or sensitivity
   * table (`gainMode`); "manual": each stage's step, LNA 0–14, mixer 0–15, VGA 0–15, with `agc` the Airspy's own on LNA and mixer.
   */
  | {
      type: "airspy";
      serial: string;
      centerHz: number;
      rateHz: number;
      gainMode: AirspyGainMode;
      gainStep: number;
      lnaStep: number;
      mixerStep: number;
      vgaStep: number;
      agc: boolean;
      biasTee: boolean;
      /** 12-bit samples packed on the USB: a quarter less of the bus (default true). */
      packing: boolean;
      ppm: number;
    }
  /**
   * bladeRF 2.0 micro (xA4 / xA9) or original bladeRF through libbladeRF (desktop app). `serial` "" = the first;
   * `bandwidthHz` the analog filter, 0 = the sample rate; `agc` the board's AGC instead of `gainDb`; `biasTee` bladeRF 2.0 only.
   */
  | { type: "bladerf"; serial: string; centerHz: number; rateHz: number; bandwidthHz: number; gainDb: number; agc: boolean; biasTee: boolean; ppm: number }
  /**
   * Any SDR with a SoapySDR module (desktop app). `args` "" = first found ("driver=hackrf,serial=…");
   * `agc` the device's AGC; else `gainDb` overall (null: left as it is), then each stage in `gains`
   * (HackRF LNA / VGA / AMP …); `settings` device settings ("biastee=true").
   */
  | { type: "soapy"; args: string; centerHz: number; rateHz: number; agc: boolean; gainDb: number | null; gains: Record<string, number>; antenna: string; settings: string; ppm: number }
  | { type: "file"; path: string; centerHz: number; rateHz: number; realtime: boolean; format?: SampleFormat }
) & { autoTune?: boolean; guardHz?: number };

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
  bladerf: DriverState;
  soapy?: SoapyState;
}

/**
 * A conventional channel. `talkgroup` (TG Number) is what calls are filed
 * under; absent: the talkgroup the air names (P25, DMR, NXDN), else the
 * frequency in kHz. `squelchDb` defaults to the section's.
 */
export interface Channel {
  freqHz: number;
  mode: "fm" | "am" | "p25" | "dmr" | "nxdn48" | "nxdn96";
  name: string;
  talkgroup?: number;
  /** The code it records: FM's CTCSS tone or DCS code ("151.4", "D023N"), P25's NAC ("293"), DMR's colour code ("1"), NXDN's RAN ("5"); absent / "": any. */
  code?: string;
  /** DMR: record only this slot (1, 2). */
  slot?: number;
  /** P25, DMR, NXDN: record only transmissions naming this talkgroup. */
  matchTalkgroup?: number;
  description?: string;
  tag?: string;
  group?: string;
  squelchDb?: number;
  /** FM / AM: the channel filter's width, kHz (4–20); absent = the mode's (FM 11, AM 11 or 16 from 225 MHz). */
  bandwidthKhz?: number;
  enabled: boolean;
  /** Digital rows: its calls' carrier offsets score its source's AutoTune (a repeater output to trust, not radios on simplex). Absent: off. */
  autoTune?: boolean;
  /** The plugins taking finished calls its calls go to (ids, or copies' labels such as "rdioscanner#2"); absent: every plugin; []: none. */
  sendTo?: string[];
}

/** How a conventional system's analog channels are demodulated; each left out is the default. */
export interface Demod {
  /** FM: undo 750 µs pre-emphasis (default true). */
  fmDeemphasis?: boolean;
  /** AM: how fast the level follows the carrier (default fast: 50 ms; slow: 0.5 s). */
  amLevel?: "fast" | "slow";
  /** AM: audio high-pass corner, Hz (default 200; 0 = none). */
  amLowCutHz?: number;
  /** Carrier held this long before the audio opens, ms (default 30). */
  squelchOpenMs?: number;
  /** Audio kept open this long after the carrier goes, ms (default 2). */
  squelchHangMs?: number;
}

/** Who is licensed on a frequency: its call sign, who holds it (as the FCC has it; absent: not looked up), and where that came from: the user (never replaced), the air (a Motorola base station ID), or a license lookup. */
export interface FccLicense {
  callSign: string;
  name?: string;
  from?: "user" | "air" | "lookup";
}
/** A system's FCC licenses by frequency (Hz on the 6.25 kHz raster, as a string). Optional, for reference; plugins are told them. */
export type FccLicenses = Record<string, FccLicense>;

/** A system's record in the community directory: its ID, its version when last shared, and when (Unix ms). */
export interface DirectoryLink {
  id: string;
  rev: number;
  sharedAt?: number;
}

/** A shared system's sharing: waiting (to decode it a while first), shared (when last), or its last error. */
export interface ShareStatus {
  state: "waiting" | "shared" | "error";
  lastShared: number | null;
  error: string | null;
}

/**
 * A site's identity (NAC, WACN, System ID are hex on the air): what a site in
 * the config expects (absent: any), and what a control channel said of itself
 * in status and the survey (absent: not heard yet). Compare with `!= null`.
 */
export interface SiteIdentity {
  nac?: number | null;
  wacn?: number | null;
  sysId?: number | null;
  rfss?: number | null;
  site?: number | null;
}

/** A SmartNet band plan the survey learned, in Trunk Recorder's names (Hz; offset a channel number). */
export interface SmartnetBandplan {
  /** "800_standard" | "800_reband" | "800_splinter" | "900" | "400_custom". */
  bandplan: string;
  bandplanBaseHz?: number;
  bandplanSpacingHz?: number;
  bandplanOffset?: number;
  bandplanHighHz?: number;
}

/**
 * A trunked system: its talkgroups, radios, rules and plugin settings, and
 * the sites it is recorded from — each a control channel to follow. A call
 * heard on several of its sites is one call (saved once, from the site that
 * heard it best).
 */
export interface System {
  shortName: string;
  /** What people call it ("County Public Safety"); the short name is its folder. */
  name?: string;
  /** Its FCC licenses by frequency (control, voice): see FccLicenses. */
  fcc?: FccLicenses;
  /** Share it with the community directory (trunkrecorder.pro/directory): its sites, channels, and the
   *  talkgroups heard (numbers, how often, encrypted), not their names. P25, SmartNet, DMR and NXDN. */
  share?: boolean;
  /** The directory's record of it, from its last share (shares land on it). */
  directory?: DirectoryLink;
  /** "smartnet": a Motorola SmartNet / SmartZone control channel (voice P25 or analog FM).
   *  "dmr": a trunked DMR site (Capacity Plus, Capacity Max, Connect Plus, Tier III); every
   *  control channel and `dmrChannelsHz` frequency is watched.
   *  "nxdn": an NXDN site (Type-C: a control channel; Type-D / IDAS: every repeater listed);
   *  every control channel and `nxdnChannelsHz` frequency is watched.
   *  "edacs": an EDACS Standard or Networked site (voice analog FM or ProVoice); `lcnTableHz`
   *  is its channel list. */
  type: "p25" | "smartnet" | "dmr" | "nxdn" | "edacs";
  /** NXDN: "typeC" (a control channel; the default) or "typeD" (IDAS distributed: list every repeater). */
  nxdnType?: "typeC" | "typeD";
  /** SmartNet: the voice of a talkgroup never heard granted. */
  defaultMode?: "digital" | "analog";
  enabled: boolean;
  /** Its own recording rules; each left out is the Recording tab's. */
  recording?: RecordingOverride;
  /** Names for its radios (Trunk Recorder's unitTagsFile and unitTagsMode). */
  unitNames?: UnitNames;
  /** Desktop: the P25 encryption keys it decrypts with, a key source's name (Config's `keys`). */
  keys?: string;
  /** Plugins' settings for this system, by plugin id (each plugin's `system_config` schema); a list for several copies of one. */
  plugins?: Record<string, SystemPluginValues>;
  /** How long plugins wait for its calls; these come before the whole recorder's (see DelayRule). */
  pluginDelays?: DelayRule[];
  /** Where it is recorded from: one site, or several of one system. */
  sites: Site[];
}

/** One site of a system: a control channel to follow, and how. */
export interface Site {
  /** Its name, unique in its system (absent: the system's short name). The talkgroup file's
   *  Preferred Site names it; calls and plugins say it. */
  name?: string;
  /** The community directory's ID for it, from the system's last share. */
  directoryId?: string;
  enabled: boolean;
  controlChannelsHz: number[];
  modulation: "auto" | "fsk4" | "qpsk";
  /** Only follow a control channel with this identity (this site, not a neighbour). */
  expect: SiteIdentity;
  /** Voice channels the survey heard (for placing sources). */
  voiceChannelsHz: number[];
  /** SmartNet: "800_standard" | "800_reband" | "800_splinter" | "900" | "400_custom" (with base / spacing / offset / high; offset a channel number). */
  bandplan?: string;
  bandplanBaseHz?: number;
  bandplanSpacingHz?: number;
  bandplanOffset?: number;
  bandplanHighHz?: number;
  /** DMR: logical channel number → frequency, Hz (Trunk Recorder's lcnTable); the rest is learned.
   *  NXDN: the channel table, channel number → frequency (Type-D: repeater number → frequency).
   *  EDACS: the site's channel list, LCN (1–25) → frequency; checked against the air. */
  lcnTableHz?: Record<string, number>;
  /** DMR: voice frequencies to watch besides the control channels (Trunk Recorder's channels). */
  dmrChannelsHz?: number[];
  /** DMR: only this colour code. */
  colorCode?: number;
  /** NXDN: the control channel's (Type-D: the repeaters') rate; default "nxdn48". */
  nxdnRate?: "nxdn48" | "nxdn96";
  /** NXDN: voice frequencies to watch besides the control channels (Type-D: the repeaters).
   *  A grant to a channel number not in `lcnTableHz` is learned when its call comes up on one. */
  nxdnChannelsHz?: number[];
  /** NXDN: only this RAN. */
  ran?: number;
  /** EDACS: the system scrambles its control channel with ESK 0xA0. */
  esk?: boolean;
}

/**
 * Plugins wait this long for the calls its fields pick, each left out
 * picking all. When several apply, one that narrows another (a system's
 * over every system's, a talkgroup's over its system's, a plugin's over
 * every plugin's) replaces it; of the rest, the longest wins.
 */
export interface DelayRule {
  /** Plugin ids (one copy: "openmhz#2"). Absent: every plugin. */
  plugins?: string[];
  /** Numbers, ranges ("1000-1099") and patterns ("507*"). Absent: every talkgroup. */
  talkgroups?: string[];
  /** Seconds (0: none, as for what it narrows). */
  delayS: number;
}

/** A plugin, as the config has it: on or off, and its settings for the whole recorder. */
export interface PluginSetup {
  enabled: boolean;
  /** As its `config` schema describes them. */
  settings?: PluginValues;
  /** Run this executable instead of the installed plugin (a build of the user's own). */
  path?: string;
  /** Give it calls decrypted with the recorder's keys (their audio and files). Off: none are. */
  shareDecrypted?: boolean;
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
  /** Desktop: keep each call's .wav once every upload plugin has handled it (with archiveM4a, Trunk Recorder's audioArchive). */
  archiveWav: boolean;
  /** Desktop: keep an .m4a of each call (Trunk Recorder's compressWav). */
  archiveM4a: boolean;
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
  controlWarnRatePerS: number;
}

export interface Recording extends RecordingRules {
  captureDir: string;
  prerollS: number;
  maxRecorders: number;
  /** Save each digital call's vocoder frames (<call>.sdr; NXDN <call>.frames.jsonl) for diagnosis. */
  captureFrames: boolean;
  /** A call heard on several sites of one system: save the best copy only. */
  dropDuplicateCalls: boolean;
  /** IMBE vocoder for P25 Phase 1 voice. */
  vocoder: "fixed" | "enhanced" | "mbelib";
  /** M4A for the plugins that upload it, and for compressWav (desktop app). */
  m4a?: { encoder: string; bitrateKbps: number };
  /**
   * Desktop: files only the upload plugins need wait in RAM, not on the disk
   * (macOS: a RAM disk the app makes; Linux: /dev/shm). `dir`: a RAM-backed
   * folder of your own instead.
   */
  ramSpool?: { enabled: boolean; sizeMb: number; dir?: string };
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
  /** Its channels' FCC licenses by frequency (they can be different licensees'): see FccLicenses. */
  fcc?: FccLicenses;
  enabled: boolean;
  /** Open threshold above the noise floor, dB (each channel may have its own). */
  squelchDb: number;
  demod?: Demod;
  /** Desktop: a CSV the channels are read from ("" = the list here). Changed with the channelFile message. */
  channelFile?: string;
  channels: Channel[];
  /** How the channel file last read (from the recorder). */
  channelFileStatus?: string;
  /** Plugins' settings for it (one more system to them), by plugin id; a list for several copies of one. */
  plugins?: Record<string, SystemPluginValues>;
  /** How long plugins wait for its calls (as System's). */
  pluginDelays?: DelayRule[];
  /** Its own recording rules; each left out is the Recording tab's. */
  recording?: RecordingOverride;
  unitNames?: UnitNames;
  /** Desktop: the encryption keys its P25 channels decrypt with (as System's). */
  keys?: string;
}

/** A key source that is a key container with no password file: unlocked with a password typed in (desktop). */
export interface LockableKeys {
  name: string;
  /** The systems that decrypt with it. */
  systems: string[];
  unlocked: boolean;
}

/** Where a set of encryption keys comes from: file names only, never a key (desktop). */
export interface KeySource {
  /** A keys file (JSON) or a KFDtool key container (`.ekc`), absolute or relative to the config file's folder. */
  file: string;
  /** A container's password, in a file of its own. */
  passwordFile?: string;
}

export interface Config {
  sources: Source[];
  systems: System[];
  conventional: Conventional[];
  recording: Recording;
  server: {
    /** The address every interface listens on. */
    bind: string;
    /** The built-in interface's port. */
    port: number;
    autoStart: boolean;
    /** Web pages from other origins that may use the built-in interface's API ("http://host:port", "null" a file, "*" any). */
    allowedOrigins?: string[];
    /** The other interfaces, by id, each on a port of its own (desktop app; docs/interfaces). */
    interfaces?: Record<string, InterfaceSetup>;
    /** Where the trunkrecorder.pro API is, for the survey's license lookup (absent: https://trunkrecorder.pro). */
    trpApi?: string;
  };
  /** The plugins, by id (desktop app). Their settings for each system are in the system. */
  plugins?: Record<string, PluginSetup>;
  /** How long plugins wait for calls, on every system (desktop app); a system's own come first. */
  pluginDelays?: DelayRule[];
  /** Encryption key sources, by name; a system names one in its `keys` (desktop app). */
  keys?: Record<string, KeySource>;
  /** The log (desktop app). */
  log?: LogSettings;
  /** What the dashboard watches on the computer (desktop app). */
  monitor?: MonitorSettings;
}

export interface MonitorSettings {
  /** Where to check the internet answers (host:port, a TCP connect every 15 s). Empty: no checks. */
  probeHosts: string[];
  /** The time server asked how far off the clock is (SNTP every 15 min; port 123 unless given). It only measures. Empty: not asked. */
  timeServer: string;
  /** The clock off by this much or more, ms: a warning, and calls marked clock_off (a problem at 5 s, or 4 times this). */
  clockWarnMs: number;
}

/**
 * What a page may do with the recorder's API. "listen": see what the recorder does and play its
 * calls, change nothing (its config comes without plugin settings and key sources). "control":
 * everything the built-in interface can.
 */
export type ApiAccess = "listen" | "control";

/** An interface besides the built-in one: static web files, served with the API beside them on a port of its own. */
export interface InterfaceSetup {
  port: number;
  /** Served (false: kept, not served). */
  enabled: boolean;
  access: ApiAccess;
  /** A folder of your own (absolute, ~/…, or relative to the config file's folder); absent: the copy installed from the store. */
  path?: string;
}

/** What an interface says it is: interface.json in its folder. */
export interface InterfaceManifest {
  /** Lowercase letters, digits and -. */
  id: string;
  name: string;
  version: string;
  description: string;
  /** The interface API it's written for (hello's `api`). */
  api: number;
  /** What it needs. */
  access: ApiAccess;
  license?: string;
  homepage?: string;
  repository?: string;
  /** Pictures of it: files in its folder (PNG, JPEG or WebP, at most 6). The built-in interface's port serves
   * installed ones at /interfaces/<id>/screenshots/<n>. */
  screenshots?: { path: string; caption?: string }[];
}

/** An interface on this recorder, and how it's doing. Its port, access and on/off are the config's. */
export interface InterfaceInfo {
  id: string;
  /** The folder it's served from. */
  folder: string;
  /** Installed from the store (false: a folder of your own). */
  installed: boolean;
  /** Served this run only (`trunk-pro --ui <folder>`); not in the config. */
  dev: boolean;
  /** Its interface.json (null: it has none). */
  manifest: InterfaceManifest | null;
  /** The repository it was installed from. */
  repository: string | null;
  /** Installed from the store's list (false: a GitHub release nobody reviewed, or a folder of your own). */
  reviewed: boolean;
  port: number;
  access: ApiAccess;
  /** Being served on its port now. */
  serving: boolean;
  /** Why it isn't served (no index.html, its port taken…); null when it's fine or off. */
  problem: string | null;
}

export interface InterfacesList {
  interfaces: InterfaceInfo[];
  /** The interface API this recorder speaks. */
  api: number;
}

/** An interface in the store (crates/trunk-pro/src/interfaces/store.rs): one release of it, pinned. */
export interface InterfaceListing {
  id: string;
  name: string;
  description: string;
  repository: string;
  homepage?: string;
  license: string;
  tier: "official" | "community" | "unlisted";
  version: string;
  api: number;
  /** What it needs: installed with this access. */
  access: ApiAccess;
  tag: string;
  commit: string;
  /** Its release archive (web files: the same on every computer). */
  asset: { url: string; sha256: string };
  /** Pictures of it, each a file of its release. The built-in interface's port serves them, checked, at /store/screenshots/<sha256>. */
  screenshots?: { url: string; sha256: string; caption?: string }[];
  /** Why this recorder can't install it (a newer interface API); null when it can. */
  unavailable: string | null;
}


export interface Device {
  index: number;
  serial: string;
  product: string;
  /** Why it can't be opened now (another program has it); absent / null = free. */
  busy?: string | null;
  /** The USB bus (host controller) it's plugged into, desktop app; dongles on one share its bandwidth. */
  bus?: string | null;
}

export type Phase = "idle" | "starting" | "running" | "stopping";

export interface PhaseState {
  phase: Phase;
  error: string | null;
  ended: boolean;
}

/** One system being recorded: trunked (with a site running) or conventional. `index` is what calls and audio carry as `system`. */
export interface SystemStatus {
  index: number;
  shortName: string;
  activeCalls: number;
  recording: number;
  /** Its conventional frequencies with a signal on them now. */
  conventionalOpen: number;
  callsConcluded: number;
  /** Its sharing with the community directory; null when it isn't shared. */
  share?: ShareStatus | null;
  /** Its sites following their control channels, in order (conventional: none). */
  sites: SiteStatus[];
}

/** A patch standing now: the supergroup, and the talkgroups patched into it. */
export interface Patch {
  supergroup: TalkgroupName;
  members: TalkgroupName[];
}

/** One running site, as its control channel describes it. `index` is what calls carry as `site`. */
export interface SiteStatus {
  index: number;
  /** Its name (a system of one site: the system's short name). */
  name: string;
  nowS: number;
  controlChannelHz: number | null;
  identity: SiteIdentity;
  good: number;
  bad: number;
  modulation: string | null;
  activeCalls: number;
  recording: number;
  callsConcluded: number;
  /** The control channel is another system's / site's: why (its grants are not followed). */
  mismatch: string | null;
  /** Every call sign its base station IDs say, each with the channel it's for (a site's licenses can be one a channel). */
  callsigns?: { callSign: string; freqHz: number | null }[];
  /** P25 SYNC_BCST: the system's own clock less the recording's, ms (positive: the recording's is behind), and when it was heard, Unix s. */
  radioClock?: { offsetMs: number; at: number; valid: boolean } | null;
}

/** One running site's detail (decodeDetail): what its control channel announces. */
export interface SiteDetail {
  index: number;
  name: string;
  /** Neighbouring sites its control channel announces. */
  adjacent: { sysId: number; rfss: number; site: number; freqHz: number }[];
  /** A trunked DMR site. */
  dmr: DmrSiteStatus | null;
  /** An NXDN site. */
  nxdn: NxdnSiteStatus | null;
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

export interface NxdnSiteStatus {
  kind: "NXDN Type-C" | "NXDN Type-D";
  /** The control channel's (Type-D: the repeaters') rate, once heard. */
  rate: "nxdn48" | "nxdn96" | null;
  /** SITE_INFO: the site's location ID. */
  location: { category: number; system: number; site: number } | null;
  ran: number | null;
  /** Direct Frequency Assignment, when the site uses it: grants carry frequencies from this base and step. */
  dfa: { baseHz: number; stepHz: number } | null;
  /** Channel number → frequency: from the config, or learned from the air. */
  channels: { channel: number; freqHz: number; configured: boolean }[];
  /** Channel numbers granted whose frequency isn't known yet. */
  unknownChannels: number[];
  /** Every watched frequency: control (Type-C), its RAN, repeater number (Type-D), and the call on it now. */
  carriers: { freqHz: number; control: boolean; ran: number | null; repeater: number | null; call: { talkgroup: TalkgroupName; source: number } | null }[];
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
  /** Every system's short name, by the number its calls and live audio carry as `system` (trunked systems first, then conventional). */
  systemNames: string[];
}

export interface SourceStatus {
  index: number;
  label: string;
  centerHz: number;
  rateHz: number;
  rateMeasured: number;
  dropped: number;
  errors: number;
  /** The last error, for 5 minutes after it. */
  lastError: string | null;
  /** When it was, Unix seconds. */
  lastErrorS?: number | null;
  ended: boolean;
  /** Its frequency error, ppm (+: signals come in high): the median of its last control channel and call measurements; null until measured. */
  errorPpm?: number | null;
  /** The correction autoTune applies now, ppm. */
  tunePpm?: number;
  /** The correction is an earlier run's score (nothing measured yet). */
  tuneSeeded?: boolean;
}

export interface CallView {
  id: number;
  /** Its system's number this run (EngineStatus.systemNames names it). */
  system: number;
  systemName: string;
  /** The site it is on (its name; none for a conventional system's call). */
  site?: string;
  talkgroup: number;
  /** Conventional P25 / DMR / NXDN: the talkgroup the air named (a channel row may file the call under another). */
  talkgroupOta?: number | null;
  alphaTag: string;
  freqHz: number;
  slot: number | null;
  analog: boolean;
  /** An NXDN call: its rate. */
  nxdn: "nxdn48" | "nxdn96" | null;
  /** NXDN: the RAN its frames carried. */
  ran: number | null;
  /** Conventional FM: the CTCSS tone ("151.4 Hz") or DCS code ("D023N") heard. */
  tone?: string | null;
  state: "recording" | "monitoring";
  reason: "ignored" | "unknown_tg" | "encrypted" | "no_source" | "no_recorder" | null;
  encrypted: boolean;
  /** Its encrypted voice is being decrypted (the system has the key). */
  decrypted: boolean;
  emergency: boolean;
  startS: number;
  sources: number[];
  /** The talkgroups patched with this one during the call. */
  patched?: TalkgroupName[];
  /** The other sites of its system carrying this call, by name (one copy is saved). */
  alsoOn?: string[];
}

export interface LogLine {
  /** Seconds into the recording (the decoder's lines; 0 for the recorder's own). */
  timeS: number;
  /** The recorder's own lines (not the decoder's): when, Unix seconds, and how serious ("info", "warning", "error"…). Warnings and errors go to every connection, the rest to those watching `log`. */
  time?: number;
  level?: string;
  kind: string;
  text: string;
  /** The system's short name, for its messages, and the site's name. */
  system?: string;
  site?: string;
}

/** Trunk Recorder's call JSON: each call's .json file, and `record` in `concluded` and `hello.history`. */
export interface CallRecord {
  /** The call's number since the recorder started. */
  call_num: number;
  short_name?: string;
  /** The site its copy was kept from (a system of one site: its short name). */
  site?: string;
  talkgroup: number;
  /** Conventional P25 / DMR / NXDN: the talkgroup the air named (`talkgroup` is what the call is filed under, which a channel row may set to another number). */
  talkgroup_ota?: number;
  talkgroup_tag: string;
  talkgroup_description: string;
  /** The talkgroup file's Tag and Category columns. */
  talkgroup_group_tag: string;
  talkgroup_group: string;
  /** Hz. */
  freq: number;
  /** How far off its channel the voice was, Hz (0: not measured). */
  freq_error: number;
  /** Unix seconds and ms. */
  start_time: number;
  stop_time: number;
  start_time_ms: number;
  stop_time_ms: number;
  /** Seconds and ms of audio. */
  call_length: number;
  call_length_ms: number;
  /** 1 / 0. */
  emergency: number;
  encrypted: number;
  /** The cipher its encryption sync named: P25 ALGID and key ID; -1 when none was heard (absent in calls saved before). */
  algid?: number;
  key_id?: number;
  /** 1: its encrypted voice was decrypted, at least in part (absent in calls saved before). */
  decrypted?: number;
  /** true: its times may be off: the recorder's clock was measured 250 ms or more off, or the operating system said it wasn't synchronized, when it ended (absent otherwise). */
  clock_off?: boolean;
  phase2_tdma: number;
  duplex: number;
  /** Its P25 priority and service-option mode bit. */
  priority: number;
  mode: number;
  /** TDMA / DMR slot (0 otherwise). */
  tdma_slot: number;
  /** P25: the NAC its voice carried (conventional) or its site's (trunked), in hex as channel files write it ("293"); "" otherwise (absent in calls saved before). */
  nac?: string;
  /** DMR color code; -1 otherwise. */
  color_code: number;
  /** NXDN RAN; -1 otherwise (absent in calls saved before NXDN). */
  ran?: number;
  audio_type: "analog" | "digital" | "digital tdma";
  /** The kind of system it came on (absent in calls saved before). */
  system_type?: "p25" | "smartnet" | "dmr" | "nxdn" | "edacs" | "conventional";
  /** What its voice was: a conventional row's mode, or a trunked grant's (absent in calls saved before). */
  voice_mode?: "p25" | "p25_phase2" | "dmr" | "nxdn48" | "nxdn96" | "provoice" | "fm" | "am";
  /** Conventional analog: the tone it was matched on ("ctcss" / "dcs"), "search" (identified, not required), else "off". */
  tone_mode: "ctcss" | "dcs" | "search" | "off";
  /** The tone heard ("151.4 Hz", "D023N"), or "". */
  tone_detected: string;
  tone_confidence: number;
  /** Always 0 (Trunk Recorder's fields). */
  source_num: number;
  recorder_num: number;
  /** Reception: the channel's level and the noise under it (dBFS), their difference (dB), and the share of voice frames decoded cleanly (digital). */
  signal?: number | null;
  noise?: number | null;
  snr?: number | null;
  clean_voice_pct?: number | null;
  /** Every talkgroup patched with this one, its own included (only when patched). */
  patched_talkgroups?: number[];
  /** The frequencies it was on (one here): `time` Unix s, `pos` / `len` s into the audio. */
  freqList: { freq: number; time: number; pos: number; len: number; error_count: number; spike_count: number }[];
  /** Voice-frame errors over the call (digital), by interval: `pos` / `len` s into the audio. */
  errorList: { pos: number; len: number; frames: number; error_count: number; bad_frames: number; max_frame_errors: number }[];
  /**
   * The radios that talked: `time` Unix s, `pos` s into the audio. `tag`: the unit's name (the unit names
   * file or the alias heard, by the system's mode); `tag_ota`: the alias heard. `lat`, `lon`: where the radio said it was (GPS, decimal degrees), when it did.
   */
  srcList: { src: number; time: number; pos: number; emergency: number; signal_system: string; tag: string; tag_ota: string; lat?: number; lon?: number }[];
}

/** A recorded call: `path` (no extension) under the capture folder. */
export interface CallEntry {
  path: string;
  record: CallRecord;
  /**
   * Whether its audio / its JSON stay in the recordings folder once the
   * plugins are done (Keep audio / Keep call JSON). False: they're gone, or
   * soon will be. Missing: kept (the browser version).
   */
  audio?: boolean;
  json?: boolean;
}

export interface Spectrum {
  source: number;
  centerHz: number;
  rateHz: number;
  bins: number[];
}

/**
 * How a source's band rolls off at its edges (answers profileSource), or why
 * it couldn't be looked at. Spectra run from −rateHz/2 to +rateHz/2 (1024
 * points, dB, uncalibrated); `rows`: a waterfall of the capture, oldest first,
 * whole dB. `lowHz` / `highHz`: how far in from each edge the noise floor is
 * 3 dB or more below its mid-band level (`referenceDb`); `lowDropDb` /
 * `highDropDb`: how far below it is at the very edge.
 */
export type SourceProfile =
  | { source: number; error: string }
  | {
      source: number;
      centerHz: number;
      rateHz: number;
      spectrum: number[];
      floor: number[];
      rows: number[][];
      referenceDb: number;
      lowHz: number;
      highHz: number;
      lowDropDb: number;
      highDropDb: number;
      /** The guard band to leave at each edge, Hz. */
      suggestedGuardHz: number;
    };

// ── first-run survey (crates/trunk-app/src/survey.rs) ────────────────────────

/** A band the survey can scan. */
export interface SurveyBand {
  id: string;
  label: string;
  loHz: number;
  hiHz: number;
  defaultOn: boolean;
}

/** A signal the scan found. `freqHz` as heard (uncorrected); `correctedHz` once the ppm is known. */
export interface SurveyCandidate {
  freqHz: number;
  correctedHz: number | null;
  band: string;
  snrDb: number;
  widthHz: number;
  kind: "control" | "smartnet" | "p25" | "dmrControl" | "dmr" | "nxdnControl" | "nxdn" | "other";
  frames: number;
  good: number;
  bad: number;
  modulation: string;
  identity: SiteIdentity;
  /** DMR: the trunking its control blocks are ("DMR Capacity Plus", …) and its colour code. */
  dmr?: { variant: string | null; colorCode: number | null } | null;
  /** NXDN: its rate and RAN, and (a control channel) its system and site codes. */
  nxdn?: { rate: "nxdn48" | "nxdn96"; ran: number | null; system: number | null; site: number | null } | null;
}

export interface SurveyMonitor {
  freqHz: number | null;
  heardHz: number;
  identity: SiteIdentity;
  /** The frequency the control channel announces for itself. */
  advertisedHz: number | null;
  secondary: number[];
  adjacent: { rfss: number; site: number; sysId: number; freqHz: number }[];
  voice: { freqHz: number; grants: number; tdma: boolean }[];
  idens: number;
  good: number;
  bad: number;
  /** Seconds of air the decoded messages account for (P25), of `elapsedS`. */
  airS?: number;
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
  /** The site it heard. */
  identity: SiteIdentity;
  voiceChannels: number[];
}

/** An FCC license near the user on a frequency the survey heard (the trunkrecorder.pro API's). */
export interface SurveyLicense {
  callSign: string | null;
  licensee: string | null;
  serviceCode: string | null;
  serviceDesc: string | null;
  freqHz: number | null;
  emission: string | null;
  distanceM: number | null;
  bearingDeg: number | null;
  bearingCardinal: string | null;
  city: string | null;
  state: string | null;
  /** "A" active; anything else has lapsed. */
  status: string | null;
  locationKind: string | null;
}

/** Who holds a call sign (the trunkrecorder.pro API's). */
export interface CallSignHolder {
  callSign: string;
  licensee: string | null;
  serviceCode: string | null;
  serviceDesc: string | null;
  status: string | null;
  city: string | null;
  state: string | null;
}

/** The licenses found on one frequency looked up; `historical`: none active on it, so these have lapsed. */
export interface LicenseResultRow {
  freqHz: number;
  historical: boolean;
  licenses: SurveyLicense[];
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

/** A service a plugin talks to, as the plugin reports it. */
export interface PluginEndpoint {
  name: string;
  state: "unknown" | "up" | "degraded" | "down";
  latencyMs?: number;
  lastOk?: number;
  lastError?: string;
}

/** What a plugin says of its work (its `metrics` message; every field optional). */
export interface PluginMetrics {
  queued?: number;
  retrying?: number;
  inFlight?: number;
  retries?: number;
  bytesSent?: number;
  latencyMs?: number;
  /** Unix seconds. */
  lastOk?: number;
  lastError?: number;
  lastErrorText?: string;
  endpoints?: PluginEndpoint[];
  extra?: Record<string, unknown>;
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
  metrics?: PluginMetrics | null;
  /** Unix seconds of its last result, success, failure. */
  lastResult?: number | null;
  lastOk?: number | null;
  lastFail?: number | null;
  /** Its process: restarts, events dropped (fell behind), seconds up. */
  restarts?: number;
  dropped?: number;
  uptimeS?: number | null;
  /** Finished calls waiting out a delay for it. */
  held?: number;
  /** Results per minute, the last hour: [minute start (Unix s), [ok, skipped, failed]]. */
  minutes?: [number, [number, number, number]][];
}

export type PluginValues = Record<string, unknown>;
/** A system's settings for a plugin: one copy of it, or several (each after
 * the first runs as a process of its own, for the systems that have it).
 * `override` in one holds the plugin's own settings it changes for this
 * system (another server, say); those it leaves out are the plugin's. */
export type SystemPluginValues = PluginValues | PluginValues[];

/** The key in a system's settings for a plugin that holds the plugin settings it overrides. */
export const OVERRIDE = "override";

/** A system's settings for a plugin, one per copy of it. */
export function pluginCopies(v: SystemPluginValues | undefined): PluginValues[] {
  return v === undefined || v === null ? [] : Array.isArray(v) ? v : [v];
}

/** A copy's own settings (without what it overrides). */
export function copyOwn(v: PluginValues | undefined): PluginValues {
  const { [OVERRIDE]: _, ...own } = v ?? {};
  return own;
}

/** The plugin settings a copy overrides. */
export function copyOverride(v: PluginValues | undefined): PluginValues {
  const o = v?.[OVERRIDE];
  return o && typeof o === "object" && !Array.isArray(o) ? (o as PluginValues) : {};
}

/** An installed plugin (or one the config names), and what it is. Whether it's on, and its settings, are in the config. */
export interface PluginInfo {
  id: string;
  /** Its executable. */
  path: string;
  /** A build of the user's own (not installed in the plugins folder). */
  custom: boolean;
  /** The GitHub repository it was installed from, when that wasn't the registry (nobody reviewed it). */
  unlistedFrom?: string | null;
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
export interface PluginListing {
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
  /** Its release downloads, by target ("aarch64-unknown-linux-gnu", "universal-apple-darwin", …). */
  assets: Record<string, { url: string; sha256: string }>;
  /** Why this recorder can't install it (no build for this computer, a newer plugin API); null when it can. */
  unavailable: string | null;
}

/** Which store: the plugins' (the registry) or the interfaces'. */
export type StoreKind = "plugin" | "interface";

/** A store's list, and where it came from. */
interface StoreList {
  /** "registry": just fetched; "saved": fetched before (it couldn't be reached); "built-in": this version's copy. */
  source: "registry" | "saved" | "built-in";
  /** When it was fetched, Unix seconds. */
  fetched: number | null;
  /** Why it couldn't be reached. */
  problem: string | null;
}

export type Store =
  | (StoreList & {
      kind: "plugin";
      /** This computer's release target, e.g. universal-apple-darwin. */
      target: string | null;
      listings: PluginListing[];
    })
  | (StoreList & { kind: "interface"; listings: InterfaceListing[] });

/** One listing of either store. */
export type StoreListing = PluginListing | InterfaceListing;

/** An install (of a plugin or an interface) as it goes. `key` is what was asked for: an id, or a repository. */
export interface InstallProgress {
  kind: StoreKind;
  key: string;
  /** What it installs, once known. */
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

// ── The dashboard ────────────────────────────────────────────────────────────
//
// Series are named by `/`-separated keys (crates/trunk-app/src/stats):
//   src/<label>/{noise,peak,clipPct,samples,dropped,errors,ppm,tune}
//   sys/<shortName>/{active,recording,calls,airMs,audioBytes,notSaved,dup}
//   sys/<shortName>/cc/{good,bad,locked,syncs,nidFails,flywheels,eqResets,frames,sep,offset,signal,noise,snr,inSync,deviation}
//   sys/<shortName>/why/<recorded|monitored|ignored|encrypted|unknown_tg|no_source|no_recorder>
//   sys/<shortName>/voice/{frames,errors,bad}
//   all/{calls,airMs,audioBytes}  day/{calls,airS,audioBytes}  eng/{load,recording,channels,recorders}
//   plat/{cpu,proCpu,proCores,proMem,mem,memAvail,swap,memPsi,memLevel,load1,temp,throttledUs,piThrottled}
//   plat/disk/<recordings|data>/{free,freePct}  net/{rx,tx,errors,up,rtt,dns}  net/probe/<target>/{up,rtt}
//   plg/<id>/{state,ok,skipped,failed,dropped,restarts,held,queued,latency,bytes}
// Running totals arrive as rates per second; everything else as it is.

/** What a dashboard watches: `spectrum:<source>`, `log`, `rf:<source>`, `decode:<shortName>`, `platform`. */
export type Topic = string;

/** One series from the history: points every `stepS` from `t0`; null where nothing was measured. */
export interface SeriesData {
  t0: number;
  stepS: number;
  /** Averages. */
  v: (number | null)[];
  /** Lowest and highest in each step. */
  lo: (number | null)[];
  hi: (number | null)[];
  /** The minutes measured in each step (a rate × n × 60 is the amount). */
  n: number[];
}

/** A channel listened to now, with its power and the noise floor under it. */
export interface ChannelSnapshot {
  system: string;
  /** The site it is for (none: a conventional channel). */
  site?: string | null;
  freqHz: number;
  source: number;
  kind: "control" | "voice" | "carrier" | "conventional";
  powerDb: number;
  noiseDb: number;
  snrDb: number;
  offsetHz: number | null;
  /** The receiver's eye opening (~10+ clean, ~1 noise). */
  quality: number | null;
  /** CQPSK's phase error, RMS degrees (under 10 clean, ~25 noise). */
  phaseErrDeg: number | null;
  calls: number;
  /** A conventional channel: its mode. */
  mode?: Channel["mode"];
  /** A conventional channel: its rows' names, each once. */
  names?: string[];
}

/** Something notable (crates/trunk-app/src/stats/events.rs). Which fields come depends on `kind`. */
export type MonitorEvent = { type: "monitorEvent" } & MonitorEventBody;

export interface MonitorEventBody {
  /** Unix seconds. */
  t: number;
  level: "info" | "ok" | "warn" | "bad";
  kind:
    | "tgFirstSeen"
    | "unitFirstSeen"
    | "tgActive"
    | "unitActive"
    | "unitAffiliated"
    | "callVolume"
    | "controlLost"
    | "controlRegained"
    | "sourceDrops"
    | "sourceClipping"
    | "sourceRecovered"
    | "pluginHealth"
    | "linkDown"
    | "linkUp"
    | "diskLow"
    | "spoolLow"
    | "spoolFull"
    | "spoolRecovered"
    | "clockOff"
    | "clockOk"
    | "clockStep";
  /** A system's short name (talkgroup, radio, call volume, control channel events). */
  system?: string;
  /** Control channel events: the site's own name, when it has one other than its system's. */
  site?: string;
  talkgroup?: number;
  alphaTag?: string;
  unit?: number;
  alias?: string;
  units?: number[];
  perMin?: number;
  freqHz?: number | null;
  /** A source's series name (its label). */
  source?: string;
  perS?: number;
  pct?: number;
  plugin?: string;
  state?: string;
  message?: string;
  target?: string;
  downS?: number;
  path?: string;
  freePct?: number;
  /** spoolLow: nearly full. */
  critical?: boolean;
  /** spoolFull: calls that went to the recordings folder for want of room. */
  calls?: number;
  /** clockOff: what's wrong, and whether it's badly off. */
  why?: string;
  bad?: boolean;
  /** clockStep: how far the wall clock jumped, ms (positive: forward). */
  byMs?: number;
}

/** The computer, as last sampled (desktop app). Missing values are listed in `unavailable`. */
export interface PlatformInfo {
  os: string;
  arch: string;
  host: string | null;
  cores: number;
  /** "docker" | "podman" | "kubernetes" | "lxc", inside one. */
  container: string | null;
  cpuLimitCores: number | null;
  memTotal: number;
  uptimeS: number;
  /** "recordings", "data", and "spool" (the RAM spool, with its `kind` and calls `overflowed` to the disk). */
  disks: { name: string; path: string; mount: string; totalBytes: number; freeBytes: number; kind?: string; overflowed?: number }[];
  /** macOS: whether Spotlight indexes the recordings folder (null: not checked yet, or not macOS). */
  spotlight?: { path: string; state: "indexed" | "excluded" | "unknown"; why: string } | null;
  probes: ProbeRow[];
  clock: ClockInfo;
  /** With the `platform` topic: each core's use, %, and the interfaces' traffic since the last sample. */
  perCore: number[] | null;
  interfaces: { name: string; rx: number; tx: number; state: string }[] | null;
  unavailable: { field: string; why: string }[];
}

/** Something checked again and again and how it's been answering: an internet target, or the time server. */
export interface ProbeRow {
  target: string;
  ok: boolean | null;
  rttMs: number | null;
  lastOk: number | null;
  lastFail: number | null;
  downSince: number | null;
  tries: number;
  fails: number;
}

/**
 * The computer's clock: what the operating system says of it, and what the time server
 * (monitor.timeServer) measured. Read only: the OS's time service keeps the clock.
 */
export interface ClockInfo {
  /** The warning threshold, ms (monitor.clockWarnMs). */
  warnMs: number;
  /** The OS's word (Linux, macOS; null elsewhere). maxErrorMs: its own loose bound. */
  os: { synced: boolean; maxErrorMs: number } | null;
  /** The time service keeping the clock, when it says (Linux: chrony, systemd-timesyncd). lastSync: Unix s. */
  service: { name: string; server: string | null; stratum: number | null; lastSync: number | null } | null;
  /** While recording: the wall clock less the clock calls are stamped by (the wall clock at Start, carried on monotonically), ms. */
  driftMs: number | null;
  /** The radio systems' own clocks (P25 SYNC_BCST) against the recording's: offsetMs > 0 when the recording's is behind; `sane`: near enough to be the right time at all; `valid`: the system vouches for it (only then is it gone by). */
  radio: { site: string; offsetMs: number; at: number; sane: boolean; valid: boolean }[];
  /** The time server asked (null: none set, or not asked yet). */
  server: string | null;
  /** Its last answer: offsetMs > 0 when this clock is behind; `at` Unix s; `current`: recent enough to go by. */
  measured: { offsetMs: number; delayMs: number; stratum: number; at: number; current: boolean } | null;
  /** Why the last query failed, while it fails. */
  error: string | null;
  /** What's wrong with the clock, decided by the recorder (the light and the events go by it). */
  problem: { level: "warn" | "bad"; cause: "clock" | "recording" | "radio" | "os"; why: string } | null;
  /** The time server's check: tries, failures, last answer. */
  check: ProbeRow | null;
}

/** A talkgroup as heard (radioQuery talkgroups / tg). */
export interface TgRow {
  tg: number;
  alphaTag: string;
  /** In the system's talkgroup file. */
  known: boolean;
  ignore: boolean;
  first: number;
  last: number;
  /** In the query's hours. */
  calls: number;
  secs: number;
  totalCalls: number;
  encPct: number;
  /** The ciphers its calls' encryption sync named, most recent first. */
  ciphers: CipherSeen[];
  /** First heard in the last day (once the system has a baseline). */
  new: boolean;
  /** Calls per hour, oldest first. */
  hourly: number[];
}

/** A cipher heard on a talkgroup: P25 ALGID and key ID. */
export interface CipherSeen {
  algid: number;
  keyId: number;
  /** What the ALGID is called ("ADP", "AES-256", …). */
  name: string;
  calls: number;
  /** Unix seconds. */
  last: number;
}

/** A radio as heard (radioQuery units / unit). */
export interface UnitRow {
  unit: number;
  alias: string;
  first: number;
  last: number;
  lastTx: number | null;
  tx: number;
  txSecs: number;
  /** The talkgroup it's affiliated with, and since when. */
  aff: number | null;
  affT: number | null;
  reg: boolean | null;
  /** [talkgroup, transmissions], most first. */
  tgs: [number, number][];
  /** [radio, calls together], most first. */
  partners: [number, number][];
  new: boolean;
}

/** A frequency's voice decoding (radioQuery freqs). */
export interface FreqRow {
  freqHz: number;
  calls: number;
  frames: number;
  errors: number;
  errPerFrame: number | null;
  totalCalls: number;
  allErrPerFrame: number | null;
  /** Voice frames that couldn't be decoded, % (in the query's hours; all time). */
  badPct: number | null;
  allBadPct: number | null;
  snr: number | null;
  freqError: number | null;
  clean: number | null;
  last: number;
  /** Bad frames % by hour, oldest first. */
  hourly: (number | null)[];
}

export interface LengthHistogram {
  /** Bin starts, s (the last is open-ended). */
  edges: number[];
  counts: number[];
}

export interface RadioSummary {
  since: number;
  talkgroups: number;
  tgs24h: number;
  units: number;
  units1h: number;
  units24h: number;
  newUnits24h: number;
  newTgs: { tg: number; first: number; alphaTag: string }[];
  unknownTgs24h: number;
  /** Heard long enough that "new" means new. */
  baseline: boolean;
}

/** Answers radioQuery; which fields come depends on `what`. */
export type RadioResult = { type: "radioResult" } & RadioResultBody;

export interface RadioResultBody {
  id: number;
  what: "summary" | "talkgroups" | "units" | "tg" | "unit" | "freqs" | "lengths";
  error?: string;
  system?: string;
  /** summary */
  systems?: Record<string, RadioSummary>;
  /** talkgroups, units, freqs */
  hours?: number;
  total?: number;
  rows?: (TgRow | UnitRow | FreqRow)[];
  /** tg / unit: the one asked about (null: never heard). */
  tg?: number;
  unit?: number;
  row?: TgRow | UnitRow | null;
  /** tg: calls per hour over the week; who talks on it; who's affiliated with it; its call lengths. */
  week?: number[];
  talkers?: { unit: number; alias: string; n: number; lastTx: number }[];
  affiliated?: { unit: number; alias: string }[];
  lengths?: LengthHistogram;
  /** unit: its latest transmissions, affiliations, talkgroups, and the radios it's heard with. */
  recent?: { t: number; tg: number; alphaTag: string; secs: number }[];
  affiliations?: { t: number; tg: number; alphaTag: string }[];
  tgs?: { tg: number; alphaTag: string; n: number }[];
  partners?: { unit: number; alias: string; n: number }[];
  /** lengths */
  histogram?: LengthHistogram;
}

/**
 * Something a recorder can do beyond recording (`hello.capabilities`).
 * - `service`: a program of its own on a computer: quit, start recording when it starts, log files, M4A encoding, files by path (capture files, channel files, browsing folders, a Trunk Recorder config, the recordings folder, the RAM spool, keeping audio and JSON once plugins are done)
 * - `drivers`: radio drivers beyond RTL-SDR (USRP, Airspy, SoapySDR), finding radios, an RTL-SDR's USB transfer sizes
 * - `plugins`: plugins (listing, adding, the store, their settings)
 * - `interfaces`: other interfaces, served on ports of their own
 * - `host`: the computer's and plugins' figures (`host`)
 * - `keys`: key containers opened with a password
 */
export type Capability = "service" | "drivers" | "plugins" | "interfaces" | "host" | "keys";

export type FromRecorder =
  /**
   * The last message of a connection's greeting. Every message before it said
   * how the recorder is now, as ordinary messages (state, config, talkgroups,
   * units, heard, history, radios, survey, monitorEvent for each recent event,
   * host; with control access also devices, keys, plugins and interfaces): the
   * interface is caught up. Wait for it before treating what's missing as
   * missing (a system with no talkgroups message has no talkgroup file).
   */
  | {
      type: "hello";
      version: string;
      platform: string;
      /** The interface API this recorder speaks (docs/interfaces). */
      api: number;
      /** The config file (control access only). */
      configPath?: string;
      surveyBands: SurveyBand[];
      /** What this recorder can do beyond recording: show only what works. The desktop app can do all of them; the browser build, none. */
      capabilities: Capability[];
    }
  /** The latest calls, newest first: on connecting (then `concluded` for each new one). */
  | { type: "history"; entries: CallEntry[] }
  /** A system's talker aliases as saved (Trunk Recorder's unitTagsOTA CSV): one for each system with some, on connecting (then `unitAlias` for each new one). */
  | { type: "units"; system: string; csv: string }
  /** Which key containers are unlocked, after an unlock, a lock or a config change. */
  | { type: "keys"; sources: LockableKeys[] }
  | { type: "radios"; radios: Radios }
  | ({ type: "state" } & PhaseState)
  /** The config, and its version: what an edit of it (setConfig) is sent with. */
  | { type: "config"; config: Config; version: number }
  /** Answers setConfig: the config's version now; `error` when the edit wasn't taken (it was of an older version — the config changed elsewhere meanwhile — or bad) or wasn't saved. */
  | { type: "configSaved"; version: number; error?: string }
  /** A system's talkgroup file (Trunk Recorder's CSV; "" for none): one for each system on connecting, and again whenever one changes. Kept beside the config, not in it. */
  | { type: "talkgroups"; system: string; csv: string }
  | { type: "status"; status: EngineStatus; sources: SourceStatus[]; load: number; calls: CallView[] }
  | ({ type: "spectrum" } & Spectrum)
  | { type: "log"; lines: LogLine[] }
  | { type: "concluded"; entry: CallEntry }
  /** A call's upload plugins are done and it kept other than its entry said (an upload failed, so its files stay). */
  | { type: "callFiles"; path: string; audio: boolean; json: boolean }
  /** A radio's talker alias, newly heard on system `system` (its short name). */
  | { type: "unitAlias"; system: string; unit: number; alias: string }
  | { type: "heard"; heard: Record<string, HeardCode[]> }
  | ({ type: "survey" } & SurveyState)
  | ({ type: "surveySpectrum" } & Spectrum)
  /** Answers licenseQuery: the licenses found on each frequency, or why there are none. */
  | { type: "licenseResult"; id: number; results: LicenseResultRow[]; error: string | null }
  /** Answers callSignQuery: each active call sign's holder (unknown or lapsed ones left out), or why there are none. */
  | { type: "callSignResult"; id: number; results: CallSignHolder[]; error: string | null }
  | ({ type: "sourceProfile" } & SourceProfile)
  | { type: "devices"; devices: Device[] }
  /** A folder on the recorder's computer (answers listDir). */
  | ({ type: "dir" } & DirListing)
  /** A Trunk Recorder config.json and the talkgroup / channel files it names, by the name it gives (answers readTrConfig). */
  | { type: "trConfig"; path: string; text?: string; files?: Record<string, string>; error: string | null }
  | { type: "error"; message: string }
  | { type: "notice"; message: string }
  | ({ type: "plugins" } & PluginsList)
  | { type: "pluginRuntime"; id: string; runtime: PluginRuntime }
  /** A store's list (answers `store`). */
  | ({ type: "store" } & Store)
  /** An install from a store or a GitHub release as it goes, to every connection with control access. */
  | ({ type: "installProgress" } & InstallProgress)
  /** Control access only (as the plugin messages). */
  | ({ type: "interfaces" } & InterfacesList)
  /** Every second while recording: each series' value now (see the series names above). */
  | { type: "stats"; t: number; values: Record<string, number> }
  /** Every 2 s (desktop app): the computer's and the plugins' series, and the computer's own figures. */
  | { type: "host"; t: number; values: Record<string, number>; platform: PlatformInfo }
  /** Topic rf:<source>, every second: the noise floor across the band (64 slices, dBFS per bin) and the channels on it. */
  | { type: "rfDetail"; source: number; profile: number[]; channels: ChannelSnapshot[] }
  /** Topic decode:<shortName>, every second: a system's decoding, for those watching its `decode:<short name>` topic: its channels, each running site's detail, the patches standing now (each once, however many sites hear it). */
  | { type: "decodeDetail"; system: string; channels: ChannelSnapshot[]; sites: SiteDetail[]; patches: Patch[] }
  | ({ type: "monitorEvent" } & MonitorEventBody)
  /** Answers subscribe: what this connection now watches. */
  | { type: "subscribed"; topics: Topic[] }
  /** Answers statsQuery: `loading` while the history files are still being read. */
  | { type: "statsResult"; id: number; from: number; to: number; loading: boolean; series: Record<string, SeriesData> }
  | ({ type: "radioResult" } & RadioResultBody)
  /** Answers olderCalls; `more`: there are older ones still. */
  | { type: "olderCalls"; before: number; entries: CallEntry[]; more: boolean }
  | { type: "quit" };

export type ToRecorder =
  /** The whole config, edited from `version` (the last `config` message's): refused when the config has changed since. */
  | { type: "setConfig"; config: Config; version: number }
  /** System `system`'s talkgroup file (Trunk Recorder's CSV; "" for none). Everyone gets it back as a `talkgroups` message. */
  | { type: "setTalkgroups"; system: string; csv: string }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "findRadios" }
  /** A folder's subfolders, for the folder picker ("" = the home folder). */
  | { type: "listDir"; path: string }
  /** A Trunk Recorder config.json (or a folder with one), with the files it names. */
  | { type: "readTrConfig"; path: string }
  /** Live audio: on/off, optionally one system's (its short name) and/or one talkgroup's. */
  | { type: "listen"; on: boolean; system: string | null; talkgroup: number | null }
  /** Open a key container (a key source with no password file) with its password; held in memory only. */
  | { type: "unlockKeys"; source: string; password: string }
  /** Forget a key container's keys: its systems stop decrypting at once. */
  | { type: "lockKeys"; source: string }
  /** Link the conventional channels to a CSV (created from the list if new), reload it (same path), or unlink (""). */
  | { type: "channelFile"; index: number; path: string }
  /** Find a system: scan `bands` with source `source`, then monitor the best control channel. */
  | { type: "surveyStart"; source: number; bands: string[]; findGain: boolean }
  /** Stop scanning and monitor this signal (as heard). */
  | { type: "surveyListen"; freqHz: number }
  | { type: "surveyRescan" }
  | { type: "surveyStop" }
  /** Who is licensed on these frequencies within `radiusM` of (lat, lon): sent to trunkrecorder.pro; answered with licenseResult. */
  /** Who holds these call signs (heard on the air): sent to trunkrecorder.pro, no location; answered with callSignResult. */
  | { type: "callSignQuery"; id: number; callSigns: string[] }
  | { type: "licenseQuery"; id: number; lat: number; lon: number; radiusM: number; signals: { freqHz: number; widthHz?: number }[] }
  /** Look at source `source`'s band (tuned to `centerHz`; a capture keeps its own) for its roll-off; answered with sourceProfile. Not while recording. */
  | { type: "profileSource"; source: number; centerHz: number }
  | { type: "plugins" }
  /** A plugin executable on the recorder's computer (a build of the user's own); it's added to the config. */
  | { type: "addPlugin"; path: string }
  /** Forget a plugin (in the config too); an installed copy is deleted. */
  | { type: "removePlugin"; id: string }
  /** A store's list (the plugins' registry, or the interfaces'); `refresh` fetches it again now. */
  | { type: "store"; kind: StoreKind; refresh?: boolean }
  /** Install (or update) a plugin or interface from its store. A new interface is served at once on the next free port. */
  | { type: "install"; kind: StoreKind; id: string }
  /** Install from a GitHub release that isn't in the store (its latest, or `tag`); an interface may only listen until given control. */
  | { type: "install"; kind: StoreKind; repository: string; tag?: string }
  /** The interfaces besides the built-in one (answered with `interfaces`). */
  | { type: "interfaces" }
  /** Serve a folder on the recorder's computer as an interface (its interface.json names it, else the folder does). */
  | { type: "addInterface"; path: string }
  /** Stop serving an interface and forget it; an installed copy is deleted. */
  | { type: "removeInterface"; id: string }
  /** What this connection watches (replaces the last): costly messages only go to those that ask. */
  | { type: "subscribe"; topics: Topic[] }
  /** The history of series (names, or prefixes ending in `*`) over `range` back from now, or `from`–`to` (Unix s), in about `points` points. */
  | { type: "statsQuery"; id: number; series: string[]; range?: "10m" | "1h" | "6h" | "24h" | "7d"; from?: number; to?: number; points?: number }
  /** Calls that started before `before` (Unix ms), newest first, from the last 24 hours (the Listen page's "Load older"). */
  | { type: "olderCalls"; before: number; limit: number }
  /** The radio registry: talkgroups, radios, frequencies heard. `key`: the talkgroup or radio for "tg" / "unit". */
  | { type: "radioQuery"; id: number; what: "summary" | "talkgroups" | "units" | "tg" | "unit" | "freqs" | "lengths"; system?: string; key?: number; hours?: number; limit?: number }
  | { type: "quit" };

/** Live audio: one 20 ms (or longer) chunk of a call, 8 kHz. */
export interface AudioChunk {
  callId: number;
  /** The system's number this run: EngineStatus.systemNames names it. */
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
