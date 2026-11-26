// A row's names, each labelled and kept as its box is left: a conventional
// row's (Setup), a talkgroup's and a radio's (their Radio pages). One editor,
// so each is named the same way wherever it is.

import { TextInput } from "./inputs.tsx";

/** One of a row's names: what it's called and an example. */
export interface NameFieldDef<K extends string> {
  key: K;
  label: string;
  placeholder: string;
}

/** A talkgroup's names, as a talkgroup file's columns (a conventional row has the last three beside its name). */
export const TALKGROUP_NAME_FIELDS: NameFieldDef<"alphaTag" | "description" | "tag" | "category">[] = [
  { key: "alphaTag", label: "Alpha tag", placeholder: "FD Disp" },
  { key: "description", label: "Description", placeholder: "Fire / EMS dispatch" },
  { key: "tag", label: "Tag", placeholder: "Fire Dispatch" },
  { key: "category", label: "Category", placeholder: "Fire" },
];

/** `values` once each (blank ones left out), most used first: offered as a name is typed. */
export function ranked(values: string[]): string[] {
  const n = new Map<string, number>();
  for (const v of values.map((v) => v.trim()).filter(Boolean)) n.set(v, (n.get(v) ?? 0) + 1);
  return [...n.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([v]) => v);
}

/** `fields`, each a labelled box (`lists`: the datalist each offers values from). */
export function NameFields<K extends string>(props: { fields: NameFieldDef<K>[]; value: (k: K) => string; onChange: (k: K, v: string) => void; lists?: Partial<Record<K, string>>; disabled?: boolean }) {
  return (
    <>
      {props.fields.map((f) => (
        <label key={f.key} className="row-name">
          <span className="small muted">{f.label}</span>
          <TextInput value={props.value(f.key)} disabled={props.disabled} placeholder={f.placeholder} list={props.lists?.[f.key]} aria-label={f.label} onChange={(v) => props.onChange(f.key, v)} />
        </label>
      ))}
    </>
  );
}

/** A datalist of `values`. */
export function Suggestions(props: { id: string; values: string[] }) {
  return (
    <datalist id={props.id}>
      {props.values.map((v) => (
        <option key={v} value={v} />
      ))}
    </datalist>
  );
}
