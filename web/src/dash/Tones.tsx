// The tones a system's calls carried: pages, alert tones, warbles, digit
// strings, Morse IDs (the calls' `tone_patterns`), each heard pattern once
// per talkgroup, with the paging codes it may be. Add makes one a signal of
// the system (its config's `signals`), by an id the call JSON and plugins use.

import { useState } from "react";
import { radioQuery, systemAt, updateConfig, usePolled, type AppState, type SystemRef } from "../controller.ts";
import { Card, Choice } from "../charts.tsx";
import { ago, num } from "../fmt.ts";
import type { Conventional, SignalDef, System, TonePatternKind, ToneRow } from "../protocol.ts";
import { type ToneSpec } from "../tonePlayer.ts";
import { defSpec, PATTERN, plain, signalId, toneMatches, tonesText, toneTolerance } from "../inband.ts";
import { PlayButton } from "../PlayButton.tsx";
import { running, type DashSystem } from "./data.ts";
import { SignalEditor } from "../SignalEditor.tsx";

export { defSpec, PATTERN, signalId, talkgroupMatches, toneMatches, tonesText, toneTolerance } from "../inband.ts";

function timing(r: ToneRow): string {
  const s = (v: number) => `${plain(v)} s`;
  if (r.pattern === "pulsed") return `${s(r.lengths[0])} on, ${s(r.lengths[1])} off`;
  if (r.pattern === "morse") return `${Math.round(1.2 / (r.lengths[0] || 0.06))} WPM`;
  if (r.pattern === "sequence" || r.pattern === "digits") return `${r.lengths.length} × ~${s(r.lengths[Math.floor(r.lengths.length / 2)] ?? 0)}`;
  return r.lengths.map(s).join(" + ");
}

/** A name for a new signal, from where it was heard. */
function suggestedName(r: ToneRow): string {
  const where = r.alphaTag || `TG ${r.tg}`;
  return r.pattern === "morse" ? `${where} ID` : `${where} ${PATTERN[r.pattern].toLowerCase()}`;
}

/** `id`, or (taken among `ids`) `id_2`, `id_3`… */
function freeId(id: string, ids: string[]): string {
  let n = 1;
  let out = id;
  while (ids.includes(out)) out = `${id}_${++n}`;
  return out;
}

const rowSpec = (r: ToneRow): ToneSpec => ({ pattern: r.pattern, tones: r.tones, lengths: r.lengths, digits: r.digits });

const rowKey = (r: ToneRow) => `${r.tg}|${r.pattern}|${r.digits}|${r.tones.join(",")}`;

type Show = "heard" | "all" | "named";

/** The configured system `name` is (trunked or conventional), and where it is. */
export function systemOf(s: AppState, name: string): { sys: System | Conventional; at: SystemRef } | null {
  const c = s.config;
  if (!c) return null;
  const t = c.systems.findIndex((x) => x.shortName === name);
  if (t >= 0) return { sys: c.systems[t], at: t };
  const v = c.conventional.findIndex((x) => x.shortName === name);
  return v >= 0 ? { sys: c.conventional[v], at: { conv: v } } : null;
}

export function editSignals(at: SystemRef, fn: (signals: SignalDef[]) => SignalDef[]): void {
  updateConfig((c) => {
    const sys = systemAt(c, at);
    if (!sys) return;
    const next = fn([...(sys.signals ?? [])]);
    if (next.length) sys.signals = next;
    else delete sys.signals;
  });
}

function AddRow({ r, at, ids, onDone }: { r: ToneRow; at: SystemRef; ids: string[]; onDone: () => void }) {
  const [name, setName] = useState(() => suggestedName(r));
  const [id, setId] = useState(() => freeId(signalId(suggestedName(r)), ids));
  const [onlyTg, setOnlyTg] = useState(true);
  const clean = signalId(id);
  const taken = ids.includes(clean);
  const save = () => {
    if (!clean || taken) return;
    const d: SignalDef = {
      id: clean,
      ...(name.trim() && name.trim() !== clean ? { name: name.trim() } : {}),
      pattern: r.pattern,
      ...(r.pattern === "digits" ? { digits: r.digits } : r.pattern === "morse" ? { digits: r.digits, tonesHz: r.tones } : { tonesHz: r.tones }),
      lengthsS: r.lengths,
      ...(onlyTg ? { talkgroups: [String(r.tg)] } : {}),
    };
    editSignals(at, (t) => [...t, d]);
    onDone();
  };
  return (
    <tr className="edit-row">
      <td colSpan={9}>
        <div className="row">
          <label className="field">
            <span className="field-label">Id</span>
            <input className="mono" autoFocus value={id} onChange={(e) => setId(e.target.value.toUpperCase())} onKeyDown={(e) => e.key === "Enter" && save()} />
          </label>
          <label className="field">
            <span className="field-label">Name</span>
            <input value={name} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && save()} />
          </label>
          <label className="check">
            <input type="checkbox" checked={onlyTg} onChange={(e) => setOnlyTg(e.target.checked)} /> Only on {r.alphaTag || `talkgroup ${r.tg}`}
          </label>
          <span className="spacer" />
          <button className="btn small primary" onClick={save} disabled={!clean || taken}>
            Save
          </button>
          <button className="btn small ghost" onClick={onDone}>
            Cancel
          </button>
        </div>
        {taken && <p className="why why-warn">This system has a signal {clean} already.</p>}
        <p className="field-hint">
          {tonesText(r)}
          {r.frame && r.pattern !== "digits" ? `, matched within ${num(toneTolerance(r.tones[0] ?? 0, true), 0)} Hz (Phase 2 sends tones on a 31.25 Hz grid)` : ""}.
          {r.ids.length > 0 && ` Likely ${r.ids.join(", ")}.`}
        </p>
      </td>
    </tr>
  );
}

function Configured({ defs, rows, at, system }: { defs: SignalDef[]; rows: ToneRow[]; at: SystemRef; system: string }) {
  const [editing, setEditing] = useState<number | null>(null);
  const add = () => {
    editSignals(at, (t) => [...t, { id: "", pattern: "two_tone" }]);
    setEditing(defs.length);
  };
  if (!defs.length)
    return (
      <div className="row">
        <span className="muted small">No signals yet: Add one heard below, or type one in from a paging list.</span>
        <span className="spacer" />
        <button className="btn ghost small" onClick={add}>
          New signal
        </button>
      </div>
    );
  return (
    <div className="table-wrap">
      <table className="calls">
        <thead>
          <tr>
            <th>Signal</th>
            <th>Pattern</th>
            <th>Tones</th>
            <th>Talkgroups</th>
            <th>Heard</th>
            <th className="nowrap">
              <button className="btn ghost small" onClick={add}>
                New signal
              </button>
            </th>
          </tr>
        </thead>
        <tbody>
          {defs.flatMap((d, i) => {
            const heard = rows.filter((r) => toneMatches(d, r));
            const n = heard.reduce((a, r) => a + r.count, 0);
            const last = Math.max(0, ...heard.map((r) => r.last));
            return [
              <tr key={i}>
                <td>
                  <span className="mono" title={`${system}:${d.id}`}>
                    {d.id}
                  </span>
                  {d.name && <div className="muted small">{d.name}</div>}
                </td>
                <td className="small">{PATTERN[d.pattern]}</td>
                <td className="mono">{tonesText({ pattern: d.pattern, tones: d.tonesHz ?? [], digits: d.digits })}</td>
                <td className="mono small">{d.talkgroups?.join(", ") || "any"}</td>
                <td className="small">{n ? `${n}× · ${ago(last)}` : "not in this span"}</td>
                <td className="nowrap">
                  <PlayButton spec={defSpec(d)} />
                  <button className="btn ghost small" onClick={() => setEditing(editing === i ? null : i)}>
                    {editing === i ? "Close" : "Edit"}
                  </button>
                  <button className="btn ghost small" title="Remove" onClick={() => editSignals(at, (t) => t.filter((_, k) => k !== i))}>
                    ✕
                  </button>
                </td>
              </tr>,
              ...(editing === i
                ? [
                    <tr key={`${i}+`}>
                      <td colSpan={6}>
                        <SignalEditor at={at} i={i} d={d} ids={defs.map((x) => x.id)} onDone={() => setEditing(null)} />
                      </td>
                    </tr>,
                  ]
                : []),
            ];
          })}
        </tbody>
      </table>
    </div>
  );
}

export function Tones({ s, sys, hours }: { s: AppState; sys: DashSystem; hours: number }) {
  const [show, setShow] = useState<Show>("heard");
  const [adding, setAdding] = useState<string | null>(null);
  const r = usePolled(`tones:${sys.name}:${hours}`, () => radioQuery({ what: "tones", system: sys.name, hours, limit: 500 }), running(s) ? 30_000 : 0);
  const rows = (r?.rows ?? []) as ToneRow[];
  const own = systemOf(s, sys.name);
  const defs = own?.sys.signals ?? [];
  const named = (row: ToneRow) => defs.find((d) => toneMatches(d, row));
  const shown = rows.filter((x) => (show === "all" ? true : show === "named" ? named(x) : x.pattern !== "beep"));
  const beeps = rows.filter((x) => x.pattern === "beep").length;
  return (
    <Card title="Tones" id="tones">
      {own && <Configured defs={defs} rows={rows} at={own.at} system={sys.name} />}
      <div className="row">
        <Choice
          value={show}
          onChange={setShow}
          label="Show"
          options={[
            { v: "heard", label: `Heard ${rows.length - beeps}` },
            { v: "all", label: `With beeps ${rows.length}` },
            { v: "named", label: `Signals of ${sys.name} ${rows.filter(named).length}` },
          ]}
        />
      </div>
      {shown.length === 0 ? (
        <p className="empty">
          No tones heard in this span{show === "heard" && beeps ? ` (${beeps} beeps: speech taken for a tone, or tones in the background)` : ""}. Pages, alert tones, DTMF and
          Morse IDs are found in the voice of every kind of call, and listed here as they're heard.
        </p>
      ) : (
        <div className="table-wrap tall">
          <table className="calls">
            <thead>
              <tr>
                <th>Talkgroup</th>
                <th>Pattern</th>
                <th>Tones</th>
                <th>Timing</th>
                <th title="Paging codes these tones may be, likeliest first: Quick Call II's, with the pager code when Motorola's encoding plan pairs them">Likely</th>
                <th>Heard</th>
                <th>Last</th>
                <th>Sent by</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.slice(0, 300).flatMap((x) => {
                const k = rowKey(x);
                const d = named(x);
                const out = [
                  <tr key={k}>
                    <td>
                      {x.alphaTag || x.tg}
                      {x.alphaTag && <span className="tag mono">{x.tg}</span>}
                    </td>
                    <td className="small">
                      {x.pattern === "digits" && x.digits.startsWith("KNOX") ? "KNOX" : x.pattern === "digits" ? "DTMF" : PATTERN[x.pattern]}
                      {x.format && <div className="muted">{x.format}</div>}
                    </td>
                    <td className="mono">{tonesText({ ...x, digits: x.digits.replace(/^KNOX /, "") })}</td>
                    <td className="mono small">{timing(x)}</td>
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
                      <PlayButton spec={rowSpec(x)} />
                      {d ? (
                        <span className="chip ok mono" title={d.name || `${sys.name}:${d.id}`}>
                          {d.id}
                        </span>
                      ) : own ? (
                        <button className="btn ghost small" title="Make it a signal of this system" onClick={() => setAdding(k)}>
                          Add
                        </button>
                      ) : null}
                    </td>
                  </tr>,
                ];
                if (own && adding === k) out.push(<AddRow key={`${k}+`} r={x} at={own.at} ids={defs.map((d) => d.id)} onDone={() => setAdding(null)} />);
                return out;
              })}
            </tbody>
          </table>
        </div>
      )}
      {shown.length > 300 && <p className="muted small">300 of {shown.length} shown.</p>}
    </Card>
  );
}
