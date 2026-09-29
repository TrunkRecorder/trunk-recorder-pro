// Config helpers for the setup form (the recorder validates again).

import type { Config, Source } from "./protocol.ts";

export const SAMPLE_RATES = [2_400_000, 2_048_000, 2_560_000, 3_200_000, 1_920_000, 1_024_000];

/** A fresh config (the web build; the desktop app gets its own from the recorder). */
export function defaultConfig(): Config {
  return {
    sources: [newDongle()],
    system: { shortName: "sys1", type: "p25", controlChannels: [], modulation: "auto", talkgroupsCsv: "", talkgroupsName: "" },
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

/** Each source's centre: as set, or automatic for the first. */
export function resolvedCenters(c: Config): (number | null)[] {
  return c.sources.map((s, i) => (s.centerHz > 0 || i > 0 ? s.centerHz || null : autoCenter(c.system.controlChannels, s.rateHz)));
}

/** Why the config can't start, or null. */
export function startProblem(c: Config): string | null {
  if (!c.sources.length) return "Add a source: a dongle or a capture file.";
  if (!c.system.controlChannels.length) return "Add at least one control channel.";
  const centers = resolvedCenters(c);
  if (centers.some((x) => !x)) return "Set a center frequency for every source (the first can be automatic when the control channels fit one dongle).";
  const covered = c.system.controlChannels.some((f) => c.sources.some((s, i) => Math.abs(f - (centers[i] ?? 0)) <= usableHalfWidth(s.rateHz)));
  if (!covered) return "No control channel falls inside any source's bandwidth — move a center frequency.";
  if (c.sources.some((s) => s.kind === "file" && !s.path)) return "Choose the capture file to replay.";
  return null;
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
  const skipped = systems.filter((x) => x.type !== "p25").map((x) => `${String(x.shortName ?? "?")} (${String(x.type)})`);
  if (skipped.length) notes.push(`Skipped non-P25 systems: ${skipped.join(", ")}.`);
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
