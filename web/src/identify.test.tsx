// "Identify surveyed sites": the dialog says why it asks for a location and
// that declining is fine; the licenses found show in the signals table and
// name a system added from the survey. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { callSigns, defaultConfig, normalizeSystem } from "./config.ts";
import { applySurvey, snapshot, transport } from "./controller.ts";
import { IdentifyDialog, parseLatLon } from "./Identify.tsx";
import type { FromRecorder, SurveyLicense, SurveySuggestion } from "./protocol.ts";
import { SurveyPanel } from "./Survey.tsx";

const license = (more: Partial<SurveyLicense>): SurveyLicense => ({
  callSign: "WPXT459", licensee: "DISTRICT OF COLUMBIA", serviceCode: "YE", serviceDesc: "PubSafty/SpecEmer", freqHz: 857_987_500, emission: "8K10F1E",
  distanceM: 1374, bearingDeg: 115, bearingCardinal: "ESE", city: "WASHINGTON", state: "DC", status: "A", locationKind: "fixed",
  ...more,
});

function surveyed(): void {
  const c = defaultConfig();
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [{ id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true }], capabilities: [] },
    {
      type: "survey", stage: "done", source: 0, bands: ["800"], message: "", error: null, progress: null, monitor: null, suggest: null,
      candidates: [{ freqHz: 857_987_500, correctedHz: null, band: "800", snrDb: 25, widthHz: 12_500, kind: "control", frames: 10, good: 10, bad: 0, modulation: "", identity: { nac: 0x443 } }],
      licenses: {
        stage: "done", lat: 38.9, lon: -77.03, radiusM: 50_000, error: null,
        results: [{ freqHz: 857_987_500, historical: false, licenses: [license({}), license({ callSign: "KNJU834", distanceM: 5283 })] }],
      },
    },
  ];
  for (const m of greeting) transport.onMessage(m);
}

test("Identify: the dialog says why it wants a location, what is sent where, and that the survey works without it", () => {
  const html = renderToString(<IdentifyDialog onClose={() => {}} />);
  assert.ok(html.includes("Use my location"), html);
  assert.ok(html.includes("trunkrecorder.pro"));
  assert.ok(html.includes("no lookup is made"));
  assert.ok(html.includes("Not now"));
  assert.ok(html.includes("150 km"));
});

test("Identify: the licenses found show in the signals table", () => {
  surveyed();
  const html = renderToString(<SurveyPanel c={snapshot().config!} />);
  assert.ok(html.includes("Identify surveyed sites"), html);
  assert.ok(html.includes("Licensed to"));
  assert.ok(html.includes("DISTRICT OF COLUMBIA"));
  assert.ok(html.includes("WPXT459"));
  assert.ok(html.includes("1 more"));
});

test("Identify: a system added from the survey is named for its licensee, with its call signs", () => {
  surveyed();
  const sug: SurveySuggestion = {
    type: "p25", bandplan: null, controlChannels: [857_987_500], ppm: null, ppmApply: null, gainDb: null, centerHz: 858_500_000,
    voiceCovered: 0, voiceTotal: 0, spanHz: 0, identity: { nac: 0x443 }, voiceChannels: [],
  };
  applySurvey(0, sug, "new");
  const sys = snapshot().config!.systems.at(-1)!;
  assert.equal(sys.name, "District of Columbia");
  assert.deepEqual(sys.fccCallSigns, ["WPXT459", "KNJU834"]);
});

test("FCC call signs: kept upper case and once; a system's stay its own when read", () => {
  assert.deepEqual(callSigns(" wpxt459, WPXT459;knju834 "), ["WPXT459", "KNJU834"]);
  const sys = normalizeSystem({ shortName: "dcfd", fccCallSigns: ["wpxt459"], sites: [{ controlChannelsHz: [1], fccCallSigns: ["knju834"] }] });
  assert.deepEqual(sys.fccCallSigns, ["WPXT459"]);
  assert.deepEqual(sys.sites[0].fccCallSigns, ["KNJU834"]);
  assert.equal(normalizeSystem({ shortName: "x" }).fccCallSigns, undefined);
});

test("a typed location: as a map copies it, or nothing", () => {
  assert.deepEqual(parseLatLon("38.9072, -77.0369"), { lat: 38.9072, lon: -77.0369 });
  assert.deepEqual(parseLatLon("38.9 -77"), { lat: 38.9, lon: -77 });
  assert.equal(parseLatLon("91, 0"), null);
  assert.equal(parseLatLon("Washington"), null);
});
