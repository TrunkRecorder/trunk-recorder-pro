// Config helpers for the setup form (the recorder validates again).

import type { Channel, Config, SiteIdentity, Source, System } from "./protocol.ts";
import { splitCsvLine } from "./talkgroups.ts";
import { dmrTalkgroup, parseAccess, sameTone } from "./tones.ts";

/** A new RTL-SDR's gain, dB (higher overloads its front end near strong transmitters). */
export const RTL_DEFAULT_GAIN_DB = 25.4;

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** A fresh config (the web build; the desktop app gets its own from the recorder). */
export function defaultConfig(): Config {
  return {
    sources: [newDongle()],
    systems: [],
    conventional: { shortName: "conv", squelchDb: 8, channels: [] },
    recording: {
      captureDir: "",
      prerollS: 1,
      maxRecorders: 32,
      callTimeoutS: 3,
      recordUnknown: true,
      recordEncrypted: false,
      recordUnitToUnit: true,
      keepSilentCalls: false,
      captureFrames: false,
      dropDuplicateCalls: true,
      normalizeAudio: true,
      vocoder: "fixed",
    },
    server: { bind: "127.0.0.1", port: 8080, autoStart: false },
  };
}

/** Sample rates offered for a USRP (any rate its master clock divides to works). */
export const USRP_RATES = [2_400_000, 4_000_000, 5_000_000, 6_400_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000];
/** Airspy R2: 10 / 2.5; Mini: 6 / 3 (and 10 with newer firmware). */
export const AIRSPY_RATES = [10_000_000, 6_000_000, 3_000_000, 2_500_000];

export function newDongle(): Source {
  return { kind: "rtlsdr", serial: "", centerHz: 0, rateHz: 2_400_000, gainDb: RTL_DEFAULT_GAIN_DB, ppm: 0 };
}
export function newUsrp(): Source {
  return { kind: "usrp", args: "", centerHz: 0, rateHz: 8_000_000, gainDb: 40, antenna: "", ppm: 0 };
}
export function newAirspy(): Source {
  return { kind: "airspy", serial: "", centerHz: 0, rateHz: 6_000_000, gain: 14, biasTee: false, ppm: 0 };
}
/** Offered for SoapySDR devices; any rate the device takes can be typed. */
export const SOAPY_RATES = [2_000_000, 2_400_000, 2_500_000, 3_000_000, 6_000_000, 8_000_000, 10_000_000];
export function newSoapy(): Source {
  return { kind: "soapy", args: "", centerHz: 0, rateHz: 8_000_000, gainDb: null, gains: "", antenna: "", settings: "", ppm: 0 };
}
export function newFile(): Source {
  return { kind: "file", path: "", centerHz: 0, rateHz: 2_400_000, realtime: true, format: "cu8" };
}

/** A capture's sample format from its name (as the recorder guesses it). */
export function formatFromPath(path: string): "cu8" | "cs16" | "cf32" {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return ["cf32", "cfile", "fc32", "complex"].includes(ext) ? "cf32" : ["cs16", "sc16"].includes(ext) ? "cs16" : "cu8";
}

/** Usable half-width of a source (the edges are filter roll-off). */
export function usableHalfWidth(rateHz: number): number {
  return (rateHz / 2) * 0.9;
}

/** A centre that fits every control channel in one source, off the DC spike. */
export function autoCenter(controlChannels: number[], rateHz: number): number | null {
  if (!controlChannels.length) return null;
  const lo = Math.min(...controlChannels);
  const hi = Math.max(...controlChannels);
  const half = usableHalfWidth(rateHz);
  if (hi - lo > 2 * half - 50_000) return null;
  let c = Math.round((lo + hi) / 2 / 1000) * 1000;
  for (let step = 0; step < 40 && controlChannels.some((f) => Math.abs(f - c) < 25_000); step++) {
    c += (step % 2 ? -1 : 1) * 12_500 * (step + 1);
  }
  return controlChannels.every((f) => Math.abs(f - c) <= half - 10_000) ? c : null;
}

/** The conventional channels that are switched on. */
export function enabledChannels(c: Config): Channel[] {
  return (c.conventional?.channels ?? []).filter((ch) => ch.enabled);
}

/** Trunk Recorder's trunked DMR settings: `lcnTable` { "<lcn>": Hz } and `channels` (candidate voice frequencies). */
function dmrImport(sys: Record<string, unknown>): Partial<System> {
  const out: Partial<System> = { type: "dmr" };
  if (sys.lcnTable && typeof sys.lcnTable === "object") {
    const t: Record<string, number> = {};
    for (const [k, v] of Object.entries(sys.lcnTable as Record<string, unknown>)) if (typeof v === "number" && v > 0) t[k] = v;
    if (Object.keys(t).length) out.lcnTable = t;
  }
  if (Array.isArray(sys.channels)) out.channels = (sys.channels as unknown[]).filter((v): v is number => typeof v === "number" && v > 0);
  return out;
}

/** A system with defaults filled in (configs saved before a field existed). */
export function normalizeSystem(x: Partial<System>): System {
  return {
    shortName: x.shortName ?? "sys1",
    ...(x.name?.trim() ? { name: x.name } : {}),
    type: x.type === "smartnet" ? "smartnet" : x.type === "dmr" ? "dmr" : "p25",
    ...(x.type === "dmr"
      ? {
          ...(x.lcnTable && Object.keys(x.lcnTable).length ? { lcnTable: x.lcnTable } : {}),
          ...(x.channels?.length ? { channels: x.channels } : {}),
          ...(typeof x.colorCode === "number" ? { colorCode: x.colorCode } : {}),
        }
      : {}),
    ...(x.type === "smartnet"
      ? {
          bandplan: x.bandplan ?? "800_standard",
          ...(x.bandplanBase ? { bandplanBase: x.bandplanBase } : {}),
          ...(x.bandplanSpacing ? { bandplanSpacing: x.bandplanSpacing } : {}),
          ...(x.bandplanOffset ? { bandplanOffset: x.bandplanOffset } : {}),
          ...(x.bandplanHigh ? { bandplanHigh: x.bandplanHigh } : {}),
          ...(x.defaultMode === "analog" ? { defaultMode: "analog" as const } : {}),
        }
      : {}),
    enabled: x.enabled ?? true,
    controlChannels: x.controlChannels ?? [],
    modulation: x.modulation ?? "auto",
    talkgroupsCsv: x.talkgroupsCsv ?? "",
    talkgroupsName: x.talkgroupsName ?? "",
    expect: x.expect ?? {},
    voiceChannels: x.voiceChannels ?? [],
    ...(x.recordUnknown === true || x.recordUnknown === false ? { recordUnknown: x.recordUnknown } : {}),
    ...(x.siteGroup?.trim() ? { siteGroup: x.siteGroup } : {}),
  };
}

/**
 * A stored config in today's shape: before several systems it had one
 * `system`, and conventional calls were filed under its name (the recorder's
 * Config does the same — crates/trunk-app/src/config.rs).
 */
export function migrateConfig(raw: Record<string, unknown>): Config {
  const base = defaultConfig();
  const r = raw as Partial<Config> & { system?: Partial<System> };
  const conv = { ...base.conventional, ...(r.conventional ?? {}) };
  let systems: System[];
  if (Array.isArray(r.systems)) systems = r.systems.map(normalizeSystem);
  else if (r.system) {
    if (!(r.conventional && "shortName" in r.conventional)) conv.shortName = r.system.shortName ?? "sys1";
    const old = normalizeSystem(r.system);
    systems = old.controlChannels.length || old.talkgroupsCsv ? [old] : [];
  } else systems = [];
  return {
    sources: r.sources ?? base.sources,
    systems,
    conventional: conv,
    recording: { ...base.recording, ...(r.recording ?? {}) },
    server: { ...base.server, ...(r.server ?? {}) },
  };
}

/** The systems being recorded, in the recorder's order (SystemStatus.index). */
export function activeSystems(c: Config): System[] {
  return c.systems.filter((x) => x.enabled && x.controlChannels.length > 0);
}

/** A system's color (the dashboard, waterfall and setup agree): by its place among the active systems. */
export function systemColor(index: number): string {
  return index < 0 || index === 65535 ? "var(--muted)" : `var(--sys-${index % 6})`;
}

/** A short name not used yet: `base`, else base2, base3… */
export function uniqueShortName(c: Config, base: string, except?: System): string {
  const clean = base.replace(/[^\w.-]/g, "") || "sys";
  const taken = new Set(c.systems.filter((x) => x !== except).map((x) => x.shortName));
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

/** A SmartNet system's settings from a Trunk Recorder system (base / spacing / high in Hz or MHz). */
function smartnetImport(sys: Record<string, unknown>): Partial<System> {
  const hz = (v: unknown, mhzBelow: number) => (typeof v === "number" && v > 0 ? (v < mhzBelow ? Math.round(v * 1e6) : v) : undefined);
  const out: Partial<System> = { type: "smartnet", bandplan: typeof sys.bandplan === "string" ? sys.bandplan : "800_standard" };
  const base = hz(sys.bandplanBase, 1e5);
  const spacing = hz(sys.bandplanSpacing, 1);
  const high = hz(sys.bandplanHigh, 1e5);
  if (base) out.bandplanBase = base;
  if (spacing) out.bandplanSpacing = spacing;
  if (high) out.bandplanHigh = high;
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
  return c.systems.find((x) => [...x.controlChannels, ...(x.channels ?? [])].some((f) => Math.abs(f - hz) < 6_000));
}

/**
 * Each source's centre: as set, or (0 = auto) placed over what the sources
 * before it don't cover yet (the recorder's Config::resolved_centers).
 */
export function resolvedCenters(c: Config): (number | null)[] {
  // A DMR site's watched frequencies are all needed (as Config::resolved_centers).
  const groups: [number[], number[]][] = activeSystems(c).map((x) => {
    const need = [...x.controlChannels, ...(x.type === "dmr" ? (x.channels ?? []) : [])];
    return [[...need, ...x.voiceChannels], need];
  });
  const conv = enabledChannels(c)
    .map((ch) => ch.freqHz)
    .filter((f) => f > 0);
  if (conv.length) groups.push([conv, conv]);
  const centers = c.sources.map((s) => s.centerHz);
  const covered = (f: number) => c.sources.some((s, i) => centers[i] > 0 && Math.abs(f - centers[i]) <= usableHalfWidth(s.rateHz));
  for (let i = 0; i < c.sources.length; i++) {
    if (centers[i] > 0) continue;
    const open = groups.filter(([, need]) => !need.some(covered));
    if (!open.length) break;
    const rate = c.sources[i].rateHz;
    centers[i] =
      autoCenter(open.flatMap((g) => g[0]), rate) ?? autoCenter(open.flatMap((g) => g[1]), rate) ?? autoCenter(open[0][0], rate) ?? autoCenter(open[0][1], rate) ?? 0;
  }
  return centers.map((x) => x || null);
}

/** The source (index) whose usable band holds `hz`, or -1. */
export function sourceCovering(c: Config, centers: (number | null)[], hz: number): number {
  return c.sources.findIndex((s, i) => centers[i] !== null && Math.abs(hz - (centers[i] ?? 0)) <= usableHalfWidth(s.rateHz));
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

/** Why the config can't start, or null (the recorder's Config::problem). */
export function startProblem(c: Config): string | null {
  if (!c.sources.length) return "Add a source: a dongle or a capture file.";
  const systems = activeSystems(c);
  const channels = enabledChannels(c);
  if (!systems.length && !channels.length) return "Add a system with a control channel, or a conventional channel.";
  const names = new Set<string>();
  for (const x of systems) {
    if (!x.shortName) return "Every system needs a short name.";
    if (names.has(x.shortName)) return `Two systems are named "${x.shortName}" — each needs its own short name (its folder).`;
    names.add(x.shortName);
  }
  const centers = resolvedCenters(c);
  const missing = centers.findIndex((x) => !x);
  if (missing >= 0)
    return `Set a center frequency for source ${missing + 1} — it couldn't be placed automatically (nothing left for it to cover, or the channels don't fit one source).`;
  const inside = (f: number) => sourceCovering(c, centers, f) >= 0;
  const lost = systems.find((x) => !x.controlChannels.some(inside));
  if (lost) return `No control channel of ${lost.shortName} falls inside any source's bandwidth — move a center frequency or add a source.`;
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
  if (c.sources.some((s) => s.kind === "file" && !s.path)) return "Choose the capture file to replay.";
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
  /** A conventional channel file wasn't loaded; `had` channels were imported without it. */
  | { kind: "channels"; file: string; had: number }
  /** Trunk Recorder's siteId: a Site lock to fill in. */
  | { kind: "siteLock"; system: string; siteId: string }
  /** An RTL-SDR that wasn't free when imported (`serial` as configured). */
  | { kind: "source"; index: number; serial: string }
  /** A source whose driver isn't installed here. */
  | { kind: "driver"; index: number; driver: "usrp" | "airspy" | "soapy" }
  /** Conventional squelch means something else here. */
  | { kind: "squelch" }
  /** Trunk Recorder plugins with no counterpart here. */
  | { kind: "plugins"; names: string[] }
  /** A plugin whose settings came over: to add (install) and turn on. */
  | { kind: "plugin"; id: string; name: string }
  /** No source covers any of the system's control channels. */
  | { kind: "coverage"; system: string };

/** A plugin's settings brought over from Trunk Recorder: as the Plugins page keeps them, by plugin id. */
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

/** The talkgroup and channel files a Trunk Recorder config.json names, as it names them. */
export function trConfigFiles(text: string): { file: string; kind: "talkgroups" | "channels"; system: string }[] {
  const j = JSON.parse(text) as { systems?: Record<string, unknown>[] };
  const out: { file: string; kind: "talkgroups" | "channels"; system: string }[] = [];
  for (const sys of j.systems ?? []) {
    const name = typeof sys.shortName === "string" ? sys.shortName : "";
    if (typeof sys.talkgroupsFile === "string" && sys.talkgroupsFile) out.push({ file: sys.talkgroupsFile, kind: "talkgroups", system: name });
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
    if (s.driver === "usrp") {
      imported.push({
        kind: "usrp",
        args: dev,
        centerHz: center,
        rateHz: typeof s.rate === "number" ? s.rate : 8_000_000,
        gainDb: gain ?? 40,
        antenna: typeof s.antenna === "string" ? s.antenna : "",
        ppm,
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
      const gains = Object.entries({ IF: s.ifGain, BB: s.bbGain, MIX: s.mixGain, LNA: s.lnaGain, TIA: s.tiaGain, PGA: s.pgaGain, AMP: s.ampGain, VGA: s.vgaGain, VGA1: s.vga1Gain, VGA2: s.vga2Gain, ...(s.gainSettings as Record<string, unknown> | undefined) })
        .filter(([, v]) => typeof v === "number" && v !== 0)
        .map(([k, v]) => `${k}=${String(v)}`)
        .join(",");
      imported.push({
        kind: "soapy",
        args: soapyArgs,
        centerHz: center,
        rateHz: typeof s.rate === "number" ? s.rate : 8_000_000,
        gainDb: s.agc === true || gain === undefined ? null : gain,
        gains,
        antenna: typeof s.antenna === "string" ? s.antenna : "",
        settings: "",
        ppm,
      });
      notes.push(`"${dev}" imported as a SoapySDR source (${soapyArgs || "first found"}): it needs SoapySDR and the device's module installed on this computer.`);
      continue;
    }
    if (/airspy/.test(dev)) {
      const sn = /airspy=(0x)?([0-9a-fA-F]{8,16})/.exec(dev)?.[2] ?? "";
      const rate = typeof s.rate === "number" && AIRSPY_RATES.includes(s.rate) ? s.rate : 6_000_000;
      imported.push({ kind: "airspy", serial: sn.toUpperCase(), centerHz: center, rateHz: rate, gain: Math.max(0, Math.min(21, Math.round(gain ?? 14))), biasTee: /bias=1/.test(dev), ppm });
      notes.push("Airspy source imported (gain as the linearity step 0–21): it needs libairspy installed on this computer.");
      continue;
    }
    const serial = /rtl=([^,\s]+)/.exec(dev)?.[1] ?? "";
    let rate = typeof s.rate === "number" ? s.rate : 2_400_000;
    if (rate > 3_200_000) {
      notes.push(`Rate ${rate} is more than an RTL-SDR can do; using 2.4 MSPS.`);
      rate = 2_400_000;
    }
    imported.push({
      kind: "rtlsdr",
      serial: /^\d$/.test(serial) ? "" : serial,
      centerHz: center,
      rateHz: rate,
      gainDb: s.agc === true ? null : gain ?? RTL_DEFAULT_GAIN_DB,
      ppm: Math.round(ppm),
    });
  }
  if (imported.length) cfg.sources = imported;
  const p25 = systems.filter((x) => x.type === "p25" || x.type === "smartnet" || x.type === "dmr");
  const conv = systems.filter((x) => x.type === "conventional" || x.type === "conventionalP25" || x.type === "conventionalDMR");
  const skipped = systems.filter((x) => !p25.includes(x) && !conv.includes(x)).map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped unsupported systems: ${skipped.join(", ")}.`);
  const importedChannels: Channel[] = [];
  for (const sys of conv) {
    const mode: Channel["mode"] = sys.type === "conventionalP25" ? "p25" : sys.type === "conventionalDMR" ? "dmr" : "fm";
    if (Array.isArray(sys.channels)) {
      for (const f of sys.channels as unknown[]) if (typeof f === "number" && f > 0) importedChannels.push({ freqHz: f, mode, name: "", enabled: true });
    }
    if (typeof sys.channelFile === "string" && sys.channelFile) {
      const csv = files[sys.channelFile];
      const read = csv === undefined ? null : parseChannelCsv(csv);
      if (!read || read.error) todo.push({ kind: "channels", file: sys.channelFile, had: importedChannels.length });
      else {
        // A file with no Mode column is its system's kind.
        const moded = /(^|[,;\t])\s*"?mode"?\s*([,;\t]|$)/im.test(csv!.split(/\r?\n/, 1)[0] ?? "");
        importedChannels.push(...read.channels.map((ch) => (moded ? ch : { ...ch, mode })));
      }
    }
  }
  if (importedChannels.length) {
    cfg.conventional = { ...cfg.conventional, channels: importedChannels };
    todo.push({ kind: "squelch" });
  }
  if (conv[0] && typeof conv[0].shortName === "string") cfg.conventional.shortName = conv[0].shortName;
  for (const x of conv) {
    systemNames.set(x, cfg.conventional.shortName);
    if (typeof x.shortName === "string" && !names.has(x.shortName)) names.set(x.shortName, cfg.conventional.shortName);
  }
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
        controlChannels: Array.isArray(sys.control_channels) ? (sys.control_channels as unknown[]).filter((v): v is number => typeof v === "number") : [],
        modulation: sys.modulation === "qpsk" || sys.modulation === "fsk4" ? sys.modulation : "auto",
        ...(typeof sys.recordUnknown === "boolean" ? { recordUnknown: sys.recordUnknown } : {}),
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
          x.talkgroupsCsv = csv;
          x.talkgroupsName = baseName(sys.talkgroupsFile);
        }
      }
      if (sys.siteId !== undefined && sys.siteId !== null && sys.siteId !== "") todo.push({ kind: "siteLock", system: x.shortName, siteId: String(sys.siteId) });
    }
    if (p25.length > 1) notes.push(`${p25.length} systems imported, each with its own folder.`);
    if (multi.length && isolated.length) notes.push(`multiSite was off for ${isolated.join(", ")}: each has a site group of its own, so every call there is saved.`);
    if (!multi.length && p25.length > 1)
      notes.push("A call heard on several sites of one system is now saved once (Trunk Recorder's multiSite was off) — switch it off under Recording to keep every copy.");
    // The same setting on every system there: the default here.
    const unknown = p25.map((x) => x.recordUnknown).filter((v): v is boolean => typeof v === "boolean");
    if (unknown.length === p25.length && unknown.every((v) => v === unknown[0])) {
      cfg.recording.recordUnknown = unknown[0];
      for (const x of cfg.systems) delete x.recordUnknown;
    }
  }
  if (typeof j.captureDir === "string") cfg.recording.captureDir = j.captureDir;
  if (typeof j.callTimeout === "number") cfg.recording.callTimeoutS = j.callTimeout;
  if (typeof j.recordUUVCalls === "boolean") cfg.recording.recordUnitToUnit = j.recordUUVCalls;
  const { plugins, other } = trPlugins(j, names, systemNames);
  if (other.length) todo.push({ kind: "plugins", names: other });
  return { config: cfg, notes, todo, plugins };
}
