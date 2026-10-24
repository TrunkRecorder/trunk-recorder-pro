// Tones typed in rather than heard: a page's two tones from a paging list, an
// alert tone, a DTMF or KNOX string, a Morse ID. Each kind asks only for what
// it is, and the note under it says what the recorder will count as it (its
// tolerances).

import { numberList } from "./fieldText.ts";
import { DraftInput, Field, NumberInput } from "./inputs.tsx";
import { PlayButton } from "./PlayButton.tsx";
import { defSpec, qc2Within, toneTolerance } from "./inband.ts";
import type { SignalDef } from "./protocol.ts";

/** A tone pattern as typed in: a signal's, less its name and where it's sent. */
export type ToneDef = Pick<SignalDef, "pattern" | "tonesHz" | "digits" | "lengthsS" | "extraTolerancePct">;

/** The kinds a tone can be typed in as: a signal's pattern, with digit strings told apart by their keypad. */
export type ToneKind = "two_tone" | "long" | "hi_low" | "pulsed" | "dtmf" | "knox" | "morse" | "sequence";

export const TONE_KINDS: { kind: ToneKind; label: string }[] = [
  { kind: "two_tone", label: "Two-tone page" },
  { kind: "long", label: "Long tone" },
  { kind: "hi_low", label: "Hi-low warble" },
  { kind: "pulsed", label: "Pulsed tone" },
  { kind: "dtmf", label: "DTMF digits" },
  { kind: "knox", label: "KNOX digits" },
  { kind: "morse", label: "Morse ID" },
  { kind: "sequence", label: "Tone sequence" },
];

/** What kind `d` is (KNOX digits are written with `KNOX ` first). */
export function kindOf(d: ToneDef): ToneKind {
  if (d.pattern === "digits") return (d.digits ?? "").startsWith("KNOX ") ? "knox" : "dtmf";
  return d.pattern === "beep" ? "long" : d.pattern;
}

/** `d` made kind `k`: what the kinds share is kept, the rest left for the new kind's fields. */
function asKind(d: ToneDef, k: ToneKind): ToneDef {
  const keys = d.pattern === "digits" ? (d.digits ?? "").replace(/^KNOX /, "") : "";
  if (k === "dtmf" || k === "knox") return { pattern: "digits", digits: `${k === "knox" ? "KNOX " : ""}${keys}` };
  if (k === "morse") return { pattern: "morse", ...(d.pattern === "morse" ? { digits: d.digits } : {}) };
  const tones = d.pattern === "digits" || d.pattern === "morse" ? undefined : d.tonesHz;
  return { pattern: k, ...(tones?.length ? { tonesHz: tones } : {}), ...(d.extraTolerancePct ? { extraTolerancePct: d.extraTolerancePct } : {}) };
}

/** Whether a kind is matched by its frequencies (not its digits or text). */
const byTones = (k: ToneKind) => k !== "dtmf" && k !== "knox" && k !== "morse";

const hz1 = (v: number) => `${Math.round(v * 10) / 10}`;

/** What the recorder counts as `d`: how near each tone has to be, as it was heard. */
export function toleranceNote(d: ToneDef): string {
  const k = kindOf(d);
  if (k === "dtmf") return "Matched by its digits, exactly, sent as DTMF.";
  if (k === "knox") return "Matched by its digits, exactly, sent as KNOX tones.";
  if (k === "morse") return "Matched by its text, in either case; its pitch isn't matched.";
  const tones = (d.tonesHz ?? []).filter((f) => f > 0);
  const extra = d.extraTolerancePct ?? 0;
  const plus = extra ? ` + ${extra} %` : "";
  const tol = (f: number, frame: boolean) => toneTolerance(f, frame) + (f * extra) / 100;
  const each = (frame: boolean) => tones.map((f) => `${hz1(f)} ± ${hz1(tol(f, frame))}`).join(", ");
  // Other paging tones a window takes in: they'd be matched too.
  const near = (frame: boolean) => [...new Set(tones.flatMap((f) => qc2Within(f, tol(f, frame))))];
  const [inAudio, inFrames] = [near(false), near(true).filter((t) => !near(false).includes(t))];
  return (
    `A tone heard counts as one of these within ±1.5 %${plus} of it when measured in the audio (P25 Phase 1, analog)` +
    (tones.length ? `: ${each(false)} Hz` : "") +
    `; and within 15.7 Hz + 0.5 %${plus} when a P25 Phase 2 tone frame names it, as those name tones only to the nearest 31.25 Hz` +
    (tones.length ? `: ${each(true)} Hz` : "") +
    "." +
    (inAudio.length ? ` That also takes in ${inAudio.join(", ")}: it would match them too.` : "") +
    (inFrames.length ? ` From Phase 2 tone frames it can't be told from ${inFrames.join(", ")}.` : "") +
    " Lengths aren't matched (pagers vary them): they're what ▶ plays."
  );
}

/** What's missing or wrong in `d`, for its kind. */
export function toneProblem(d: ToneDef): string | null {
  const k = kindOf(d);
  if (k === "dtmf" || k === "knox") return (d.digits ?? "").replace(/^KNOX /, "") ? null : "Type its digits";
  if (k === "morse") return d.digits ? null : "Type its text";
  const tones = d.tonesHz ?? [];
  if (tones.some((f) => !(f >= 0 && f < 4000))) return "Tones are between 0 and 4000 Hz";
  const n = k === "two_tone" || k === "hi_low" ? 2 : k === "sequence" ? 0 : 1;
  if (n && tones.filter((f) => f > 0).length !== n) return n === 2 ? "Both tones, in Hz" : "Its tone, in Hz";
  if (!n && tones.length < 2) return "Its tones, in order";
  return null;
}

const hzList = numberList(/[\s,;/→]+/);

/** A tone's fields, as its kind has them. */
export function ToneFields(props: { d: ToneDef; onChange: (d: ToneDef) => void; id: string }) {
  const { d } = props;
  const k = kindOf(d);
  const set = (fn: (x: ToneDef) => void) => {
    const x: ToneDef = { ...d, tonesHz: [...(d.tonesHz ?? [])], lengthsS: [...(d.lengthsS ?? [])] };
    fn(x);
    // (Trailing gaps go; one left within is 0.)
    for (const key of ["tonesHz", "lengthsS"] as const) {
      const v = Array.from(x[key] ?? [], (n) => n ?? 0);
      while (v.length && !v[v.length - 1]) v.pop();
      if (v.length) x[key] = v;
      else delete x[key];
    }
    if (!x.digits) delete x.digits;
    if (!x.extraTolerancePct) delete x.extraTolerancePct;
    props.onChange(x);
  };
  const tone = (i: number, label: string) => (
    <Field label={label}>
      <NumberInput label={label} value={d.tonesHz?.[i] || undefined} onChange={(v) => set((x) => void (x.tonesHz![i] = v ?? 0))} />
    </Field>
  );
  const length = (i: number, label: string, usual: number) => (
    <Field label={label} hint={`For ▶ (${usual} s if empty)`}>
      <NumberInput label={label} value={d.lengthsS?.[i] || undefined} onChange={(v) => set((x) => void (x.lengthsS![i] = v ?? 0))} />
    </Field>
  );
  const keys = (d.digits ?? "").replace(/^KNOX /, "");
  return (
    <>
      <Field label="Kind">
        <select id={`${props.id}-kind`} aria-label="Kind" value={k} onChange={(e) => props.onChange(asKind(d, e.target.value as ToneKind))}>
          {TONE_KINDS.map((o) => (
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
      {k === "long" && (
        <>
          {tone(0, "Tone, Hz")}
          {length(0, "Lasts, s", 3)}
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
          <input
            className="mono"
            aria-label="Digits"
            value={keys}
            onChange={(e) => set((x) => void (x.digits = `${k === "knox" ? "KNOX " : ""}${e.target.value.toUpperCase().replace(/[^0-9A-D*#]/g, "")}`))}
          />
        </Field>
      )}
      {k === "morse" && (
        <Field label="Text" hint="The ID it sends (a call sign)">
          <input className="mono" aria-label="Text" value={d.digits ?? ""} onChange={(e) => set((x) => void (x.digits = e.target.value.toUpperCase()))} />
        </Field>
      )}
      {byTones(k) && (
        <Field label="Extra tolerance, %" hint="Added to the usual (below); empty: none">
          <NumberInput label="Extra tolerance, %" value={d.extraTolerancePct} onChange={(v) => set((x) => void (x.extraTolerancePct = v))} />
        </Field>
      )}
    </>
  );
}
