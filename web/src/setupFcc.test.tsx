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
import { setCardOpen } from "./SetupLayout.tsx";

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

test("many licenses fold to a line (count and call signs), and open when asked, remembered as cards are", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "ipsc", type: "p25", controlChannelsHz: [853_475_000] })];
  // A few: open, the table there.
  c.systems[0].fcc = { "853475000": { callSign: "WQEE678", from: "air" } };
  connected(c);
  let html = renderToString(<SystemCard c={c} i={0} />);
  assert.ok(html.includes('class="set-group-toggle" aria-expanded="true"') && html.includes("WQEE678"), "a few: open");
  // A statewide system's dozens: folded, saying what's in it, its table hidden.
  c.systems[0].fcc = Object.fromEntries(Array.from({ length: 20 }, (_, n) => [String(851_000_000 + n * 12_500), { callSign: `WQEE${600 + (n % 5)}`, from: "lookup" as const }]));
  connected(c);
  html = renderToString(<SystemCard c={c} i={0} />);
  assert.ok(html.includes('aria-expanded="false"'), "many: folded");
  assert.match(html, /20 frequencies, 5 call signs: WQEE600, WQEE601, WQEE602, WQEE603…/);
  assert.match(html, /class="set-group-body cols-1"[^>]*hidden=""/);
  // Opened: kept by its key.
  setCardOpen("fcc:sys:ipsc", true);
  html = renderToString(<SystemCard c={c} i={0} />);
  assert.ok(html.includes('class="set-group-toggle" aria-expanded="true"') && !/20 frequencies/.test(html));
  setCardOpen("fcc:sys:ipsc", false);
});
