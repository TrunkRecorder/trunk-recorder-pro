// A system from the directory known only by its licenses (no one has shared
// it): its bundle has no site to follow, only its licensed frequencies. The
// import dialog starts the survey looking for it, the survey marks its
// signals, and a system added from it gets its licenses and name. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, normalizeSystem } from "./config.ts";
import { applySurvey, bandsHolding, findOnAir, lookFor, snapshot, transport } from "./controller.ts";
import { findExisting, ImportDialog, parseBundle, toFind, type Bundle } from "./directoryImport.tsx";
import { lookForBundle } from "./lookFor.ts";
import type { Config, FromRecorder, SurveyBand, SurveySuggestion, ToRecorder } from "./protocol.ts";
import { SurveyPanel } from "./Survey.tsx";

const lic = { callSign: "WPXT459", name: "DISTRICT OF COLUMBIA", from: "lookup" as const };
/** DC Fire and EMS known only by its license, as the directory hands it over (GET /systems/fcc_wpxt459/config). */
const bundle = (): Bundle => ({
  format: "trp-system",
  v: 1,
  source: { directory: "https://trunkrecorder.pro/directory", id: "fcc_wpxt459", rev: 0, url: "https://trunkrecorder.pro/directory/systems/fcc_wpxt459", licensee: "DISTRICT OF COLUMBIA" },
  system: { shortName: "district-of-columbia", type: "p25", enabled: true, fcc: { "855462500": lic, "857987500": lic, "858987500": lic, "859987500": lic }, sites: [] },
  talkgroupsCsv: "",
});

/** Looking for it, as its import does. */
const looking = () => lookForBundle(bundle(), "District of Columbia");

const BANDS: SurveyBand[] = [
  { id: "700", label: "700 MHz", loHz: 769e6, hiHz: 775e6, defaultOn: true },
  { id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true },
  { id: "uhf", label: "UHF", loHz: 450e6, hiHz: 470e6, defaultOn: false },
];

const text = (h: string) => h.replace(/<!-- -->/g, "");

/** Connected (idle, a radio) with config `c`, and what the page sends kept in `sent`. */
function connected(c: Config = defaultConfig(), survey?: FromRecorder): ToRecorder[] {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => void sent.push(m);
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: c, version: 1 });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, surveyBands: BANDS, capabilities: [] });
  if (survey) transport.onMessage(survey);
  return sent;
}

test("known only by its licenses: a bundle with no site and licensed frequencies; the bands they're in", () => {
  assert.ok(parseBundle(bundle()) && toFind(bundle()));
  assert.ok(!toFind({ ...bundle(), system: { ...bundle().system, fcc: {} } }), "nothing to find it by");
  assert.deepEqual(bandsHolding([857_987_500], BANDS), ["800"]);
  assert.deepEqual(bandsHolding([160e6], BANDS), ["700", "800"], "none: the usual ones");
});

test("the dialog says what's known, and starting the survey scans its bands from Setup's systems", () => {
  const sent = connected();
  const html = text(renderToString(<ImportDialog bundle={bundle()} onClose={() => {}} />));
  assert.ok(html.includes("Find District of Columbia") && html.includes("WPXT459") && html.includes("4 licensed frequencies"), html);
  assert.ok(html.includes("Start the survey") && !html.includes("Short name"));
  // Its button: the survey, looking for it, on the 800 MHz band only.
  assert.equal(findOnAir(looking()), null);
  const start = sent.findLast((m) => m.type === "surveyStart");
  assert.ok(start && start.type === "surveyStart");
  assert.deepEqual(start.bands, ["800"]);
  // Just its licensed frequencies, with every receiver (its kind is P25, but a license doesn't say which frequency is what).
  assert.deepEqual(start.lookFor?.freqsHz, [855_462_500, 857_987_500, 858_987_500, 859_987_500]);
  assert.deepEqual(start.lookFor?.sites, []);
  assert.equal(snapshot().view, "setup");
  assert.equal(snapshot().setupTab, "systems");
  assert.equal(snapshot().surveyFor?.bundle?.source.id, "fcc_wpxt459");
  lookFor(null);
});

test("you have it already: by its call sign, updated with the licenses it lacks", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [857_987_500], fcc: { "857987500": { callSign: "WPXT459", from: "air" } } })];
  connected(c);
  assert.equal(findExisting(c, bundle()), 0);
  const html = text(renderToString(<ImportDialog bundle={bundle()} onClose={() => {}} />));
  assert.ok(html.includes("You have it already: dcfd") && html.includes("Update dcfd") && html.includes("add what it lacks (licenses)"), html);
});

test("the survey marks its signals, first; a system added from one of its control channels gets its licenses and name", () => {
  connected(defaultConfig(), {
    type: "survey", stage: "done", source: 0, bands: ["800"], message: "", error: null, progress: null, monitor: null, suggest: null,
    candidates: [
      { freqHz: 851_012_000, correctedHz: 851_012_500, band: "800", snrDb: 30, widthHz: 12_500, kind: "control", frames: 10, good: 10, bad: 0, modulation: "", identity: { nac: 0x123 } },
      { freqHz: 857_987_000, correctedHz: 857_987_500, band: "800", snrDb: 25, widthHz: 12_500, kind: "control", frames: 10, good: 10, bad: 0, modulation: "", identity: { nac: 0x443 } },
    ],
  });
  lookFor(looking());
  const html = text(renderToString(<SurveyPanel c={snapshot().config!} />));
  assert.ok(html.includes("Looking for") && html.includes("4 licensed frequencies"), html);
  assert.match(html, /<tr class="[^"]*\bsought\b/);
  assert.ok(html.includes("Licensed to") && html.includes("District of Columbia · "));
  assert.ok(html.indexOf("857.9875") < html.indexOf("851.0125"), "its signal first");
  const sug: SurveySuggestion = {
    type: "p25", bandplan: null, controlChannels: [857_987_500], ppm: null, ppmApply: null, gainDb: null, centerHz: 858_500_000,
    voiceCovered: 0, voiceTotal: 0, spanHz: 0, identity: { nac: 0x443 }, voiceChannels: [858_987_500, 770_000_000],
  };
  applySurvey(0, sug, "new");
  const sys = snapshot().config!.systems.at(-1)!;
  assert.equal(sys.name, "District of Columbia");
  assert.deepEqual(sys.fcc, { "857987500": lic, "858987500": lic });
  // Found: the banner says so (findExisting: its call sign), and offers sharing it.
  const after = text(renderToString(<SurveyPanel c={snapshot().config!} />));
  assert.ok(after.includes(`${sys.shortName}</b> is District of Columbia`) && after.includes("Share it"), after);
  lookFor(null);
});

test("another system's control channel, added while looking: not given its licenses", () => {
  connected();
  lookFor(looking());
  const sug: SurveySuggestion = {
    type: "p25", bandplan: null, controlChannels: [851_012_500], ppm: null, ppmApply: null, gainDb: null, centerHz: 851_500_000,
    voiceCovered: 0, voiceTotal: 0, spanHz: 0, identity: { nac: 0x123 }, voiceChannels: [858_987_500],
  };
  applySurvey(0, sug, "new");
  assert.equal(snapshot().config!.systems.at(-1)!.fcc, undefined);
  lookFor(null);
});
