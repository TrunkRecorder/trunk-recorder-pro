// Setup's source editors as rendered (react-dom/server): a HackRF's stages and a
// USRP's wire format. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, SOURCE_KINDS } from "./config.ts";
import { transport } from "./controller.ts";
import type { Config, FromRecorder } from "./protocol.ts";
import { SourceCard } from "./Setup.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

function setup(c: Config): void {
  const none = { available: true, detail: "", devices: [] };
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service", "drivers"] },
    { type: "radios", radios: { usrp: none, airspy: none, bladerf: none, hackrf: { available: true, detail: "built in", devices: [{ serial: "0000a1", label: "HackRF One · SN 0000a1" }] } } },
  ];
  for (const m of greeting) transport.onMessage(m);
}

test("Setup: a HackRF's editor has its stages and bias-T, and needs no driver", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.hackrf.fresh()];
  setup(c);
  const html = renderToString(<SourceCard c={c} i={0} />);
  const page = text(html);
  assert.match(page, /LNA \(IF\) gain/);
  assert.match(page, /VGA \(baseband\) gain/);
  assert.match(page, /Amp .*\+14 dB — leave off near strong signals: it overloads easily/);
  assert.match(page, /Bias-T/);
  assert.match(page, /HackRF One · SN 0000a1/, "the radio its driver found");
  assert.doesNotMatch(page, /brew install|apt install/, "nothing to install");
  // The LNA offers 0 to 40 in 8 dB steps, the VGA 0 to 62 in 2.
  const lna = html.match(/<select[^>]*aria-label="LNA \(IF\) gain"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.deepEqual([...lna.matchAll(/value="(\d+)"/g)].map((m) => m[1]), ["0", "8", "16", "24", "32", "40"]);
  const vga = html.match(/<select[^>]*aria-label="VGA \(baseband\) gain"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.equal([...vga.matchAll(/value="(\d+)"/g)].length, 32);
});

test("Setup: a USRP's editor offers the wire format", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.usrp.fresh()];
  setup(c);
  const html = renderToString(<SourceCard c={c} i={0} />);
  const sel = html.match(/<select[^>]*aria-label="Wire format"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.deepEqual([...sel.matchAll(/value="([^"]*)"/g)].map((m) => m[1]), ["", "sc16", "sc12", "sc8"]);
  assert.match(text(sel), /Auto — sc16, or sc12 for a B2xx on USB 2\.0 above 8 MSPS sc16 sc12 \(B2xx\) sc8/);
});
