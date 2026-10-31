// Plugin settings as plugins' JSON Schemas lay them out, apart from the
// pages that show them (so the setup guide and Setup can share these
// without importing each other).

import { copyOverride, pluginCopies } from "./protocol.ts";
import type { Config, PluginInfo, PluginSchema, PluginValues } from "./protocol.ts";

/** A schema's fields, in its `x-order`. */
export function fieldsOf(schema: PluginSchema | undefined): [string, PluginSchema][] {
  const props = schema?.properties ?? {};
  const order = schema?.["x-order"] ?? Object.keys(props);
  return order.filter((k) => props[k]).map((k) => [k, props[k]]);
}

/** A system was renamed: plugin settings that name it (`x-system` fields) follow. Changes `x`. */
export function renameSystemRefs(x: Config, from: string, to: string, plugins: PluginInfo[]): void {
  if (!from || from === to) return;
  const walk = (schema: PluginSchema | undefined, v: unknown) => {
    if (!v || typeof v !== "object") return;
    const obj = v as PluginValues;
    for (const [k, f] of fieldsOf(schema)) {
      if (f["x-system"] && obj[k] === from) obj[k] = to;
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

