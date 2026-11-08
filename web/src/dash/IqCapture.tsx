// A recording of what the radios receive, made while everything is working:
// the button that makes one and the list of those on disk.

import { useEffect, useState } from "react";
import { iqCaptureDelete, iqCaptures, iqCaptureStart, iqRuleCheck, updateConfig, useApp, type IqCaptures } from "../controller.ts";
import { Card, Choice, Hint } from "../charts.tsx";
import { bytes } from "../fmt.ts";
import type { CaptureRule, IqCapture } from "../protocol.ts";

const LENGTHS = ["30", "60", "120"] as const;
type Length = (typeof LENGTHS)[number];

/** What decoding did while a capture was made, in words. */
export function iqCaptureText(c: IqCapture): string {
  const total = c.messagesGood + c.messagesBad;
  const decoded = total ? `${Math.round((c.messagesGood / total) * 100)}% of ${total.toLocaleString("en-US")} control messages decoded` : "no control messages";
  const calls = `${c.calls.toLocaleString("en-US")} ${c.calls === 1 ? "call" : "calls"}`;
  const radios = `${c.radios} ${c.radios === 1 ? "radio" : "radios"}`;
  return `${c.seconds} s · ${radios} · ${bytes(c.bytes)} · ${decoded} · ${calls}`;
}

/** The card, as `initial` says things are (tests); it asks the recorder for that itself otherwise. */
export function IqCaptureCard({ initial }: { initial?: IqCaptures }) {
  const [list, setList] = useState<IqCaptures | null>(initial ?? null);
  const [error, setError] = useState<string | null>(initial?.error ?? null);
  const [length, setLength] = useState<Length>("60");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const shown = (m: Awaited<ReturnType<typeof iqCaptures>>) => {
    if (m.type === "iqCaptures") {
      setList(m);
      setError(m.error ?? null);
    } else setError(m.message);
  };
  useEffect(() => void iqCaptures().then(shown), []);
  // (The recorder says how it's going: asked each second while it's going.)
  const active = list?.active;
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => void iqCaptures().then(shown), 1000);
    return () => clearInterval(t);
  }, [active?.name]);
  const start = () => {
    setBusy(true);
    void iqCaptureStart(Number(length), note.trim()).then((m) => {
      setBusy(false);
      setNote("");
      shown(m);
    });
  };
  const remove = (c: IqCapture) => {
    if (!confirm(`Delete the capture from ${c.name} (${bytes(c.bytes)})? This can't be undone.`)) return;
    void iqCaptureDelete(c.name).then(shown);
  };
  const recording = list?.recording ?? false;
  return (
    <Card title="Capture a known-good sample" id="iq-capture">
      <Hint>
        When everything is decoding well, record what every radio receives. Replaying it later shows whether a change to the decoders keeps working on your system. It stays on this computer: nothing is sent anywhere.
      </Hint>
      <div className="row">
        <Choice label="Length" value={length} options={LENGTHS.map((v) => ({ v, label: `${v} s` }))} onChange={setLength} />
        <input className="search grow" aria-label="Note" placeholder="Note (optional)" value={note} maxLength={200} onChange={(e) => setNote(e.target.value)} />
        <button className="btn primary" disabled={busy || !!active || !recording} onClick={start}>
          {active ? `Capturing… ${Math.min(Math.floor(active.elapsedS), active.seconds)} of ${active.seconds} s` : "Capture known-good sample"}
        </button>
      </div>
      {error && <p className="warn">{error}</p>}
      {list && list.captures.length > 0 && (
        <ul className="iq-list">
          {list.captures.map((c) => (
            <li key={c.name}>
              <strong>{c.name}</strong> {c.tier !== "wideband" && <span className="badge-kind">{c.tier}</span>} {c.rule && <span className="badge-kind">rule {c.rule}</span>} {iqCaptureText(c)}
              {c.lostSamples > 0 && <span className="warn"> · {c.lostSamples.toLocaleString("en-US")} samples lost</span>}
              {c.note && <div className="hint">{c.note}</div>}
              <div className="hint">{c.path}</div>
              <button className="btn ghost small" onClick={() => remove(c)}>
                Delete…
              </button>
            </li>
          ))}
        </ul>
      )}
      {list && <IqCaptureRules presets={list.presets} />}
    </Card>
  );
}

/** What a preset is for, in words. */
const PRESET_TEXT: Record<string, string> = {
  "control-lost": "A control channel stops decoding",
  "control-failing": "A control channel keeps failing to decode",
  "channel-not-in-table": "A channel isn't in the band plan",
  "poor-voice": "Calls keep coming through badly",
  "no-audio": "A granted call has no audio",
  clipping: "A radio is clipping",
  "healthy-sample": "One sample of every radio a day",
};

/** The capture rules: presets to switch on, and any others as JSON. */
export function IqCaptureRules({ presets }: { presets: CaptureRule[] }) {
  const s = useApp();
  const [text, setText] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const rules = s.config?.diagnostics?.rules ?? [];
  const on = (id: string) => rules.some((r) => r.id === id && r.enabled);
  const setRules = (next: CaptureRule[]) => updateConfig((c) => void (c.diagnostics = { ...c.diagnostics, rules: next }));
  const toggle = (p: CaptureRule) => setRules(on(p.id) ? rules.filter((r) => r.id !== p.id) : [...rules.filter((r) => r.id !== p.id), p]);
  const others = rules.filter((r) => !presets.some((p) => p.id === r.id));
  const add = () => {
    let rule: CaptureRule;
    try {
      rule = { enabled: true, ...JSON.parse(text) } as CaptureRule;
    } catch (e) {
      setProblem(`That isn't JSON: ${(e as Error).message}`);
      return;
    }
    void iqRuleCheck(rule).then((m) => {
      const why = m.type === "iqRuleChecked" ? m.error : m.message;
      if (why) return setProblem(why);
      setRules([...rules.filter((r) => r.id !== rule.id), rule]);
      setText("");
      setProblem(null);
    });
  };
  return (
    <div className="iq-rules">
      <h4>Capture automatically</h4>
      <Hint>Keeps a recording here when something goes wrong, within a daily limit. Nothing is sent anywhere. Changes apply the next time recording starts.</Hint>
      {presets.map((p) => (
        <label key={p.id} className="row">
          <input type="checkbox" checked={on(p.id)} onChange={() => toggle(p)} />
          {PRESET_TEXT[p.id] ?? p.id}
        </label>
      ))}
      {others.map((r) => (
        <div key={r.id} className="row">
          <code>{r.id}</code> on {r.on}
          <button className="btn ghost small" onClick={() => setRules(rules.filter((x) => x.id !== r.id))}>
            Remove
          </button>
        </div>
      ))}
      <details>
        <summary>Add a rule</summary>
        <textarea className="iq-rule" aria-label="Rule" rows={6} value={text} onChange={(e) => setText(e.target.value)} placeholder={'{ "id": "my-rule", "on": "call.end", "if": "call.snr < 6.0", "capture": [{ "tier": "voice" }] }'} />
        <button className="btn ghost" disabled={!text.trim()} onClick={add}>
          Add
        </button>
        {problem && <p className="warn">{problem}</p>}
      </details>
    </div>
  );
}
