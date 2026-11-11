// The RF page's capture card, as rendered (react-dom/server). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { IqCaptureCard, IqCaptureReviewPanel, iqCaptureText, sendText, waterfallColor } from "./dash/IqCapture.tsx";
import type { IqCapture, SystemStatus } from "./protocol.ts";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");
const capture: IqCapture = { name: "20261108-140311", path: "/calls/iq-captures/20261108-140311", tier: "wideband", purpose: "healthy", seconds: 60, radios: 2, bytes: 576_000_000, note: "Clear day", lostSamples: 0, messagesGood: 990, messagesBad: 10, calls: 1 };
const answer = { type: "iqCaptures" as const, id: 1, waiting: [], captures: [capture], root: "/calls/iq-captures", recording: true, canSend: true, presets: [{ id: "poor-voice", enabled: true, on: "call.end", capture: [{ tier: "voice" as const }] }] };

const site = (index: number, name: string) => ({ index, name }) as SystemStatus["sites"][number];
const systems = [{ shortName: "capplus", sites: [site(0, "capplus")] }, { shortName: "dcfd", sites: [site(1, "North"), site(2, "South")] }] as SystemStatus[];

test("While recording, a control channel is one pick and one press, and nothing is sent", () => {
  const html = renderToString(<IqCaptureCard initial={answer} systems={systems} />);
  assert.match(html, /<button class="btn primary">Record the control channel<\/button>/);
  assert.match(html, /<option value="0" selected="">capplus<\/option><option value="1">dcfd · North<\/option>/, "each running site, by its system");
  assert.match(text(html), /It stays on this computer unless you send it/);
  assert.match(text(html), /A problem/);
  assert.match(text(html), /Clear day/);
  assert.match(text(html), /\/calls\/iq-captures\/20261108-140311/);
});

test("The button waits for the radios", () => {
  const html = renderToString(<IqCaptureCard initial={{ ...answer, recording: false }} systems={systems} />);
  assert.match(html, /<button class="btn primary" disabled="">Record the control channel/);
  assert.match(text(html), /Start recording first/);
  assert.match(renderToString(<IqCaptureCard initial={answer} systems={[]} />), /disabled=""/, "no site to record");
});

test("Captures waiting on the air say how they're going, and can be given up", () => {
  const waiting = [{ key: 1, text: "30 s of capplus's control channel", seconds: 30, elapsedS: 12.7 }, { key: 2, text: "The next call on dcfd, talkgroup 101", elapsedS: 3 }];
  const html = text(renderToString(<IqCaptureCard initial={{ ...answer, waiting, active: { name: "x", seconds: 60, elapsedS: 5 } }} systems={systems} />));
  assert.match(html, /30 s of capplus's control channel: 12 of 30 s/);
  assert.match(html, /The next call on dcfd, talkgroup 101: waiting for a call…/);
  assert.match(html, /Every radio: 5 of 60 s/);
  assert.match(html, /Cancel/);
});

test("Why a capture failed is said, and so are lost samples", () => {
  assert.match(text(renderToString(<IqCaptureCard initial={{ ...answer, error: "Recording stopped during the capture." }} />)), /Recording stopped during the capture/);
  assert.match(text(renderToString(<IqCaptureCard initial={{ ...answer, captures: [{ ...capture, lostSamples: 4096 }] }} />)), /4,096 samples lost/);
});

test("Capture rules can be switched on, and a rule capture says which rule made it", () => {
  const html = renderToString(<IqCaptureCard initial={{ ...answer, captures: [{ ...capture, tier: "voice", purpose: "rule", rule: "poor-voice" }] }} />);
  assert.match(text(html), /Capture automatically when something goes wrong/);
  assert.match(text(html), /Calls keep coming through badly/);
  assert.match(html, /type="checkbox"/);
  assert.match(text(html), /voice rule poor-voice/);
});

test("Sending asks for a yes, knowing it's public", () => {
  const review = { type: "iqCaptureReview" as const, id: 1, name: "x", metadata: { "trp:tier": "wideband" }, diagnostics: { app: {} }, waterfalls: [{ width: 1, height: 1, levels: "ff" }] };
  const html = renderToString(<IqCaptureReviewPanel name="x" bytes={1_000_000} initial={review} onSend={() => {}} onClose={() => {}} />);
  assert.match(text(html), /become public for anyone to download and decode/);
  assert.match(html, /<button class="btn primary" disabled="">Send to the public corpus/, "not until it's agreed to");
  assert.match(html, /<canvas/);
});

test("A capture says how its sending went", () => {
  const send = { state: "sending" as const, receipt: "", id: "a", doneBytes: 1_000_000, totalBytes: 2_000_000 };
  assert.equal(sendText({ ...capture, send }), "Sending… 1.00 MB of 2.00 MB");
  assert.equal(sendText({ ...capture, send: { ...send, state: "sent", receipt: "abc" } }), "Sent. Receipt abc");
  assert.equal(sendText({ ...capture, send: { ...send, state: "failed", error: "no answer" } }), "Not sent: no answer");
  assert.match(renderToString(<IqCaptureCard initial={answer} />), /Review and send…/);
  assert.doesNotMatch(renderToString(<IqCaptureCard initial={{ ...answer, canSend: false }} />), /Review and send/, "no service to send to yet");
});

test("A waterfall runs from black to bright, and a signal is brighter than the noise", () => {
  assert.deepEqual(waterfallColor(0), [0, 0, 4]);
  assert.deepEqual(waterfallColor(1), [252, 255, 164]);
  const sum = (c: number[]) => c.reduce((a, b) => a + b);
  for (const t of [0.1, 0.3, 0.5, 0.7, 0.9]) assert.ok(sum(waterfallColor(t + 0.05)) > sum(waterfallColor(t)));
});

test("Where captures go can be chosen", () => {
  assert.match(text(renderToString(<IqCaptureCard initial={answer} />)), /Save captures in/);
  assert.match(renderToString(<IqCaptureCard initial={answer} />), /aria-label="Capture folder"/);
});

test("A capture in words", () => {
  assert.equal(iqCaptureText(capture), "60 s · 2 radios · 576 MB · 99% of 1,000 control messages decoded · 1 call");
  assert.match(iqCaptureText({ ...capture, messagesGood: 0, messagesBad: 0, radios: 1 }), /1 radio · .* no control messages/);

  assert.equal(iqCaptureText({ ...capture, tier: "control", seconds: 30, bytes: 11_520_000 }), "30 s · 11.5 MB", "a channel's: no radios or call counts it didn't keep");
  assert.match(text(renderToString(<IqCaptureCard initial={{ ...answer, captures: [{ ...capture, tier: "control", purpose: "bug" }] }} systems={systems} />)), /control problem/);
});
