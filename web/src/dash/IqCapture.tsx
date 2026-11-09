// A recording of what the radios receive, made while everything is working:
// the button that makes one and the list of those on disk.

import { useEffect, useRef, useState } from "react";
import { iqCaptureDelete, iqCaptureReview, iqCaptures, iqCaptureSend, iqCaptureStart, iqCaptureWithdraw, iqRuleCheck, updateConfig, useApp, type IqCaptureReview, type IqCaptures } from "../controller.ts";
import { Card, Choice, Hint } from "../charts.tsx";
import { bytes } from "../fmt.ts";
import type { CaptureRule, IqCapture, IqWaterfall } from "../protocol.ts";

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
  const sending = list?.captures.some((c) => c.send?.state === "sending") ?? false;
  useEffect(() => {
    if (!active && !sending) return;
    const t = setInterval(() => void iqCaptures().then(shown), 1000);
    return () => clearInterval(t);
  }, [active?.name, sending]);
  const [reviewing, setReviewing] = useState<string | null>(null);
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
              <div className="row">
                {sendText(c) && <span className={c.send?.state === "failed" ? "warn" : "hint"}>{sendText(c)}</span>}
                {list.canSend && (!c.send || c.send.state === "failed") && (
                  <button className="btn ghost small" onClick={() => setReviewing(reviewing === c.name ? null : c.name)}>
                    Review and send…
                  </button>
                )}
                {c.send?.state === "sent" && (
                  <button className="btn ghost small" onClick={() => confirm("Withdraw this capture from the public corpus? Copies others made can't be recalled.") && void iqCaptureWithdraw(c.name).then(shown)}>
                    Withdraw…
                  </button>
                )}
                <button className="btn ghost small" onClick={() => remove(c)}>
                  Delete…
                </button>
              </div>
              {reviewing === c.name && <IqCaptureReviewPanel name={c.name} bytes={c.bytes} onClose={() => setReviewing(null)} onSend={() => void iqCaptureSend(c.name).then((m) => { setReviewing(null); shown(m); })} />}
            </li>
          ))}
        </ul>
      )}
      {list && <IqCaptureRules presets={list.presets} />}
    </Card>
  );
}

/** How sending a capture stands, in words. */
export function sendText(c: IqCapture): string {
  const s = c.send;
  if (!s) return "";
  if (s.state === "sending") return `Sending… ${bytes(s.doneBytes)} of ${bytes(s.totalBytes)}`;
  if (s.state === "sent") return `Sent. Receipt ${s.receipt}`;
  return `Not sent: ${s.error ?? "it failed"}`;
}

// Black through purple and orange to white, so a signal stands out from the noise around it.
const STOPS = [
  [0, 0, 0, 4],
  [0.25, 87, 16, 110],
  [0.5, 188, 55, 84],
  [0.75, 249, 142, 9],
  [1, 252, 255, 164],
];

/** The colour of level `t`, 0 to 1. */
export function waterfallColor(t: number): number[] {
  let i = 0;
  while (i < STOPS.length - 2 && t > STOPS[i + 1][0]) i++;
  const [a, b] = [STOPS[i], STOPS[i + 1]];
  return [1, 2, 3].map((k) => a[k] + ((b[k] - a[k]) * (t - a[0])) / (b[0] - a[0]));
}

/** A waterfall's levels drawn on a canvas. */
function WaterfallImage({ w }: { w: IqWaterfall }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const ctx = ref.current?.getContext("2d");
    if (!ctx) return;
    const image = ctx.createImageData(w.width, w.height);
    for (let i = 0; i < w.width * w.height; i++) {
      const [r, g, b] = waterfallColor(parseInt(w.levels.slice(i * 2, i * 2 + 2), 16) / 255);
      image.data.set([r, g, b, 255], i * 4);
    }
    ctx.putImageData(image, 0, 0);
  }, [w]);
  return <canvas ref={ref} width={w.width} height={w.height} className="iq-waterfall" />;
}

/** What a capture would publish, and the owner's yes. */
export function IqCaptureReviewPanel({ name, bytes: size, initial, onSend, onClose }: { name: string; bytes: number; initial?: IqCaptureReview; onSend: () => void; onClose: () => void }) {
  const [review, setReview] = useState<IqCaptureReview | null>(initial ?? null);
  const [problem, setProblem] = useState<string | null>(initial?.error ?? null);
  const [agreed, setAgreed] = useState(false);
  useEffect(() => {
    if (initial) return;
    void iqCaptureReview(name).then((m) => (m.type === "iqCaptureReview" ? (setReview(m), setProblem(m.error ?? null)) : setProblem(m.message)));
  }, [name]);
  if (problem) return <p className="warn">{problem}</p>;
  if (!review) return <p className="hint">Preparing a preview…</p>;
  return (
    <div className="iq-review">
      {review.waterfalls.map((w, i) => (
        <WaterfallImage key={i} w={w} />
      ))}
      <details>
        <summary>Its metadata</summary>
        <pre>{JSON.stringify(review.metadata, null, 2)}</pre>
      </details>
      <details>
        <summary>Its diagnostics</summary>
        <pre>{JSON.stringify(review.diagnostics, null, 2)}</pre>
      </details>
      <label className="row">
        <input type="checkbox" checked={agreed} onChange={(e) => setAgreed(e.target.checked)} />
        <span>
          I understand this recording ({bytes(size)}) and its diagnostics become public for anyone to download and decode, including all the radio traffic it holds. It also shows roughly where I am. A withdrawn recording may already have been copied.
        </span>
      </label>
      <div className="row">
        <button className="btn primary" disabled={!agreed} onClick={onSend}>
          Send to the public corpus
        </button>
        <button className="btn ghost" onClick={onClose}>
          Cancel
        </button>
      </div>
    </div>
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
