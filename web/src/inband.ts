// In-band tone patterns (pages, alerts, digits, Morse IDs) as the interface
// writes, plays and matches them: the pure part of the Tones card and the tone editors.

import type { SignalDef, TonePatternKind, ToneRow } from "./protocol.ts";
import type { ToneSpec } from "./tonePlayer.ts";

export const PATTERN: Record<TonePatternKind, string> = {
  two_tone: "Two-tone",
  long: "Long tone",
  hi_low: "Hi-low",
  pulsed: "Pulsed",
  digits: "Digits",
  morse: "Morse",
  sequence: "Sequence",
  beep: "Beep",
};

/** Motorola Quick Call II's tones, by code, as the recorder's tone tables have them (crates/trunk-core/src/dsp/inband/tables.rs). */
const QC2: [string, number][] = (
  [
    ["11", [349.0, 368.5, 389.0, 410.8, 433.7, 457.9, 483.5, 510.5, 539.0, 330.5]],
    ["12", [600.9, 634.5, 669.9, 707.3, 746.8, 788.5, 832.5, 879.0, 928.1, 569.1]],
    ["14", [339.6, 358.6, 378.6, 399.8, 422.1, 445.7, 470.5, 496.8, 524.6, 321.7]],
    ["15", [584.8, 617.4, 651.9, 688.3, 726.8, 767.4, 810.2, 855.5, 903.2, 553.9]],
    ["19", [1153.4, 1185.2, 1217.8, 1251.4, 1285.8, 1321.2, 1357.6, 1395.0, 1433.4, 1122.5]],
    ["17", [1513.5, 1555.2, 1598.0, 1642.0, 1687.2, 1733.7, 1781.5, 1830.5, 1881.0, 1472.9]],
    ["20", [1989.0, 2043.8, 2094.5, 2155.6, 2212.2, 2271.7, 2334.6, 2401.0, 2468.2, 1930.2]],
  ] as [string, number[]][]
)
  .flatMap(([g, hz]) => hz.map((f, i): [string, number] => [`${g}${(i + 1) % 10}`, f]))
  .concat([["138", 288.5], ["108", 296.5], ["139", 304.7], ["109", 313.0], ["160", 953.7], ["130", 979.9], ["161", 1006.9], ["131", 1034.7], ["162", 1063.2], ["189", 1092.4]]);

/** QC2 tones other than `hz` itself that are within `tol` Hz of it: "321.7 Hz (QC2 140)". */
export function qc2Within(hz: number, tol: number): string[] {
  return QC2.filter(([, f]) => Math.abs(f - hz) <= tol && Math.abs(f - hz) > 0.002 * hz)
    .sort((a, b) => Math.abs(a[1] - hz) - Math.abs(b[1] - hz))
    .map(([code, f]) => `${f} Hz (QC2 ${code})`);
}

/** How far a heard tone may be from a configured one, Hz: as the recorder's tone tables have it. */
export function toneTolerance(hz: number, frame: boolean): number {
  return frame ? 15.7 + 0.005 * hz : 0.015 * hz;
}

/** A talkgroup list entry (a number, a range "1000-1099", a pattern "507*" / "12?45") names `tg`. */
export function talkgroupMatches(p: string, tg: number): boolean {
  const t = p.trim();
  const range = /^(\d+)\s*-\s*(\d+)$/.exec(t);
  if (range) return tg >= Number(range[1]) && tg <= Number(range[2]);
  const re = new RegExp(`^${t.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".")}$`);
  return re.test(String(tg));
}

/** Whether signal `d` is what `r` heard (as the recorder matches: crates/trunk-core/src/dsp/inband/signal.rs). */
export function toneMatches(d: SignalDef, r: ToneRow): boolean {
  if (d.pattern !== r.pattern) return false;
  if (d.talkgroups?.length && !d.talkgroups.some((p) => talkgroupMatches(p, r.tg))) return false;
  if (r.pattern === "digits") return (d.digits ?? "") === r.digits;
  if (r.pattern === "morse") return (d.digits ?? "").toUpperCase() === r.digits.toUpperCase();
  const want = d.tonesHz ?? [];
  return want.length === r.tones.length && want.every((f, i) => Math.abs(f - r.tones[i]) <= toneTolerance(f, r.frame) + (f * (d.extraTolerancePct ?? 0)) / 100);
}

/** A number to a hundredth, no trailing zeros: 781.25, 875, 0.98. */
export const plain = (v: number) => String(Math.round(v * 100) / 100);

/** The tones as text: "781.25 → 875 Hz", "812.5 / 1500 Hz", "12#", Morse's text. */
export function tonesText(r: { pattern: TonePatternKind; tones: number[]; digits?: string }): string {
  if (r.pattern === "digits") return r.digits ?? "";
  if (r.pattern === "morse") return `${r.digits ?? ""} (${plain(r.tones[0] ?? 0)} Hz)`;
  const hz = r.tones.map(plain);
  const sep = r.pattern === "hi_low" ? " / " : r.pattern === "sequence" ? " · " : " → ";
  return `${hz.slice(0, 8).join(sep)}${hz.length > 8 ? " …" : ""} Hz`;
}

/** An id as signals have them: letters, digits and _, upper case ("Norwalk EMS 1 page" → NORWALK_EMS_1_PAGE). */
export function signalId(text: string): string {
  return text.toUpperCase().replace(/[^A-Z0-9]+/g, "_").replace(/^_+|_+$/g, "").slice(0, 40);
}

/** What ▶ plays for a signal (or a tone typed in). */
export const defSpec = (d: Pick<SignalDef, "pattern" | "tonesHz" | "lengthsS" | "digits">): ToneSpec => ({ pattern: d.pattern, tones: d.tonesHz ?? [], lengths: d.lengthsS ?? [], digits: d.digits });
