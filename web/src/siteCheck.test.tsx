// Which of a system's sites can be heard here: a sites file (or a directory
// bundle, or a system set up already) looked for by the survey, on a spare
// radio while recording; each site's verdict, and what to do with them.
// `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newDongle, normalizeSystem } from "./config.ts";
import { findOnAir, lookFor, snapshot, transport } from "./controller.ts";
import { asType, ImportDialog, miles, sitesFileBundle } from "./directoryImport.tsx";
import { lookForBundle, lookForMessage, lookForSystem } from "./lookFor.ts";
import type { Config, FromRecorder, Source, SurveyBand, ToRecorder } from "./protocol.ts";
import { SurveyPanel } from "./Survey.tsx";

/** A RadioReference sites export, as it comes (a site a row, its frequencies to the row's end). */
const CSV = [
  "RFSS,Site Dec,Site Hex,Site NAC,Description,County Name,Lat,Lon,Range,Frequencies",
  '1,1,001,212,"Walker Tower (Highland)",Ulster,41.72,-74.0,20,451.175000c,451.775000,452.375000',
  "1,2,002,212,Mt Beacon,Dutchess,41.49,-73.95,20,452.175000c",
  "1,3,003,212,Far Away,Erie,42.88,-78.87,20,453.100000c,453.200000c",
  "",
].join("\r\n");

const BANDS: SurveyBand[] = [{ id: "uhf", label: "UHF", loHz: 450e6, hiHz: 470e6, defaultOn: true }];
const text = (h: string) => h.replace(/<!-- -->/g, "");

/** Connected with config `c` (and phase `phase`, the radios plugged in `serials`), what the page sends kept. */
function connected(c: Config, phase: "idle" | "running" = "idle", serials: string[] = [], survey?: FromRecorder): ToRecorder[] {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => void sent.push(m);
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: c, version: 1 });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, surveyBands: BANDS, capabilities: ["drivers"] });
  transport.onMessage({ type: "devices", devices: serials.map((serial, index) => ({ index, serial, product: "RTL2838UHIDIR", busy: null })) });
  transport.onMessage({ type: "state", phase, error: null, ended: false });
  if (survey) transport.onMessage(survey);
  return sent;
}

test("a sites file: its sites, control channels, site lock and places; not one: null", () => {
  const b = sitesFileBundle(CSV, "trs_sites_7363.csv")!;
  assert.equal(b.system.type, undefined, "a sites file doesn't say");
  assert.equal(b.system.shortName, "rr-7363");
  assert.equal(b.source.directory, "");
  assert.deepEqual(
    b.system.sites.map((x) => [x.name, x.controlChannelsHz, x.voiceChannelsHz, x.expect]),
    [
      ["1 Walker Tower (Highland)", [451_175_000], [451_775_000, 452_375_000], { rfss: 1, site: 1, nac: 0x212 }],
      ["2 Mt Beacon", [452_175_000], [], { rfss: 1, site: 2, nac: 0x212 }],
      ["3 Far Away", [453_100_000, 453_200_000], [], { rfss: 1, site: 3, nac: 0x212 }],
    ],
  );
  assert.deepEqual(b.locations, [[41.72, -74], [41.49, -73.95], [42.88, -78.87]]);
  assert.ok(miles(b.locations![0]!, b.locations![1]!) < 20 && miles(b.locations![0]!, b.locations![2]!) > 200);
  assert.equal(sitesFileBundle("Name,Talkgroup\nx,1", "talkgroups.csv"), null);
  // Its kind chosen: a site lock only for P25 (other kinds' site numbers aren't their air's).
  assert.deepEqual(asType(b, "dmr").system.sites[0].expect, {});
  assert.deepEqual(asType(b, "p25").system.sites[0].expect, { rfss: 1, site: 1, nac: 0x212 });
});

test("its import asks the kind of system, and offers to check the sites on the air first", () => {
  connected(defaultConfig());
  const html = text(renderToString(<ImportDialog bundle={sitesFileBundle(CSV, "trs_sites_7363.csv")!} onClose={() => {}} />));
  assert.ok(html.includes("Import trs_sites_7363.csv") && html.includes("Kind of system") && html.includes("RadioReference sites · 3 sites"), html);
  assert.ok(html.includes("Check which I can hear") && html.includes("Near: a city or ZIP code"), html);
  // Only the ticked ones are looked for; what the recorder is sent leaves out where each came from.
  const l = lookForBundle(asType(sitesFileBundle(CSV, "trs_sites_7363.csv")!, "p25"), "rr-7363", [0, 2]);
  assert.deepEqual(l.sites.map((x) => [x.index, x.freqsHz]), [[0, [451_175_000]], [2, [453_100_000, 453_200_000]]]);
  assert.deepEqual(lookForMessage(l, 5), {
    freqsHz: [],
    sites: [
      { label: "1 Walker Tower (Highland)", freqsHz: [451_175_000], expect: { rfss: 1, site: 1, nac: 0x212 } },
      { label: "3 Far Away", freqsHz: [453_100_000, 453_200_000], expect: { rfss: 1, site: 3, nac: 0x212 } },
    ],
    type: "p25",
    listenS: 5,
  });
});

test("while recording, a spare radio scans for a system's sites; none spare: it says so", () => {
  const c = defaultConfig();
  c.sources = [{ ...newDongle(), serial: "1" } as Source];
  c.systems = [normalizeSystem({ shortName: "onevoice", type: "dmr", sites: [{ name: "a", controlChannelsHz: [452_100_000] }, { name: "b", controlChannelsHz: [], dmrChannelsHz: [452_200_000, 452_300_000] }] })];
  const sent = connected(c, "running", ["1", "2"]);
  assert.equal(findOnAir(lookForSystem(c.systems[0])), null);
  const start = sent.findLast((m) => m.type === "surveyStart");
  assert.ok(start && start.type === "surveyStart");
  assert.equal(start.radio?.type, "rtlsdr");
  assert.equal(start.radio?.type === "rtlsdr" && start.radio.serial, "2", "the radio no source uses");
  // A Capacity Plus site (no control channel): its repeaters.
  assert.deepEqual(start.lookFor?.sites.map((x) => x.freqsHz), [[452_100_000], [452_200_000, 452_300_000]]);
  assert.equal(start.lookFor?.type, "dmr");
  lookFor(null);
  connected(c, "running", ["1"]);
  assert.match(findOnAir(lookForSystem(c.systems[0])) ?? "", /stop recording to scan, or plug in a spare radio/);
  lookFor(null);
});

test("each site's verdict; a system's sites not heard can be turned off, a bundle's heard ones imported", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "onevoice", type: "dmr", sites: [{ name: "a", controlChannelsHz: [452_100_000] }, { name: "b", controlChannelsHz: [452_200_000] }, { name: "c", controlChannelsHz: [452_300_000] }] })];
  const survey: FromRecorder = {
    type: "survey", stage: "done", source: null, radio: "RTL-SDR SN 2", bands: [], message: "", error: null, progress: null, monitor: null, suggest: null, candidates: [],
    lookFor: { freqs: 3, probed: 3, listenS: 60, type: "dmr" },
    sites: [
      { label: "a", verdict: "active", why: "DMR Connect Plus on 452.10000 MHz", freqHz: 452_100_000, quality: "good", snrDb: 24 },
      { label: "b", verdict: "unconfirmed", why: "DMR Tier III on 452.20000 MHz, but this system is DMR Connect Plus", freqHz: 452_200_000, quality: "marginal", snrDb: 9 },
      { label: "c", verdict: "inactive", why: "nothing on 1 frequency", freqHz: null, quality: null, snrDb: null },
    ],
  };
  connected(c, "running", [], survey);
  lookFor(lookForSystem(c.systems[0]));
  const html = text(renderToString(<SurveyPanel c={snapshot().config!} />));
  assert.ok(html.includes("Checking which of <b>onevoice</b>&#x27;s 3 sites can be heard here"), html);
  assert.ok(html.includes("On RTL-SDR SN 2, a spare radio, beside the recording"), html);
  assert.ok(html.includes("Heard") && html.includes("Unconfirmed") && html.includes("Not heard") && html.includes("but this system is DMR Connect Plus"));
  assert.match(html, /<tr class="q-good"><td>a<\/td>/);
  assert.ok(html.includes("2 sites of 3 heard here, 1 not") && html.includes("Turn off the 1 site not heard"), html);
  // From a bundle instead: its heard sites to import.
  lookFor(lookForBundle(asType(sitesFileBundle(CSV, "trs_sites_7363.csv")!, "dmr"), "rr-7363"));
  const b = text(renderToString(<SurveyPanel c={snapshot().config!} />));
  assert.ok(b.includes("Import the 2 sites heard…") && b.includes("The ones heard can be imported"), b);
  lookFor(null);
});
