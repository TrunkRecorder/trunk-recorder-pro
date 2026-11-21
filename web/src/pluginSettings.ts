// Plugin settings as plugins' JSON Schemas lay them out, apart from the
// pages that show them (so the setup guide and Setup can share these
// without importing each other).

import { copyOverride, copyPerSite, pluginCopies } from "./protocol.ts";
import type { Config, PluginInfo, PluginManifest, PluginSchema, PluginValues } from "./protocol.ts";

/** A schema's fields, in its `x-order`. */
export function fieldsOf(schema: PluginSchema | undefined): [string, PluginSchema][] {
  const props = schema?.properties ?? {};
  const order = schema?.["x-order"] ?? Object.keys(props);
  return order.filter((k) => props[k]).map((k) => [k, props[k]]);
}

/** The fields of `schema` that `names` names, in its order. */
function offered(schema: PluginSchema | undefined, names: string[] | undefined): [string, PluginSchema][] {
  const named = new Set(names ?? []);
  return fieldsOf(schema).filter(([k]) => named.has(k));
}

/** `schema` cut down to the fields `names` names. */
function cutDown(schema: PluginSchema | undefined, names: string[] | undefined): PluginSchema {
  const fields = offered(schema, names);
  const keys = new Set(fields.map(([k]) => k));
  return { ...schema, properties: Object.fromEntries(fields), "x-order": fields.map(([k]) => k), required: (schema?.required ?? []).filter((k) => keys.has(k)) };
}

/** The plugin's settings a system may set again for itself (`system_overrides`, each one of its settings): none unless it names them. */
export const overridable = (m: PluginManifest) => offered(m.config, m.system_overrides);

/** The plugin's settings schema, cut down to those a system may set again. */
export const overridableSchema = (m: PluginManifest) => cutDown(m.config, m.system_overrides);

/** The plugin's settings for each system a site may set again for itself (`site_overrides`): none unless it names them. */
export const siteOverridable = (m: PluginManifest) => offered(m.system_config, m.site_overrides);

/** Its settings-for-each-system schema, cut down to those a site may set again. */
export const siteSchema = (m: PluginManifest) => cutDown(m.system_config, m.site_overrides);

/** What a system's copy of a plugin sets again of those it may (the rest are ignored: [`ignoredOf`]). */
export function overrideOf(m: PluginManifest, copy: PluginValues | undefined): PluginValues {
  const keys = new Set(overridable(m).map(([k]) => k));
  return Object.fromEntries(Object.entries(copyOverride(copy)).filter(([k]) => keys.has(k)));
}

/** What a system's copy of a plugin, or one of its sites, sets again that it may not: the recorder ignores them. */
export function ignoredOf(m: PluginManifest, copy: PluginValues | undefined): string[] {
  const not = (values: PluginValues, ok: [string, PluginSchema][]) => {
    const keys = new Set(ok.map(([k]) => k));
    return Object.entries(values).filter(([k, v]) => !keys.has(k) && v !== null && v !== undefined).map(([k]) => k);
  };
  const sites = Object.values(copyPerSite(copy)).flatMap((v) => not(v, siteOverridable(m)));
  return [...new Set([...not(copyOverride(copy), overridable(m)), ...sites])];
}

/** Plugin `id` is turned off for the system with id `system` (`systemsOff`). */
export function offFor(c: Config, id: string, system: number | undefined): boolean {
  return system !== undefined && !!c.plugins?.[id]?.systemsOff?.includes(system);
}

/** Plugin `id` is turned off for the site with id `site` (`sitesOff`). */
export function offForSite(c: Config, id: string, site: number | undefined): boolean {
  return site !== undefined && !!c.plugins?.[id]?.sitesOff?.includes(site);
}

/** A system was renamed: plugin settings that name it (`x-system` fields, `x-systems` lists) follow. Changes `x`. */
export function renameSystemRefs(x: Config, from: string, to: string, plugins: PluginInfo[]): void {
  if (!from || from === to) return;
  const walk = (schema: PluginSchema | undefined, v: unknown) => {
    if (!v || typeof v !== "object") return;
    const obj = v as PluginValues;
    for (const [k, f] of fieldsOf(schema)) {
      if (f["x-system"] && obj[k] === from) obj[k] = to;
      else if (f["x-systems"] && Array.isArray(obj[k])) obj[k] = (obj[k] as unknown[]).map((n) => (n === from ? to : n));
      else if (f.type === "object") walk(f, obj[k]);
      else if (f.type === "array" && f.items?.type === "object" && Array.isArray(obj[k])) for (const item of obj[k] as unknown[]) walk(f.items, item);
    }
  };
  for (const p of plugins) {
    if (!p.manifest) continue;
    walk(p.manifest.config, x.plugins?.[p.id]?.settings);
    for (const sys of [...x.systems, ...x.conventional])
      for (const v of pluginCopies(sys.plugins?.[p.id])) {
        walk(p.manifest.system_config, v);
        walk(p.manifest.config, copyOverride(v));
        for (const site of Object.values(copyPerSite(v))) walk(p.manifest.system_config, site);
      }
  }
}

