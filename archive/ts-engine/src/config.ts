// Trunk Recorder Lite configuration: one source, one system, recording rules.
// Shaped after Trunk Recorder's config.json (docs/CONFIGURE.md) so a TR config
// can be imported, but flattened to what a single browser tab runs.

import type { P25Modulation } from "./protocols/p25/controlDecoder.ts";

export interface LiteConfig {
  source: {
    centerHz: number;
    rateHz: number;
    /** null = tuner AGC. */
    gainDb: number | null;
    ppm: number;
    serial: string;
  };
  system: {
    shortName: string;
    type: "p25";
    controlChannels: number[];
    modulation: P25Modulation;
    talkgroupsCsv: string;
    talkgroupsName: string;
  };
  recording: {
    prerollS: number;
    maxRecorders: number;
    callTimeoutS: number;
    recordUnknown: boolean;
    recordEncrypted: boolean;
    recordUnitToUnit: boolean;
    keepSilentCalls: boolean;
  };
}

export const DEFAULT_CONFIG: LiteConfig = {
  source: { centerHz: 0, rateHz: 2_400_000, gainDb: 38.6, ppm: 0, serial: "" },
  system: { shortName: "sys1", type: "p25", controlChannels: [], modulation: "auto", talkgroupsCsv: "", talkgroupsName: "" },
  recording: {
    prerollS: 1,
    maxRecorders: 32,
    callTimeoutS: 3,
    recordUnknown: true,
    recordEncrypted: false,
    recordUnitToUnit: true,
    keepSilentCalls: false,
  },
};

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** Usable half-width of a source, Hz (the edges are filter roll-off). */
export function usableHalfWidth(rateHz: number): number {
  return (rateHz / 2) * 0.9;
}

/**
 * A center frequency covering all control channels, kept off every one of them
 * by ≥ 25 kHz (the RTL-SDR's DC spike sits at the center). Null when the
 * control channels span more than one source can see.
 */
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

export interface ImportResult {
  config: LiteConfig;
  notes: string[];
}

/**
 * Import a Trunk Recorder config.json: the first source and the first P25
 * system. Anything Lite can't do is reported, not silently dropped.
 */
export function importTrunkRecorderConfig(text: string, base: LiteConfig = DEFAULT_CONFIG): ImportResult {
  const j = JSON.parse(text) as Record<string, unknown>;
  const notes: string[] = [];
  const cfg: LiteConfig = structuredClone(base);
  const sources = (j.sources as Record<string, unknown>[] | undefined) ?? [];
  const systems = (j.systems as Record<string, unknown>[] | undefined) ?? [];
  if (sources.length > 1) notes.push(`${sources.length} sources in the file; Lite uses one dongle, so only the first was imported.`);
  const s = sources[0];
  if (s) {
    if (typeof s.center === "number") cfg.source.centerHz = s.center;
    if (typeof s.rate === "number") cfg.source.rateHz = s.rate;
    if (typeof s.error === "number" && !s.ppm) notes.push(`Source "error" (${s.error} Hz) is not supported; set ppm instead.`);
    if (typeof s.ppm === "number") cfg.source.ppm = s.ppm;
    if (typeof s.gain === "number") cfg.source.gainDb = s.gain;
    if (s.agc === true) cfg.source.gainDb = null;
    const dev = typeof s.device === "string" ? s.device : "";
    const serial = /rtl=([^,\s]+)/.exec(dev)?.[1];
    if (serial && !/^\d$/.test(serial)) cfg.source.serial = serial;
    if (s.driver && s.driver !== "osmosdr") notes.push(`Source driver "${String(s.driver)}" — Lite only drives RTL-SDR dongles.`);
    if (cfg.source.rateHz > 3_200_000) {
      notes.push(`Rate ${cfg.source.rateHz} is above what an RTL-SDR can do; using 2.4 MSPS.`);
      cfg.source.rateHz = 2_400_000;
    }
  }
  const p25 = systems.filter((x) => x.type === "p25");
  const skipped = systems.filter((x) => x.type !== "p25").map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped non-P25 systems: ${skipped.join(", ")}. Lite supports P25 only for now.`);
  if (p25.length > 1) notes.push(`${p25.length} P25 systems in the file; only the first was imported.`);
  const sys = p25[0];
  if (sys) {
    if (typeof sys.shortName === "string") cfg.system.shortName = sys.shortName;
    if (Array.isArray(sys.control_channels)) cfg.system.controlChannels = (sys.control_channels as number[]).filter((v) => typeof v === "number");
    if (sys.modulation === "qpsk" || sys.modulation === "fsk4") cfg.system.modulation = sys.modulation;
    if (typeof sys.talkgroupsFile === "string") notes.push(`Talkgroups file "${sys.talkgroupsFile}" can't be read from here — load the CSV in Setup.`);
    if (typeof sys.recordUnknown === "boolean") cfg.recording.recordUnknown = sys.recordUnknown;
    if (typeof sys.hideEncrypted === "boolean" && sys.hideEncrypted === false) {
      /* display-only in TR */
    }
  }
  if (typeof j.callTimeout === "number") cfg.recording.callTimeoutS = j.callTimeout;
  if (typeof j.recordUUVCalls === "boolean") cfg.recording.recordUnitToUnit = j.recordUUVCalls;
  if (Array.isArray(j.plugins) && j.plugins.length) notes.push("Plugins (uploaders, streamers) are not available in Lite yet.");
  const out = cfg.system.controlChannels.filter((f) => Math.abs(f - cfg.source.centerHz) > usableHalfWidth(cfg.source.rateHz));
  if (out.length && cfg.source.centerHz) notes.push(`Control channel(s) ${out.map((f) => formatMhz(f)).join(", ")} MHz fall outside the source's bandwidth.`);
  return { config: cfg, notes };
}
