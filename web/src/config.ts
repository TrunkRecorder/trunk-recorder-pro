// Config helpers for the setup form (the recorder validates again).

import type { Channel, Config, Conventional, LogSettings, RecordingOverride, SiteIdentity, Source, System, UnitNames } from "./protocol.ts";
import { normalizeTalkgroupCsv, splitCsvLine } from "./talkgroups.ts";
import { dmrTalkgroup, parseAccess, sameTone } from "./tones.ts";

/** A new RTL-SDR's gain, dB (higher overloads its front end near strong transmitters). */
export const RTL_DEFAULT_GAIN_DB = 25.4;

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** A fresh config (the web build; the desktop app gets its own from the recorder). */
export function defaultConfig(): Config {
  return {
    sources: [newDongle()],
    systems: [],
    conventional: [],
    recording: {
      captureDir: "",
      prerollS: 1,
      maxRecorders: 32,
      callTimeoutS: 3,
      recordUnknown: true,
      recordEncrypted: false,
      recordUnitToUnit: true,
      keepSilentCalls: false,
      minCallS: 0,
      maxCallS: 0,
      minTransmissionS: 0,
      captureFrames: false,
      dropDuplicateCalls: true,
      normalizeAudio: true,
      digitalLevelDb: 0,
      analogLevelDb: 0,
      compressWav: false,
      audioArchive: true,
      callLog: true,
      archiveFilesOnFailure: true,
      filenameFormat: "",
      vocoder: "fixed",
    },
    server: { bind: "127.0.0.1", port: 8080, autoStart: false },
    log: defaultLog(),
  };
}

/** The log's defaults (the recorder's LogSettings). */
export function defaultLog(): LogSettings {
  return {
    level: "info",
    console: true,
    file: false,
    dir: "",
    syslogFriendly: false,
    syslog: false,
    color: "",
    frequencyFormat: "mhz",
    talkgroupDisplayFormat: "id",
    statusAsString: true,
    controlWarnRatePerS: 10,
  };
}

/** Sample rates offered for a USRP (any rate its master clock divides to works). */
export const USRP_RATES = [2_400_000, 4_000_000, 5_000_000, 6_400_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000];
/** Airspy R2: 10 / 2.5; Mini: 6 / 3 (and 10 with newer firmware). */
export const AIRSPY_RATES = [10_000_000, 6_000_000, 3_000_000, 2_500_000];

export function newDongle(): Source {
  return { type: "rtlsdr", serial: "", centerHz: 0, rateHz: 2_400_000, gainDb: RTL_DEFAULT_GAIN_DB, agc: false, ppm: 0 };
}
export function newUsrp(): Source {
  return { type: "usrp", args: "", centerHz: 0, rateHz: 8_000_000, gainDb: 40, agc: false, antenna: "", ppm: 0 };
}
export function newAirspy(): Source {
  return { type: "airspy", serial: "", centerHz: 0, rateHz: 6_000_000, gainMode: "linearity", gainStep: 14, lnaStep: 10, mixerStep: 10, vgaStep: 10, agc: false, biasTee: false, ppm: 0 };
}
/** Offered for SoapySDR devices; any rate the device takes can be typed. */
export const SOAPY_RATES = [2_000_000, 2_400_000, 2_500_000, 3_000_000, 6_000_000, 8_000_000, 10_000_000];
export function newSoapy(): Source {
  return { type: "soapy", args: "", centerHz: 0, rateHz: 8_000_000, agc: true, gainDb: null, gains: {}, antenna: "", settings: "", ppm: 0 };
}
export function newFile(): Source {
  return { type: "file", path: "", centerHz: 0, rateHz: 2_400_000, realtime: true, format: "cu8" };
}

/** A capture's sample format from its name (as the recorder guesses it). */
export function formatFromPath(path: string): "cu8" | "cs16" | "cf32" {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return ["cf32", "cfile", "fc32", "complex"].includes(ext) ? "cf32" : ["cs16", "sc16"].includes(ext) ? "cs16" : "cu8";
}

/** Left unused at each edge of a source's band unless it says otherwise (`guardHz`), Hz. */
export const DEFAULT_GUARD_HZ = 75_000;

/** How far from a source's centre a channel may be: half its band less the guard (never under a quarter of the band). */
export function usableHalfWidth(rateHz: number, guardHz: number = DEFAULT_GUARD_HZ): number {
  return Math.max(rateHz / 2 - Math.max(0, guardHz), rateHz / 4);
}

/** A centre that fits every control channel in one source, off the DC spike. */
export function autoCenter(controlChannels: number[], rateHz: number, guardHz?: number): number | null {
  if (!controlChannels.length) return null;
  const lo = Math.min(...controlChannels);
  const hi = Math.max(...controlChannels);
  const half = usableHalfWidth(rateHz, guardHz);
  if (hi - lo > 2 * half - 50_000) return null;
  let c = Math.round((lo + hi) / 2 / 1000) * 1000;
  for (let step = 0; step < 40 && controlChannels.some((f) => Math.abs(f - c) < 25_000); step++) {
    c += (step % 2 ? -1 : 1) * 12_500 * (step + 1);
  }
  return controlChannels.every((f) => Math.abs(f - c) <= half - 10_000) ? c : null;
}

/** The conventional channels being recorded: switched on, in a system that is. */
export function enabledChannels(c: Config): Channel[] {
  return (c.conventional ?? []).filter((v) => v.enabled).flatMap((v) => v.channels.filter((ch) => ch.enabled));
}

/** A conventional system with its defaults filled in. */
export function normalizeConventional(x: Partial<Conventional>): Conventional {
  return { shortName: "conv", enabled: true, squelchDb: 8, channels: [], ...x };
}

/** A new conventional system (not yet in the config), named uniquely: conv, conv2… */
export function newConventional(c: Config, patch: Partial<Conventional> = {}): Conventional {
  return normalizeConventional({ ...patch, shortName: uniqueShortName(c, patch.shortName ?? "conv") });
}

/** The conventional system with this short name. */
export function conventionalNamed(c: Config, shortName: string): Conventional | undefined {
  return c.conventional.find((x) => x.shortName === shortName);
}

/** Trunk Recorder's trunked DMR settings: `lcnTable` { "<lcn>": Hz } and `channels` (candidate voice frequencies), in Hz (a value under 100000 read as MHz). */
function dmrImport(sys: Record<string, unknown>): Partial<System> {
  const hz = (v: unknown) => (typeof v === "number" && v > 0 ? (v < 1e5 ? Math.round(v * 1e6) : v) : undefined);
  const out: Partial<System> = { type: "dmr" };
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
 * ControlChannel::identity_fields): the lock compares only these. DMR states
 * none (its sites are told apart by colour code).
 */
export function siteLockFields(type: System["type"]): (keyof SiteIdentity)[] {
  return type === "smartnet" ? ["sysId", "site"] : type === "dmr" ? [] : ["nac", "wacn", "sysId", "rfss", "site"];
}

/** A system with defaults filled in (configs saved before a field existed). */
export function normalizeSystem(x: Partial<System>): System {
  return {
    shortName: x.shortName ?? "sys1",
    ...(x.name?.trim() ? { name: x.name } : {}),
    type: x.type === "smartnet" ? "smartnet" : x.type === "dmr" ? "dmr" : "p25",
    ...(x.type === "dmr"
      ? {
          ...(x.lcnTableHz && Object.keys(x.lcnTableHz).length ? { lcnTableHz: x.lcnTableHz } : {}),
          ...(x.dmrChannelsHz?.length ? { dmrChannelsHz: x.dmrChannelsHz } : {}),
          ...(typeof x.colorCode === "number" ? { colorCode: x.colorCode } : {}),
        }
      : {}),
    ...(x.type === "smartnet"
      ? {
          bandplan: x.bandplan ?? "800_standard",
          ...(x.bandplanBaseHz ? { bandplanBaseHz: x.bandplanBaseHz } : {}),
          ...(x.bandplanSpacingHz ? { bandplanSpacingHz: x.bandplanSpacingHz } : {}),
          ...(x.bandplanOffset ? { bandplanOffset: x.bandplanOffset } : {}),
          ...(x.bandplanHighHz ? { bandplanHighHz: x.bandplanHighHz } : {}),
          ...(x.defaultMode === "analog" ? { defaultMode: "analog" as const } : {}),
        }
      : {}),
    enabled: x.enabled ?? true,
    controlChannelsHz: x.controlChannelsHz ?? [],
    modulation: x.modulation ?? "auto",
    talkgroupsCsv: x.talkgroupsCsv ?? "",
    talkgroupsName: x.talkgroupsName ?? "",
    expect: x.expect ?? {},
    voiceChannelsHz: x.voiceChannelsHz ?? [],
    ...(x.recording && Object.keys(x.recording).length ? { recording: x.recording } : {}),
    ...(x.unitNames && (x.unitNames.csv || x.unitNames.mode) ? { unitNames: x.unitNames } : {}),
    ...(x.siteGroup?.trim() ? { siteGroup: x.siteGroup } : {}),
  };
}

/** A config stored by this browser, with any setting it lacks at its default. */
export function storedConfig(raw: Partial<Config>): Config {
  const base = defaultConfig();
  return {
    ...base,
    ...raw,
    systems: (raw.systems ?? []).map(normalizeSystem),
    conventional: Array.isArray(raw.conventional) ? raw.conventional.map(normalizeConventional) : [],
    recording: { ...base.recording, ...(raw.recording ?? {}) },
    server: { ...base.server, ...(raw.server ?? {}) },
    log: { ...defaultLog(), ...(raw.log ?? {}) },
  };
}

/** Why a filename format can't be used, or null: filenameFormat.ts. */
export { FILENAME_TOKENS, filenameProblem } from "./filenameFormat.ts";

/** The systems being recorded, in the recorder's order (SystemStatus.index). */
export function activeSystems(c: Config): System[] {
  return c.systems.filter((x) => x.enabled && x.controlChannelsHz.length > 0);
}

/** A system's color (the dashboard, waterfall and setup agree): by its place among the active systems. */
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
export function newSystem(c: Config, patch: Partial<System> = {}): System {
  const sys = normalizeSystem(patch);
  sys.shortName = uniqueShortName(c, patch.shortName ?? `sys${c.systems.length + 1}`);
  return sys;
}

/** A SmartNet system's settings from a Trunk Recorder system (its base / spacing / high are MHz; Hz here — a value already in Hz is kept). */
function smartnetImport(sys: Record<string, unknown>): Partial<System> {
  const hz = (v: unknown, mhzBelow: number) => (typeof v === "number" && v > 0 ? (v < mhzBelow ? Math.round(v * 1e6) : v) : undefined);
  const out: Partial<System> = { type: "smartnet", bandplan: typeof sys.bandplan === "string" ? sys.bandplan : "800_standard" };
  const base = hz(sys.bandplanBase, 1e5);
  const spacing = hz(sys.bandplanSpacing, 1);
  const high = hz(sys.bandplanHigh, 1e5);
  if (base) out.bandplanBaseHz = base;
  if (spacing) out.bandplanSpacingHz = spacing;
  if (high) out.bandplanHighHz = high;
  if (typeof sys.bandplanOffset === "number") out.bandplanOffset = sys.bandplanOffset;
  return out;
}

/** A default short name for a site: "p25-<sysid>-<rfss>-<site>", or "sysN". */
export function siteName(c: Config, id: SiteIdentity, smartnet = false): string {
  if (smartnet && id.sysId != null) return uniqueShortName(c, `smartnet-${id.sysId.toString(16)}`);
  if (id.sysId != null && id.site != null) return uniqueShortName(c, `p25-${id.sysId.toString(16)}-${id.rfss ?? 0}-${id.site}`);
  return uniqueShortName(c, `sys${c.systems.length + 1}`);
}

/** Sites of one system: the same WACN and System ID (both known). */
export function sameSystem(a: SiteIdentity, b: SiteIdentity): boolean {
  return a.wacn != null && a.sysId != null && a.wacn === b.wacn && a.sysId === b.sysId;
}

/** Other sites of `sys`'s system as configured: the same site group, or (none named) the same site lock WACN / System ID. */
export function siteSiblings(c: Config, sys: System): System[] {
  const group = sys.siteGroup?.trim();
  return c.systems.filter((x) => x !== sys && (group ? x.siteGroup?.trim() === group : !x.siteGroup?.trim() && x.type === sys.type && sameSystem(x.expect, sys.expect)));
}

/** The system a control channel is already configured on, if any. */
export function systemWithChannel(c: Config, hz: number): System | undefined {
  return c.systems.find((x) => [...x.controlChannelsHz, ...(x.dmrChannelsHz ?? [])].some((f) => Math.abs(f - hz) < 6_000));
}

/**
 * Each source's centre: as set, or (0 = auto) placed over what the sources
 * before it don't cover yet (the recorder's Config::resolved_centers).
 */
export function resolvedCenters(c: Config): (number | null)[] {
  // A DMR site's watched frequencies are all needed (as Config::resolved_centers).
  const groups: [number[], number[]][] = activeSystems(c).map((x) => {
    const need = [...x.controlChannelsHz, ...(x.type === "dmr" ? (x.dmrChannelsHz ?? []) : [])];
    return [[...need, ...x.voiceChannelsHz], need];
  });
  for (const v of c.conventional.filter((x) => x.enabled)) {
    const conv = v.channels.filter((ch) => ch.enabled && ch.freqHz > 0).map((ch) => ch.freqHz);
    if (conv.length) groups.push([conv, conv]);
  }
  const centers = c.sources.map((s) => s.centerHz);
  const covered = (f: number) => c.sources.some((s, i) => centers[i] > 0 && Math.abs(f - centers[i]) <= usableHalfWidth(s.rateHz, s.guardHz));
  for (let i = 0; i < c.sources.length; i++) {
    if (centers[i] > 0) continue;
    const open = groups.filter(([, need]) => !need.some(covered));
    if (!open.length) break;
    const { rateHz: rate, guardHz: guard } = c.sources[i];
    centers[i] =
      autoCenter(open.flatMap((g) => g[0]), rate, guard) ??
      autoCenter(open.flatMap((g) => g[1]), rate, guard) ??
      autoCenter(open[0][0], rate, guard) ??
      autoCenter(open[0][1], rate, guard) ??
      0;
  }
  return centers.map((x) => x || null);
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
 * Each channel's talkgroup: its own; else a DMR row's talkgroup in its Tone;
 * else by its place among the rows on its frequency — the frequency in kHz,
 * then that with a digit (154325, 1543251 …). The recorder's channel_talkgroups.
 */
export function channelTalkgroups(channels: Channel[]): number[] {
  return channels.map((c, i) => {
    if (c.talkgroup !== undefined) return c.talkgroup;
    const air = c.mode === "dmr" ? dmrTalkgroup(c.tone) : undefined;
    if (air !== undefined) return air;
    const k = channels.slice(0, i).filter((o) => sameFreq(o.freqHz, c.freqHz)).length;
    return k === 0 ? defaultTalkgroup(c.freqHz) : defaultTalkgroup(c.freqHz) * 10 + k;
  });
}

/** A talkgroup for another row on `freqHz`: the first of 1543251, 1543252 … no row uses. */
export function nextTalkgroup(channels: Channel[], freqHz: number): number {
  const used = new Set(channelTalkgroups(channels));
  let k = 1;
  while (used.has(defaultTalkgroup(freqHz) * 10 + k)) k++;
  return defaultTalkgroup(freqHz) * 10 + k;
}

/** Why rows on one frequency can't run together, or null (the recorder's check_channels). */
function rowsProblem(rows: Channel[]): string | null {
  const mhz = formatMhz(rows[0].freqHz);
  if (rows.some((r) => r.mode !== rows[0].mode)) return `Conventional channel ${mhz} MHz is listed with different modes — rows sharing a frequency need the same one.`;
  const what = { fm: "tone", p25: "NAC", dmr: "colour code, slot or talkgroup" }[rows[0].mode];
  const tones = rows.map((r) => {
    const t = parseAccess(r.mode, r.tone ?? "");
    return "tone" in t ? t.tone : "";
  });
  for (let i = 0; i < tones.length; i++)
    for (let j = 0; j < i; j++) {
      if (!tones[i] && !tones[j]) return `Conventional channel ${mhz} MHz is listed twice without a ${what} — give each row its own (one may have none).`;
      if (tones[i] && tones[j] && sameTone(tones[j], tones[i]))
        return `Conventional channel ${mhz} MHz has two rows for ${tones[i] === tones[j] ? tones[i] : `${tones[j]} and ${tones[i]} (the same signal)`}.`;
    }
  return null;
}

/** Two systems sharing a short name, or one without (the recorder's Config::name_problem): every system, on or off. */
export function nameProblem(c: Config): string | null {
  const names = new Set<string>();
  for (const x of [...c.systems, ...c.conventional]) {
    if (!x.shortName.trim()) return "Every system needs a short name.";
    if (names.has(x.shortName)) return `Two systems are named "${x.shortName}" — each needs a short name of its own (it is its folder, and how plugins know it).`;
    names.add(x.shortName);
  }
  return null;
}

/** Conventional systems a config may have (the recorder's MAX_CONVENTIONAL). */
export const MAX_CONVENTIONAL = 256;

/** The SmartNet band plans the recorder knows besides 400… / OBT (smartnet::Bandplan::from_config). */
const BANDPLANS = ["", "800_standard", "800_domestic", "800_reband", "800_rebanded", "800_splinter", "800_domestic_splinter", "900"];

/** Why a SmartNet system's band plan can't be used, or null (smartnet::Bandplan::from_config). */
export function bandplanProblem(x: System): string | null {
  const n = x.bandplan ?? "";
  if (n.startsWith("400") || n.toLowerCase() === "obt") {
    const base = x.bandplanBaseHz ?? 0;
    if (!(base > 0) || !((x.bandplanSpacingHz ?? 0) > 0) || !((x.bandplanHighHz ?? 0) > base))
      return `SmartNet band plan ${n} needs its base, spacing and high frequency (high above base) and offset.`;
    return null;
  }
  return BANDPLANS.includes(n) ? null : `unknown SmartNet band plan "${n}" (800_standard, 800_reband, 800_splinter, 900, 400_custom).`;
}

/** Why the config can't start, or null (the recorder's Config::problem). */
export function startProblem(c: Config): string | null {
  if (!c.sources.length) return "Add a source: a dongle or a capture file.";
  const systems = activeSystems(c);
  const channels = enabledChannels(c);
  if (!systems.length && !channels.length) return "Add a system with a control channel, or a conventional channel.";
  const named = nameProblem(c);
  if (named) return named;
  const centers = resolvedCenters(c);
  const missing = centers.findIndex((x) => !x);
  if (missing >= 0)
    return `Set a center frequency for source ${missing + 1} — it couldn't be placed automatically (nothing left for it to cover, or the channels don't fit one source).`;
  const inside = (f: number) => sourceCovering(c, centers, f) >= 0;
  for (const x of systems) {
    const plan = x.type === "smartnet" ? bandplanProblem(x) : null;
    if (plan) return `${x.shortName}: ${plan}`;
    if (x.type === "dmr") {
      const out = [...x.controlChannelsHz, ...(x.dmrChannelsHz ?? [])].filter((f) => !inside(f)).map((f) => formatMhz(f));
      if (out.length) return `${x.shortName}: DMR frequencies outside every source's bandwidth: ${out.join(", ")} MHz — move a center frequency or add a source.`;
    }
    if (!x.controlChannelsHz.some(inside)) return `No control channel of ${x.shortName} falls inside any source's bandwidth — move a center frequency or add a source.`;
  }
  if (c.conventional.length > MAX_CONVENTIONAL) return `At most ${MAX_CONVENTIONAL} conventional systems.`;
  const owner: [number, string][] = [];
  for (const v of c.conventional.filter((x) => x.enabled))
    for (const ch of v.channels.filter((x) => x.enabled)) {
      const other = owner.find(([f, o]) => sameFreq(f, ch.freqHz) && o !== v.shortName);
      if (other) return `Conventional channel ${formatMhz(ch.freqHz)} MHz is in both ${other[1]} and ${v.shortName} — a frequency belongs to one conventional system.`;
      owner.push([ch.freqHz, v.shortName]);
    }
  if (channels.some((ch) => !(ch.freqHz > 0))) return "A conventional channel has no frequency yet.";
  const outside = channels.filter((ch) => !inside(ch.freqHz)).map((ch) => formatMhz(ch.freqHz));
  if (outside.length) return `Conventional channel(s) outside every source's bandwidth: ${outside.join(", ")} MHz — move a center frequency or disable them.`;
  for (const ch of channels) {
    const t = parseAccess(ch.mode, ch.tone ?? "");
    if ("error" in t) return `Conventional channel ${formatMhz(ch.freqHz)} MHz: ${t.error}.`;
  }
  const done: number[] = [];
  for (const ch of channels) {
    if (done.some((f) => sameFreq(f, ch.freqHz))) continue;
    done.push(ch.freqHz);
    const p = rowsProblem(channels.filter((o) => sameFreq(o.freqHz, ch.freqHz)));
    if (p) return p;
  }
  if (c.sources.some((s) => s.type === "file" && !s.path)) return "Choose the capture file to replay.";
  return null;
}

// Conventional channels as CSV, for a spreadsheet. The same rules as the
// recorder's channel file (crates/trunk-app/src/channels.rs) — keep them together.

/** The columns channelsToCsv writes. */
export const CHANNEL_CSV_HEADER = "TG Number,Frequency,Tone,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Enable";

/** RFC-4180-ish split on `delim`. */
function splitOn(line: string, delim: string): string[] {
  if (delim === ",") return splitCsvLine(line);
  const out: string[] = [];
  let cur = "";
  let q = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (q) {
      if (ch === '"' && line[i + 1] === '"') {
        cur += '"';
        i++;
      } else if (ch === '"') q = false;
      else cur += ch;
    } else if (ch === '"') q = true;
    else if (ch === delim) {
      out.push(cur.trim());
      cur = "";
    } else cur += ch;
  }
  out.push(cur.trim());
  return out;
}

/** A frequency cell: MHz with a decimal point (under 10 GHz in MHz), else Hz. */
function freqCell(cell: string, decimalComma: boolean): number | null {
  const t = (decimalComma ? cell.replace(/,/g, ".") : cell).trim();
  const v = Number(t);
  if (!t || !Number.isFinite(v) || v <= 0) return null;
  return t.includes(".") && v < 10_000 ? Math.round(v * 1e6) : Math.round(v);
}

function rowsList(rows: number[]): string {
  return rows.slice(0, 8).join(", ") + (rows.length > 8 ? ` and ${rows.length - 8} more` : "");
}

/**
 * Conventional channels from a CSV: Trunk Recorder's channel file (TG Number,
 * Frequency, Alpha Tag, Description, Tag, Category, Enable) plus Mode and
 * Squelch dB; commas, semicolons or tabs; Excel's byte-order mark and decimal
 * commas. `error` when there is no Frequency column.
 */
export function parseChannelCsv(text: string): { channels: Channel[]; notes: string[]; error?: string } {
  const lines = text
    .replace(/^\uFEFF/, "")
    .split(/\r?\n/)
    .map((l, i) => ({ row: i + 1, l }))
    .filter(({ l }) => l.trim() && !l.trimStart().startsWith("#"));
  if (!lines.length) return { channels: [], notes: [], error: "The channel file is empty." };
  const headLine = lines[0].l;
  const delim = [",", ";", "\t"].reduce((best, d) => (headLine.split(d).length > headLine.split(best).length ? d : best), ",");
  const decimalComma = delim !== ",";
  const head = splitOn(headLine, delim).map((h) => h.toLowerCase());
  const col = (...names: string[]) => head.findIndex((h) => names.includes(h));
  const cFreq = col("frequency", "freq", "freqhz");
  if (cFreq < 0) return { channels: [], notes: [], error: "No Frequency column: the first row must name the columns (TG Number, Frequency, Mode, Alpha Tag, …)." };
  const cTg = col("tg number", "talkgroup", "tg");
  const cName = col("alpha tag", "name");
  const cDesc = col("description");
  const cTag = col("tag");
  const cGroup = col("category", "group");
  const cMode = col("mode");
  const cSq = col("squelch db", "squelchdb");
  const cTrSq = col("squelch");
  const cEnable = col("enable", "enabled");
  const cTone = col("tone");
  const channels: Channel[] = [];
  const badFreq: number[] = [];
  const badMode: number[] = [];
  const badTg: number[] = [];
  const badSq: number[] = [];
  const badTone: string[] = [];
  let search = false;
  for (const { row, l } of lines.slice(1)) {
    const f = splitOn(l, delim);
    const at = (i: number) => (i >= 0 ? f[i] ?? "" : "");
    const freqHz = freqCell(at(cFreq), decimalComma);
    if (freqHz === null) {
      badFreq.push(row);
      continue;
    }
    const m = at(cMode).toLowerCase();
    const raw = at(cTone);
    let mode: Channel["mode"] = "fm";
    // No Mode: a NAC means P25, a colour code DMR.
    if (!m && (/NAC/i.test(raw) || raw.startsWith("$"))) mode = "p25";
    else if (!m && /^CC/i.test(raw)) mode = "dmr";
    else if (["p25", "digital", "d"].includes(m)) mode = "p25";
    else if (m === "dmr") mode = "dmr";
    else if (!["", "fm", "nfm", "analog", "a"].includes(m)) badMode.push(row);
    const ch: Channel = { freqHz, mode, name: at(cName), enabled: !["false", "no", "0", "off"].includes(at(cEnable).toLowerCase()) };
    const tgText = at(cTg);
    if (tgText) {
      const tg = Number(tgText);
      if (Number.isInteger(tg) && tg > 0 && tg < 2 ** 32) ch.talkgroup = tg;
      else badTg.push(row);
    }
    const sqText = at(cSq);
    if (sqText) {
      const sq = Number(decimalComma ? sqText.replace(/,/g, ".") : sqText);
      if (Number.isFinite(sq) && sq >= 3 && sq <= 40) ch.squelchDb = sq;
      else badSq.push(row);
    }
    if (at(cDesc)) ch.description = at(cDesc);
    if (at(cTag)) ch.tag = at(cTag);
    if (at(cGroup)) ch.group = at(cGroup);
    search ||= raw.toLowerCase() === "s";
    const t = parseAccess(mode, raw);
    if ("error" in t) badTone.push(`row ${row}: ${t.error}`);
    else if (t.tone) ch.tone = t.tone;
    channels.push(ch);
  }
  const notes: string[] = [];
  if (badFreq.length) notes.push(`Skipped row(s) ${rowsList(badFreq)} — no usable frequency.`);
  if (badMode.length) notes.push(`Row(s) ${rowsList(badMode)}: unknown Mode (use fm or p25) — read as fm.`);
  if (badTg.length) notes.push(`Row(s) ${rowsList(badTg)}: TG Number isn't a positive whole number — using the default.`);
  if (badSq.length) notes.push(`Row(s) ${rowsList(badSq)}: Squelch dB must be 3–40 (dB above the noise) — using the default.`);
  if (badTone.length) notes.push(`Tone not read (the row records any): ${badTone.join("; ")}.`);
  if (search) notes.push("Tone S (search) needs no setting here: every analog call's tone is identified and written to its JSON.");
  if (cTrSq >= 0 && cSq < 0) notes.push("The Squelch column (Trunk Recorder's absolute level) was not read: use Squelch dB, in dB above the noise floor.");
  return { channels, notes };
}

/** MHz with at least 4 and at most 6 decimals (154.4300, 154.43125). */
export function mhzCell(hz: number): string {
  const [int, frac = ""] = (hz / 1e6).toFixed(6).split(".");
  return `${int}.${frac.replace(/0+$/, "").padEnd(4, "0")}`;
}

function csvCell(s: string | undefined): string {
  const v = s ?? "";
  return /[,";\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
}

/** The channels as CSV (CHANNEL_CSV_HEADER's columns), which parseChannelCsv and the recorder read back unchanged. */
export function channelsToCsv(channels: Channel[]): string {
  const rows = channels.map((c) =>
    [
      c.talkgroup ?? "",
      mhzCell(c.freqHz),
      csvCell(c.tone),
      c.mode,
      csvCell(c.name),
      csvCell(c.description),
      csvCell(c.tag),
      csvCell(c.group),
      c.squelchDb ?? "",
      String(c.enabled),
    ].join(","),
  );
  return [CHANNEL_CSV_HEADER, ...rows].join("\n") + "\n";
}

/** Parse a comma/space separated list of MHz (or Hz) values. */
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
  bladerf: "bladerf",
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
 * when it's a radio with its own source (rtl=, airspy=, uhd). "soapy=0,driver=sdrplay"
 * keeps its arguments; "hackrf=0" → "driver=hackrf"; "hackrf=0000000000000000a06063c8"
 * → "driver=hackrf,serial=…". Other osmosdr options (bias=1, buffers=…) are left out.
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
  | { kind: "siteLock"; system: string; siteId: string }
  /** An RTL-SDR that wasn't free when imported (`serial` as configured). */
  | { kind: "source"; index: number; serial: string }
  /** A source whose driver isn't installed here. */
  | { kind: "driver"; index: number; driver: "usrp" | "airspy" | "soapy" }
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
 * and Broadcastify keys on the systems (and their servers at the top),
 * uploadScript, and the rdioscanner, openmhz, broadcastify and simplestream
 * entries of `plugins`. `names`: Trunk Recorder short name → short name here;
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
  // Built in to Trunk Recorder: keys on each system, servers at the top.
  for (const sys of list<Record<string, unknown>>(j.systems)) {
    const here = imported.get(sys);
    if (here === undefined) continue;
    if (str(sys.apiKey)) forSystem(plugin("openmhz", "OpenMHz"), null, { apiKey: str(sys.apiKey), systemName: str(sys.openmhzSystemId) }, here);
    if (str(sys.broadcastifyApiKey)) forSystem(plugin("broadcastify", "Broadcastify Calls"), null, { apiKey: str(sys.broadcastifyApiKey), systemId: num(sys.broadcastifySystemId) }, here);
    if (str(sys.uploadScript)) forSystem(plugin("upload-script", "Upload script"), null, { script: str(sys.uploadScript) }, here);
  }
  if (out.has("openmhz") && str(j.uploadServer)) plugin("openmhz", "OpenMHz").config.server = str(j.uploadServer);
  if (out.has("broadcastify")) {
    const b = plugin("broadcastify", "Broadcastify Calls");
    if (str(j.broadcastifyCallsServer)) b.config.server = str(j.broadcastifyCallsServer);
    if (j.broadcastifySslVerifyDisable === true) b.config.skipCertificateCheck = true;
  }
  for (const pl of list<Record<string, unknown>>(j.plugins)) {
    const what = `${str(pl.name)} ${str(pl.library)}`.toLowerCase();
    if (pl.enabled === false) continue;
    const systems = list<Record<string, unknown>>(pl.systems);
    if (/rdio/.test(what)) {
      const p = plugin("rdioscanner", "Rdio Scanner");
      if (str(pl.server)) p.config.server = str(pl.server);
      for (const x of systems) forSystem(p, x.shortName, { apiKey: str(x.apiKey), systemId: num(x.systemId) });
    } else if (/openmhz/.test(what)) {
      const p = plugin("openmhz", "OpenMHz");
      if (str(pl.server ?? pl.uploadServer)) p.config.server = str(pl.server ?? pl.uploadServer);
      for (const x of systems) forSystem(p, x.shortName, { apiKey: str(x.apiKey), systemName: str(x.openmhzSystemId) });
    } else if (/broadcastify/.test(what)) {
      const p = plugin("broadcastify", "Broadcastify Calls");
      if (str(pl.broadcastifyCallsServer ?? pl.server)) p.config.server = str(pl.broadcastifyCallsServer ?? pl.server);
      if (pl.broadcastifySslVerifyDisable === true) p.config.skipCertificateCheck = true;
      for (const x of systems) forSystem(p, x.shortName, { apiKey: str(x.apiKey ?? x.broadcastifyApiKey), systemId: num(x.systemId ?? x.broadcastifySystemId) });
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
      if (streams.length) plugin("simplestream", "Simple stream").config.streams = streams;
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
  set("compressWav", bool("compressWav"));
  set("audioArchive", bool("audioArchive"));
  set("callLog", bool("callLog"));
  const db = (x: number | undefined, base: number) => (x && x > 0 && x !== base ? Math.round(20 * Math.log10(x / base) * 10) / 10 : undefined);
  set("digitalLevelDb", db(num("digitalLevels"), 1));
  set("analogLevelDb", db(num("analogLevels"), 8));
  if (typeof sys.filenameFormat === "string" && sys.filenameFormat.trim()) o.filenameFormat = sys.filenameFormat.trim();
  return o;
}

/** The talkgroup and channel files a Trunk Recorder config.json names, as it names them. */
export function trConfigFiles(text: string): { file: string; kind: "talkgroups" | "channels" | "units"; system: string }[] {
  const j = JSON.parse(text) as { systems?: Record<string, unknown>[] };
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

/**
 * Import a Trunk Recorder config.json: its sources (one per dongle / SDR),
 * every P25 system (each site is a system here too) and its conventional
 * channels — with the talkgroup and channel files it names, from `files`
 * (by the name the config gives). What couldn't be finished is in `todo`;
 * `notes` say what changed meaning on the way.
 */
export function importTrunkRecorderConfig(
  text: string,
  base: Config,
  files: Record<string, string> = {},
): { config: Config; notes: string[]; todo: ImportTodo[]; plugins: PluginImport[] } {
  const j = JSON.parse(text) as Record<string, unknown>;
  const notes: string[] = [];
  const todo: ImportTodo[] = [];
  // Trunk Recorder short name → the one here (made unique; the first system of a name), and each system → its name here.
  const names = new Map<string, string>();
  const systemNames = new Map<unknown, string>();
  const cfg: Config = structuredClone(base);
  const sources = (j.sources as Record<string, unknown>[] | undefined) ?? [];
  const systems = (j.systems as Record<string, unknown>[] | undefined) ?? [];
  const imported: Source[] = [];
  for (const s of sources) {
    const dev = typeof s.device === "string" ? s.device : "";
    const center = typeof s.center === "number" ? s.center : 0;
    // Trunk Recorder's "error" is a fixed offset in Hz; here it is ppm.
    const ppm = typeof s.ppm === "number" ? s.ppm : typeof s.error === "number" && center ? Math.round((s.error / center) * 1e6 * 100) / 100 : 0;
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
    if (s.driver && s.driver !== "osmosdr") {
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
        ppm,
        ...autoTune,
      });
      notes.push(
        `Airspy source imported (${manual ? "its LNA, mixer and VGA stages" : agc ? "AGC" : "gain as the linearity step 0–21"}): it needs libairspy installed on this computer.`,
      );
      continue;
    }
    const serial = /rtl=([^,\s]+)/.exec(dev)?.[1] ?? "";
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
      ...autoTune,
    });
  }
  if (imported.length) cfg.sources = imported;
  const p25 = systems.filter((x) => x.type === "p25" || x.type === "smartnet" || x.type === "dmr");
  const conv = systems.filter((x) => x.type === "conventional" || x.type === "conventionalP25" || x.type === "conventionalDMR");
  const skipped = systems.filter((x) => !p25.includes(x) && !conv.includes(x)).map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped unsupported systems: ${skipped.join(", ")}.`);
  // Each conventional system is one here: its channels (listed, or its channel file), short name and rules.
  cfg.conventional = [];
  for (const sys of conv) {
    const mode: Channel["mode"] = sys.type === "conventionalP25" ? "p25" : sys.type === "conventionalDMR" ? "dmr" : "fm";
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
    if (channels.length) todo.push({ kind: "squelch", system: x.shortName });
    systemNames.set(sys, x.shortName);
    if (typeof sys.shortName === "string" && !names.has(sys.shortName)) names.set(sys.shortName, x.shortName);
  }
  if (conv.length > 1) notes.push(`${conv.length} conventional systems imported, each with its own folder and channels.`);
  if (p25.length) {
    // Trunk Recorder drops duplicates only among systems with multiSite on;
    // its multiSiteSystemName is a site group here (else grouped from the air).
    const multi = p25.filter((x) => x.multiSite === true);
    const isolated: string[] = [];
    cfg.systems = [];
    for (const sys of p25) {
      const msName = typeof sys.multiSiteSystemName === "string" ? sys.multiSiteSystemName.trim() : "";
      const ownName = typeof sys.shortName === "string" ? sys.shortName : "";
      const siteGroup = sys.multiSite === true ? msName : multi.length && ownName ? ownName : "";
      if (sys.multiSite !== true && siteGroup) isolated.push(ownName);
      const x = newSystem(cfg, {
        shortName: typeof sys.shortName === "string" ? sys.shortName : undefined,
        controlChannelsHz: Array.isArray(sys.control_channels) ? (sys.control_channels as unknown[]).filter((v): v is number => typeof v === "number") : [],
        modulation: sys.modulation === "qpsk" || sys.modulation === "fsk4" ? sys.modulation : "auto",
        ...(Object.keys(trRecording(sys)).length ? { recording: trRecording(sys) } : {}),
        ...(sys.type === "smartnet" ? smartnetImport(sys) : {}),
        ...(sys.type === "dmr" ? dmrImport(sys) : {}),
        ...(siteGroup ? { siteGroup } : {}),
      });
      cfg.systems.push(x);
      systemNames.set(sys, x.shortName);
      if (ownName && !names.has(ownName)) names.set(ownName, x.shortName);
      if (typeof sys.talkgroupsFile === "string" && sys.talkgroupsFile) {
        const csv = files[sys.talkgroupsFile];
        if (csv === undefined) todo.push({ kind: "talkgroups", system: x.shortName, file: sys.talkgroupsFile });
        else {
          x.talkgroupsCsv = normalizeTalkgroupCsv(csv);
          x.talkgroupsName = baseName(sys.talkgroupsFile);
        }
      }
      if (sys.siteId !== undefined && sys.siteId !== null && sys.siteId !== "") todo.push({ kind: "siteLock", system: x.shortName, siteId: String(sys.siteId) });
      const units = trUnitNames(sys, files, x.shortName, todo);
      if (units) x.unitNames = units;
    }
    if (p25.length > 1) notes.push(`${p25.length} systems imported, each with its own folder.`);
    if (multi.length && isolated.length) notes.push(`multiSite was off for ${isolated.join(", ")}: each has a site group of its own, so every call there is saved.`);
    if (!multi.length && p25.length > 1)
      notes.push("A call heard on several sites of one system is now saved once (Trunk Recorder's multiSite was off) — switch it off under Recording to keep every copy.");
  }
  const owners = [...cfg.systems.map((x) => x as { recording?: RecordingOverride }), ...cfg.conventional];
  // An instance-level format is every system's that has none of its own (a system's own wins, as in Trunk Recorder).
  if (typeof j.filenameFormat === "string" && j.filenameFormat.trim()) {
    const top = j.filenameFormat.trim();
    if (!owners.length) cfg.recording.filenameFormat = top;
    for (const o of owners) if (o.recording?.filenameFormat === undefined) o.recording = { ...o.recording, filenameFormat: top };
  }
  // A setting the same on every system there is the Recording tab's here; the rest stay each system's own.
  if (owners.length) {
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
  if (systems.some((x) => x.conversationMode === false)) notes.push("conversationMode off (one file per transmission) isn't supported: each call is one file.");
  const { plugins, other } = trPlugins(j, names, systemNames);
  if (other.length) todo.push({ kind: "plugins", names: other });
  return { config: cfg, notes, todo, plugins };
}
