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

/** A frequency as MHz (has a decimal point, under 10 GHz in MHz) or Hz — Trunk Recorder's rule. */
function freqFrom(text: string): number | null {
  const v = Number(text);
  if (!Number.isFinite(v) || v <= 0) return null;
  return text.includes(".") && v < 10_000 ? Math.round(v * 1e6) : Math.round(v);
}

/**
 * Conventional channels from a CSV: Trunk Recorder's channelFile (TG Number,
 * Frequency, Tone, Alpha Tag, Description, Tag, Category, Enable, Squelch …)
 * plus an optional Mode column (fm / p25, or A / D). What can't carry over is
 * reported.
 */
export function parseChannelCsv(text: string, dfltMode: Channel["mode"] = "fm"): { channels: Channel[]; notes: string[] } {
  const lines = text.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith("#"));
  const notes: string[] = [];
  if (!lines.length) return { channels: [], notes: ["The file is empty."] };
  const head = splitCsvLine(lines[0]).map((h) => h.toLowerCase());
  const col = (...names: string[]) => head.findIndex((h) => names.includes(h));
  const cFreq = col("frequency", "freq", "freqhz");
  if (cFreq < 0) return { channels: [], notes: ["No Frequency column (the first row must name the columns)."] };
  const cTg = col("tg number", "talkgroup", "tg");
  const cName = col("alpha tag", "name");
  const cDesc = col("description");
  const cTag = col("tag");
  const cGroup = col("category", "group");
  const cEnable = col("enable", "enabled");
  const cMode = col("mode");
  const cTone = col("tone");
  const cSq = col("squelch");
  const channels: Channel[] = [];
  let toned = 0;
  let squelched = 0;
  let bad = 0;
  for (const line of lines.slice(1)) {
    const f = splitCsvLine(line);
    const at = (i: number) => (i >= 0 ? f[i] ?? "" : "");
    const freqHz = freqFrom(at(cFreq));
    if (freqHz === null) {
      bad++;
      continue;
    }
    const m = at(cMode).toLowerCase();
    const mode: Channel["mode"] = m === "p25" || m === "d" || m === "digital" ? "p25" : m === "fm" || m === "a" || m === "analog" ? "fm" : dfltMode;
    const tg = Number.parseInt(at(cTg), 10);
    const ch: Channel = { freqHz, mode, name: at(cName), enabled: at(cEnable).toLowerCase() !== "false" };
    if (Number.isFinite(tg) && tg > 0) ch.talkgroup = tg;
    if (at(cDesc)) ch.description = at(cDesc);
    if (at(cTag)) ch.tag = at(cTag);
    if (at(cGroup)) ch.group = at(cGroup);
    if (Number(at(cTone)) > 0) toned++;
    if (at(cSq)) squelched++;
    channels.push(ch);
  }
  if (bad) notes.push(`${bad} row(s) without a usable frequency were skipped.`);
  if (toned) notes.push(`${toned} channel(s) have a tone: tones aren't matched yet, so they record whatever is on the frequency.`);
  if (squelched) notes.push("Squelch values were not carried over: here squelch is dB above the measured noise floor, not an absolute level.");
  if (cMode < 0 && channels.length) notes.push(`No Mode column: imported as ${dfltMode === "fm" ? "analog FM" : "P25"}.`);
  return { channels, notes };
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
