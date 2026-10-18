// Applying what the survey found: its identity and band plan go into the
// config as they are (the same shapes). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig } from "./config.ts";
import { applySurvey, snapshot, transport } from "./controller.ts";
import type { FromRecorder, SurveySuggestion } from "./protocol.ts";

test("a SmartNet system found by the survey: its site identity and band plan as the config has them", () => {
  const c = defaultConfig();
  c.sources.push({ ...c.sources[0] });
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: [] },
  ];
  for (const m of greeting) transport.onMessage(m);
  const sug: SurveySuggestion = {
    type: "smartnet",
    bandplan: { bandplan: "400_custom", bandplanBaseHz: 489_087_500, bandplanSpacingHz: 25_000, bandplanOffset: 380, bandplanHighHz: 496_612_500 },
    controlChannels: [496_437_500],
    ppm: null,
    ppmApply: null,
    gainDb: null,
    centerHz: 496_000_000,
    voiceCovered: 0,
    voiceTotal: 0,
    spanHz: 0,
    identity: { sysId: 0x4a2 },
    voiceChannels: [],
  };
  applySurvey(0, sug, "new");
  const site = snapshot().config!.systems[0].sites[0];
  assert.deepEqual(site.expect, { sysId: 0x4a2 });
  assert.equal(site.bandplan, "400_custom");
  assert.equal(site.bandplanBaseHz, 489_087_500);
  assert.equal(site.bandplanSpacingHz, 25_000);
  assert.equal(site.bandplanHighHz, 496_612_500);
});
