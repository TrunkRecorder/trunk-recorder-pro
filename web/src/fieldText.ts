// What a setup input shows for a value, and the value it reads back from
// what was typed. Pure, so `npm test` covers them; DraftInput (inputs.tsx)
// pairs one with an <input>.
import { formatGain, formatMhz, parseFreqList } from "./config.ts";

export interface TextCodec<T> {
  format: (v: T) => string;
  parse: (text: string) => T;
}

/** Text, kept trimmed (a space typed at the end stays in the field until it's left). */
export const asTyped: TextCodec<string> = {
  format: (v) => v,
  parse: (text) => text.trim(),
};

/** A number; "," works as the decimal point. Blank or unreadable = undefined. */
export const decimal: TextCodec<number | undefined> = {
  format: (v) => (v === undefined ? "" : String(v)),
  parse: (text) => {
    const v = text.trim().replace(",", ".");
    return v === "" || !Number.isFinite(Number(v)) ? undefined : Number(v);
  },
};

/** A gain in dB, shown to a tenth (the value kept may be longer: an RTL-SDR step is an f32). */
export const gain: TextCodec<number | undefined> = {
  format: (v) => (v === undefined ? "" : formatGain(v)),
  parse: decimal.parse,
};

/** A whole number ≥ 0 in hex (293, 0x293). Blank or unreadable = undefined. */
export const hex: TextCodec<number | undefined> = {
  format: (v) => (v === undefined ? "" : v.toString(16).toUpperCase()),
  parse: (text) => {
    const v = text.trim().replace(/^0x/i, "");
    if (!/^[0-9a-f]+$/i.test(v)) return undefined;
    return parseInt(v, 16);
  },
};

/** One frequency in MHz (or Hz); blank or unreadable = 0. */
export const mhz: TextCodec<number> = {
  format: (hz) => (hz ? formatMhz(hz) : ""),
  parse: (text) => parseFreqList(text)[0] ?? 0,
};

/** A sample rate in MSPS (2.4 = 2,400,000); unreadable or not above 0 = 0. */
export const msps: TextCodec<number> = {
  format: (hz) => String(hz / 1e6),
  parse: (text) => {
    const v = Number(text.trim().replace(",", "."));
    return Number.isFinite(v) && v > 0 ? Math.round(v * 1e6) : 0;
  },
};

/** Frequencies in MHz, with commas or spaces between. */
export const mhzList: TextCodec<number[]> = {
  format: (hz) => hz.map((f) => formatMhz(f)).join(", "),
  parse: parseFreqList,
};

/** A channel table: "101=452.275, 102=452.3" ↔ { "101": 452275000, … }. */
export const lcnTable: TextCodec<Record<string, number>> = {
  format: (t) =>
    Object.entries(t)
      .map(([k, v]) => `${k}=${(v / 1e6).toFixed(5).replace(/0+$/, "")}`)
      .join(", "),
  parse: (text) => {
    const t: Record<string, number> = {};
    for (const part of text.split(/[,;\s]+/)) {
      const m = /^(\d+)=(\d+(?:\.\d+)?)$/.exec(part.trim());
      if (m) {
        const v = Number(m[2]);
        t[m[1]] = v < 1e5 ? Math.round(v * 1e6) : v;
      }
    }
    return t;
  },
};

/** Seconds, typed in minutes (1.5 = 90 s). Blank, unreadable or below 0 = 0. */
export const minutes: TextCodec<number> = {
  format: (s) => (s > 0 ? String(Math.round((s / 60) * 100) / 100) : ""),
  parse: (text) => {
    const v = Number(text.trim().replace(",", "."));
    return text.trim() === "" || !Number.isFinite(v) || v < 0 ? 0 : Math.round(v * 60);
  },
};

const items = (text: string, sep: RegExp) =>
  text
    .split(sep)
    .map((x) => x.trim())
    .filter((x) => x !== "");

/** Items split by `sep`, empty ones dropped. */
export function textList(sep: RegExp): TextCodec<string[]> {
  return { format: (v) => v.join(", "), parse: (text) => items(text, sep) };
}

/** Numbers split by `sep`; one that isn't a number comes back as NaN, for the field to flag. */
export function numberList(sep: RegExp): TextCodec<number[]> {
  return { format: (v) => v.join(", "), parse: (text) => items(text, sep).map(Number) };
}

/**
 * Talkgroups as typed ("1001, 1000-1099, 507*"): commas, semicolons or spaces between, and a range's dash with spaces
 * around it or not ("1000 - 1099" is one range). As the plugin SDK's `talkgroup_list` reads them.
 */
export const talkgroupList: TextCodec<string[]> = {
  format: (v) => v.join(", "),
  parse: (text) => items(text.replace(/\s*-\s*/g, "-"), /[\s,;]+/),
};

/** What's wrong with a talkgroup list, as the SDK's `check_talkgroup` has it: its first entry that isn't a talkgroup, a range or a pattern. null: nothing. */
export function talkgroupProblem(list: readonly unknown[]): string | null {
  const number = (s: string) => /^\d+$/.test(s) && Number(s) <= 0xffffffff;
  const ok = (e: string) => {
    const dash = e.indexOf("-");
    if (dash >= 0) return number(e.slice(0, dash).trim()) && number(e.slice(dash + 1).trim());
    return /[*?]/.test(e) ? /^[\d*?]+$/.test(e) : number(e);
  };
  const bad = list.map((x) => String(x).trim()).find((e) => e && !ok(e));
  return bad === undefined ? null : `"${bad}" isn't a talkgroup: numbers, ranges like 1000-1099, or patterns like 507*`;
}

/** A short name: it names a folder, so letters, digits, "_", "." and "-" only. */
export function cleanShortName(text: string): string {
  return text.replace(/[^\w.-]/g, "");
}

export function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}
