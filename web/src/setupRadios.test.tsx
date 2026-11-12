// Setup's source editors as rendered (react-dom/server): a HackRF's stages and a
// USRP's wire format. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { gainLine } from "./dash/Rf.tsx";
import { renderToString } from "react-dom/server";
import { defaultConfig, SOURCE_KINDS } from "./config.ts";
import { transport } from "./controller.ts";
import type { Config, EngineStatus, FromRecorder } from "./protocol.ts";
import { SourceCard } from "./Setup.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

function setup(c: Config): void {
  const none = { available: true, detail: "", devices: [] };
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service", "drivers"] },
    { type: "radios", radios: { usrp: none, airspy: none, hydrasdr: none, bladerf: none, hackrf: { available: true, detail: "built in", devices: [{ serial: "0000a1", label: "HackRF One · SN 0000a1" }] } } },
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

test("Setup's AutoTune: the ppm to set is measured against the radio's as running, so setting it twice doesn't move it twice", () => {
  const rtl = (serial: string, ppm: number) => ({ ...SOURCE_KINDS.rtlsdr.fresh(), serial, ppm, autoTune: true }) as Config["sources"][number];
  const c = defaultConfig();
  c.sources = [rtl("200", 2), rtl("91", 0)];
  setup(c);
  // Recording: SN 200 measures +3.6 ppm against its 2, so −2 removes it; SN 91 nothing yet.
  const status = (settings: Config["sources"]) =>
    transport.onMessage({
      type: "status",
      status: { systems: [], recording: 0 } as unknown as EngineStatus,
      sources: settings.map((s, index) => ({ index, label: `RTL-SDR SN ${"serial" in s ? s.serial : ""}`, centerHz: 851e6, rateHz: 2.4e6, rateMeasured: 2.4e6, dropped: 0, errors: 0, lastError: null, ended: false, settings: s, ...(index === 0 ? { errorPpm: 3.6, ppmApply: -2, tunePpm: 3.6 } : {}) })),
      load: 0,
      calls: [],
    });
  transport.onMessage({ type: "state", phase: "running", error: null, ended: false, restartNeeded: false });
  status(c.sources);
  assert.match(text(renderToString(<SourceCard c={c} i={0} />)), /Measured error: \+3\.60 ppm · corrected by \+3\.60 ppm · Set correction to -2 ppm/);
  // Set: the radio still runs at 2 and measures the same, and nothing more is offered — it applies at the next Start.
  const set = { ...c, sources: [rtl("200", -2), rtl("91", 0)] };
  let page = text(renderToString(<SourceCard c={set} i={0} />));
  assert.match(page, /correction set to -2 ppm, from the next Start/);
  assert.doesNotMatch(page, /Set correction to/);
  // Sources moved in Setup since Start: each card shows its own radio's measurement, not the one at its place.
  const moved = { ...c, sources: [rtl("91", 0), rtl("200", 2)] };
  assert.doesNotMatch(text(renderToString(<SourceCard c={moved} i={0} />)), /Measured error/);
  page = text(renderToString(<SourceCard c={moved} i={1} />));
  assert.match(page, /Set correction to -2 ppm/);
});

test("A radio's gain says what it's set to when that isn't what Setup asks", () => {
  const rtl = { type: "rtlsdr" as const, serial: "", centerHz: 851_000_000, rateHz: 2_400_000, gainDb: 34, agc: false, ppm: 0, bufLen: 0, buffers: 0 };
  assert.equal(gainLine(rtl, "33.8 dB"), "34 dB · set to 33.8 dB");
  assert.equal(gainLine(rtl, null), "34 dB", "the same: said once");
});
