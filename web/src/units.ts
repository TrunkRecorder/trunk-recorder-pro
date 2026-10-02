// Radios' talker aliases as the recorder saves them — Trunk Recorder's
// unitTagsOTA CSV (crates/trunk-core/src/trunk/units.rs): headerless,
// unitID,alias,source,timestamp,WACN,SYS,talkgroup; the newest line wins.

import { splitCsvLine } from "./talkgroups.ts";

/** Unit ID → alias. */
export type UnitAliases = Record<number, string>;

/** A unit names file's patterns (the recorder's UnitTags, crates/trunk-core/src/trunk/units.rs). */
const namers = new Map<string, (id: number) => string | undefined>();

/** The name a unit names file gives `id` (first match; `$1` / `\1` take a pattern's groups). */
export function unitName(csv: string, id: number): string | undefined {
  let f = namers.get(csv);
  if (!f) {
    const tags: [RegExp, string][] = [];
    for (const line of csv.split(/\r?\n/)) {
      const t = line.trim();
      if (!t || t.startsWith("#")) continue;
      const [pat = "", name = ""] = splitCsvLine(t).map((x) => x.trim());
      if (!pat || !name) continue;
      try {
        const re = pat.length > 1 && pat.startsWith("/") && pat.endsWith("/") ? new RegExp(pat.slice(1, -1)) : new RegExp(`^${pat.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`);
        tags.push([re, name.replace(/\\(\d+)/g, "$$$1")]);
      } catch {
        // Not a regular expression: left out, as the recorder does.
      }
    }
    f = (n: number) => {
      const s = String(n);
      const hit = tags.find(([re]) => re.test(s));
      return hit ? s.replace(hit[0], hit[1]) : undefined;
    };
    namers.set(csv, f);
  }
  return f(id);
}

/** Lines in a unit names file. */
export function unitNameCount(csv: string): number {
  return csv.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith("#") && l.includes(",")).length;
}

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
