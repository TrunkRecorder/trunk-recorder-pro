// The tones a system's calls carried: pages, alert tones, warbles, digit
// strings, Morse IDs (the calls' `tone_patterns`), each heard pattern once
// per talkgroup, with the paging codes it may be and the system's signals it
// is: all as the recorder works them out (the tones log, stats/radio.rs).
// Add makes one a signal of the system (its config's `signals`, set on its
// card in Setup), by an id the call JSON and plugins use.

import { useState } from "react";
import { radioQuery, usePolled, type AppState, type SystemRef } from "../controller.ts";
import { Card, Choice } from "../charts.tsx";
import { ago } from "../fmt.ts";
import type { Conventional, SignalDef, System, ToneRow } from "../protocol.ts";
import { PlayButton } from "../PlayButton.tsx";
import { signalId } from "../rules.ts";
import { showTodo } from "../Setup.tsx";
import { addSignal } from "../Signals.tsx";
import { running, type DashSystem } from "./data.ts";

const rowKey = (r: ToneRow) => `${r.tg}|${r.pattern}|${r.knox}|${r.digits}|${r.tones.join(",")}`;

/** A signal made from a row: what was heard, on its talkgroup, by its talkgroup's name (or its pattern's). */
export const rowSignal = (r: ToneRow): SignalDef => ({
  id: signalId(r.alphaTag || r.pattern),
  pattern: r.pattern,
  ...(r.tones.length ? { tonesHz: r.tones } : {}),
  ...(r.digits ? { digits: r.digits } : {}),
  ...(r.knox ? { knox: true } : {}),
  ...(r.lengths.length ? { lengthsS: r.lengths } : {}),
  talkgroups: [String(r.tg)],
});

type Show = "heard" | "all" | "named";

/** The configured system `name` is (trunked or conventional), and where it is. */
function systemOf(s: AppState, name: string): { sys: System | Conventional; at: SystemRef } | null {
  const c = s.config;
  if (!c) return null;
  const t = c.systems.findIndex((x) => x.shortName === name);
  if (t >= 0) return { sys: c.systems[t], at: t };
  const v = c.conventional.findIndex((x) => x.shortName === name);
  return v >= 0 ? { sys: c.conventional[v], at: { conv: v } } : null;
}

/** The tones log's table (rows as the recorder sent them). */
export function ToneTable({ rows, system, at }: { rows: ToneRow[]; system: string; at: SystemRef | null }) {
  return (
    <div className="table-wrap tall">
      <table className="calls">
        <thead>
          <tr>
            <th>Talkgroup</th>
            <th title="As the recorder heard it: tones, lengths, and a page's format by its timing">Heard</th>
            <th title="Paging codes these tones may be, likeliest first: Quick Call II's, with the pager code when Motorola's encoding plan pairs them">Likely</th>
            <th>Times</th>
            <th>Last</th>
            <th>Sent by</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {rows.map((x) => (
            <tr key={rowKey(x)}>
              <td>
                {x.alphaTag || x.tg}
                {x.alphaTag && <span className="tag mono">{x.tg}</span>}
              </td>
              <td className="mono" title={x.frame ? "From the vocoder's tone frames (Phase 2's name tones to the nearest 31.25 Hz)" : "Measured in the audio"}>
                {x.text}
              </td>
              <td className="small">
                {x.ids.length ? (
                  <>
                    <b>{x.ids[0]}</b>
                    {x.ids.length > 1 && <span className="muted"> · {x.ids.slice(1).join(" · ")}</span>}
                  </>
                ) : x.pattern === "two_tone" ? (
                  <span className="muted" title="No Quick Call II pair (or other plan's) fits these tones">
                    custom tones
                  </span>
                ) : (
                  "—"
                )}
              </td>
              <td className="mono">{x.count}</td>
              <td className="small">{ago(x.last)}</td>
              <td className="small">{x.radios.map((u) => u.name || u.unit).join(", ") || "—"}</td>
              <td className="nowrap">
                <PlayButton spec={x} />
                {x.signals.length ? (
                  x.signals.map((id) => (
                    <span key={id} className="chip ok mono" title={`${system}:${id}`}>
                      {id}
                    </span>
                  ))
                ) : at !== null ? (
                  <button className="btn ghost small" title="Make it a signal of this system, on this talkgroup (edit it on the system's card in Setup)" onClick={() => addSignal(at, rowSignal(x))}>
                    Add
                  </button>
                ) : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function Tones({ s, sys, hours }: { s: AppState; sys: DashSystem; hours: number }) {
  const [show, setShow] = useState<Show>("heard");
  const own = systemOf(s, sys.name);
  const defs = own?.sys.signals ?? [];
  // Asked again when a pattern is heard, and when the signals change (the recorder names rows by them).
  const heard = s.events.findLast((e) => e.kind === "tonePattern" && e.system === sys.name)?.t ?? 0;
  const key = `tones:${sys.name}:${hours}:${heard}:${JSON.stringify(defs)}`;
  const r = usePolled(key, () => radioQuery({ what: "tones", system: sys.name, hours, limit: 500 }), running(s) ? 30_000 : 0);
  const rows = (r?.rows ?? []) as ToneRow[];
  const shown = rows.filter((x) => (show === "all" ? true : show === "named" ? x.signals.length > 0 : x.pattern !== "beep"));
  const beeps = rows.filter((x) => x.pattern === "beep").length;
  return (
    <Card title="Tones" id="tones">
      {own && (
        <div className="row">
          <span className="muted small">
            {defs.length ? `Signals of ${sys.name}: ${defs.map((d) => d.id).join(", ")}.` : "No signals yet: Add one heard below, or type one in from a paging list on the system's card in Setup."}
          </span>
          <span className="spacer" />
          <button className="btn ghost small" onClick={() => showTodo(typeof own.at === "number" ? `signals-${sys.name}` : `convsignals-${sys.name}`)}>
            Signals in Setup
          </button>
        </div>
      )}
      <div className="row">
        <Choice
          value={show}
          onChange={setShow}
          label="Show"
          options={[
            { v: "heard", label: `Heard ${rows.length - beeps}` },
            { v: "all", label: `With beeps ${rows.length}` },
            { v: "named", label: `Signals of ${sys.name} ${rows.filter((x) => x.signals.length).length}` },
          ]}
        />
      </div>
      {shown.length === 0 ? (
        <p className="empty">
          No tones heard in this span{show === "heard" && beeps ? ` (${beeps} beeps: speech taken for a tone, or tones in the background)` : ""}. Pages, alert tones, DTMF and
          Morse IDs are found in the voice of every kind of call, and listed here as they're heard.
        </p>
      ) : (
        <ToneTable rows={shown.slice(0, 300)} system={sys.name} at={own?.at ?? null} />
      )}
      {shown.length > 300 && <p className="muted small">300 of {shown.length} shown.</p>}
    </Card>
  );
}
