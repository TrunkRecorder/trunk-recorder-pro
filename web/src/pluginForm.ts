// What a plugin's settings form needs beyond drawing one field: lists that
// name items of another list (`x-ref`, kept in step when an item is renamed),
// and the items a plugin's buttons elsewhere add (`contribute`).

import { fieldsOf } from "./pluginSettings.ts";
import type { PluginSchema, PluginValues, TonePatternKind } from "./protocol.ts";
import { tonesText } from "./inband.ts";

/** How other settings name an item of a list: its `id`, else its `name`. */
export function refKey(item: unknown): string {
  if (!item || typeof item !== "object") return "";
  const o = item as PluginValues;
  return typeof o.id === "string" && o.id ? o.id : typeof o.name === "string" ? o.name : "";
}

/** The items of top-level list `list` in `root`, as choices: their key, and what to call them. */
export function refOptions(root: PluginValues | undefined, list: string): { value: string; label: string }[] {
  const items = Array.isArray(root?.[list]) ? (root![list] as unknown[]) : [];
  return items
    .map((x) => {
      const value = refKey(x);
      const name = (x as PluginValues | null)?.name;
      return { value, label: typeof name === "string" && name && name !== value ? `${name} (${value})` : value };
    })
    .filter((o) => o.value);
}

/** Each `x-ref` to `list` under `schema` in `v` that names `from` names `to` instead (changes `v`). */
function renameIn(schema: PluginSchema | undefined, v: unknown, list: string, from: string, to: string): void {
  if (!v || typeof v !== "object" || Array.isArray(v)) return;
  const obj = v as PluginValues;
  for (const [k, f] of fieldsOf(schema)) {
    const x = obj[k];
    if (f["x-ref"] === list) {
      if (x === from) obj[k] = to;
      else if (Array.isArray(x)) obj[k] = x.map((e) => (e === from ? to : e));
    } else if (f.type === "object") renameIn(f, x, list, from, to);
    else if (f.type === "array" && f.items?.type === "object" && Array.isArray(x)) for (const item of x) renameIn(f.items, item, list, from, to);
  }
}

/** Whether any field under `schema` names items of `list`. */
function refersTo(schema: PluginSchema | undefined, list: string): boolean {
  return fieldsOf(schema).some(([, f]) => f["x-ref"] === list || refersTo(f, list) || (f.type === "array" && refersTo(f.items, list)));
}

/** Each referenced list's items' last keys that weren't empty, by place: a name cleared to be typed again is still what it was. */
export type KeyMemory = Record<string, string[]>;

/**
 * The settings `next`, edited from `prev`, with what names a renamed item of a referenced list following it: an item
 * keeping its place in the list whose key changed (`Station` → `Station pager`), by way of an empty one too (`memory`,
 * kept between edits).
 */
export function followRenames(schema: PluginSchema | undefined, prev: PluginValues | undefined, next: PluginValues, memory: KeyMemory = {}): PluginValues {
  let out = next;
  for (const [k, f] of fieldsOf(schema)) {
    if (f.type !== "array" || f.items?.type !== "object" || !refersTo(schema, k)) continue;
    const a = Array.isArray(prev?.[k]) ? (prev![k] as unknown[]) : [];
    const b = Array.isArray(next[k]) ? (next[k] as unknown[]) : [];
    // Added or removed: places moved, so start again from the keys as they are.
    if (a.length !== b.length || memory[k]?.length !== a.length) memory[k] = a.map(refKey);
    if (a.length !== b.length) {
      memory[k] = b.map(refKey);
      continue;
    }
    b.forEach((x, i) => {
      const [from, to] = [refKey(a[i]) || memory[k][i], refKey(x)];
      if (from && to && from !== to) {
        if (out === next) out = structuredClone(next);
        renameIn(schema, out, k, from, to);
      }
      memory[k][i] = to || from;
    });
  }
  return out;
}

/**
 * A contribution's `item` with its `{placeholders}` filled in from `vars`: a string that is only a placeholder becomes
 * its value as it is (in a list, `["{signal}"]`), others have the values written in.
 */
export function fillTemplate(item: unknown, vars: Record<string, string>): unknown {
  if (typeof item === "string") {
    const only = /^\{(\w+)\}$/.exec(item);
    if (only && only[1] in vars) return vars[only[1]];
    return item.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? vars[k] : m));
  }
  if (Array.isArray(item)) return item.map((x) => fillTemplate(x, vars));
  if (item && typeof item === "object") return Object.fromEntries(Object.entries(item).map(([k, x]) => [k, fillTemplate(x, vars)]));
  return item;
}

/** Whether field `f` applies, given its object's `values` (`x-when`: {field: value or [values]}; `schema`: the object's). */
export function applies(schema: PluginSchema | undefined, values: PluginValues, f: PluginSchema): boolean {
  const siblings = schema?.properties ?? {};
  return Object.entries(f["x-when"] ?? {}).every(([k, want]) => {
    const v = values[k] ?? siblings[k]?.default ?? siblings[k]?.enum?.[0];
    return Array.isArray(want) ? want.includes(v as string) : v === want;
  });
}

/** How a value reads in a summary: a choice by its label, a list joined (the first few), a switch on by its title. "": nothing set. */
export function summaryValue(f: PluginSchema, v: unknown): string {
  const label = (s: PluginSchema | undefined, x: unknown) => {
    const i = s?.enum?.indexOf(x as string | number) ?? -1;
    return i >= 0 ? (s?.["x-enum-labels"]?.[i] ?? String(x)) : String(x);
  };
  if (f.type === "boolean") return v === true ? (f.title ?? "") : "";
  if (f["x-tones"] && Array.isArray(v))
    return v
      .filter((t: { tonesHz?: number[]; digits?: string }) => t.tonesHz?.some((f) => f > 0) || t.digits)
      .map((t: { pattern?: TonePatternKind; tonesHz?: number[]; digits?: string }) => tonesText({ pattern: t.pattern ?? "two_tone", tones: t.tonesHz ?? [], digits: t.digits }))
      .join(", ");
  if (Array.isArray(v)) {
    const items = v.filter((x) => x !== "" && x !== null && x !== undefined).map((x) => label(f.items, x));
    return items.length > 3 ? `${items.slice(0, 3).join(", ")} +${items.length - 3}` : items.join(", ");
  }
  if (v === undefined || v === null || v === "") return f.enum && f.default !== undefined ? label(f, f.default) : "";
  return label(f, v);
}

/**
 * An item's line (`x-summary`: segments such as "{event}", "{systems|any system}", "→ {to}"): each `{field}` read as
 * [`summaryValue`] has it, else its fallback after `|`; a segment with a field empty and no fallback, or that doesn't
 * apply (`x-when`), is left out. The rest joined with " · ".
 */
export function summarize(schema: PluginSchema | undefined, item: PluginValues): string {
  const props = schema?.properties ?? {};
  return (schema?.["x-summary"] ?? [])
    .map((seg) => {
      let keep = true;
      const text = seg.replace(/\{(\w+)(?:\|([^}]*))?\}/g, (_, k: string, fallback?: string) => {
        const f = props[k];
        if (!f || !applies(schema, item, f)) {
          keep = false;
          return "";
        }
        const v = summaryValue(f, item[k]);
        if (!v && fallback === undefined) keep = false;
        return v || fallback || "";
      });
      return keep ? text : "";
    })
    .filter(Boolean)
    .join(" · ");
}
