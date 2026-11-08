// A recording of what the radios receive, made while everything is working:
// the button that makes one and the list of those on disk.

import { useEffect, useState } from "react";
import { iqCaptureDelete, iqCaptures, iqCaptureStart, type IqCaptures } from "../controller.ts";
import { Card, Choice, Hint } from "../charts.tsx";
import { bytes } from "../fmt.ts";
import type { IqCapture } from "../protocol.ts";

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
              <strong>{c.name}</strong> {iqCaptureText(c)}
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
    </Card>
  );
}
