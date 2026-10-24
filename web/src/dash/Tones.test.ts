// The tones log's matching of heard tones to configured ones. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ToneRow } from "../protocol.ts";
import { signalId, talkgroupMatches, toneMatches, tonesText } from "./Tones.tsx";

const row = (r: Partial<ToneRow>): ToneRow => ({
  tg: 20341, alphaTag: "Norwalk EMS 1", pattern: "two_tone", tones: [781.25, 875], lengths: [0.98, 2.98], digits: "", format: "Quick Call II",
  count: 3, first: 0, last: 0, radios: [], frame: true, ids: ["QC2 126/128"], ...r,
});

test("talkgroup lists: numbers, ranges, patterns", () => {
  assert.ok(talkgroupMatches("20341", 20341));
  assert.ok(talkgroupMatches("20000-20999", 20341) && !talkgroupMatches("20000-20340", 20341));
  assert.ok(talkgroupMatches("203*", 20341) && talkgroupMatches("2034?", 20341) && !talkgroupMatches("2035?", 20341));
});

test("a configured page matches what was heard within Phase 2's grid", () => {
  // The chart's QC2 126/128 (788.5, 879.0) against the tone frames' 781.25 / 875.
  const page = { id: "NORWALK_EMS", pattern: "two_tone" as const, tonesHz: [788.5, 879.0], talkgroups: ["20341"] };
  assert.ok(toneMatches(page, row({})));
  assert.ok(!toneMatches(page, row({ tg: 20342 })), "another talkgroup");
  assert.ok(!toneMatches(page, row({ tones: [875, 781.25] })), "B then A is another page");
  assert.ok(toneMatches(page, row({ frame: false })), "from the audio: within 1.5 %");
  assert.ok(!toneMatches({ ...page, tonesHz: [760, 879] }, row({ frame: false, tones: [781.25, 875] })), "A 2.8 % off in the audio");
  assert.ok(toneMatches({ ...page, tonesHz: [760, 879], extraTolerancePct: 1.5 }, row({ frame: false, tones: [781.25, 875] })), "within 1.5 % + 1.5 %");
  assert.ok(toneMatches({ id: "X", pattern: "digits", digits: "12#" }, row({ pattern: "digits", tones: [], digits: "12#" })));
});

test("tones as text", () => {
  assert.equal(tonesText(row({})), "781.25 → 875 Hz");
  assert.equal(tonesText(row({ pattern: "hi_low", tones: [812.5, 1500] })), "812.5 / 1500 Hz");
  assert.equal(tonesText(row({ pattern: "digits", digits: "12#" })), "12#");
});

test("patterns play as their tones, at their lengths", async () => {
  const { schedule, keyHz } = await import("../tonePlayer.ts");
  assert.deepEqual(schedule({ pattern: "two_tone", tones: [781.25, 875], lengths: [0.98, 2.98] }), [{ hz: [781.25], s: 0.98 }, { hz: [875], s: 2.98 }]);
  assert.deepEqual(schedule({ pattern: "two_tone", tones: [788.5, 879], lengths: [] }).map((x) => x.s), [1, 3], "a configured page with no lengths: Quick Call II's");
  assert.equal(schedule({ pattern: "long", tones: [812.5], lengths: [7.96] })[0].s, 4, "long tones are cut short");
  assert.deepEqual(keyHz("5", false), [770, 1336]);
  assert.deepEqual(keyHz("*", true), [820, 1052]);
  assert.deepEqual(schedule({ pattern: "digits", tones: [], lengths: [], digits: "12" }).map((x) => x.hz), [[697, 1209], [], [697, 1336], []]);
});

test("Morse: text, speed and keying", async () => {
  const { schedule, morseSteps } = await import("../tonePlayer.ts");
  const r = row({ pattern: "morse", tones: [990], lengths: [0.05], digits: "WRV713" });
  assert.equal(tonesText(r), "WRV713 (990 Hz)");
  assert.ok(toneMatches({ id: "PW_ID", pattern: "morse", digits: "wrv713" }, r));
  // E T: dot, letter gap, dash.
  const s = (x: { s: number }[]) => x.map((st) => Math.round(st.s * 1000));
  assert.deepEqual(s(morseSteps("ET", 990, 0.05)), [50, 150, 150]);
  assert.deepEqual(s(morseSteps("E E", 990, 0.05)), [50, 350, 50], "a word's gap is 7 dots");
  assert.equal(schedule({ pattern: "morse", tones: [990], lengths: [0.05], digits: "E" })[0].s, 0.05);
});

test("signal ids", () => {
  assert.equal(signalId("Norwalk EMS 1 page"), "NORWALK_EMS_1_PAGE");
  assert.equal(signalId("  csp-fire!! "), "CSP_FIRE");
});

test("a typed-in tone's note names the paging tones its window takes in", async () => {
  const { toleranceNote } = await import("../ToneSpecs.tsx");
  const note = (extra?: number) => toleranceNote({ pattern: "two_tone", tonesHz: [313.0, 1642.0], ...(extra ? { extraTolerancePct: extra } : {}) });
  assert.ok(note().includes("313 ± 4.7, 1642 ± 24.6 Hz"), note());
  assert.ok(!note().includes("would match them too"), "±1.5 % stays clear of QC2's neighbours");
  assert.ok(note().includes("can't be told from 304.7 Hz (QC2 139)"), "a tone frame's 31.25 Hz step takes in neighbours below 1.1 kHz");
  assert.ok(note(1.5).includes("also takes in 304.7 Hz (QC2 139), 321.7 Hz (QC2 140)"), note(1.5));
});
