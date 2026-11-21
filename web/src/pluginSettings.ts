// Plugin settings as plugins' JSON Schemas lay them out, apart from the
// pages that show them (so the setup guide and Setup can share these
// without importing each other).

import { copyOverride, pluginCopies } from "./protocol.ts";
import type { Config, PluginInfo, PluginManifest, PluginSchema, PluginValues } from "./protocol.ts";

/** A schema's fields, in its `x-order`. */
export function fieldsOf(schema: PluginSchema | undefined): [string, PluginSchema][] {
  const props = schema?.properties ?? {};
  const order = schema?.["x-order"] ?? Object.keys(props);
  return order.filter((k) => props[k]).map((k) => [k, props[k]]);
}

/** The plugin's settings a system may set again for itself (`system_overrides`, each one of its settings): none unless it names them. */
export function overridable(m: PluginManifest): [string, PluginSchema][] {
  const offered = new Set(m.system_overrides ?? []);
  return fieldsOf(m.config).filter(([k]) => offered.has(k));
}

/** The plugin's settings schema, cut down to those a system may set again. */
export function overridableSchema(m: PluginManifest): PluginSchema {
  const fields = overridable(m);
  const keys = new Set(fields.map(([k]) => k));
  return { ...m.config, properties: Object.fromEntries(fields), "x-order": fields.map(([k]) => k), required: (m.config?.required ?? []).filter((k) => keys.has(k)) };
}

/** What a system's copy of a plugin sets again of those it may (the rest are ignored: [`ignoredOf`]). */
export function overrideOf(m: PluginManifest, copy: PluginValues | undefined): PluginValues {
  const keys = new Set(overridable(m).map(([k]) => k));
  return Object.fromEntries(Object.entries(copyOverride(copy)).filter(([k]) => keys.has(k)));
}

/** What a system's copy of a plugin sets again that it may not: the recorder ignores them. */
export function ignoredOf(m: PluginManifest, copy: PluginValues | undefined): string[] {
  const keys = new Set(overridable(m).map(([k]) => k));
  return Object.entries(copyOverride(copy))
    .filter(([k, v]) => !keys.has(k) && v !== null && v !== undefined)
    .map(([k]) => k);
}

/** Plugin `id` is turned off for the system with id `system` (`systemsOff`). */
export function offFor(c: Config, id: string, system: number | undefined): boolean {
  return system !== undefined && !!c.plugins?.[id]?.systemsOff?.includes(system);
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
      }
  }
}

