// A system's signals (its config's `signals`): the tones it sends that its
// calls and plugins name, typed in from a paging list (RadioReference's, a
// department's) or added from what the dashboard's Tones card heard. Like
// plugin delays, few systems have any, so they're a disclosure on the
// system's card. What's wrong with one, and what the recorder counts as it,
// are the recorder's (rules.ts): Start refuses a config with one it can't use.

import { useState } from "react";
import { editList, systemAt, useApp, type SystemRef } from "./controller.ts";
import { numberList, talkgroupList, type TextCodec } from "./fieldText.ts";
import { DraftInput, Field, NumberInput, TalkgroupHint } from "./inputs.tsx";
import { PlayButton } from "./PlayButton.tsx";
import { SignalButtons } from "./Plugins.tsx";
import type { Config, SignalDef } from "./protocol.ts";
import { signalId, signalNote, signalProblem, signalText } from "./rules.ts";
import type { ToneSpec } from "./tonePlayer.ts";

/** What ▶ plays for a signal. */
export const defSpec = (d: SignalDef): ToneSpec => ({ pattern: d.pattern, tones: d.tonesHz ?? [], lengths: d.lengthsS ?? [], digits: d.digits, knox: d.knox });

/** The kinds a signal can be typed in as: its pattern, with digit strings told apart by their keypad (`knox`). */
type Kind = SignalDef["pattern"] | "dtmf" | "knox";

const KINDS: { kind: Kind; label: string }[] = [
  { kind: "two_tone", label: "Two-tone page" },
  { kind: "long", label: "Long tone" },
  { kind: "beep", label: "Beep" },
  { kind: "hi_low", label: "Hi-low warble" },
  { kind: "pulsed", label: "Pulsed tone" },
  { kind: "dtmf", label: "DTMF digits" },
  { kind: "knox", label: "KNOX digits" },
  { kind: "morse", label: "Morse ID" },
  { kind: "sequence", label: "Tone sequence" },
];

const kindOf = (d: SignalDef): Kind => (d.pattern === "digits" ? (d.knox ? "knox" : "dtmf") : d.pattern);
const kindLabel = (d: SignalDef) => KINDS.find((k) => k.kind === kindOf(d))?.label ?? d.pattern;

/** `d` made kind `k`: what the kinds share is kept, the rest left for the new kind's fields. */
function asKind(d: SignalDef, k: Kind): SignalDef {
  const keep = { id: d.id, ...(d.name ? { name: d.name } : {}), ...(d.talkgroups?.length ? { talkgroups: d.talkgroups } : {}) };
  if (k === "dtmf" || k === "knox") return { ...keep, pattern: "digits", ...(d.pattern === "digits" && d.digits ? { digits: d.digits } : {}), ...(k === "knox" ? { knox: true } : {}) };
  if (k === "morse") return { ...keep, pattern: "morse", ...(d.pattern === "morse" && d.digits ? { digits: d.digits } : {}) };
  const tones = d.pattern === "digits" || d.pattern === "morse" ? undefined : d.tonesHz;
  return { ...keep, pattern: k, ...(tones?.length ? { tonesHz: tones } : {}), ...(d.extraTolerancePct ? { extraTolerancePct: d.extraTolerancePct } : {}) };
}

/** How plugins' settings name system `system`'s signal `id`: "system:ID". */
export const signalRef = (system: string, id: string) => `${system}:${id}`;

/** Every system's named signals, as choices (a plugin's `x-signals`). */
export function signalChoices(c: Config): { value: string; label: string }[] {
  return [...c.systems, ...c.conventional].flatMap((x) =>
    (x.signals ?? []).filter((d) => d.id).map((d) => ({ value: signalRef(x.shortName, d.id), label: d.name ? `${d.name} (${signalRef(x.shortName, d.id)})` : signalRef(x.shortName, d.id) })),
  );
}

/** An id as typed ("Town fire"), kept as one (TOWN_FIRE). */
const idCodec: TextCodec<string> = { format: (v) => v, parse: signalId };

const hzList = numberList(/[\s,;/→]+/);

/** `x` without its empty parts. */
function tidy(x: SignalDef): SignalDef {
  const d = { ...x };
  // (Trailing gaps go; one left within is 0.)
  for (const key of ["tonesHz", "lengthsS"] as const) {
    const v = Array.from(d[key] ?? [], (n) => n ?? 0);
    while (v.length && !v[v.length - 1]) v.pop();
    if (v.length) d[key] = v;
    else delete d[key];
  }
  if (!d.talkgroups?.length) delete d.talkgroups;
  if (!d.name) delete d.name;
  if (!d.digits) delete d.digits;
  if (!d.knox) delete d.knox;
  if (!d.extraTolerancePct) delete d.extraTolerancePct;
  return d;
}

/**
 * One signal's fields, as its kind has them; what's wrong with it and what the recorder counts as it under them. `taken`:
 * an id another signal has. `bare`: its tones alone (a tone typed into a plugin's settings), without an id, name or
 * talkgroups.
 */
export function SignalFields(props: { d: SignalDef; taken: (id: string) => boolean; onChange: (d: SignalDef) => void; bare?: boolean }) {
  const { d } = props;
  const [clash, setClash] = useState<string | null>(null);
  const k = kindOf(d);
  const set = (fn: (x: SignalDef) => void) => {
    const x: SignalDef = { ...d, tonesHz: [...(d.tonesHz ?? [])], lengthsS: [...(d.lengthsS ?? [])] };
    fn(x);
    props.onChange(tidy(x));
  };
  const tone = (i: number, label: string) => (
    <Field label={label}>
      <NumberInput label={label} narrow value={d.tonesHz?.[i] || undefined} onChange={(v) => set((x) => void (x.tonesHz![i] = v ?? 0))} />
    </Field>
  );
  const length = (i: number, label: string, usual: number) => (
    <Field label={label} hint={`For ▶ (${usual} s if empty)`}>
      <NumberInput label={label} narrow value={d.lengthsS?.[i] || undefined} onChange={(v) => set((x) => void (x.lengthsS![i] = v ?? 0))} />
    </Field>
  );
  // (Bare, it has no id: only its tones can be wrong.)
  const problem = clash ? `This system has a signal ${clash} already.` : signalProblem(props.bare ? { ...d, id: d.id || "TONE" } : d);
  return (
    <>
      <div className="row-editor">
        {!props.bare && (
        <Field label="Id" hint="What the call JSON and plugins name it by">
          <DraftInput
            className="mono"
            aria-label="Id"
            placeholder="TOWN_FIRE"
            codec={idCodec}
            value={d.id}
            onChange={(id) => {
              setClash(id && id !== d.id && props.taken(id) ? id : null);
              if (!props.taken(id)) set((x) => void (x.id = id));
            }}
            onBlur={() => setClash(null)}
          />
        </Field>
        )}
        {!props.bare && (
          <Field label="Name">
            <input value={d.name ?? ""} placeholder={d.id} onChange={(e) => set((x) => void (x.name = e.target.value))} />
          </Field>
        )}
        <Field label="Kind">
          <select aria-label="Kind" value={k} onChange={(e) => props.onChange(asKind(d, e.target.value as Kind))}>
            {KINDS.map((o) => (
              <option key={o.kind} value={o.kind}>
                {o.label}
              </option>
            ))}
          </select>
        </Field>
        {k === "two_tone" && (
          <>
            {tone(0, "Tone A, Hz")}
            {tone(1, "Tone B, Hz")}
            {length(0, "A lasts, s", 1)}
            {length(1, "B lasts, s", 3)}
          </>
        )}
        {(k === "long" || k === "beep") && (
          <>
            {tone(0, "Tone, Hz")}
            {length(0, "Lasts, s", k === "long" ? 3 : 0.2)}
          </>
        )}
        {k === "hi_low" && (
          <>
            {tone(0, "Low tone, Hz")}
            {tone(1, "High tone, Hz")}
            {length(0, "Each lasts, s", 0.25)}
          </>
        )}
        {k === "pulsed" && (
          <>
            {tone(0, "Tone, Hz")}
            {length(0, "On, s", 0.3)}
            {length(1, "Off, s", 0.2)}
          </>
        )}
        {k === "sequence" && (
          <Field label="Tones, Hz, in order" hint="As the list gives them (1124, 1197, 1275)">
            <DraftInput className="mono" aria-label="Tones" codec={hzList} value={d.tonesHz ?? []} onChange={(v) => set((x) => void (x.tonesHz = v))} />
          </Field>
        )}
        {(k === "dtmf" || k === "knox") && (
          <Field label="Digits" hint="0–9, A–D, * and #">
            <input className="mono" aria-label="Digits" value={d.digits ?? ""} onChange={(e) => set((x) => void (x.digits = e.target.value.toUpperCase().replace(/[^0-9A-D*#]/g, "")))} />
          </Field>
        )}
        {k === "morse" && (
          <Field label="Text" hint="The ID it sends (a call sign)">
            <input className="mono" aria-label="Text" value={d.digits ?? ""} onChange={(e) => set((x) => void (x.digits = e.target.value.toUpperCase()))} />
          </Field>
        )}
        {d.pattern !== "digits" && d.pattern !== "morse" && (
          <Field label="Extra tolerance, %" hint="Added to the usual (below); empty: none">
            <NumberInput label="Extra tolerance, %" narrow value={d.extraTolerancePct} onChange={(v) => set((x) => void (x.extraTolerancePct = v))} />
          </Field>
        )}
        {!props.bare && (
          <Field label="Talkgroups" hint={<TalkgroupHint list={d.talkgroups} text="Numbers, ranges (1000-1099), patterns (507*); empty: any" />}>
            <DraftInput className="mono" aria-label="Talkgroups" placeholder="any" codec={talkgroupList} value={d.talkgroups ?? []} onChange={(v) => set((x) => void (x.talkgroups = v))} />
          </Field>
        )}
      </div>
      {problem && <span className="field-hint warn-text">{problem}</span>}
      <span className="field-hint">{signalNote(d)}</span>
    </>
  );
}

const WHAT =
  "The tones this system sends that you want named: a page's two tones, an alert, a DTMF or KNOX string, a Morse ID. Calls carrying one list its id (the call JSON's signals), plugins hear it named, and the dashboard's Tones card marks it; Add there makes one from tones heard.";

/** A system's signals (its card's Plugins group): a row each, opened to edit; a new one is added once it's complete. */
export function SystemSignals(props: { system: SystemRef }) {
  const c = useApp().config;
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState<SignalDef | null>(null);
  const sys = c && systemAt(c, props.system);
  if (!c || !sys?.shortName) return null;
  const defs = sys.signals ?? [];
  const edit = (fn: (all: SignalDef[]) => SignalDef[]) => editList(props.system, "signals", fn);
  const taken = (i: number | null) => (id: string) => defs.some((x, j) => j !== i && x.id === id);
  const remove = (i: number) => {
    edit((all) => all.filter((_, j) => j !== i));
    // (The one open stays open: it moves up a row if this was above it.)
    setEditing((e) => (e === null || e === i ? null : e > i ? e - 1 : e));
  };
  const add = () => {
    if (!draft || signalProblem(draft) || taken(null)(draft.id)) return;
    edit((all) => [...all, draft]);
    setDraft(null);
  };
  const anchor = typeof props.system === "number" ? `signals-${sys.shortName}` : `convsignals-${sys.shortName}`;
  return (
    <details className="help override" id={`need-${anchor}`}>
      <summary>
        Signals
        <span className="muted">{defs.length ? ` — ${defs.length}` : " — none"}</span>
      </summary>
      <p className="field-hint">{WHAT}</p>
      {(defs.length > 0 || draft) && (
        <div className="table-wrap channels-wrap">
          <table className="calls channels">
            <thead>
              <tr>
                <th>Id</th>
                <th>Kind</th>
                <th>Sends</th>
                <th>Talkgroups</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {defs.flatMap((d, i) => {
                const bad = signalProblem(d);
                return [
                  <tr key={i}>
                    <td>
                      <span className="mono">{d.id || "—"}</span>
                      {d.name && <div className="muted small">{d.name}</div>}
                      {bad && <div className="warn-text small">{bad}</div>}
                    </td>
                    <td className="small">{kindLabel(d)}</td>
                    <td className="mono">{signalText(d, false)}</td>
                    <td className="mono small">{d.talkgroups?.join(", ") || "any"}</td>
                    <td className="nowrap">
                      <SignalButtons system={sys.shortName} d={d} />
                      <PlayButton spec={defSpec(d)} />
                      <button type="button" className="btn ghost small" onClick={() => setEditing(editing === i ? null : i)}>
                        {editing === i ? "Done" : "Edit"}
                      </button>
                      <button type="button" className="btn ghost small danger" aria-label={`Remove signal ${d.id || i + 1}`} onClick={() => remove(i)}>
                        Remove
                      </button>
                    </td>
                  </tr>,
                  ...(editing === i
                    ? [
                        <tr key={`${i}+`} className="rule-edit">
                          <td colSpan={5}>
                            <SignalFields d={d} taken={taken(i)} onChange={(x) => edit((all) => all.map((o, j) => (j === i ? x : o)))} />
                          </td>
                        </tr>,
                      ]
                    : []),
                ];
              })}
              {draft && (
                <tr className="rule-edit">
                  <td colSpan={5}>
                    <SignalFields d={draft} taken={taken(null)} onChange={setDraft} />
                    <div className="row">
                      <PlayButton spec={defSpec(draft)} />
                      <span className="spacer" />
                      <button type="button" className="btn small primary" disabled={!!signalProblem(draft) || taken(null)(draft.id)} onClick={add}>
                        Add
                      </button>
                      <button type="button" className="btn small ghost" onClick={() => setDraft(null)}>
                        Cancel
                      </button>
                    </div>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
      {!draft && (
        <div className="row">
          <button type="button" className="btn ghost small" onClick={() => setDraft({ id: "", pattern: "two_tone" })}>
            + Add a signal
          </button>
        </div>
      )}
    </details>
  );
}

/**
 * Add a signal to system `at` (the Tones card's Add: one heard, there): its
 * id made free among the system's, as a second, third… one.
 */
export function addSignal(at: SystemRef, d: SignalDef): void {
  editList(at, "signals", (all) => {
    let id = d.id;
    for (let n = 2; all.some((x) => x.id === id); n++) id = `${d.id}_${n}`;
    return [...all, { ...d, id }];
  });
}
