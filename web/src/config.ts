// Config helpers for the setup form (the recorder validates again).

import type { Channel, Config, Conventional, Device, DirectoryLink, FccLicense, FccLicenses, FoundRadio, LogSettings, Radios, RecordingOverride, Site, SiteIdentity, Source, System, SystemStatus, UnitNames } from "./protocol.ts";
import { normalizeTalkgroupCsv, splitCsvLine } from "./talkgroups.ts";
import { defaultConfig, parseChannelCsv, readConfig, usableHalfWidth } from "./rules.ts";

// The recorder's rules, as it applies them (rules.ts):
export { autoCenter, bandplanProblem, channelsToCsv, defaultConfig, DEFAULT_GUARD_HZ, nameProblem, parseChannelCsv, planCenters, readConfig, resolvedCenters, startProblem, usableHalfWidth } from "./rules.ts";

/**
 * The settings the recorder reads only when it starts. Left out are those it
 * takes while recording (plugins, their settings and delays,
 * the log, the dashboard's monitor, the web server's origins / home /
 * interfaces; with `liveGain`, the desktop recorder, a radio's gain) and what
 * only matters when the app launches (server address, start on launch).
 */
export function restartKey(c: Config, liveGain: boolean): string {
  return JSON.stringify({
    sources: liveGain ? c.sources.map(withoutLiveGain) : c.sources,
    // (Sharing and the directory's link apply live: a first share saves the link.)
    systems: c.systems.map(({ plugins: _a, pluginDelays: _d, share: _s, directory: _l, ...s }) => ({ ...s, sites: s.sites.map(({ directoryId: _i, ...x }) => x) })),
    conventional: c.conventional.map(({ plugins: _a, pluginDelays: _d, channelFileStatus: _f, ...s }) => s),
    recording: c.recording,
  });
}

/** A source without the gain settings a running recorder takes from it (as runtime::live_gain decides). */
function withoutLiveGain(x: Source): Source {
  const k = SOURCE_KINDS[x.type];
  const agc = "agc" in x && x.agc;
  if (!k.liveGain || (agc && !k.liveAgc)) return x;
  const y: Record<string, unknown> = { ...x };
  for (const f of [...k.liveGain, ...(k.liveAgc ? ["agc"] : [])]) delete y[f];
  return y as Source;
}

/** A new RTL-SDR's gain, dB (higher overloads its front end near strong transmitters). */
export const RTL_DEFAULT_GAIN_DB = 25.4;

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** A fresh config (the web build; the desktop app gets its own from the recorder). */
/** The log's defaults (the recorder's LogSettings). */
export function defaultLog(): LogSettings {
  return defaultConfig().log!;
}

export const USRP_RATES = [2_400_000, 4_000_000, 5_000_000, 6_400_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000];
/** Airspy R2: 10 / 2.5; Mini: 6 / 3 (and 10 with newer firmware). */
export const AIRSPY_RATES = [10_000_000, 6_000_000, 3_000_000, 2_500_000];

export function newDongle(): Source {
  return { type: "rtlsdr", serial: "", centerHz: 0, rateHz: 2_400_000, gainDb: RTL_DEFAULT_GAIN_DB, agc: false, ppm: 0, bufLen: 0, buffers: 0 };
}
export function newUsrp(): Source {
  return { type: "usrp", args: "", centerHz: 0, rateHz: 8_000_000, gainDb: 40, agc: false, antenna: "", ppm: 0 };
}
export function newAirspy(): Source {
  return { type: "airspy", serial: "", centerHz: 0, rateHz: 6_000_000, gainMode: "linearity", gainStep: 14, lnaStep: 10, mixerStep: 10, vgaStep: 10, agc: false, biasTee: false, packing: true, ppm: 0 };
}
/** Offered for a bladeRF (any rate 0.52–61.44 MSPS can be typed; above ~40 MSPS needs USB 3 and a fast CPU). */
export const BLADERF_RATES = [2_400_000, 4_000_000, 5_000_000, 6_000_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000, 25_000_000, 30_720_000];
export function newBladerf(): Source {
  return { type: "bladerf", serial: "", centerHz: 0, rateHz: 10_000_000, bandwidthHz: 0, gainDb: 30, agc: false, biasTee: false, ppm: 0 };
}
/** Offered for SoapySDR devices; any rate the device takes can be typed. */
export const SOAPY_RATES = [2_000_000, 2_400_000, 2_500_000, 3_000_000, 6_000_000, 8_000_000, 10_000_000];
export function newSoapy(): Source {
  return { type: "soapy", args: "", centerHz: 0, rateHz: 8_000_000, agc: true, gainDb: null, gains: {}, antenna: "", settings: "", ppm: 0 };
}
export function newFile(): Source {
  return { type: "file", path: "", centerHz: 0, rateHz: 2_400_000, realtime: true, format: "cu8" };
}

export type Os = "windows" | "macos" | "linux";

/** A driver library a kind of radio needs (desktop app), and how to install it. */
export interface Driver {
  /** "libairspy", "UHD". */
  name: string;
  /** Install commands, and what to do on Windows. */
  macos: string;
  linux: string;
  windows: string;
  /** A step after installing ("run uhd_images_downloader"). */
  then?: string;
}

/**
 * What the interface knows of each kind of source — Setup's card, the setup
 * guide, the survey, the import. Adding a radio starts here (and in protocol.ts).
 */
export interface SourceKind {
  label: string;
  /** A new source of this kind. */
  fresh: () => Source;
  /** Sample rates offered; `freeRate`: any other can be typed too. */
  rates: number[];
  freeRate?: boolean;
  rateHint?: string;
  /** The field that names its device ("" = the first found). */
  device?: "serial" | "args";
  /** The library it needs, loaded at run time (desktop app only). */
  driver?: Driver;
  /** The name SoapySDR lists it under, when this source drives it natively. */
  soapyDriver?: string;
  /** Its gain settings, which the desktop recorder takes while recording (runtime::live_gain). */
  liveGain?: string[];
  /** Its AGC switches on or off while recording too (else only with AGC off is the gain live). */
  liveAgc?: boolean;
  /** The radios it covers, when its name doesn't say ("HackRF, SDRplay…"). */
  covers?: string;
}

export const SOURCE_KINDS: Record<Source["type"], SourceKind> = {
  rtlsdr: { label: "RTL-SDR", fresh: newDongle, rates: SAMPLE_RATES, device: "serial", soapyDriver: "rtlsdr", liveGain: ["gainDb"], liveAgc: true },
  usrp: {
    label: "USRP",
    fresh: newUsrp,
    rates: USRP_RATES,
    freeRate: true,
    rateHint: "MSPS; wider costs more CPU",
    device: "args",
    driver: { name: "UHD", macos: "brew install uhd", linux: "sudo apt install libuhd-dev uhd-host", windows: "Ettus's UHD installer", then: "run uhd_images_downloader" },
    soapyDriver: "uhd",
    liveGain: ["gainDb"],
  },
  airspy: {
    label: "Airspy",
    fresh: newAirspy,
    rates: AIRSPY_RATES,
    rateHint: "R2: 10 or 2.5; Mini: 6 or 3",
    device: "serial",
    driver: { name: "libairspy", macos: "brew install airspy", linux: "sudo apt install libairspy0", windows: "put airspy.dll (from airspy-tools) next to trunk-pro.exe" },
    soapyDriver: "airspy",
    liveGain: ["gainMode", "gainStep", "lnaStep", "mixerStep", "vgaStep"],
    liveAgc: true,
  },
  bladerf: {
    label: "bladeRF",
    fresh: newBladerf,
    rates: BLADERF_RATES,
    freeRate: true,
    rateHint: "MSPS, 0.52–61.44 (the original bladeRF: up to 40)",
    device: "serial",
    driver: { name: "libbladeRF 2.x", macos: "brew install libbladerf", linux: "sudo apt install libbladerf2 bladerf-fpga-hostedxa4", windows: "Nuand's bladeRF installer" },
    soapyDriver: "bladerf",
    liveGain: ["gainDb"],
  },
  soapy: {
    label: "SoapySDR",
    fresh: newSoapy,
    rates: SOAPY_RATES,
    freeRate: true,
    rateHint: "MSPS, one the device supports",
    device: "args",
    driver: { name: "SoapySDR and your radio's module", macos: "brew install soapysdr soapyhackrf", linux: "sudo apt install soapysdr0.8-module-all", windows: "PothosSDR or radioconda" },
    covers: "HackRF, SDRplay, LimeSDR, PlutoSDR…",
    liveGain: ["gainDb"],
  },
  file: { label: "Capture file", fresh: newFile, rates: [...SAMPLE_RATES, 8_000_000, 10_000_000] },
};

/** The kinds of source that need a driver (and so the desktop app): the recorder reports each in `radios`. */
export type DriverKind = keyof Radios;
export const DRIVER_KINDS = (Object.keys(SOURCE_KINDS) as Source["type"][]).filter((k): k is DriverKind => !!SOURCE_KINDS[k].driver);
/** A source that needs a driver, and so the desktop app. */
export const needsDriver = (x: Source): x is Extract<Source, { type: DriverKind }> => !!SOURCE_KINDS[x.type].driver;

/** The device a source names ("" = the first found, or a file). */
export function deviceId(x: Source): string {
  const f = SOURCE_KINDS[x.type].device;
  return f ? ((x as Record<string, unknown>)[f] as string) : "";
}

/** A device's key ("airspy:…"): two sources with one aren't both used. */
export const deviceKey = (type: Source["type"], id: string) => `${type}:${id}`;
export const sourceKey = (x: Source) => deviceKey(x.type, deviceId(x));

/** A new source of `type` for device `id` (`found`: as its driver listed it — it starts at a rate the radio runs at). */
export function sourceFor(type: Source["type"], id: string, found?: FoundRadio): Source {
  const k = SOURCE_KINDS[type];
  const s = (k.device ? { ...k.fresh(), [k.device]: id } : k.fresh()) as Source;
  s.rateHz = rateFor(s.rateHz, found);
  return s;
}

/**
 * The radio a source names ("" = the first found), as its driver listed it; undefined when
 * it isn't listed (not attached, not searched for, or not a driver's).
 */
export function foundRadio(radios: Radios | null | undefined, x: Source): FoundRadio | undefined {
  if (!needsDriver(x) || !radios) return undefined;
  const devices: FoundRadio[] = radios[x.type]?.devices ?? [];
  const id = deviceId(x).trim();
  if (!id) return devices[0];
  // (A bladeRF may be named by the start of its serial.)
  return devices.find((d) => (x.type === "bladerf" ? !!d.serial?.toLowerCase().startsWith(id.toLowerCase()) : (d.serial ?? d.args) === id));
}

/** Whether the radio runs at `hz`, as it said when opened (true when that isn't known). */
export function rateOk(hz: number, found: FoundRadio | undefined): boolean {
  if (found?.rates?.length) return found.rates.includes(hz);
  if (found?.rateRange) return hz >= found.rateRange[0] && hz <= found.rateRange[1];
  return true;
}

/** `hz` if the radio runs at it (or that isn't known), else the nearest it does: its first rate, or the end of its range. */
export function rateFor(hz: number, found: FoundRadio | undefined): number {
  if (rateOk(hz, found)) return hz;
  if (found?.rates?.length) return found.rates[0];
  const [lo, hi] = found!.rateRange!;
  return Math.min(Math.max(hz, lo), hi);
}

/** What the radio runs at, MSPS ("10 or 2.5", "0.52–61.44"), when it said; null when not. */
export function ratesText(found: FoundRadio | undefined): string | null {
  const m = (hz: number) => Math.round(hz / 1e4) / 100;
  if (found?.rates?.length) return found.rates.map(m).join(" or ");
  if (found?.rateRange) return `${m(found.rateRange[0])}–${m(found.rateRange[1])}`;
  return null;
}

/** "Airspy R2", "bladeRF 2.0": the kind with the model its driver found, if it said. */
export function radioName(type: Source["type"], found: FoundRadio | undefined): string {
  const kind = SOURCE_KINDS[type].label;
  const model = found?.model;
  return !model ? kind : model.toLowerCase().startsWith(kind.toLowerCase()) ? model : `${kind} ${model}`;
}

export interface Detected {
  key: string;
  kind: Source["type"];
  name: string;
  detail: string;
  source: Source;
  /** Why it can't be used now (another program has it), or null. */
  busy: string | null;
}

/** Every radio the recorder can see (its RTL-SDRs, what the drivers found), as a source each. */
export function detectRadios(devices: Device[], r: Radios | null): Detected[] {
  const out: Detected[] = devices.map((d) => ({
    key: deviceKey("rtlsdr", d.serial),
    kind: "rtlsdr",
    name: d.product || "RTL-SDR",
    detail: d.serial ? `Serial ${d.serial}` : "RTL-SDR",
    source: { ...newDongle(), serial: d.serial },
    busy: d.busy ?? null,
  }));
  // Each radio with a driver of its own, as its driver found them.
  const native = new Set(devices.length ? ["rtlsdr"] : []);
  for (const kind of DRIVER_KINDS) {
    const k = SOURCE_KINDS[kind];
    if (kind === "soapy") continue;
    const found = r?.[kind].devices ?? [];
    if (found.length && k.soapyDriver) native.add(k.soapyDriver);
    for (const d of found) {
      const id = (k.device === "serial" ? d.serial : d.args) ?? "";
      out.push({ key: deviceKey(kind, id), kind, name: k.label, detail: d.label, source: sourceFor(kind, id, d), busy: null });
    }
  }
  // SoapySDR also lists radios with a driver of their own here; those are shown once, natively.
  for (const d of r?.soapy?.devices ?? []) {
    if (native.has(d.driver)) continue;
    out.push({ key: deviceKey("soapy", d.args), kind: "soapy", name: d.label || d.driver, detail: `SoapySDR · ${d.driver}`, source: sourceFor("soapy", d.args), busy: null });
  }
  // Two dongles with no serial can't be told apart; keep one.
  return out.filter((d, i) => out.findIndex((o) => o.key === d.key) === i);
}

/** A capture's sample format from its name (as the recorder guesses it). */
export function formatFromPath(path: string): "cu8" | "cs16" | "cf32" {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return ["cf32", "cfile", "fc32", "complex"].includes(ext) ? "cf32" : ["cs16", "sc16"].includes(ext) ? "cs16" : "cu8";
}

/** Left unused at each edge of a source's band unless it says otherwise (`guardHz`), Hz. */
/** The conventional channels being recorded: switched on, in a system that is. */
export function enabledChannels(c: Config): Channel[] {
  return (c.conventional ?? []).filter((v) => v.enabled).flatMap((v) => v.channels.filter((ch) => ch.enabled));
}

/** A conventional system with its defaults filled in. */
export function normalizeConventional(x: Partial<Conventional>): Conventional {
  const { fcc: _, ...rest } = x;
  return { shortName: "conv", enabled: true, squelchDb: 8, ...rest, ...fccMap(x.fcc), channels: Array.isArray(x.channels) ? x.channels : [] };
}

/** A new conventional system (not yet in the config), named uniquely: conv, conv2… */
export function newConventional(c: Config, patch: Partial<Conventional> = {}): Conventional {
  return normalizeConventional({ ...patch, shortName: uniqueShortName(c, patch.shortName ?? "conv") });
}

/** Trunk Recorder's trunked DMR settings: `lcnTable` { "<lcn>": Hz } and `channels` (candidate voice frequencies), in Hz (a value under 100000 read as MHz). */
function dmrImport(sys: Record<string, unknown>): SystemInput {
  const hz = (v: unknown) => (typeof v === "number" && v > 0 ? (v < 1e5 ? Math.round(v * 1e6) : v) : undefined);
  const out: SystemInput = { type: "dmr" };
  if (sys.lcnTable && typeof sys.lcnTable === "object") {
    const t: Record<string, number> = {};
    for (const [k, v] of Object.entries(sys.lcnTable as Record<string, unknown>)) if (hz(v)) t[k] = hz(v)!;
    if (Object.keys(t).length) out.lcnTableHz = t;
  }
  if (Array.isArray(sys.channels)) out.dmrChannelsHz = (sys.channels as unknown[]).map(hz).filter((v): v is number => v !== undefined);
  return out;
}

/**
 * The site-lock fields a system's protocol states (the recorder's
 * ControlChannel::identity_fields): the lock compares only these. DMR and
 * NXDN Type-D state none (their sites are told apart by colour code / RAN);
 * NXDN Type-C its system and site codes.
 */
export function siteLockFields(sys: Pick<System, "type" | "nxdnType">): (keyof SiteIdentity)[] {
  if (sys.type === "nxdn") return sys.nxdnType === "typeD" ? [] : ["sysId", "site"];
  return sys.type === "smartnet" ? ["sysId", "site"] : sys.type === "edacs" ? ["site"] : sys.type === "dmr" ? [] : ["nac", "wacn", "sysId", "rfss", "site"];
}

/** A system's type as written: "p25" for one this version doesn't know. */
export function systemType(t: unknown): System["type"] {
  return t === "smartnet" || t === "dmr" || t === "nxdn" || t === "edacs" ? t : "p25";
}

/** Frequencies a watching protocol (DMR, NXDN, EDACS) needs besides the control channels (the recorder's Site::watched_channels). */
export function watchedChannels(sys: Pick<System, "type">, x: Site): number[] {
  if (sys.type === "edacs") return Object.entries(x.lcnTableHz ?? {}).filter(([k, f]) => +k >= 1 && +k <= 25 && f > 0).map(([, f]) => f);
  return sys.type === "dmr" ? (x.dmrChannelsHz ?? []) : sys.type === "nxdn" ? (x.nxdnChannelsHz ?? []) : [];
}

/** A site's fields in an older config's system (before sites, each system was one, its fields at the top). */
const SITE_KEYS = [
  "controlChannelsHz", "modulation", "expect", "voiceChannelsHz", "bandplan", "bandplanBaseHz", "bandplanSpacingHz", "bandplanOffset", "bandplanHighHz",
  "lcnTableHz", "dmrChannelsHz", "colorCode", "nxdnRate", "nxdnChannelsHz", "ran", "esk",
] as const satisfies readonly (keyof Site)[];

/** Every key of System this version knows (the rest are kept as they are: a newer version's, a hand edit's). */
const SYSTEM_KEYS: (keyof System)[] = [
  "shortName", "name", "fcc", "share", "directory", "type", "nxdnType", "defaultMode", "enabled", "recording", "unitNames", "keys", "plugins", "pluginDelays", "signals", "sites",
];

/** Every key of Site this version knows. */
const SITE_FIELDS: readonly string[] = ["name", "directoryId", "enabled", ...SITE_KEYS];

/** A frequency's key in FccLicenses: Hz on the 6.25 kHz raster. */
export const fccKey = (hz: number) => String(Math.round(hz / 6250) * 6250);

/** FCC licenses as the config keeps them: keys on the raster, call signs upper case, empty ones and names left out; absent when there are none. */
function fccMap(x: unknown): { fcc?: FccLicenses } {
  const out: FccLicenses = {};
  for (const [k, v] of Object.entries(plain<Record<string, unknown>>(x) ?? {})) {
    const e = plain<Partial<FccLicense>>(v);
    const callSign = typeof e?.callSign === "string" ? e.callSign.trim().toUpperCase() : "";
    const hz = Number(k);
    if (!callSign || !(hz > 0)) continue;
    const name = typeof e?.name === "string" ? e.name.trim() : "";
    const from = e?.from === "air" || e?.from === "lookup" ? e.from : undefined;
    out[fccKey(hz)] = { callSign, ...(name ? { name } : {}), ...(from ? { from } : {}) };
  }
  return Object.keys(out).length ? { fcc: out } : {};
}

/** Whether some licenses have a call sign: for now, a system is shared only once it has one (Identify, the air, or typed). */
export const hasCallSign = (fcc: FccLicenses | undefined) => Object.values(fcc ?? {}).some((l) => l.callSign.trim() !== "");

/** The trunkrecorder.pro API base the config names (`server.trpApi`), no trailing slash: the FCC lookups, the directory and its map. */
export const trpBase = (c: Config | null | undefined) => (c?.server?.trpApi || "https://trunkrecorder.pro").replace(/\/+$/, "");

/** The license that names a system: its control channels' (in order), else the call sign on the most of its frequencies. */
export function fccPrimary(fcc: FccLicenses | undefined, controlChannelsHz: number[]): FccLicense | null {
  if (!fcc) return null;
  for (const f of controlChannelsHz) if (fcc[fccKey(f)]) return fcc[fccKey(f)];
  const n = new Map<string, { k: number; l: FccLicense }>();
  for (const l of Object.values(fcc)) n.set(l.callSign, { k: (n.get(l.callSign)?.k ?? 0) + 1, l });
  return [...n.values()].sort((a, b) => b.k - a.k || a.l.callSign.localeCompare(b.l.callSign))[0]?.l ?? null;
}

/**
 * A system as code that makes one writes it: the system's fields, with one
 * site's at the top level, or `sites`.
 */
export type SystemInput = Partial<Omit<System, "sites">> & Partial<Omit<Site, "name" | "enabled">> & { sites?: Partial<Site>[] };

/** `x` without the keys in `known`: what this version doesn't know of it, kept as it was (as the recorder's `other`). */
function unknownKeys<T extends object>(x: T, known: readonly string[]): Record<string, unknown> {
  return Object.fromEntries(Object.entries(x).filter(([k]) => !known.includes(k)));
}

/** A plain object (not an array, not null), or undefined. */
const plain = <T extends object>(v: unknown): T | undefined => (v && typeof v === "object" && !Array.isArray(v) ? (v as T) : undefined);

/** A site with defaults filled in, for a system of `type` (fields of another protocol left out); keys this version doesn't know are kept. */
export function normalizeSite(x: Partial<Site>, sys: Pick<System, "type">): Site {
  const type = sys.type;
  return {
    ...unknownKeys(x, SITE_FIELDS),
    ...(x.name?.trim() ? { name: x.name } : {}),
    ...(x.directoryId ? { directoryId: x.directoryId } : {}),
    enabled: x.enabled ?? true,
    controlChannelsHz: Array.isArray(x.controlChannelsHz) ? x.controlChannelsHz : [],
    modulation: x.modulation ?? "auto",
    expect: plain<SiteIdentity>(x.expect) ?? {},
    voiceChannelsHz: Array.isArray(x.voiceChannelsHz) ? x.voiceChannelsHz : [],
    ...(type === "dmr"
      ? {
          ...(x.lcnTableHz && Object.keys(x.lcnTableHz).length ? { lcnTableHz: x.lcnTableHz } : {}),
          ...(x.dmrChannelsHz?.length ? { dmrChannelsHz: x.dmrChannelsHz } : {}),
          ...(typeof x.colorCode === "number" ? { colorCode: x.colorCode } : {}),
        }
      : {}),
    ...(type === "nxdn"
      ? {
          ...(x.nxdnRate === "nxdn96" ? { nxdnRate: "nxdn96" as const } : {}),
          ...(x.lcnTableHz && Object.keys(x.lcnTableHz).length ? { lcnTableHz: x.lcnTableHz } : {}),
          ...(x.nxdnChannelsHz?.length ? { nxdnChannelsHz: x.nxdnChannelsHz } : {}),
          ...(typeof x.ran === "number" ? { ran: x.ran } : {}),
        }
      : {}),
    ...(type === "edacs"
      ? {
          ...(x.lcnTableHz && Object.keys(x.lcnTableHz).length ? { lcnTableHz: x.lcnTableHz } : {}),
          ...(x.esk ? { esk: true } : {}),
        }
      : {}),
    ...(type === "smartnet"
      ? {
          bandplan: x.bandplan ?? "800_standard",
          ...(x.bandplanBaseHz ? { bandplanBaseHz: x.bandplanBaseHz } : {}),
          ...(x.bandplanSpacingHz ? { bandplanSpacingHz: x.bandplanSpacingHz } : {}),
          ...(x.bandplanOffset ? { bandplanOffset: x.bandplanOffset } : {}),
          ...(x.bandplanHighHz ? { bandplanHighHz: x.bandplanHighHz } : {}),
        }
      : {}),
  };
}

/**
 * A system with defaults filled in (configs saved before a field existed);
 * keys this version doesn't know are kept. An older config's system — one
 * site's fields at its top — gets that site.
 */
export function normalizeSystem(x: SystemInput): System {
  const type = systemType(x.type);
  const sites: Partial<Site>[] = Array.isArray(x.sites)
    ? x.sites.filter((s): s is Partial<Site> => !!plain(s))
    : [{ ...Object.fromEntries(SITE_KEYS.filter((k) => k in x).map((k) => [k, x[k]])), ...(x.enabled === false ? { enabled: false } : {}) }];
  return {
    ...unknownKeys(x, [...SYSTEM_KEYS, ...SITE_KEYS]),
    shortName: x.shortName ?? "sys1",
    ...(x.name?.trim() ? { name: x.name } : {}),
    ...fccMap(x.fcc),
    ...(x.share ? { share: true } : {}),
    ...(plain<DirectoryLink>(x.directory)?.id ? { directory: { id: x.directory!.id } } : {}),
    type,
    ...(type === "nxdn" && x.nxdnType === "typeD" ? { nxdnType: "typeD" as const } : {}),
    ...(type === "smartnet" && x.defaultMode === "analog" ? { defaultMode: "analog" as const } : {}),
    enabled: x.enabled ?? true,
    ...(x.recording && Object.keys(x.recording).length ? { recording: x.recording } : {}),
    ...(x.unitNames && (x.unitNames.csv || x.unitNames.mode) ? { unitNames: x.unitNames } : {}),
    ...(x.keys?.trim() ? { keys: x.keys } : {}),
    ...(plain(x.plugins) && Object.keys(x.plugins!).length ? { plugins: x.plugins } : {}),
    ...(x.pluginDelays?.length ? { pluginDelays: x.pluginDelays } : {}),
    ...(x.signals?.length ? { signals: x.signals } : {}),
    sites: sites.map((s) => normalizeSite(s, { type })),
  };
}

/** A config stored by this browser, read as the recorder reads one (older ones upgraded, what it lacks at its default). */
export function storedConfig(raw: unknown): Config {
  return readConfig(raw).config;
}

/** Why a filename format can't be used, or null: filenameFormat.ts. */
export { FILENAME_TOKENS, filenameProblem } from "./filenameFormat.ts";

/** A site being recorded: enabled, with a control channel. */
export const siteActive = (x: Site) => x.enabled && x.controlChannelsHz.length > 0;

/** The systems being recorded (enabled, with a site that is), in the recorder's order (SystemStatus.index). */
export function activeSystems(c: Config): System[] {
  return c.systems.filter((x) => x.enabled && x.sites.some(siteActive));
}

/** The sites being recorded, each with its system, in the recorder's order (SiteStatus.index, a call's `site`). */
export function activeSites(c: Config): { sys: System; site: Site }[] {
  return activeSystems(c).flatMap((sys) => sys.sites.filter(siteActive).map((site) => ({ sys, site })));
}

/** A site's name: its own, else its system's short name. */
export const siteNameOf = (sys: System, x: Site) => x.name?.trim() || sys.shortName;

/** A name for a new site of `sys` none of its sites has: from what its control channel says ("dcfd-1-3"), else "dcfd-2"…. */
export function newSiteName(sys: System, id: SiteIdentity = {}): string {
  const taken = new Set(sys.sites.map((x) => siteNameOf(sys, x)));
  const base = id.site != null ? `${sys.shortName}-${id.rfss != null ? `${id.rfss}-` : ""}${id.site}` : `${sys.shortName}-${sys.sites.length + 1}`;
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

/**
 * A system's colour, by short name: its place among the trunked systems in
 * the config, so it's the same in Setup, on the dashboard and on the
 * waterfall. Conventional systems are muted; a name not in the config, a line.
 */
export function systemColor(c: Config | null | undefined, name: string): string {
  const k = c?.systems.findIndex((x) => x.shortName === name) ?? -1;
  if (k >= 0) return `var(--sys-${k % 6})`;
  return c?.conventional.some((x) => x.shortName === name) ? "var(--muted)" : "var(--line)";
}

/**
 * A short name no system uses yet, trunked or conventional: `base`, else
 * base2, base3… A short name is a system's identity (its folder, how plugins
 * know it), so no two share one.
 */
export function uniqueShortName(c: Config, base: string, except?: System | Conventional): string {
  const clean = base.replace(/[^\w.-]/g, "") || "sys";
  const taken = new Set([...c.systems, ...c.conventional].filter((x) => x !== except).map((x) => x.shortName));
  if (!taken.has(clean)) return clean;
  const stem = clean.replace(/\d+$/, "");
  for (let n = 2; ; n++) if (!taken.has(`${stem}${n}`)) return `${stem}${n}`;
}

/** A new system (not yet in the config), named uniquely. */
export function newSystem(c: Config, patch: SystemInput = {}): System {
  const sys = normalizeSystem(patch);
  sys.shortName = uniqueShortName(c, patch.shortName ?? `sys${c.systems.length + 1}`);
  return sys;
}

/** A SmartNet system's settings from a Trunk Recorder system (its base / spacing / high are MHz; Hz here — a value already in Hz is kept). */
function smartnetImport(sys: Record<string, unknown>): SystemInput {
  const hz = (v: unknown, mhzBelow: number) => (typeof v === "number" && v > 0 ? (v < mhzBelow ? Math.round(v * 1e6) : v) : undefined);
  const out: SystemInput = { type: "smartnet", bandplan: typeof sys.bandplan === "string" ? sys.bandplan : "800_standard" };
  const base = hz(sys.bandplanBase, 1e5);
  const spacing = hz(sys.bandplanSpacing, 1);
  const high = hz(sys.bandplanHigh, 1e5);
  if (base) out.bandplanBaseHz = base;
  if (spacing) out.bandplanSpacingHz = spacing;
  if (high) out.bandplanHighHz = high;
  if (typeof sys.bandplanOffset === "number") out.bandplanOffset = sys.bandplanOffset;
  return out;
}

/** A default short name for a new system, from its site: "p25-<sysid>-<rfss>-<site>", or "sysN". */
export function shortNameFor(c: Config, id: SiteIdentity, smartnet = false): string {
  if (smartnet && id.sysId != null) return uniqueShortName(c, `smartnet-${id.sysId.toString(16)}`);
  if (id.sysId != null && id.site != null) return uniqueShortName(c, `p25-${id.sysId.toString(16)}-${id.rfss ?? 0}-${id.site}`);
  return uniqueShortName(c, `sys${c.systems.length + 1}`);
}

/** Sites of one system: the same WACN and System ID (both known). */
export function sameSystem(a: SiteIdentity, b: SiteIdentity): boolean {
  return a.wacn != null && a.sysId != null && a.wacn === b.wacn && a.sysId === b.sysId;
}

/**
 * Systems set up apart whose sites announce one system on the air (P25:
 * the same WACN and System ID; SmartNet, NXDN Type-C: the same System ID),
 * heard by the running recorder (`live`) or locked: each pair once, for
 * Setup to offer making them one.
 */
export function sameOnTheAir(c: Config, live: SystemStatus[]): [System, System][] {
  const key = (sys: System): string | null => {
    if (sys.type === "dmr" || (sys.type === "nxdn" && sys.nxdnType === "typeD")) return null;
    const heard = live.find((y) => y.shortName === sys.shortName)?.sites.map((x) => x.identity) ?? [];
    const ids: SiteIdentity[] = [...heard, ...sys.sites.map((x) => x.expect)];
    const id = ids.find((i) => i.sysId != null && (sys.type !== "p25" || i.wacn != null));
    return id ? `${sys.type}:${id.wacn ?? ""}.${id.sysId}` : null;
  };
  const keyed = c.systems.map((sys) => [sys, key(sys)] as const);
  const out: [System, System][] = [];
  keyed.forEach(([a, ka], i) => keyed.slice(i + 1).forEach(([b, kb]) => ka && ka === kb && out.push([a, b])));
  return out;
}

const hexText = (v: number | null | undefined) => (v === null || v === undefined ? "?" : v.toString(16).toUpperCase());

/** "WACN BEE00 · SysID 445 · site 1-3" from what is known. */
export function siteText(id: SiteIdentity): string {
  const parts: string[] = [];
  if (id.wacn != null) parts.push(`WACN ${hexText(id.wacn)}`);
  if (id.sysId != null) parts.push(`SysID ${hexText(id.sysId)}`);
  if (id.nac != null) parts.push(`NAC ${hexText(id.nac)}`);
  if (id.site != null) parts.push(id.rfss != null ? `site ${id.rfss}-${id.site}` : `site ${id.site}`);
  else if (id.rfss != null) parts.push(`RFSS ${id.rfss}`);
  return parts.join(" · ");
}

/** The system (and its site) a control channel is already configured on, if any. */
export function systemWithChannel(c: Config, hz: number): System | undefined {
  return c.systems.find((sys) => sys.sites.some((x) => [...x.controlChannelsHz, ...watchedChannels(sys, x)].some((f) => Math.abs(f - hz) < 6_000)));
}

/** The source (index) whose usable band holds `hz`, or -1. */
export function sourceCovering(c: Config, centers: (number | null)[], hz: number): number {
  return c.sources.findIndex((s, i) => centers[i] !== null && Math.abs(hz - (centers[i] ?? 0)) <= usableHalfWidth(s.rateHz, s.guardHz));
}

/** A conventional channel's talkgroup when none is given: its frequency in kHz. */
export function defaultTalkgroup(freqHz: number): number {
  return Math.round(freqHz / 1000);
}

const sameFreq = (a: number, b: number) => Math.abs(a - b) < 1;

/**
 * What each channel files a call under when neither it nor the air names a
 * talkgroup: by its place among the rows on its frequency — the frequency in
 * kHz, then that with a digit (154325, 1543251 …). The recorder's
 * default_talkgroups.
 */
export function defaultTalkgroups(channels: Channel[]): number[] {
  return channels.map((c, i) => {
    const k = channels.slice(0, i).filter((o) => sameFreq(o.freqHz, c.freqHz)).length;
    return k === 0 ? defaultTalkgroup(c.freqHz) : defaultTalkgroup(c.freqHz) * 10 + k;
  });
}

/** A talkgroup for another row on `freqHz`: the first of 1543251, 1543252 … no row uses. */
export function nextTalkgroup(channels: Channel[], freqHz: number): number {
  const used = new Set(channels.map((c, i) => c.talkgroup ?? defaultTalkgroups(channels)[i]));
  let k = 1;
  while (used.has(defaultTalkgroup(freqHz) * 10 + k)) k++;
  return defaultTalkgroup(freqHz) * 10 + k;
}

/** An analog channel's filter width a config may set, kHz, and the demod ranges (the recorder's config.rs). */
export const BANDWIDTH_KHZ: [number, number] = [4, 20];
export const AM_LOW_CUT_MAX_HZ = 600;
export const SQUELCH_OPEN_MAX_MS = 500;
export const SQUELCH_HANG_MAX_MS = 1000;

/** The filter width a channel with none set gets, kHz (dsp::AnalogTuning): AM from 225 MHz (UHF military air) is wider. */
export function defaultBandwidthKhz(ch: Channel): number {
  return ch.mode === "am" && ch.freqHz >= 225e6 ? 16 : 11;
}

/** Widths to pick from for an analog channel, kHz. */
export const BANDWIDTH_PRESETS: Record<"fm" | "am", { khz: number; label: string }[]> = {
  fm: [
    { khz: 11, label: "narrow" },
    { khz: 16, label: "wide FM" },
  ],
  am: [
    { khz: 6, label: "8.33 ch." },
    { khz: 11, label: "25 kHz ch." },
    { khz: 16, label: "wide" },
  ],
};

export function mhzCell(hz: number): string {
  const [int, frac = ""] = (hz / 1e6).toFixed(6).split(".");
  return `${int}.${frac.replace(/0+$/, "").padEnd(4, "0")}`;
}

export function parseFreqList(text: string): number[] {
  return text
    .split(/[\s,;]+/)
    .map((t) => t.trim())
    .filter(Boolean)
    .map(Number)
    .filter((v) => Number.isFinite(v) && v > 0)
    .map((v) => (v < 10_000 ? Math.round(v * 1e6) : Math.round(v)));
}

export function formatMhz(hz: number, digits = 5): string {
  return (hz / 1e6).toFixed(digits);
}

/** A gain in dB to a tenth, for display (an RTL-SDR step is an f32: 33.799999…). */
export function formatGain(db: number): string {
  return String(Math.round(db * 10) / 10);
}

/** osmosdr device strings → SoapySDR's driver names, for radios with no source of their own here. */
const OSMOSDR_DRIVERS: Record<string, string> = {
  hackrf: "hackrf",
  airspyhf: "airspyhf",
  lime: "lime",
  limesdr: "lime",
  sdrplay: "sdrplay",
  plutosdr: "plutosdr",
  redpitaya: "redpitaya",
  xtrx: "xtrx",
};

/**
 * A Trunk Recorder osmosdr device string → SoapySDR device arguments, or null
 * when it's a radio with its own source (rtl=, airspy=, bladerf=, uhd). "soapy=0,driver=sdrplay"
 * keeps its arguments; "hackrf=0" → "driver=hackrf"; "hackrf=0000000000000000a06063c8"
 * → "driver=hackrf,serial=…". Other osmosdr options (bias=1, buffers=…) are left out
 * (an RTL-SDR source keeps buflen= and buffers=).
 */
export function osmosdrToSoapy(dev: string): string | null {
  const parts = dev.split(",").map((p) => p.trim()).filter(Boolean);
  const [key, value = ""] = (parts[0] ?? "").split("=");
  if (key.toLowerCase() === "soapy") return parts.slice(1).join(",");
  const driver = OSMOSDR_DRIVERS[key.toLowerCase()];
  if (!driver) return null;
  // A small number is osmosdr's device index; anything longer is a serial.
  return /^\d{1,2}$/.test(value) || value === "" ? `driver=${driver}` : `driver=${driver},serial=${value}`;
}

/**
 * Something an import couldn't finish, for the user to (the setup form
 * highlights it until it's done). Systems are named by short name.
 */
export type ImportTodo =
  /** Its talkgroup file wasn't loaded. */
  | { kind: "talkgroups"; system: string; file: string }
  /** Its unit names file (unitTagsFile) wasn't loaded. */
  | { kind: "units"; system: string; file: string }
  /** A conventional system's channel file wasn't loaded; `had` channels were imported without it. */
  | { kind: "channels"; system: string; file: string; had: number }
  /** Trunk Recorder's siteId: a Site lock to fill in. */
  | { kind: "siteLock"; system: string; siteId: string; site?: number }
  /** An RTL-SDR that wasn't free when imported (`serial` as configured). */
  | { kind: "source"; index: number; serial: string }
  /** A source whose driver isn't installed here. */
  | { kind: "driver"; index: number; driver: DriverKind }
  /** Conventional squelch means something else here (a conventional system's). */
  | { kind: "squelch"; system: string }
  /** Trunk Recorder plugins with no counterpart here. */
  | { kind: "plugins"; names: string[] }
  /** A plugin whose settings came over: to add (install) and turn on. */
  | { kind: "plugin"; id: string; name: string }
  /** No source covers any of the system's control channels. */
  | { kind: "coverage"; system: string };

/** A plugin's settings brought over from Trunk Recorder, by plugin id: for the whole recorder, and for each system by short name. */
export interface PluginImport {
  id: string;
  name: string;
  config: Record<string, unknown>;
  /** By short name, as imported. */
  systems: Record<string, Record<string, unknown>>;
}

/**
 * Trunk Recorder's uploaders and streamers, as this app's plugins: OpenMHz
 * and Broadcastify keys on the systems (with Broadcastify's talkgroup allow /
 * deny lists; their servers, broadcastifyOTA at the top), uploadScript, and
 * the rdioscanner (talkgroupAllow / talkgroupDeny too), openmhz, broadcastify
 * and simplestream entries of `plugins`. `name` is the plugin's manifest name
 * (shown when it isn't installed). `names`: Trunk Recorder short name → short name here;
 * `imported`: each of its systems → its short name here (keys follow the system, even renamed).
 * Plugins with nothing like them here come back in `other`.
 */
function trPlugins(j: Record<string, unknown>, names: Map<string, string>, imported: Map<unknown, string>): { plugins: PluginImport[]; other: string[] } {
  const out = new Map<string, PluginImport>();
  const other: string[] = [];
  const plugin = (id: string, name: string) => out.get(id) ?? out.set(id, { id, name, config: {}, systems: {} }).get(id)!;
  const str = (v: unknown) => (typeof v === "string" ? v.trim() : typeof v === "number" ? String(v) : "");
  const num = (v: unknown) => (typeof v === "number" ? v : typeof v === "string" && /^\d+$/.test(v.trim()) ? Number(v) : undefined);
  const short = (v: unknown) => names.get(str(v)) ?? str(v);
  /** A system's settings, leaving out the empty ones. */
  const forSystem = (p: PluginImport, sys: unknown, values: Record<string, unknown>, name = short(sys)) => {
    const kept = Object.fromEntries(Object.entries(values).filter(([, v]) => v !== undefined && v !== ""));
    if (name && Object.keys(kept).length) p.systems[name] = { ...p.systems[name], ...kept };
  };
  const list = <T,>(v: unknown) => (Array.isArray(v) ? (v as T[]) : []);
  /** A talkgroup allow / deny list (numbers or patterns like 507*), or undefined when there's none. */
  const patterns = (v: unknown) => {
    const kept = list<unknown>(v).map(str).filter((x) => x);
    return kept.length ? kept : undefined;
  };
  /** The first of a system's keys that has a list (Trunk Recorder's Broadcastify takes several names). */
  const firstList = (x: Record<string, unknown>, keys: string[]) => keys.map((k) => patterns(x[k])).find((v) => v);
  const bcfyAllow = (x: Record<string, unknown>) => firstList(x, ["broadcastifyAllow", "broadcastifyWhitelist", "talkgroupWhitelist"]);
  const bcfyDeny = (x: Record<string, unknown>) => firstList(x, ["broadcastifyDeny", "broadcastifyBlacklist", "talkgroupBlacklist"]);
  // Built in to Trunk Recorder: keys on each system, servers at the top.
  for (const sys of list<Record<string, unknown>>(j.systems)) {
    const here = imported.get(sys);
    if (here === undefined) continue;
    if (str(sys.apiKey)) forSystem(plugin("openmhz", "OpenMHz"), null, { apiKey: str(sys.apiKey), systemName: str(sys.openmhzSystemId) }, here);
    if (str(sys.broadcastifyApiKey))
      forSystem(
        plugin("broadcastify", "Broadcastify Calls"),
        null,
        { apiKey: str(sys.broadcastifyApiKey), systemId: num(sys.broadcastifySystemId), talkgroupAllow: bcfyAllow(sys), talkgroupDeny: bcfyDeny(sys) },
        here,
      );
    if (str(sys.uploadScript)) forSystem(plugin("upload-script", "Upload script"), null, { script: str(sys.uploadScript) }, here);
  }
  if (out.has("openmhz") && str(j.uploadServer)) plugin("openmhz", "OpenMHz").config.server = str(j.uploadServer);
  if (out.has("broadcastify")) {
    const b = plugin("broadcastify", "Broadcastify Calls");
    if (str(j.broadcastifyCallsServer)) b.config.server = str(j.broadcastifyCallsServer);
    if (j.broadcastifySslVerifyDisable === true) b.config.skipCertificateCheck = true;
    if (typeof j.broadcastifyOTA === "boolean") b.config.talkerAliases = j.broadcastifyOTA;
  }
  for (const pl of list<Record<string, unknown>>(j.plugins)) {
    const what = `${str(pl.name)} ${str(pl.library)}`.toLowerCase();
    if (pl.enabled === false) continue;
    const systems = list<Record<string, unknown>>(pl.systems);
    if (/rdio/.test(what)) {
      const p = plugin("rdioscanner", "Rdio Scanner");
      if (str(pl.server)) p.config.server = str(pl.server);
      for (const x of systems) forSystem(p, x.shortName, { apiKey: str(x.apiKey), systemId: num(x.systemId), talkgroupAllow: patterns(x.talkgroupAllow), talkgroupDeny: patterns(x.talkgroupDeny) });
    } else if (/openmhz/.test(what)) {
      const p = plugin("openmhz", "OpenMHz");
      if (str(pl.server ?? pl.uploadServer)) p.config.server = str(pl.server ?? pl.uploadServer);
      for (const x of systems) forSystem(p, x.shortName, { apiKey: str(x.apiKey), systemName: str(x.openmhzSystemId) });
    } else if (/broadcastify/.test(what)) {
      const p = plugin("broadcastify", "Broadcastify Calls");
      if (str(pl.broadcastifyCallsServer ?? pl.server)) p.config.server = str(pl.broadcastifyCallsServer ?? pl.server);
      if (pl.broadcastifySslVerifyDisable === true) p.config.skipCertificateCheck = true;
      if (typeof pl.broadcastifyOTA === "boolean") p.config.talkerAliases = pl.broadcastifyOTA;
      for (const x of systems)
        forSystem(p, x.shortName, { apiKey: str(x.apiKey ?? x.broadcastifyApiKey), systemId: num(x.systemId ?? x.broadcastifySystemId), talkgroupAllow: bcfyAllow(x), talkgroupDeny: bcfyDeny(x) });
    } else if (/simplestream/.test(what)) {
      const streams = list<Record<string, unknown>>(pl.streams).map((st) => ({
        url: str(st.url) || `${st.useTCP === true ? "tcp" : "udp"}://${str(st.address) || "127.0.0.1"}:${num(st.port) ?? 9123}`,
        TGID: num(st.TGID) ?? 0,
        shortName: st.shortName ? short(st.shortName) : "",
        sendTGID: st.sendTGID === true,
        sendJSON: st.sendJSON === true,
        sendCallStart: st.sendCallStart === true,
        sendCallEnd: st.sendCallEnd === true,
      }));
      if (streams.length) plugin("simplestream", "simplestream").config.streams = streams;
    } else other.push(str(pl.name) || str(pl.library).replace(/^lib|\.(so|dylib|dll)$/g, "") || "plugin");
  }
  return { plugins: [...out.values()], other };
}

/** A Trunk Recorder system's unitTagsFile (from `files`, else a to-do) and unitTagsMode. */
function trUnitNames(sys: Record<string, unknown>, files: Record<string, string>, shortName: string, todo: ImportTodo[]): UnitNames | undefined {
  const out: UnitNames = {};
  const mode = sys.unitTagsMode;
  if (mode === "ota" || mode === "user_only" || mode === "none") out.mode = mode;
  if (typeof sys.unitTagsFile === "string" && sys.unitTagsFile) {
    const csv = files[sys.unitTagsFile];
    if (csv === undefined) todo.push({ kind: "units", system: shortName, file: sys.unitTagsFile });
    else {
      out.csv = csv;
      out.name = baseName(sys.unitTagsFile);
    }
  }
  return out.csv || out.mode ? out : undefined;
}

/**
 * A Trunk Recorder system's recording settings, as a system's own here. Its
 * digitalLevels / analogLevels (×1 and ×8 by default) become dB from those.
 */
function trRecording(sys: Record<string, unknown>): RecordingOverride {
  const o: RecordingOverride = {};
  const num = (k: string) => (typeof sys[k] === "number" ? (sys[k] as number) : undefined);
  const bool = (k: string) => (typeof sys[k] === "boolean" ? (sys[k] as boolean) : undefined);
  const set = <K extends keyof RecordingOverride>(k: K, v: RecordingOverride[K] | undefined) => {
    if (v !== undefined) o[k] = v;
  };
  set("recordUnknown", bool("recordUnknown"));
  set("minCallS", num("minDuration"));
  set("maxCallS", num("maxDuration"));
  set("minTransmissionS", num("minTransmissionDuration"));
  // (audioArchive kept the audio, compressWav an .m4a of it too: as the recorder's migrate.rs reads them.)
  const archive = bool("audioArchive");
  set("archiveWav", archive);
  set("archiveM4a", archive === false ? false : bool("compressWav"));
  set("callLog", bool("callLog"));
  const db = (x: number | undefined, base: number) => (x && x > 0 && x !== base ? Math.round(20 * Math.log10(x / base) * 10) / 10 : undefined);
  set("digitalLevelDb", db(num("digitalLevels"), 1));
  set("analogLevelDb", db(num("analogLevels"), 8));
  if (typeof sys.filenameFormat === "string" && sys.filenameFormat.trim()) o.filenameFormat = sys.filenameFormat.trim();
  return o;
}

/** The talkgroup and channel files a Trunk Recorder config.json names, as it names them. */
export function trConfigFiles(text: string): { file: string; kind: "talkgroups" | "channels" | "units"; system: string }[] {
  const j = JSON.parse(text.replace(/^\uFEFF/, "")) as { systems?: Record<string, unknown>[] };
  const out: { file: string; kind: "talkgroups" | "channels" | "units"; system: string }[] = [];
  for (const sys of j.systems ?? []) {
    const name = typeof sys.shortName === "string" ? sys.shortName : "";
    if (typeof sys.talkgroupsFile === "string" && sys.talkgroupsFile) out.push({ file: sys.talkgroupsFile, kind: "talkgroups", system: name });
    if (typeof sys.unitTagsFile === "string" && sys.unitTagsFile) out.push({ file: sys.unitTagsFile, kind: "units", system: name });
    if (typeof sys.channelFile === "string" && sys.channelFile) out.push({ file: sys.channelFile, kind: "channels", system: name });
  }
  return out;
}

/** "talkgroups/dc.csv" → "dc.csv". */
const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** Which device a source is, when it names one (null: the first free, or a file). */
const deviceOf = (s: Source): string | null => (deviceId(s) ? sourceKey(s) : null);

/**
 * Import a Trunk Recorder config.json: its sources (one per dongle / SDR),
 * every P25 system (each site is a system here too) and its conventional
 * channels — with the talkgroup and channel files it names, from `files`
 * (by the name the config gives). What couldn't be finished is in `todo`;
 * `notes` say what changed meaning on the way.
 *
 * `merge`: added to `base` instead of replacing its radios and systems (a
 * setup split over several Trunk Recorder configs, imported one by one).
 * A system already there (same short name and kind, and for a trunked one a
 * control channel in common) is left as it is; the Recording and log
 * settings stay `base`'s, and the config's that differ become its systems'
 * own. `added`: the sources (indices) and systems (short names) it brought.
 */
export function importTrunkRecorderConfig(
  text: string,
  base: Config,
  files: Record<string, string> = {},
  merge = false,
): { config: Config; talkgroups: Record<string, string>; notes: string[]; todo: ImportTodo[]; plugins: PluginImport[]; added: { sources: number[]; systems: string[] } } {
  // A byte-order mark (Windows editors add one) is not JSON; Trunk Recorder skips it, so do we.
  const j = JSON.parse(text.replace(/^\uFEFF/, "")) as Record<string, unknown>;
  const notes: string[] = [];
  const todo: ImportTodo[] = [];
  // Trunk Recorder short name → the one here (made unique; the first system of a name), and each system → its name here.
  const names = new Map<string, string>();
  const systemNames = new Map<unknown, string>();
  const cfg: Config = structuredClone(base);
  // Each system's talkgroup file, by its short name here (kept beside the config: setTalkgroups).
  const talkgroups: Record<string, string> = {};
  const sources = (j.sources as Record<string, unknown>[] | undefined) ?? [];
  const systems = (j.systems as Record<string, unknown>[] | undefined) ?? [];
  const imported: Source[] = [];
  for (const s of sources) {
    const dev = typeof s.device === "string" ? s.device : "";
    const center = typeof s.center === "number" ? s.center : 0;
    // Trunk Recorder's "error" is a fixed offset in Hz it tunes up by (+ = signals come in high); here it is ppm, the other way.
    const ppm = typeof s.ppm === "number" ? s.ppm : typeof s.error === "number" && center ? Math.round((-s.error / center) * 1e6 * 100) / 100 : 0;
    const gain = typeof s.gain === "number" ? s.gain : undefined;
    const agc = s.agc === true;
    const autoTune = s.autoTune === true ? { autoTune: true } : {};
    const stage = (v: unknown) => (typeof v === "number" ? v : undefined);
    if (s.driver === "usrp") {
      imported.push({
        type: "usrp",
        args: dev,
        centerHz: center,
        rateHz: typeof s.rate === "number" ? s.rate : 8_000_000,
        gainDb: gain ?? 40,
        agc,
        antenna: typeof s.antenna === "string" ? s.antenna : "",
        ppm,
        ...autoTune,
      });
      notes.push("USRP source imported: it needs UHD installed on this computer.");
      continue;
    }
    // Trunk Recorder builds with a native bladeRF driver use "driver": "bladerf" (same device string).
    const bladerfDriver = s.driver === "bladerf";
    if (s.driver && s.driver !== "osmosdr" && !bladerfDriver) {
      notes.push(`Source driver "${String(s.driver)}" skipped — not supported.`);
      continue;
    }
    const soapyArgs = osmosdrToSoapy(dev);
    if (soapyArgs !== null) {
      const gains: Record<string, number> = {};
      const stages = { IF: s.ifGain, BB: s.bbGain, MIX: s.mixGain, LNA: s.lnaGain, TIA: s.tiaGain, PGA: s.pgaGain, AMP: s.ampGain, VGA: s.vgaGain, VGA1: s.vga1Gain, VGA2: s.vga2Gain };
      for (const [k, v] of Object.entries({ ...stages, ...(s.gainSettings as Record<string, unknown> | undefined) })) if (typeof v === "number" && v !== 0) gains[k] = v;
      imported.push({
        type: "soapy",
        args: soapyArgs,
        centerHz: center,
        rateHz: typeof s.rate === "number" ? s.rate : 8_000_000,
        agc: agc || (gain === undefined && !Object.keys(gains).length),
        gainDb: gain ?? null,
        gains,
        antenna: typeof s.antenna === "string" ? s.antenna : "",
        settings: "",
        ppm,
        ...autoTune,
      });
      notes.push(`"${dev}" imported as a SoapySDR source (${soapyArgs || "first found"}): it needs SoapySDR and the device's module installed on this computer.`);
      continue;
    }
    if (bladerfDriver || /(^|,)\s*bladerf(=|,|$)/i.test(dev)) {
      // osmosdr: "bladerf=0" (index) or "bladerf=<serial>", anywhere in the string ("numchan=1,bladerf=…"),
      // with options such as biastee=rx|on, bandwidth=…
      const id = /bladerf=([^,\s]*)/i.exec(dev)?.[1] ?? "";
      const bw = /bandwidth=(\d+(?:\.\d+)?(?:e\d+)?)/i.exec(dev)?.[1];
      const bwHz = typeof s.bandwidth === "number" ? s.bandwidth : bw ? Number(bw) : 0;
      // Trunk Recorder's bladeRF gain is one overall dB value (gain, or gainSettings.full).
      const full = stage((s.gainSettings as Record<string, unknown> | undefined)?.full) ?? stage((s.gainSettings as Record<string, unknown> | undefined)?.FULL);
      imported.push({
        type: "bladerf",
        serial: /^\d{1,2}$/.test(id) ? "" : id,
        centerHz: center,
        rateHz: typeof s.rate === "number" ? s.rate : 10_000_000,
        bandwidthHz: bwHz,
        gainDb: gain ?? full ?? 30,
        agc,
        biasTee: /bias(tee)?=(1|on|rx|both|true)/i.test(dev),
        ppm,
        ...autoTune,
      });
      notes.push(`bladeRF source imported (${agc ? "AGC" : `gain ${gain ?? full ?? 30} dB`}): it needs libbladeRF 2.x and the board's FPGA image installed on this computer.`);
      continue;
    }
    if (/airspy/.test(dev)) {
      const sn = /airspy=(0x)?([0-9a-fA-F]{8,16})/.exec(dev)?.[2] ?? "";
      const rate = typeof s.rate === "number" && AIRSPY_RATES.includes(s.rate) ? s.rate : 6_000_000;
      // Trunk Recorder sets the Airspy's stages by name (LNA, MIX, IF = VGA); else one overall gain, read here as the linearity step.
      const lna = stage(s.lnaGain) ?? stage((s.gainSettings as Record<string, unknown> | undefined)?.LNA);
      const mix = stage(s.mixGain) ?? stage((s.gainSettings as Record<string, unknown> | undefined)?.MIX);
      const vga = stage(s.ifGain) ?? stage(s.vgaGain) ?? stage((s.gainSettings as Record<string, unknown> | undefined)?.IF);
      const manual = lna !== undefined || mix !== undefined || vga !== undefined;
      const clamp = (v: number | undefined, max: number) => Math.max(0, Math.min(max, Math.round(v ?? 10)));
      imported.push({
        type: "airspy",
        serial: sn.toUpperCase(),
        centerHz: center,
        rateHz: rate,
        gainMode: manual || agc ? "manual" : "linearity",
        gainStep: Math.max(0, Math.min(21, Math.round(gain ?? 14))),
        lnaStep: clamp(lna, 14),
        mixerStep: clamp(mix, 15),
        vgaStep: clamp(vga, 15),
        agc,
        biasTee: /bias=1/.test(dev),
        packing: true,
        ppm,
        ...autoTune,
      });
      notes.push(
        `Airspy source imported (${manual ? "its LNA, mixer and VGA stages" : agc ? "AGC" : "gain as the linearity step 0–21"}): it needs libairspy installed on this computer.`,
      );
      continue;
    }
    const serial = /rtl=([^,\s]+)/.exec(dev)?.[1] ?? "";
    // osmosdr's buflen= / buffers= (bytes per USB transfer, transfers in flight).
    const bufLen = Number(/buflen=(\d+)/.exec(dev)?.[1] ?? 0);
    const buffers = Number(/buffers=(\d+)/.exec(dev)?.[1] ?? 0);
    const okLen = bufLen % 512 === 0 && bufLen <= 16 << 20;
    if (!okLen) notes.push(`buflen=${bufLen} isn't a multiple of 512 bytes; using the default USB transfer size.`);
    let rate = typeof s.rate === "number" ? s.rate : 2_400_000;
    if (rate > 3_200_000) {
      notes.push(`Rate ${rate} is more than an RTL-SDR can do; using 2.4 MSPS.`);
      rate = 2_400_000;
    }
    imported.push({
      type: "rtlsdr",
      serial: /^\d$/.test(serial) ? "" : serial,
      centerHz: center,
      rateHz: rate,
      gainDb: gain ?? RTL_DEFAULT_GAIN_DB,
      agc,
      ppm: Math.round(ppm),
      bufLen: okLen ? bufLen : 0,
      buffers: Math.min(buffers, 256),
      ...autoTune,
    });
  }
  const addedSources: number[] = [];
  if (!merge) {
    if (imported.length) cfg.sources = imported;
    addedSources.push(...cfg.sources.keys());
  } else {
    for (const src of imported) {
      const dev = deviceOf(src);
      const same = dev === null ? undefined : cfg.sources.find((x) => deviceOf(x) === dev);
      if (same && same.centerHz === src.centerHz && same.rateHz === src.rateHz) continue;
      if (same) notes.push(`${dev!.replace(":", " ")} is already one of your radios, centred elsewhere: one device can't be both — change or remove one.`);
      addedSources.push(cfg.sources.push(src) - 1);
    }
    if (imported.length > addedSources.length) notes.push(`${imported.length - addedSources.length} of its radios ${imported.length - addedSources.length === 1 ? "is" : "are"} already here.`);
  }
  // A system of this config already in `base` (merging): left as it is.
  const already: string[] = [];
  const existing = (sys: Record<string, unknown>, list: (System | Conventional)[], same: (x: System | Conventional) => boolean) => {
    if (!merge || typeof sys.shortName !== "string") return undefined;
    const name = sys.shortName.replace(/[^\w.-]/g, "");
    const x = list.find((y) => y.shortName === name && same(y));
    if (x) {
      already.push(x.shortName);
      systemNames.set(sys, x.shortName);
      if (!names.has(sys.shortName)) names.set(sys.shortName, x.shortName);
    }
    return x;
  };
  const mine: { recording?: RecordingOverride }[] = [];
  const p25 = systems.filter((x) => x.type === "p25" || x.type === "smartnet" || x.type === "dmr");
  const conv = systems.filter((x) => x.type === "conventional" || x.type === "conventionalP25" || x.type === "conventionalDMR");
  const skipped = systems.filter((x) => !p25.includes(x) && !conv.includes(x)).map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped unsupported systems: ${skipped.join(", ")}.`);
  // Each conventional system is one here: its channels (listed, or its channel file), short name and rules.
  if (!merge) cfg.conventional = [];
  for (const sys of conv) {
    const mode: Channel["mode"] = sys.type === "conventionalP25" ? "p25" : sys.type === "conventionalDMR" ? "dmr" : "fm";
    if (existing(sys, cfg.conventional, (y) => (y as Conventional).channels.some((ch) => ch.mode === mode))) continue;
    const channels: Channel[] = [];
    if (Array.isArray(sys.channels)) {
      for (const f of sys.channels as unknown[]) if (typeof f === "number" && f > 0) channels.push({ freqHz: f, mode, name: "", enabled: true });
    }
    const x = newConventional(cfg, { shortName: typeof sys.shortName === "string" ? sys.shortName : undefined, ...(sys.enabled === false ? { enabled: false } : {}) });
    if (typeof sys.channelFile === "string" && sys.channelFile) {
      const csv = files[sys.channelFile];
      const read = csv === undefined ? null : parseChannelCsv(csv);
      if (!read || read.error) todo.push({ kind: "channels", system: x.shortName, file: sys.channelFile, had: channels.length });
      else {
        // A file with no Mode column is its system's kind.
        const moded = /(^|[,;\t])\s*"?mode"?\s*([,;\t]|$)/im.test(csv!.split(/\r?\n/, 1)[0] ?? "");
        channels.push(...read.channels.map((ch) => (moded ? ch : { ...ch, mode })));
      }
    }
    x.channels = channels;
    const own = trRecording(sys);
    if (Object.keys(own).length) x.recording = own;
    const units = trUnitNames(sys, files, x.shortName, todo);
    if (units) x.unitNames = units;
    cfg.conventional.push(x);
    mine.push(x);
    if (channels.length) todo.push({ kind: "squelch", system: x.shortName });
    systemNames.set(sys, x.shortName);
    if (typeof sys.shortName === "string" && !names.has(sys.shortName)) names.set(sys.shortName, x.shortName);
  }
  if (conv.length > 1) notes.push(`${conv.length} conventional systems imported, each with its own folder and channels.`);
  if (p25.length) {
    // Trunk Recorder's sites of one system: multiSite on, one multiSiteSystemName. Here they are
    // one system (named so), each a site named as Trunk Recorder named it; the first's talkgroup
    // file, unit names and rules are the system's.
    const groupOf = (sys: Record<string, unknown>) => (sys.multiSite === true && typeof sys.multiSiteSystemName === "string" ? sys.multiSiteSystemName.trim() : "");
    const members = new Map<string, number>();
    for (const sys of p25) if (groupOf(sys)) members.set(groupOf(sys), (members.get(groupOf(sys)) ?? 0) + 1);
    const made = new Map<string, System>();
    if (!merge) cfg.systems = [];
    for (const sys of p25) {
      const ccs = Array.isArray(sys.control_channels) ? (sys.control_channels as unknown[]) : [];
      const kind = sys.type === "smartnet" || sys.type === "dmr" ? sys.type : "p25";
      if (existing(sys, cfg.systems, (y) => (y as System).type === kind && (y as System).sites.some((x) => x.controlChannelsHz.some((f) => ccs.includes(f))))) continue;
      const ownName = typeof sys.shortName === "string" ? sys.shortName : "";
      const group = (members.get(groupOf(sys)) ?? 0) > 1 ? groupOf(sys) : "";
      const one = newSystem(cfg, {
        shortName: group || (ownName ? ownName : undefined),
        controlChannelsHz: ccs.filter((v): v is number => typeof v === "number"),
        modulation: sys.modulation === "qpsk" || sys.modulation === "fsk4" ? sys.modulation : "auto",
        ...(Object.keys(trRecording(sys)).length ? { recording: trRecording(sys) } : {}),
        ...(sys.type === "smartnet" ? smartnetImport(sys) : {}),
        // Trunk Recorder's defaultMode is global and only SmartNet's; here each SmartNet system has its own.
        ...(sys.type === "smartnet" && j.defaultMode === "analog" ? { defaultMode: "analog" as const } : {}),
        ...(sys.type === "dmr" ? dmrImport(sys) : {}),
      });
      if (group && ownName) one.sites[0].name = ownName.replace(/[^\w.-]/g, "");
      // A later site of a system already made here joins it.
      const x = (group && made.get(group)) || one;
      if (x === one) {
        cfg.systems.push(x);
        mine.push(x);
        if (group) made.set(group, x);
      } else {
        x.sites.push(one.sites[0]);
      }
      const site = x.sites.length - 1;
      systemNames.set(sys, x.shortName);
      if (ownName && !names.has(ownName)) names.set(ownName, x.shortName);
      if (typeof sys.talkgroupsFile === "string" && sys.talkgroupsFile) {
        const csv = files[sys.talkgroupsFile];
        const had = talkgroups[x.shortName];
        if (x !== one && had) {
          if (csv !== undefined && normalizeTalkgroupCsv(csv) !== had) notes.push(`${ownName}'s talkgroup file wasn't used: ${x.shortName}'s sites share the first one's.`);
        } else if (csv === undefined) todo.push({ kind: "talkgroups", system: x.shortName, file: sys.talkgroupsFile });
        else talkgroups[x.shortName] = normalizeTalkgroupCsv(csv);
      }
      if (sys.siteId !== undefined && sys.siteId !== null && sys.siteId !== "") todo.push({ kind: "siteLock", system: x.shortName, siteId: String(sys.siteId), site });
      if (x === one) {
        const units = trUnitNames(sys, files, x.shortName, todo);
        if (units) x.unitNames = units;
      }
    }
    const joined = [...made.values()].filter((x) => x.sites.length > 1);
    for (const x of joined) notes.push(`${x.shortName}: ${x.sites.length} sites (${x.sites.map((s) => siteNameOf(x, s)).join(", ")}), one system: a call heard on several is saved once.`);
    const apart = mine.filter((x): x is System => "sites" in x).length;
    if (apart > 1) notes.push(`${apart} systems imported, each with its own folder.`);
  }
  if (already.length) notes.push(`Already here, left as ${already.length === 1 ? "it is" : "they are"}: ${already.join(", ")}.`);
  const owners = merge ? mine : [...cfg.systems.map((x) => x as { recording?: RecordingOverride }), ...cfg.conventional];
  // An instance-level format is every system's that has none of its own (a system's own wins, as in Trunk Recorder).
  if (typeof j.filenameFormat === "string" && j.filenameFormat.trim()) {
    const top = j.filenameFormat.trim();
    if (!owners.length && !merge) cfg.recording.filenameFormat = top;
    for (const o of owners) if (o.recording?.filenameFormat === undefined) o.recording = { ...o.recording, filenameFormat: top };
  }
  if (merge) {
    // The Recording tab's settings stay; what this config sets the same is dropped, what it sets otherwise is its systems' own.
    const rec = cfg.recording as unknown as Record<string, unknown>;
    for (const o of owners) {
      for (const k of Object.keys(o.recording ?? {}) as (keyof RecordingOverride)[]) if (o.recording![k] === rec[k]) delete o.recording![k];
      if (o.recording && !Object.keys(o.recording).length) delete o.recording;
    }
    const differs = [
      typeof j.captureDir === "string" && j.captureDir !== cfg.recording.captureDir && "captureDir",
      typeof j.callTimeout === "number" && j.callTimeout !== cfg.recording.callTimeoutS && "callTimeout",
      typeof j.recordUUVCalls === "boolean" && j.recordUUVCalls !== cfg.recording.recordUnitToUnit && "recordUUVCalls",
      typeof j.archiveFilesOnFailure === "boolean" && j.archiveFilesOnFailure !== cfg.recording.archiveFilesOnFailure && "archiveFilesOnFailure",
    ].filter((k): k is string => !!k);
    if (differs.length) notes.push(`Its ${differs.join(", ")} ${differs.length === 1 ? "wasn't" : "weren't"} taken: your Recording settings stay.`);
  } else if (owners.length) {
    // A setting the same on every system there is the Recording tab's here; the rest stay each system's own.
    const keys = new Set(owners.flatMap((o) => Object.keys(o.recording ?? {}))) as Set<keyof RecordingOverride>;
    for (const k of keys) {
      const vals = owners.map((o) => o.recording?.[k]);
      if (vals.every((v) => v !== undefined && v === vals[0])) {
        (cfg.recording as unknown as Record<string, unknown>)[k] = vals[0];
        for (const o of owners) delete o.recording?.[k];
      }
    }
    for (const o of owners) if (o.recording && !Object.keys(o.recording).length) delete o.recording;
  }
  if (systems.some((x) => x.conversationMode === false)) notes.push("conversationMode off (one file per transmission) isn't supported: each call is one file.");
  const { plugins, other } = trPlugins(j, names, systemNames);
  if (other.length) todo.push({ kind: "plugins", names: other });
  const added = { sources: addedSources, systems: mine.map((o) => (o as System | Conventional).shortName) };
  if (merge) return { config: cfg, talkgroups, notes, todo, plugins, added };
  if (typeof j.captureDir === "string") cfg.recording.captureDir = j.captureDir;
  if (typeof j.callTimeout === "number") cfg.recording.callTimeoutS = j.callTimeout;
  if (typeof j.recordUUVCalls === "boolean") cfg.recording.recordUnitToUnit = j.recordUUVCalls;
  if (typeof j.archiveFilesOnFailure === "boolean") cfg.recording.archiveFilesOnFailure = j.archiveFilesOnFailure;
  // The log: Trunk Recorder's keys at the top (its talkgroupDisplayFormat is a system's: the first one's).
  const log: LogSettings = { ...defaultLog(), ...(cfg.log ?? {}) };
  const levels = ["trace", "debug", "info", "warning", "error", "fatal"];
  if (typeof j.logLevel === "string" && levels.includes(j.logLevel)) log.level = j.logLevel as LogSettings["level"];
  if (typeof j.consoleLog === "boolean") log.console = j.consoleLog;
  if (typeof j.logFile === "boolean") log.file = j.logFile;
  if (typeof j.logDir === "string") log.dir = j.logDir;
  if (typeof j.syslogFriendly === "boolean") log.syslogFriendly = j.syslogFriendly;
  if (typeof j.logColor === "string" && ["all", "console", "logfile", "none"].includes(j.logColor)) log.color = j.logColor;
  if (j.frequencyFormat === "exp" || j.frequencyFormat === "mhz" || j.frequencyFormat === "hz") log.frequencyFormat = j.frequencyFormat;
  if (typeof j.statusAsString === "boolean") log.statusAsString = j.statusAsString;
  if (typeof j.controlWarnRate === "number") log.controlWarnRatePerS = j.controlWarnRate;
  const tgFormat = systems.map((x) => x.talkgroupDisplayFormat).find((v) => v === "id" || v === "id_tag" || v === "tag_id");
  if (tgFormat) log.talkgroupDisplayFormat = tgFormat as LogSettings["talkgroupDisplayFormat"];
  if (log.file && log.dir && !log.dir.startsWith("/")) notes.push(`The log folder "${log.dir}" is now next to the config file.`);
  cfg.log = log;
  return { config: cfg, talkgroups, notes, todo, plugins, added };
}
