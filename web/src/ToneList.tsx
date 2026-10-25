// A plugin's list of tones typed in (`x-tones` in its settings), such as a
// notification rule's: each tone's fields as its kind has them (a system's
// signal's, less its id, name and talkgroups), ▶, and what the recorder would
// count as it.

import { PlayButton } from "./PlayButton.tsx";
import { defSpec, SignalFields } from "./Signals.tsx";
import type { SignalDef } from "./protocol.ts";

/** A tone typed in: a signal's tones, without an id, name or talkgroups. */
export type ToneDef = Omit<SignalDef, "id" | "name" | "talkgroups">;

/** A list of tones (`x-tones`): each its fields, ▶ and what will count as it. */
export function ToneList(props: { value: unknown; onChange: (v: ToneDef[] | undefined) => void; id: string }) {
  const tones = (Array.isArray(props.value) ? props.value : []) as ToneDef[];
  const set = (next: ToneDef[]) => props.onChange(next.length ? next : undefined);
  return (
    <div className="stack tone-list">
      {tones.map((raw, i) => {
        const d: SignalDef = { id: "", ...raw, pattern: raw.pattern ?? "two_tone" };
        return (
          <div key={i} className="tone-spec">
            <SignalFields
              bare
              d={d}
              taken={() => false}
              onChange={({ id: _id, name: _name, talkgroups: _tg, ...x }) => set(tones.map((t, j) => (j === i ? x : t)))}
            />
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
