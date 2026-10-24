// A plugin's list of tones typed in (`x-tones` in its settings), such as a
// notification rule's: each tone's fields as its kind has them, ▶, and what
// the recorder would count as it.

import { PlayButton } from "./PlayButton.tsx";
import { defSpec } from "./inband.ts";
import { toleranceNote, ToneFields, toneProblem, type ToneDef } from "./ToneSpecs.tsx";

/** A list of tones (`x-tones`): each its fields, ▶ and what will count as it. */
export function ToneList(props: { value: unknown; onChange: (v: ToneDef[] | undefined) => void; id: string }) {
  const tones = (Array.isArray(props.value) ? props.value : []) as ToneDef[];
  const set = (next: ToneDef[]) => props.onChange(next.length ? next : undefined);
  return (
    <div className="stack tone-list">
      {tones.map((raw, i) => {
        const d = { ...raw, pattern: raw.pattern ?? "two_tone" };
        const problem = toneProblem(d);
        return (
          <div key={i} className="tone-spec">
            <div className="tone-spec-fields">
              <ToneFields d={d} onChange={(x) => set(tones.map((t, j) => (j === i ? x : t)))} id={`${props.id}-${i}`} />
            </div>
            {problem && <p className="field-hint warn-text">{problem}</p>}
            <p className="field-hint">{toleranceNote(d)}</p>
            <div className="row">
              <PlayButton spec={defSpec(d)} />
              <span className="spacer" />
              <button type="button" className="btn ghost small" onClick={() => set(tones.filter((_, j) => j !== i))}>
                Remove tone
              </button>
            </div>
          </div>
        );
      })}
      <div>
        <button type="button" className="btn small" onClick={() => set([...tones, { pattern: "two_tone" }])}>
          Add a tone
        </button>
      </div>
    </div>
  );
}
