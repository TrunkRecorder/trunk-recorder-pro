// Config helpers for the setup form (the recorder validates again).

import type { Channel, Config, Source } from "./protocol.ts";
import { splitCsvLine } from "./talkgroups.ts";

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** A fresh config (the web build; the desktop app gets its own from the recorder). */
export function defaultConfig(): Config {
  return {
    sources: [newDongle()],
    system: { shortName: "sys1", type: "p25", controlChannels: [], modulation: "auto", talkgroupsCsv: "", talkgroupsName: "" },
    conventional: { squelchDb: 8, channels: [] },
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
    },
    server: { bind: "127.0.0.1", port: 8080, autoStart: false },
  };
}

/** Sample rates offered for a USRP (any rate its master clock divides to works). */
export const USRP_RATES = [2_400_000, 4_000_000, 5_000_000, 6_400_000, 8_000_000, 10_000_000, 12_500_000, 16_000_000, 20_000_000];
/** Airspy R2: 10 / 2.5; Mini: 6 / 3 (and 10 with newer firmware). */
export const AIRSPY_RATES = [10_000_000, 6_000_000, 3_000_000, 2_500_000];

export function newDongle(): Source {
  return { kind: "rtlsdr", serial: "", centerHz: 0, rateHz: 2_400_000, gainDb: 38.6, ppm: 0 };
}
export function newUsrp(): Source {
  return { kind: "usrp", args: "", centerHz: 0, rateHz: 8_000_000, gainDb: 40, antenna: "", ppm: 0 };
}
export function newAirspy(): Source {
  return { kind: "airspy", serial: "", centerHz: 0, rateHz: 6_000_000, gain: 14, biasTee: false, ppm: 0 };
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

/** The frequencies the first source's automatic centre is placed over. */
export function autoCenterFor(c: Config, rateHz: number): number | null {
  const ccs = c.system.controlChannels;
  return autoCenter([...ccs, ...enabledChannels(c).map((ch) => ch.freqHz)], rateHz) ?? autoCenter(ccs, rateHz);
}

/** Each source's centre: as set, or automatic for the first (over the control and conventional channels). */
export function resolvedCenters(c: Config): (number | null)[] {
  return c.sources.map((s, i) => (s.centerHz > 0 || i > 0 ? s.centerHz || null : autoCenterFor(c, s.rateHz)));
}

/** A conventional channel's talkgroup when none is given: its frequency in kHz. */
export function defaultTalkgroup(freqHz: number): number {
  return Math.round(freqHz / 1000);
}

/** Why the config can't start, or null (the recorder's Config::problem). */
export function startProblem(c: Config): string | null {
  if (!c.sources.length) return "Add a source: a dongle or a capture file.";
  const trunked = c.system.controlChannels.length > 0;
  const channels = enabledChannels(c);
  if (!trunked && !channels.length) return "Add a control channel (trunked system) or a conventional channel.";
  const centers = resolvedCenters(c);
  if (centers.some((x) => !x)) return "Set a center frequency for every source (the first can be automatic when the channels fit one source).";
  const inside = (f: number) => c.sources.some((s, i) => Math.abs(f - (centers[i] ?? 0)) <= usableHalfWidth(s.rateHz));
  if (trunked && !c.system.controlChannels.some(inside)) return "No control channel falls inside any source's bandwidth — move a center frequency.";
  if (channels.some((ch) => !(ch.freqHz > 0))) return "A conventional channel has no frequency yet.";
  const outside = channels.filter((ch) => !inside(ch.freqHz)).map((ch) => formatMhz(ch.freqHz));
  if (outside.length) return `Conventional channel(s) outside every source's bandwidth: ${outside.join(", ")} MHz — move a center frequency or disable them.`;
  const seen = new Set<number>();
  const dup = channels.find((ch) => (seen.has(Math.round(ch.freqHz)) ? true : (seen.add(Math.round(ch.freqHz)), false)));
  if (dup) return `Conventional channel ${formatMhz(dup.freqHz)} MHz is listed twice.`;
  if (c.sources.some((s) => s.kind === "file" && !s.path)) return "Choose the capture file to replay.";
  return null;
}

// Conventional channels as CSV, for a spreadsheet. The same rules as the
// recorder's channel file (crates/trunk-app/src/channels.rs) — keep them together.

/** The columns channelsToCsv writes. */
export const CHANNEL_CSV_HEADER = "TG Number,Frequency,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Enable";

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
  let toned = 0;
  for (const { row, l } of lines.slice(1)) {
    const f = splitOn(l, delim);
    const at = (i: number) => (i >= 0 ? f[i] ?? "" : "");
    const freqHz = freqCell(at(cFreq), decimalComma);
    if (freqHz === null) {
      badFreq.push(row);
      continue;
    }
    const m = at(cMode).toLowerCase();
    let mode: Channel["mode"] = "fm";
    if (["p25", "digital", "d"].includes(m)) mode = "p25";
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
    if (Number(at(cTone)) > 0) toned++;
    channels.push(ch);
  }
  const notes: string[] = [];
  if (badFreq.length) notes.push(`Skipped row(s) ${rowsList(badFreq)} — no usable frequency.`);
  if (badMode.length) notes.push(`Row(s) ${rowsList(badMode)}: unknown Mode (use fm or p25) — read as fm.`);
  if (badTg.length) notes.push(`Row(s) ${rowsList(badTg)}: TG Number isn't a positive whole number — using the default.`);
  if (badSq.length) notes.push(`Row(s) ${rowsList(badSq)}: Squelch dB must be 3–40 (dB above the noise) — using the default.`);
  if (toned) notes.push(`${toned} channel(s) have a Tone: tones aren't matched yet, so they record whatever is on the frequency.`);
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

/**
 * Import a Trunk Recorder config.json: its RTL-SDR sources (one per dongle)
 * and first P25 system. What can't be carried over is reported.
 */
export function importTrunkRecorderConfig(text: string, base: Config): { config: Config; notes: string[] } {
  const j = JSON.parse(text) as Record<string, unknown>;
  const notes: string[] = [];
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
      gainDb: s.agc === true ? null : gain ?? 38.6,
      ppm: Math.round(ppm),
    });
  }
  if (imported.length) cfg.sources = imported;
  const p25 = systems.filter((x) => x.type === "p25");
  const conv = systems.filter((x) => x.type === "conventional" || x.type === "conventionalP25");
  const skipped = systems.filter((x) => x.type !== "p25" && !conv.includes(x)).map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped unsupported systems: ${skipped.join(", ")}.`);
  const importedChannels: Channel[] = [];
  for (const sys of conv) {
    const mode: Channel["mode"] = sys.type === "conventionalP25" ? "p25" : "fm";
    if (Array.isArray(sys.channels)) {
      for (const f of sys.channels as unknown[]) if (typeof f === "number" && f > 0) importedChannels.push({ freqHz: f, mode, name: "", enabled: true });
    }
    if (typeof sys.channelFile === "string") {
      notes.push(`Channel file "${sys.channelFile}" (${mode === "p25" ? "P25" : "analog"}): load it under Conventional channels → Import CSV.`);
    }
  }
  if (importedChannels.length) {
    cfg.conventional = { ...(cfg.conventional ?? { squelchDb: 8, channels: [] }), channels: importedChannels };
    notes.push(`${importedChannels.length} conventional channel(s) imported. Squelch here is dB above the noise floor (default 8), not Trunk Recorder's absolute level.`);
  }
  if (!p25.length && conv[0] && typeof conv[0].shortName === "string") cfg.system.shortName = conv[0].shortName;
  if (p25.length > 1) notes.push(`${p25.length} P25 systems in the file; only the first was imported.`);
  const sys = p25[0];
  if (sys) {
    if (typeof sys.shortName === "string") cfg.system.shortName = sys.shortName;
    if (Array.isArray(sys.control_channels)) cfg.system.controlChannels = (sys.control_channels as number[]).filter((v) => typeof v === "number");
    if (sys.modulation === "qpsk" || sys.modulation === "fsk4") cfg.system.modulation = sys.modulation;
    if (typeof sys.talkgroupsFile === "string") notes.push(`Talkgroups file "${sys.talkgroupsFile}": load the CSV in Setup.`);
    if (typeof sys.recordUnknown === "boolean") cfg.recording.recordUnknown = sys.recordUnknown;
  }
  if (typeof j.captureDir === "string") cfg.recording.captureDir = j.captureDir;
  if (typeof j.callTimeout === "number") cfg.recording.callTimeoutS = j.callTimeout;
  if (typeof j.recordUUVCalls === "boolean") cfg.recording.recordUnitToUnit = j.recordUUVCalls;
  if (Array.isArray(j.plugins) && j.plugins.length) notes.push("Plugins (uploaders, streamers) aren't available yet.");
  return { config: cfg, notes };
}
