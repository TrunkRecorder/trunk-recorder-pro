// The setup guide's scan: what it found is shown as figures (value, unit),
// not inside sentences. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import type { FromRecorder, SurveyMonitor } from "./protocol.ts";
import { ScanStep } from "./Onboarding.tsx";

const monitor: SurveyMonitor = {
  freqHz: 857_987_500, heardHz: 857_987_500, identity: { sysId: 0x3cf, wacn: 0xbee00, rfss: 1, site: 4 }, advertisedHz: 857_987_500, secondary: [], adjacent: [],
  voice: [{ freqHz: 858_987_500, grants: 3, tdma: false }, { freqHz: 859_987_500, grants: 1, tdma: false }], idens: 1,
  good: 86, bad: 14, modulation: "c4fm", snrDb: 20, offsetHz: 0, ppm: 1.24, gain: { state: "off", steps: [], bestDb: null }, elapsedS: 12, ready: false, smartnet: null,
};

function connect(survey: FromRecorder): void {
  transport.send = () => {};
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: defaultConfig(), version: 1 });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, access: "control", surveyBands: [{ id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true }], capabilities: [] });
  transport.onMessage(survey);
}

const render = () => renderToString(<ScanStep s={snapshot()} c={snapshot().config!} found={null} onFound={() => {}} onBack={() => {}} onNext={() => {}} />);
const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");

test("Setup guide: scanning shows how far and how many found as figures", () => {
  connect({ type: "survey", stage: "scanning", source: 0, bands: ["800"], message: "", error: null, progress: { hop: 6, hops: 10, band: "800", centerHz: 855e6 }, candidates: [], monitor: null, suggest: null });
  const t = text(render());
  assert.match(t, /Swept 50 %/, t);
  assert.match(t, /Found 0/);
});

test("Setup guide: listening to a control channel shows its figures, each labelled", () => {
  connect({ type: "survey", stage: "monitoring", source: 0, bands: ["800"], message: "", error: null, progress: null, candidates: [], monitor, suggest: null });
  const html = render();
  const t = text(html);
  assert.match(t, /Control channel 857\.9875 MHz/, t);
  assert.match(t, /Type P25/);
  assert.match(t, /Listening 12 s/);
  assert.match(t, /Decoded 86 %/);
  assert.match(t, /System ID 3CF WACN BEE00 Site 1-4/);
  assert.match(t, /Tuning correction \+1\.2 ppm/);
  assert.match(t, /Voice channels 2/);
  assert.ok(!t.includes("of messages decoded") && !t.includes("seen so far"), "no figures inside sentences");
});
