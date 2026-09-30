// Radios' talker aliases as the recorder saves them — Trunk Recorder's
// unitTagsOTA CSV (crates/trunk-core/src/trunk/units.rs): headerless,
// unitID,alias,source,timestamp,WACN,SYS,talkgroup; the newest line wins.

import { splitCsvLine } from "./talkgroups.ts";

/** Unit ID → alias. */
export type UnitAliases = Record<number, string>;

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
