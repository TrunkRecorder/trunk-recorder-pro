// A signal edited, or made from nothing: tones from a paging list
// (RadioReference's, a department's), a DTMF string, a Morse ID — a system's
// `signals` entry, as the Tones log's Add makes from what was heard.

import { useState } from "react";
import { type SystemRef } from "./controller.ts";
import { talkgroupList } from "./fieldText.ts";
import { DraftInput, Field } from "./inputs.tsx";
import type { SignalDef } from "./protocol.ts";
import { editSignals } from "./dash/Tones.tsx";
import { defSpec, signalId } from "./inband.ts";
import { PlayButton } from "./PlayButton.tsx";
import { toleranceNote, ToneFields, toneProblem } from "./ToneSpecs.tsx";

/** Signal `i` of the system at `at` (`ids`: its signals' ids, for telling one taken). */
export function SignalEditor({ at, i, d, ids, onDone }: { at: SystemRef; i: number; d: SignalDef; ids: string[]; onDone: () => void }) {
  const set = (fn: (x: SignalDef) => void) =>
    editSignals(at, (all) => {
      const x = { ...all[i] };
      fn(x);
      for (const k of ["tonesHz", "lengthsS", "talkgroups"] as const) if (!x[k]?.length) delete x[k];
      if (!x.name) delete x.name;
      if (!x.digits) delete x.digits;
      if (!x.extraTolerancePct) delete x.extraTolerancePct;
      all[i] = x;
      return all;
    });
  const [id, setId] = useState(d.id);
  const clean = signalId(id);
  const taken = clean !== d.id && ids.includes(clean);
  return (
    <div className="edit-row stack">
      <div className="grid2">
        <Field label="Id" hint={taken ? `This system has ${clean} already` : !clean ? "Give it an id first (TOWN_FIRE): the call JSON and plugins name it by it" : "Letters, digits and _: what the call JSON and plugins name it by"}>
          <input
            className="mono"
            autoFocus={!d.id}
            placeholder="TOWN_FIRE"
            value={id}
            onChange={(e) => setId(e.target.value.toUpperCase())}
            onBlur={() => (clean && !taken ? set((x) => void (x.id = clean)) : setId(d.id))}
          />
        </Field>
        <Field label="Name">
          <input value={d.name ?? ""} placeholder={d.id} onChange={(e) => set((x) => void (x.name = e.target.value))} />
        </Field>
        <ToneFields d={d} onChange={(t) => set((x) => Object.assign(x, { tonesHz: undefined, lengthsS: undefined, digits: undefined, extraTolerancePct: undefined }, t))} id={`sig-${i}`} />
        <Field label="Talkgroups" hint="Numbers, ranges (1000-1099), patterns (507*); empty: any">
          <DraftInput className="mono" aria-label="Talkgroups" placeholder="any" codec={talkgroupList} value={d.talkgroups ?? []} onChange={(v) => set((x) => void (x.talkgroups = v))} />
        </Field>
      </div>
      {toneProblem(d) && <p className="field-hint warn-text">{toneProblem(d)}</p>}
      <p className="field-hint">{toleranceNote(d)}</p>
      <div className="row">
        <PlayButton spec={defSpec(d)} />
        <span className="spacer" />
        <button className="btn small" onClick={onDone}>
          Done
        </button>
      </div>
    </div>
  );
}
