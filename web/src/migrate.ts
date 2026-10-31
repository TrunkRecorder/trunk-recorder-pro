// Upgrading what an older version left — for a while only. Before 2026-10
// each site was a system of its own (research/multi-site-systems.md), and
// talkgroups were kept as talkgroup files' text (before v0.2.12). An
// older config is upgraded by the recorder's crates/trunk-app/src/migrate.rs
// (the page reads configs through it: rules.ts); what is the page's own, a
// selection kept by an older Listen page, goes through here. To drop the
// upgrade, delete both (and their tests) and the few lines the compiler then
// points at.

import type { Talkgroup } from "./protocol.ts";
import { configTalkgroups } from "./rules.ts";
import { readTalkgroupFile } from "./talkgroups.ts";

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
