// Upgrading what an older version left — for a while only. Before 2026-10
// each site was a system of its own (research/multi-site-systems.md), and
// talkgroups were kept as talkgroup files' text (before v0.2.12), and unit
// names in the config (`unitNames.csv`, before v0.3.1). An
// older config is upgraded by the recorder's crates/trunk-app/src/migrate.rs
// (the page reads configs through it: rules.ts); what is the page's own, a
// selection kept by an older Listen page, goes through here. To drop the
// upgrade, delete both (and their tests) and the few lines the compiler then
// points at.

import type { Talkgroup, UnitName } from "./protocol.ts";
import { configTalkgroups, configUnitNames } from "./rules.ts";
import { readTalkgroupFile } from "./talkgroups.ts";
import { readUnitNamesFile } from "./units.ts";

/**
 * The talkgroups this browser kept (`kept`, by short name), with what an
 * older version kept them as read in: a talkgroup file's text (before
 * v0.2.12), and the files an older config held (`rawConfig`'s
 * `talkgroupsCsv`, before v0.1.16, read as the recorder reads them:
 * trunk_app::migrate::talkgroups_of). Saved again, they're rows.
 */
export function migrateTalkgroups(rawConfig: unknown, kept: Record<string, Talkgroup[] | string>): Record<string, Talkgroup[]> {
  const old = JSON.parse(configTalkgroups(JSON.stringify(rawConfig ?? {}))) as Record<string, string>;
  const rows = (v: Talkgroup[] | string) => (typeof v === "string" ? readTalkgroupFile(v).talkgroups : v);
  return Object.fromEntries(Object.entries({ ...old, ...kept }).map(([k, v]) => [k, rows(v)] as const).filter(([, v]) => v.length));
}

/**
 * The unit names this browser kept (`kept`, by short name), with those an
 * older config held (`rawConfig`'s `unitNames.csv`, before v0.3.1, read as
 * the recorder reads them: trunk_app::migrate::unit_names_of), each from
 * that file. Its own kept win. Saved again, they're rows.
 */
export function migrateUnitNames(rawConfig: unknown, kept: Record<string, UnitName[]>): Record<string, UnitName[]> {
  const old = JSON.parse(configUnitNames(JSON.stringify(rawConfig ?? {}))) as Record<string, string>;
  const at = Math.floor(Date.now() / 1000);
  const moved = Object.entries(old)
    .filter(([k]) => !kept[k]?.length)
    .map(([k, csv]) => [k, readUnitNamesFile(csv).unitNames.map((r) => ({ ...r, origin: { kind: "file" as const, name: "the config", at } }))] as const)
    .filter(([, v]) => v.length);
  return { ...Object.fromEntries(moved), ...kept };
}

/**
 * A Listen selection kept before systems had sites: a system of sites was
 * heard under its group's name ("p25:bee00.1a2"), its sites listed in
 * `sites` — it is its first site's (the system's) name now. `kept` as read.
 */
export function migrateSelection<T>(kept: T & { sites?: unknown }): { name: (n: string) => string; key: (k: string) => string } {
  const groups = new Map<string, string>();
  for (const [site, g] of Object.entries(kept.sites && typeof kept.sites === "object" ? kept.sites : {})) if (typeof g === "string" && !groups.has(g)) groups.set(g, site);
  const name = (n: string) => groups.get(n) ?? n;
  return { name, key: (k: string) => `${name(k.slice(0, k.lastIndexOf(":")))}${k.slice(k.lastIndexOf(":"))}` };
}
