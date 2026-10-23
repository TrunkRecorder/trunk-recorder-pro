// Setup's FCC licenses section: shown only once a frequency has a license
// (Identify, or the air, fills it). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newConventional, normalizeSystem } from "./config.ts";
import { transport } from "./controller.ts";
import type { Config } from "./protocol.ts";
import { ConventionalPanel, SystemCard } from "./Setup.tsx";

function connected(c: Config) {
  for (const m of [{ type: "config", config: c, version: 1 }, { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service"] }] as const) transport.onMessage(m as never);
}

test("a system's FCC licenses section: none until a frequency has a license", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [857_987_500] })];
  connected(c);
  assert.ok(!renderToString(<SystemCard c={c} i={0} />).includes("<h4>FCC licenses</h4>"));
  c.systems[0].fcc = { "857987500": { callSign: "WPXT459", from: "air" } };
  connected(c);
  const html = renderToString(<SystemCard c={c} i={0} />);
  assert.ok(html.includes("<h4>FCC licenses</h4>") && html.includes("WPXT459"));
});

test("a conventional system's too", () => {
  const c = defaultConfig();
  c.conventional.push(newConventional(c, { shortName: "town", channels: [{ freqHz: 154_325_000, mode: "fm", name: "", enabled: true }] }));
  connected(c);
  assert.ok(!renderToString(<ConventionalPanel c={c} k={0} />).includes("<h4>FCC licenses</h4>"));
  c.conventional[0].fcc = { "154325000": { callSign: "KAAA999", name: "TOWN", from: "lookup" } };
  connected(c);
  assert.ok(renderToString(<ConventionalPanel c={c} k={0} />).includes("<h4>FCC licenses</h4>"));
});
