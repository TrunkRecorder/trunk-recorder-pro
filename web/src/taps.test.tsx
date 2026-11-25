// The Signal taps table as rendered (Setup → Recording → Advanced): a row per
// tap saved to files, and a read-only one per tap a plugin takes. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem } from "./config.ts";
import { transport } from "./controller.ts";
import { signalTaps } from "./rules.ts";
import { SignalTaps } from "./Taps.tsx";
import type { PluginInfo } from "./protocol.ts";

const html = (n: React.ReactElement) => renderToString(n).replace(/<!-- -->/g, "");

const plugin = (id: string, subscribe: string[]): PluginInfo => ({
  id, path: `/plugins/${id}/${id}`, custom: false, problem: null,
  manifest: { id, name: id, version: "1.0.0", description: "", api: 1, subscribe, audio_formats: [] },
  runtime: { state: "off", message: "", ok: 0, skipped: 0, failed: 0, lastFailure: "", log: [] },
});

test("Signal taps: the list is the recorder's", () => {
  assert.deepEqual(signalTaps().map((t) => [t.id, t.carries]), [["fm.discriminator", "samples"], ["voice.air", "frames"], ["voice.frames", "frames"], ["voice.errors", "notes"]]);
});

test("Signal taps: saved rows can be edited; a plugin's are automatic", () => {
  const c = defaultConfig();
  c.systems.push({ ...newSystem(c, { shortName: "east" }), id: 1 }, { ...newSystem(c, { shortName: "west" }), id: 2 });
  transport.onMessage({ type: "config", config: c, version: 1 });
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [] });
  let page = html(<SignalTaps />);
  assert.match(page, />Signal taps</);
  assert.match(page, />off</);
  assert.match(page, />Add tap</);
  assert.ok(!page.includes("<table"), "nothing on: no table");

  c.recording.taps = [{ tap: "fm.discriminator", systems: [2] }, { tap: "voice.errors" }, { tap: "voice.frames" }];
  c.plugins = { pager: { enabled: true, settings: {} }, idle: { enabled: false, settings: {} } };
  transport.onMessage({ type: "config", config: c, version: 2 });
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("pager", ["call.concluded", "tap.voice.air"]), plugin("idle", ["tap.fm.discriminator"])] });
  page = html(<SignalTaps />);
  assert.match(page, />4 on</);
  // The first row: west alone; the second: every system.
  assert.match(page, /aria-label="Systems tap 1 is taken from".*?aria-pressed="false"[^>]*>All<.*?aria-pressed="false"[^>]*>east<.*?aria-pressed="true"[^>]*>west</);
  assert.match(page, /aria-label="Systems tap 2 is taken from".*?aria-pressed="true"[^>]*>All</);
  assert.match(page, /\.wav \+ \.jsonl/);
  // The vocoded frames (what "Save vocoder frames" was): MimoSDR's file beside the sidecar.
  assert.match(page, /\.sdr \+ \.jsonl/);
  assert.equal((page.match(/>Remove</g) ?? []).length, 3);
  // The plugin that's on: its tap, with no controls. The one that's off: nothing.
  assert.match(page, /<tr class="auto"><td>Voice frames off the air.*?>pager<.*?>Automatic</);
  assert.equal((page.match(/class="auto"/g) ?? []).length, 1);
});
