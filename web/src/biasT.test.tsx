// The bias-T switch where a radio is picked for a scan: the setup guide's
// radio cards, and Find my system's spare radio. An Airspy or bladeRF powers
// an LNA through the antenna cable; the recorder sets it when it opens the
// radio, so a scan on a radio that isn't a source yet needs the switch here.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, detectRadios, hasBiasT } from "./config.ts";
import { snapshot, startSurvey, transport } from "./controller.ts";
import { RadiosStep } from "./Onboarding.tsx";
import type { Radios, ToRecorder } from "./protocol.ts";
import { SurveyPanel } from "./Survey.tsx";

const none = { available: false, detail: "", devices: null };
const RADIOS: Radios = { usrp: none, bladerf: none, airspy: { available: true, detail: "", devices: [{ serial: "735863DC2F650AE3", label: "Airspy R2 735863DC2F650AE3" }] } };
const NO_RADIOS: Radios = { usrp: none, bladerf: none, airspy: { available: true, detail: "", devices: [] } };

/** Connected as the desktop app, with the recorder in `phase`, `radios` found by their drivers and `serials` of dongles plugged in; what the page sends kept. */
function connected(phase: "idle" | "running", radios: Radios | null, serials: string[] = []): ToRecorder[] {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => void sent.push(m);
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: defaultConfig(), version: 1 });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, surveyBands: [{ id: "800", label: "800 MHz", loHz: 851e6, hiHz: 869e6, defaultOn: true }], capabilities: ["drivers"] });
  transport.onMessage({ type: "devices", devices: serials.map((serial, index) => ({ index, serial, product: "RTL2838UHIDIR", busy: null })) });
  if (radios) transport.onMessage({ type: "radios", radios });
  transport.onMessage({ type: "state", phase, error: null, ended: false });
  transport.onMessage({ type: "survey", stage: "idle" });
  return sent;
}

test("an Airspy and a bladeRF have a bias-T; the others don't", () => {
  const airspy = detectRadios([], RADIOS)[0].source;
  assert.equal(airspy.type, "airspy");
  assert.ok(hasBiasT(airspy));
  assert.ok(!hasBiasT(defaultConfig().sources[0]), "the default source is an RTL-SDR");
});

test("the setup guide's radio cards offer bias-T on an Airspy, not on a dongle", () => {
  connected("idle", RADIOS, ["00000001"]);
  const html = renderToString(<RadiosStep s={snapshot()} c={snapshot().config!} onNext={() => {}} onBack={() => {}} />);
  assert.equal((html.match(/Bias-T/g) ?? []).length, 1, html);
  assert.ok(html.includes("powers an LNA via the antenna cable"));
  connected("idle", NO_RADIOS, ["00000001"]);
  const dongle = renderToString(<RadiosStep s={snapshot()} c={snapshot().config!} onNext={() => {}} onBack={() => {}} />);
  assert.ok(!dongle.includes("Bias-T"), dongle);
});

test("Find my system offers bias-T when the scan's radio is a spare Airspy", () => {
  connected("running", RADIOS);
  const html = renderToString(<SurveyPanel c={snapshot().config!} />);
  assert.ok(html.includes("No source uses it"), "the spare is the radio while recording");
  assert.ok(html.includes("Bias-T"), html);
  connected("idle", RADIOS);
  const idle = renderToString(<SurveyPanel c={snapshot().config!} />);
  assert.ok(!idle.includes("Bias-T"), "source 1, an RTL-SDR, is the radio when idle");
});

test("a scan on a spare radio sends its bias-T to the recorder", () => {
  const sent = connected("running", RADIOS);
  const airspy = detectRadios([], RADIOS)[0].source;
  if (!hasBiasT(airspy)) throw new Error("an Airspy has a bias-T");
  startSurvey({ ...airspy, biasTee: true }, ["800"], false);
  const m = sent.findLast((x) => x.type === "surveyStart");
  assert.ok(m && m.type === "surveyStart" && m.radio && m.radio.type === "airspy" && m.radio.biasTee === true, JSON.stringify(m));
});
