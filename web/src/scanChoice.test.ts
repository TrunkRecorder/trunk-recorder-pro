// What Find my system scans (scanChoice.tsx): the choice remembered, and a band shown by its numbers. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { canScan, savedPick, scanRangeLabel } from "./scanChoice.tsx";
import { startSurvey, transport } from "./controller.ts";
import type { SurveyBand, ToRecorder } from "./protocol.ts";

const BANDS: SurveyBand[] = [
  { id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true },
  { id: "uhf", label: "UHF", loHz: 450e6, hiHz: 470e6, defaultOn: true },
  { id: "uhf-420", label: "UHF 420–450", loHz: 420e6, hiHz: 450e6, defaultOn: false },
];

const store = new Map<string, string>();
(globalThis as Record<string, unknown>).localStorage = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
};

test("a band is shown by its name, a range by its numbers", () => {
  assert.equal(scanRangeLabel({ id: "uhf", loHz: 450e6, hiHz: 470e6 }, BANDS), "UHF");
  assert.equal(scanRangeLabel({ id: null, loHz: 433_050_000, hiHz: 434_790_000 }, BANDS), "433.05–434.79 MHz");
  // A band this page doesn't know: still its numbers.
  assert.equal(scanRangeLabel({ id: "x", loHz: 420e6, hiHz: 450e6 }, BANDS), "420–450 MHz");
  assert.equal(scanRangeLabel(null, BANDS), "");
});

test("bands and ranges are remembered alike", () => {
  store.clear();
  assert.deepEqual(savedPick(BANDS), { bands: ["800", "uhf"], ranges: { ranges: [], bad: [] } }, "nothing kept: the usual bands");
  store.set("trp.surveyChoice", JSON.stringify({ bands: ["uhf-420", "gone"], ranges: [{ loHz: 433e6, hiHz: 435e6 }, { loHz: 5, hiHz: 1 }] }));
  assert.deepEqual(savedPick(BANDS), { bands: ["uhf-420"], ranges: { ranges: [{ loHz: 433e6, hiHz: 435e6 }], bad: [] } }, "a band gone and a range that isn't one dropped");
  store.set("trp.surveyChoice", "not json");
  assert.deepEqual(savedPick(BANDS).bands, ["800", "uhf"]);
});

test("a scan needs something to scan, and nothing typed wrong", () => {
  const r = { loHz: 420e6, hiHz: 450e6 };
  assert.equal(canScan({ bands: [], ranges: { ranges: [r], bad: [] } }), true, "ranges alone");
  assert.equal(canScan({ bands: ["uhf"], ranges: { ranges: [], bad: [] } }), true, "bands alone");
  assert.equal(canScan({ bands: [], ranges: { ranges: [], bad: [] } }), false, "nothing");
  assert.equal(canScan({ bands: ["uhf"], ranges: { ranges: [r], bad: ["abc"] } }), false, "something that isn't a range");
});

test("a scan is started with its ranges, in Hz, beside its bands", () => {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => void sent.push(m);
  const start = () => sent.findLast((m) => m.type === "surveyStart");
  startSurvey(0, { bands: [], ranges: [{ loHz: 420e6, hiHz: 450e6 }] }, false);
  assert.deepEqual(start(), { type: "surveyStart", source: 0, bands: [], ranges: [{ loHz: 420e6, hiHz: 450e6 }], findGain: false });
  // None: left out, as before there were any.
  startSurvey(0, { bands: ["uhf"], ranges: [] }, false);
  assert.deepEqual(start(), { type: "surveyStart", source: 0, bands: ["uhf"], findGain: false });
});
