// Advice for the sources where they're set: a line on a source's card
// (what to set, and Use) and above them what it all does (Apply all). Use
// puts a source back on auto when auto places it where the advice says;
// what's heard weighs it, each frequency by its calls. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem, sourceAdvice } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import type { Config, FromRecorder, RadioResult, ToRecorder } from "./protocol.ts";
import { SourceCard } from "./Setup.tsx";
import { AdviceNote, heardOf, SourcesAdvice, takeAdvice } from "./sourceAdvice.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

/** A P25 site and one RTL-SDR whose centre was typed in too low for its voice channels. */
function tooLow(): Config {
  const c = defaultConfig();
  c.sources = [{ type: "rtlsdr", serial: "", centerHz: 849_500_000, rateHz: 2_400_000, gainDb: 25, agc: false, ppm: 0, bufLen: 0, buffers: 0 }];
  c.systems.push(newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_012_500] }));
  c.systems[0].sites[0].voiceChannelsHz = [851_512_500, 852_012_500];
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: [] },
  ];
  for (const m of greeting) transport.onMessage(m);
  return c;
}

/** Sent to the recorder while `f` runs (and its edits settle). */
async function sent(f: () => void): Promise<ToRecorder[]> {
  const out: ToRecorder[] = [];
  const send = transport.send.bind(transport);
  transport.send = (m) => void out.push(m);
  try {
    f();
    await new Promise((r) => setTimeout(r, 400));
  } finally {
    transport.send = send;
  }
  return out;
}

test("Setup: a source's card says the suggested centre and rate, and the sources' banner what they do", () => {
  const c = tooLow();
  const a = sourceAdvice(c, [undefined]);
  assert.ok(a);
  const card = text(renderToString(<SourceCard c={c} i={0} advice={a} />));
  assert.match(card, /Suggested: auto \(85\d\.\d{4} MHz\)( at [\d.]+ MSPS)? · Use/, card);
  const banner = text(renderToString(<SourcesAdvice advice={a} />));
  assert.match(banner, /With the suggested settings, the sources would reach what a system needs that no source reaches now and reach more of the channels the systems use\. .* Apply all/, banner);
  assert.doesNotMatch(text(renderToString(<SourceCard c={c} i={0} advice={null} />)), /Suggested:/, "no advice: no line");
});

test("Use: back on auto when auto places it where the advice says; the rate written in", async () => {
  const c = tooLow();
  const a = sourceAdvice(c, [undefined])!;
  assert.ok(a.sources[0].auto);
  const out = await sent(() => takeAdvice(a, 0));
  const saved = out.find((m): m is Extract<ToRecorder, { type: "setConfig" }> => m.type === "setConfig");
  assert.ok(saved, JSON.stringify(out));
  assert.deepEqual([saved.config.sources[0].centerHz, saved.config.sources[0].rateHz], [0, a.sources[0].rateHz]);
  assert.equal(snapshot().config!.sources[0].centerHz, 0);
});

test("what's heard: each frequency by its calls, decoded and out of band", () => {
  const freqs = { type: "radioResult", id: 1, what: "freqs", rows: [{ freqHz: 851_512_500, calls: 40 }], outOfBand: [{ freqHz: 855_012_500, calls: 7 }, { freqHz: 851_512_500, calls: 2 }] } as unknown as RadioResult;
  assert.deepEqual(heardOf([["dcfd", freqs], ["quiet", null]]), { dcfd: [{ hz: 851_512_500, weight: 42 }, { hz: 855_012_500, weight: 7 }] });
});

test("RF page: the note that Setup has suggested settings, what they'd do, and the way there", () => {
  const a = sourceAdvice(tooLow(), [undefined]);
  assert.match(text(renderToString(<AdviceNote advice={a} />)), /With the settings Setup suggests, the sources would reach what a system needs .* Review in Setup/);
  assert.equal(renderToString(<AdviceNote advice={null} />), "");
});
