// Upgrading what an older version left — for a while only. Before 2026-10
// each site was a system of its own (research/multi-site-systems.md). An
// older config is upgraded by the recorder's crates/trunk-app/src/migrate.rs
// (the page reads configs through it: rules.ts); what is the page's own, a
// selection kept by an older Listen page, goes through here. To drop the
// upgrade, delete both (and their tests) and the few lines the compiler then
// points at.

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
