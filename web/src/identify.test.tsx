// Identifying systems from FCC licenses: the location fields say why and that
// declining is fine; licenses found show in the survey's table, name a
// system added from it, and can be kept as a system's FCC name and call sign.
// `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, fccPrimary, normalizeConventional, normalizeSystem } from "./config.ts";
import { applySurvey, editFcc, identifySurvey, lookupLicensesByFreq, mergeLookup, nameCallSigns, siteFrequencies, snapshot, surveySignals, transport } from "./controller.ts";
import { FccTable, IdentifyFcc, parseLatLon, WhereFields, WhyLocation } from "./Identify.tsx";
import type { FccLicenses, FromRecorder, LicenseResultRow, SurveyLicense, SurveySuggestion, ToRecorder } from "./protocol.ts";
import { ScanStep } from "./Onboarding.tsx";
import { SurveyPanel } from "./Survey.tsx";

const license = (more: Partial<SurveyLicense>): SurveyLicense => ({
  callSign: "WPXT459", licensee: "DISTRICT OF COLUMBIA", serviceCode: "YE", serviceDesc: "PubSafty/SpecEmer", freqHz: 857_987_500, emission: "8K10F1E",
  distanceM: 1374, bearingDeg: 115, bearingCardinal: "ESE", city: "WASHINGTON", state: "DC", status: "A", locationKind: "fixed",
  ...more,
});
const results: LicenseResultRow[] = [
  { freqHz: 857_987_500, historical: false, licenses: [license({}), license({ callSign: "KNJU834", distanceM: 5283 })] },
  { freqHz: 858_987_500, historical: false, licenses: [license({ freqHz: 858_987_500 })] },
];

/** Connected, with a survey that found DCFD's control channel; what the page sends is kept in `sent`. */
function surveyed(): ToRecorder[] {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => void sent.push(m);
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: defaultConfig(), version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [{ id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true }], capabilities: [] },
    {
      type: "survey", stage: "done", source: 0, bands: ["800"], message: "", error: null, progress: null, monitor: null, suggest: null,
      candidates: [
        { freqHz: 857_987_000, correctedHz: 857_987_500, band: "800", snrDb: 25, widthHz: 12_500, kind: "control", frames: 10, good: 10, bad: 0, modulation: "", identity: { nac: 0x443 } },
        { freqHz: 858_100_000, correctedHz: null, band: "800", snrDb: 7, widthHz: 30_000, kind: "other", frames: 0, good: 0, bad: 0, modulation: "", identity: {} },
      ],
    },
  ];
  for (const m of greeting) transport.onMessage(m);
  return sent;
}

/** Answer the last licenseQuery sent. */
function answer(sent: ToRecorder[]): Promise<void> {
  const q = sent.findLast((m) => m.type === "licenseQuery");
  assert.ok(q && q.type === "licenseQuery");
  transport.onMessage({ type: "licenseResult", id: q.id, results, error: null });
  return new Promise((r) => setTimeout(r, 0));
}

test("Identify: the location fields say why, what is sent where, and that it works without", () => {
  const html = renderToString(
    <>
      <WhyLocation what="the systems found" />
      <WhereFields value={{ text: "", radiusKm: 50 }} onChange={() => {}} />
    </>,
  );
  assert.ok(html.includes("Use my location"), html);
  assert.ok(html.includes("trunkrecorder.pro"));
  assert.ok(html.includes("no lookup is made"));
  assert.ok(html.includes("150 km"));
});

test("Identify: the survey's signals are looked up (not the weak unknown one), and show in its table", async () => {
  const sent = surveyed();
  assert.deepEqual(surveySignals(snapshot().survey), [{ freqHz: 857_987_500, widthHz: 12_500 }]);
  identifySurvey(38.9, -77.03, 50_000);
  const q = sent.findLast((m) => m.type === "licenseQuery");
  assert.ok(q && q.type === "licenseQuery" && q.lat === 38.9 && q.radiusM === 50_000);
  await answer(sent);
  assert.equal(snapshot().surveyLicenses?.stage, "done");
  const html = renderToString(<SurveyPanel c={snapshot().config!} />);
  assert.ok(html.includes("Licensed to"), html);
  assert.ok(html.includes("DISTRICT OF COLUMBIA") && html.includes("WPXT459") && html.includes("1 more"));
  assert.ok(!html.includes("Download survey"));
});

test("Identify: a system added from the survey gets the licensee on each of its frequencies, and its name", async () => {
  const sent = surveyed();
  identifySurvey(38.9, -77.03, 50_000);
  await answer(sent);
  const sug: SurveySuggestion = {
    type: "p25", bandplan: null, controlChannels: [857_987_500], ppm: null, ppmApply: null, gainDb: null, centerHz: 858_500_000,
    voiceCovered: 0, voiceTotal: 0, spanHz: 0, identity: { nac: 0x443 }, voiceChannels: [858_987_500],
  };
  applySurvey(0, sug, "new");
  const sys = snapshot().config!.systems.at(-1)!;
  assert.equal(sys.name, "District of Columbia");
  assert.deepEqual(sys.fcc, {
    "857987500": { callSign: "WPXT459", name: "DISTRICT OF COLUMBIA", from: "lookup" },
    "858987500": { callSign: "WPXT459", name: "DISTRICT OF COLUMBIA", from: "lookup" },
  });
});

test("FCC licenses: a lookup fills what has none, names a call sign heard, and never replaces the user's or the air's", () => {
  const fcc: FccLicenses = {
    "857987500": { callSign: "WPXT459" },
    "859087500": { callSign: "KNJA391", from: "air" },
    "859987500": { callSign: "KNJE616", from: "air" },
    "860000000": { callSign: "WOLD1", name: "OLD", from: "lookup" },
  };
  const found = lookupLicensesByFreq([
    { freqHz: 857_987_500, historical: false, licenses: [license({ callSign: "KNJU834" })] },
    { freqHz: 859_087_500, historical: false, licenses: [license({ callSign: "KNJA391" })] },
    { freqHz: 860_000_000, historical: false, licenses: [license({ callSign: "WNEW1", licensee: "NEW" })] },
    { freqHz: 861_000_000, historical: false, licenses: [license({ callSign: "wxyz12" })] },
  ]);
  assert.equal(mergeLookup(fcc, found), 3);
  assert.deepEqual(fcc["857987500"], { callSign: "WPXT459" }, "the user's");
  assert.deepEqual(fcc["859087500"], { callSign: "KNJA391", from: "air", name: "DISTRICT OF COLUMBIA" }, "heard, now named");
  assert.equal(fcc["860000000"].callSign, "WNEW1", "an older lookup's");
  assert.equal(fcc["861000000"].callSign, "WXYZ12");
  // By call sign: names what has none.
  assert.equal(nameCallSigns(fcc, [{ callSign: "KNJE616", licensee: "DISTRICT OF COLUMBIA", serviceCode: "YE", serviceDesc: null, status: "A", city: null, state: null }]), 1);
  assert.equal(fcc["859987500"].name, "DISTRICT OF COLUMBIA");
  // The one that names it: its control channel's, else the call sign on the most frequencies.
  assert.equal(fccPrimary(fcc, [857_987_500])?.callSign, "WPXT459");
  assert.equal(fccPrimary({ a: { callSign: "B2C" }, b: { callSign: "A1B" }, c: { callSign: "B2C" } }, [])?.callSign, "B2C");
});

test("Identify on a system: its frequencies, and its licenses as a table to edit", () => {
  surveyed();
  const sys = normalizeSystem({ shortName: "dcfd", fcc: { "859087500": { callSign: "knja391", from: "air" } }, sites: [{ controlChannelsHz: [857_987_500, 858_987_500], voiceChannelsHz: [857_987_500, 0] }] });
  assert.deepEqual(siteFrequencies(sys.sites), [{ freqHz: 857_987_500 }, { freqHz: 858_987_500 }]);
  const target = { kind: "system" as const, index: 0 };
  assert.ok(renderToString(<IdentifyFcc target={target} what="dcfd" freqs={[857_987_500]} fcc={sys.fcc} />).includes("Identify"));
  const table = renderToString(<FccTable target={target} fcc={sys.fcc} />);
  assert.ok(table.includes("859.0875") && table.includes("KNJA391") && table.includes("heard"), table);
  // (On whatever system 0 is now: an edit of an earlier test's is still being saved.)
  editFcc(target, (f) => (f["857987500"] = { callSign: "WPXT459" }));
  assert.deepEqual(snapshot().config!.systems[0].fcc!["857987500"], { callSign: "WPXT459" });
  editFcc(target, (f) => {
    for (const k of Object.keys(f)) delete f[k];
  });
  assert.equal(snapshot().config!.systems[0].fcc, undefined);
});

test("FCC licenses as the config keeps them: on the raster, upper case, empty ones left out; a conventional system's too", () => {
  const sys = normalizeSystem({ shortName: "dcfd", fcc: { "857987600": { callSign: " wpxt459 ", name: " DC " }, "858000000": { callSign: "" }, x: { callSign: "A1B" } } as never, sites: [] });
  assert.deepEqual(sys.fcc, { "857987500": { callSign: "WPXT459", name: "DC" } });
  assert.ok(!("fcc" in normalizeSystem({ shortName: "x", fcc: {} })));
  assert.deepEqual(normalizeConventional({ shortName: "county", fcc: { "155475000": { callSign: "kqx123", from: "air" } } }).fcc, { "155475000": { callSign: "KQX123", from: "air" } });
});

test("a typed location: as a map copies it, or nothing", () => {
  assert.deepEqual(parseLatLon("38.9072, -77.0369"), { lat: 38.9072, lon: -77.0369 });
  assert.deepEqual(parseLatLon("38.9 -77"), { lat: 38.9, lon: -77 });
  assert.equal(parseLatLon("91, 0"), null);
  assert.equal(parseLatLon("Washington"), null);
});

test("Setup guide: choosing bands offers naming what it finds, off unless a location was given before", () => {
  surveyed();
  transport.onMessage({ type: "survey", stage: "idle" });
  const c = snapshot().config!;
  const html = renderToString(<ScanStep s={snapshot()} c={c} found={null} onFound={() => {}} onBack={() => {}} onNext={() => {}} />);
  assert.ok(html.includes("Name what it finds (optional)"), html);
  assert.ok(html.includes("scan without sharing a location"));
  assert.ok(!html.includes("Use my location"));
});
