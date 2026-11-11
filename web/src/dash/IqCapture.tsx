// Captures of what TRP receives, to replay or send in: a control channel,
// the next call or every radio, by hand; the list of those on disk; and the
// rules that make them by themselves, for the corner cases.

import { useEffect, useRef, useState } from "react";
import { iqCaptureCancel, iqCaptureDelete, iqCaptureReview, iqCaptures, iqCaptureSend, iqCaptureStart, iqCaptureWithdraw, iqRuleCheck, updateConfig, useApp, type IqCaptureReview, type IqCaptures } from "../controller.ts";
import { Card, Choice, Hint } from "../charts.tsx";
import { bytes } from "../fmt.ts";
import type { CaptureRule, CaptureWhat, IqCapture, IqWaterfall, SystemStatus } from "../protocol.ts";

type What = CaptureWhat["kind"];
const WHATS: { v: What; label: string }[] = [
  { v: "control", label: "A control channel" },
  { v: "call", label: "The next call" },
  { v: "radios", label: "Everything the radios receive" },
];
/** What each is for, in words. */
const WHAT_TEXT: Record<What, string> = {
  control: "Records one site's control channel from now on. Small, and the best start when calls aren't granted or followed, or the control channel decodes badly.",
  call: "Waits for the next call on a system (or one talkgroup) and records its voice channel from start to end. For calls that sound bad, are silent or go missing.",
  radios: "Records everything every radio receives. Large, so use it when the problem isn't one channel's, or for a known-good sample of your system.",
};
const LENGTHS: Record<What, string[]> = { control: ["30", "60"], call: [], radios: ["30", "60", "120"] };

/** What a capture shows. */
type Shows = "problem" | "good";

/** What decoding did while a capture was made, in words. */
export function iqCaptureText(c: IqCapture): string {
  if (c.tier !== "wideband") return `${c.seconds} s · ${bytes(c.bytes)}`;
  const total = c.messagesGood + c.messagesBad;
  const decoded = total ? `${Math.round((c.messagesGood / total) * 100)}% of ${total.toLocaleString("en-US")} control messages decoded` : "no control messages";
  const calls = `${c.calls.toLocaleString("en-US")} ${c.calls === 1 ? "call" : "calls"}`;
  const radios = `${c.radios} ${c.radios === 1 ? "radio" : "radios"}`;
  return `${c.seconds} s · ${radios} · ${bytes(c.bytes)} · ${decoded} · ${calls}`;
}

/** The running sites, by what a capture of one is asked by, and their names ("DCFD · North"). */
export function captureSites(systems: SystemStatus[]): { site: number; label: string }[] {
  return systems.flatMap((y) => y.sites.map((x) => ({ site: x.index, label: !x.name || x.name === y.shortName ? y.shortName : `${y.shortName} · ${x.name}` })));
}

/** The card, as `initial` says things are and with `systems` running (tests); it asks the recorder for those itself otherwise. */
export function IqCaptureCard({ initial, systems: given }: { initial?: IqCaptures; systems?: SystemStatus[] }) {
  const app = useApp();
  const systems = given ?? app.status?.systems ?? [];
  const sites = captureSites(systems);
  const [list, setList] = useState<IqCaptures | null>(initial ?? null);
  const [error, setError] = useState<string | null>(initial?.error ?? null);
  const [what, setWhat] = useState<What>("control");
  const [length, setLength] = useState("30");
  const [site, setSite] = useState<number | null>(null);
  const [system, setSystem] = useState("");
  const [talkgroup, setTalkgroup] = useState("");
  const [shows, setShows] = useState<Shows>("problem");
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
  const waiting = list?.waiting ?? [];
  const sending = list?.captures.some((c) => c.send?.state === "sending") ?? false;
  useEffect(() => {
    if (!active && !sending && !waiting.length) return;
    const t = setInterval(() => void iqCaptures().then(shown), 1000);
    return () => clearInterval(t);
  }, [active?.name, sending, waiting.length]);
  const [reviewing, setReviewing] = useState<string | null>(null);
  const pickWhat = (w: What) => {
    setWhat(w);
    if (LENGTHS[w].length && !LENGTHS[w].includes(length)) setLength(LENGTHS[w][0]);
  };
  const chosenSite = site ?? sites[0]?.site ?? null;
  const chosenSystem = system || systems[0]?.shortName || "";
  const tg = talkgroup.trim() ? Number(talkgroup.trim()) : undefined;
  const asked: CaptureWhat | null =
    what === "radios" ? { kind: "radios" } : what === "control" ? (chosenSite === null ? null : { kind: "control", site: chosenSite }) : chosenSystem && (tg === undefined || Number.isInteger(tg)) ? { kind: "call", system: chosenSystem, ...(tg === undefined ? {} : { talkgroup: tg }) } : null;
  const start = () => {
    if (!asked) return;
    setBusy(true);
    void iqCaptureStart(asked, Number(length), note.trim(), shows === "problem").then((m) => {
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
  const label = what === "control" ? "Record the control channel" : what === "call" ? "Record the next call" : "Record every radio";
  return (
    <Card title="Capture what TRP receives" id="iq-capture">
      <Hint>Record a control channel, a call or every radio, to replay later or to send in when something isn't working. It stays on this computer unless you send it.</Hint>
      <Choice label="What to capture" value={what} options={WHATS} onChange={pickWhat} />
      <Hint>{WHAT_TEXT[what]}</Hint>
      <div className="row">
        {what === "control" &&
          (sites.length ? (
            <select aria-label="Site" value={chosenSite ?? ""} onChange={(e) => setSite(Number(e.target.value))}>
              {sites.map((x) => (
                <option key={x.site} value={x.site}>
                  {x.label}
                </option>
              ))}
            </select>
          ) : (
            <span className="hint">No control channel is running.</span>
          ))}
        {what === "call" && (
          <>
            <select aria-label="System" value={chosenSystem} onChange={(e) => setSystem(e.target.value)}>
              {systems.map((y) => (
                <option key={y.shortName} value={y.shortName}>
                  {y.shortName}
                </option>
              ))}
            </select>
            <input className="search" aria-label="Talkgroup" inputMode="numeric" placeholder="Any talkgroup" value={talkgroup} onChange={(e) => setTalkgroup(e.target.value.replace(/[^0-9]/g, ""))} />
          </>
        )}
        {LENGTHS[what].length > 0 && <Choice label="Length" value={length} options={LENGTHS[what].map((v) => ({ v, label: `${v} s` }))} onChange={setLength} />}
      </div>
      <div className="row">
        <Choice label="It shows" value={shows} options={[{ v: "problem", label: "A problem" }, { v: "good", label: "Everything working" }]} onChange={setShows} />
        <input className="search grow" aria-label="Note" placeholder={shows === "problem" ? "What's wrong (optional)" : "Note (optional)"} value={note} maxLength={200} onChange={(e) => setNote(e.target.value)} />
        <button className="btn primary" disabled={busy || !recording || !asked || (what === "radios" && !!active)} onClick={start}>
          {what === "radios" && active ? `Capturing… ${Math.min(Math.floor(active.elapsedS), active.seconds)} of ${active.seconds} s` : label}
        </button>
      </div>
      {!recording && list && <p className="hint">Start recording first: a capture is of what the radios are receiving now.</p>}
      {error && <p className="warn">{error}</p>}
      {(waiting.length > 0 || active) && (
        <ul className="iq-list">
          {active && (
            <li>
              Every radio: {Math.min(Math.floor(active.elapsedS), active.seconds)} of {active.seconds} s
            </li>
          )}
          {waiting.map((w) => (
            <li key={w.key} className="row">
              <span className="grow">
                {w.text}: {w.seconds ? `${Math.min(Math.floor(w.elapsedS), w.seconds)} of ${w.seconds} s` : "waiting for a call…"}
              </span>
              <button className="btn ghost small" onClick={() => void iqCaptureCancel(w.key).then(shown)}>
                Cancel
              </button>
            </li>
          ))}
        </ul>
      )}
      {list && list.captures.length > 0 && (
        <ul className="iq-list">
          {list.captures.map((c) => (
            <li key={c.name}>
              <strong>{c.name}</strong> {c.tier !== "wideband" && <span className="badge-kind">{c.tier}</span>} {c.purpose === "bug" && <span className="badge-kind">problem</span>} {c.rule && <span className="badge-kind">rule {c.rule}</span>} {iqCaptureText(c)}
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
      {list && (
        <details className="iq-rules">
          <summary>Capture automatically when something goes wrong</summary>
          <IqCaptureRules presets={list.presets} />
        </details>
      )}
      {list && (
        <details className="iq-rules">
          <summary>Save captures in</summary>
          <CaptureFolder onSaved={() => setTimeout(() => void iqCaptures().then(shown), 700)} />
        </details>
      )}
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

/** Where captures go. A capture of several radios can write tens of MB a second, so a fast disk helps. */
export function CaptureFolder({ onSaved }: { onSaved: () => void }) {
  const s = useApp();
  const saved = s.config?.diagnostics?.captureDir ?? "";
  const [text, setText] = useState(saved);
  useEffect(() => setText(saved), [saved]);
  const commit = () => {
    if (text.trim() === saved) return;
    updateConfig((c) => void (c.diagnostics = { ...c.diagnostics, captureDir: text.trim() || undefined }));
    onSaved();
  };
  return (
    <div>
      <div className="row">
        <input className="search grow" aria-label="Capture folder" placeholder="iq-captures in the recordings folder" value={text} onChange={(e) => setText(e.target.value)} onBlur={commit} />
      </div>
      <Hint>Use a fast disk: a capture of several radios can write tens of MB per second, and a busy disk can slow your recordings too. Captures already made stay where they are.</Hint>
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
    <div>
      <Hint>For a problem that comes and goes: a rule keeps a recording here when it happens, within a daily limit. Nothing is sent anywhere. Changes apply the next time recording starts.</Hint>
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
