// Upgrading what an older version left — for a while only. Before 2026-10
// each site was a system of its own (research/multi-site-systems.md). What
// reads an older config, or a selection kept by an older Listen page, goes
// through here; the recorder's side is crates/trunk-app/src/migrate.rs. To
// drop the upgrade, delete both (and their tests) and the few lines the
// compiler then points at.

import { normalizeSystem, type SystemInput } from "./config.ts";
import type { SiteIdentity, System } from "./protocol.ts";

/** An older config's system: one site's fields at its top, joined to others by a site group. */
export type OldSystem = SystemInput & { siteGroup?: string };

/**
 * An older config's systems, each a site of its own, made systems with
 * sites (the recorder's migrate_sites): those joined by a site group, or by
 * site locks naming one system, become one — the first's short name and
 * settings, each site named as its system was. Returns what changed, to tell.
 */
export function migrateSystems(systems: OldSystem[]): { systems: System[]; notes: string[] } {
  if (systems.every((x) => Array.isArray(x.sites))) return { systems: systems.map(normalizeSystem), notes: [] };
  const notes = ["Systems have sites now: each system's control channels are its first site's."];
  const group = (x: OldSystem): string | null => {
    if (Array.isArray(x.sites)) return null;
    if (x.siteGroup?.trim()) return `group:${x.siteGroup.trim()}`;
    const e: SiteIdentity = x.expect && typeof x.expect === "object" ? x.expect : {};
    const h = (n: number) => n.toString(16);
    const type = x.type ?? "p25";
    if (type === "p25" && e.wacn != null && e.sysId != null) return `p25:${h(e.wacn)}.${h(e.sysId)}`;
    if (type === "smartnet" && e.sysId != null) return `smartnet:${h(e.sysId)}`;
    if (type === "nxdn" && x.nxdnType !== "typeD" && e.sysId != null) return `nxdn:${h(e.sysId)}`;
    return null;
  };
  const kept: [string | null, System, string[]][] = [];
  const what: [keyof System, string][] = [
    ["talkgroupsCsv", "talkgroup file"], ["unitNames", "unit names"], ["recording", "recording rules"], ["plugins", "plugin settings"],
    ["pluginDelays", "plugin delays"], ["keys", "keys"], ["defaultMode", "default mode"], ["nxdnType", "NXDN type"],
  ];
  for (const x of systems) {
    const g = group(x);
    const { siteGroup: _, ...rest } = x;
    const sys = normalizeSystem(rest);
    // Each old system's site is named as the system was.
    if (!Array.isArray(x.sites)) sys.sites[0].name = sys.shortName;
    const first = g ? kept.find(([h]) => h === g) : undefined;
    if (!first) {
      kept.push([g, sys, []]);
      continue;
    }
    const [, to, joined] = first;
    const dropped = what.filter(([k]) => sys[k] !== undefined && sys[k] !== "" && JSON.stringify(sys[k]) !== JSON.stringify(to[k])).map(([, w]) => w);
    if (dropped.length) notes.push(`${sys.shortName}'s ${dropped.join(", ")} weren't kept: ${to.shortName} has its system's now.`);
    if (sys.enabled) to.enabled = true;
    to.sites.push(...sys.sites.map((s) => ({ ...s, enabled: s.enabled && sys.enabled })));
    joined.push(sys.shortName);
  }
  for (const [, sys, joined] of kept) {
    if (joined.length) notes.push(`${sys.shortName} and ${joined.join(", ")} are one system now, ${sys.shortName}, with a site each.`);
    // A site named as its system leaves its name out.
    for (const site of sys.sites) if (site.name === sys.shortName) delete site.name;
  }
  return { systems: kept.map(([, sys]) => sys), notes };
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
