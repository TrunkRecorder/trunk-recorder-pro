// The Tones card's and the signal editor's use of the recorder's rules
// (rules.ts: the wasm build of trunk-app's): a row made a signal, what a
// pattern plays as, what's wrong with a signal and what it takes in. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ToneRow } from "../protocol.ts";
import { signalId, signalNote, signalProblem, toneSteps } from "../rules.ts";
import { rowSignal } from "./Tones.tsx";

const row = (r: Partial<ToneRow>): ToneRow => ({
  tg: 20341, alphaTag: "Norwalk EMS 1", pattern: "two_tone", tones: [781.25, 875], lengths: [0.98, 2.98], digits: "", knox: false, cycles: 0, format: "Quick Call II",
  text: "two-tone 781.25 → 875 Hz, 0.98 + 2.98 s (Quick Call II)", count: 3, first: 0, last: 0, radios: [], frame: true, ids: ["QC2 126/128"], signals: [], ...r,
});

test("Add makes a signal of what was heard, on its talkgroup, that the recorder takes", () => {
  const d = rowSignal(row({}));
  assert.deepEqual(d, { id: "NORWALK_EMS_1", pattern: "two_tone", tonesHz: [781.25, 875], lengthsS: [0.98, 2.98], talkgroups: ["20341"] });
  assert.equal(signalProblem(d), null);
  const knox = rowSignal(row({ alphaTag: "", pattern: "digits", tones: [], lengths: [0.1, 0.1], digits: "12", knox: true }));
  assert.equal(knox.id, "DIGITS", "no alpha tag: named by its pattern");
  assert.equal(knox.knox, true);
  assert.equal(knox.digits, "12", "plain digits: KNOX is the flag");
  assert.equal(signalProblem(knox), null);
});

test("a signal's problems and note are the recorder's", () => {
  assert.match(signalProblem({ id: "", pattern: "two_tone", tonesHz: [788.5, 879] }) ?? "", /needs an id/);
  assert.match(signalProblem({ id: "X", pattern: "two_tone", tonesHz: [788.5] }) ?? "", /two tones/);
  assert.match(signalProblem({ id: "X", pattern: "digits" }) ?? "", /digits/);
  assert.equal(signalProblem({ id: "X", pattern: "beep", tonesHz: [1000] }), null);
  assert.ok(signalNote({ id: "X", pattern: "two_tone", tonesHz: [313.0, 1642.0] }).length > 0);
  assert.equal(signalId("Norwalk EMS 1 page"), "NORWALK_EMS_1_PAGE");
});

test("patterns play as the recorder says they sound", () => {
  assert.deepEqual(toneSteps({ pattern: "two_tone", tones: [781.25, 875], lengths: [0.98, 2.98] }).map((x) => x.hz), [[781.25], [875]]);
  const keys = toneSteps({ pattern: "digits", tones: [], lengths: [], digits: "5", knox: false });
  assert.deepEqual(keys[0].hz, [770, 1336], "DTMF 5");
  assert.notDeepEqual(toneSteps({ pattern: "digits", tones: [], lengths: [], digits: "5", knox: true })[0].hz, keys[0].hz, "KNOX's pad is another");
  assert.ok(toneSteps({ pattern: "morse", tones: [990], lengths: [0.05], digits: "E" }).length > 0);
});
