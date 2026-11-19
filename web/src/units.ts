// A system's radios' names: its own (Trunk Recorder's unitTagsFile, kept as
// rows beside the config and read, written and edited by the recorder's own
// code through rules.ts), and the talker aliases heard, as the recorder saves
// them — Trunk Recorder's unitTagsOTA CSV (crates/trunk-core/src/trunk/units.rs):
// headerless, unitID,alias,source,timestamp,WACN,SYS,talkgroup; the newest line wins.

import { systemOf, type CallEntry, type Origin, type UnitName } from "./protocol.ts";
import { unitNamesEdit, unitNamesFromCsv, unitNamesToCsv, type UnitNamesFileReport } from "./rules.ts";
import { splitCsvLine } from "./talkgroups.ts";

/** Unit ID → alias. */
export type UnitAliases = Record<number, string>;

/** Each system's radios whose IDs came in MDC1200 (by short name, then unit ID). */
export type MdcUnits = Record<string, Record<number, true>>;

/** `mdc` with the MDC1200 radios of `calls` (their `srcList`'s `signal_system`); `mdc` itself when there are none new. */
export function mdcHeard(mdc: MdcUnits, calls: CallEntry[]): MdcUnits {
  let out = mdc;
  for (const c of calls) {
    const system = systemOf(c);
    for (const x of c.record.srcList ?? []) {
      if (x.signal_system !== "MDC1200" || out[system]?.[x.src]) continue;
      if (out === mdc) out = { ...mdc };
      out[system] = { ...out[system], [x.src]: true };
    }
  }
  return out;
}

/** A radio's ID as shown: an MDC1200 one in hex, 4 digits, as radios are programmed (7187 → "1C13"); any other in decimal. */
export function unitIdText(mdc: Record<number, true> | undefined, id: number): string {
  return mdc?.[id] ? id.toString(16).toUpperCase().padStart(4, "0") : String(id);
}

/** Whether a radio's ID matches `filter` (lower case) as typed: its decimal, or the hex it's shown in (with or without 0x). */
export function unitIdMatches(mdc: Record<number, true> | undefined, id: number, filter: string): boolean {
  return String(id).includes(filter) || (!!mdc?.[id] && unitIdText(mdc, id).toLowerCase().includes(filter.replace(/^0x/, "")));
}

/** A unit names file (Trunk Recorder's unitTagsFile) read as the recorder reads one. */
export function readUnitNamesFile(text: string): UnitNamesFileReport {
  return unitNamesFromCsv(text);
}

/** Radio names as a unit names file to download. */
export function unitNamesFileText(rows: UnitName[]): string {
  return unitNamesToCsv(rows);
}

/** `rows` with radio `unit` named `name` by `origin` ("": its own name taken out; its own row goes before the patterns): the recorder's own edit. */
export function withUnitName(rows: UnitName[], unit: number, name: string, origin: Origin): UnitName[] {
  return unitNamesEdit(rows, unit, name, origin);
}

/** Radio `unit`'s own row of `rows` (not a pattern's). */
export function ownUnitName(rows: UnitName[] | undefined, unit: number): UnitName | undefined {
  return rows?.find((r) => r.unit === String(unit));
}

/** Whether `rows` name radios by pattern too (the first match names a radio). */
export const isPattern = (r: UnitName) => r.unit.startsWith("/");

export function parseUnitsCsv(text: string): UnitAliases {
  const out: UnitAliases = {};
  const when: Record<number, number> = {};
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const [id, alias, , ts] = splitCsvLine(line);
    const unit = Number.parseInt(id ?? "", 10);
    const t = Number.parseInt(ts ?? "", 10) || 0;
    if (!Number.isFinite(unit) || !alias || (when[unit] ?? -1) > t) continue;
    out[unit] = alias;
    when[unit] = t;
  }
  return out;
}
