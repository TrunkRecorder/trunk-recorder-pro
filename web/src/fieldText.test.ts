// What the setup's inputs show and read back (fieldText.ts). `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { asTyped, cleanShortName, decimal, finishDraft, verbatim, gain, hex, lcnTable, mhz, mhzList, minutes, msps, numberList, talkgroupList, talkgroupProblem, textList, type TextCodec } from "./fieldText.ts";

/** Each value shown reads back as itself. */
function roundTrips<T>(codec: TextCodec<T>, values: T[]) {
  for (const v of values) assert.deepEqual(codec.parse(codec.format(v)), v, `${JSON.stringify(v)} → "${codec.format(v)}"`);
}

test("decimal: blank and part-typed text read as nothing, not 0", () => {
  for (const t of ["", " ", "-", ".", "abc"]) assert.equal(decimal.parse(t), undefined, JSON.stringify(t));
  assert.equal(decimal.parse("12"), 12);
  assert.equal(decimal.parse("-5"), -5);
  assert.equal(decimal.parse("0.5"), 0.5);
  assert.equal(decimal.parse("0,5"), 0.5);
  assert.equal(decimal.parse("1."), 1);
  roundTrips(decimal, [undefined, 0, 8, -20, 0.25]);
});

test("gain: shown to a tenth, kept as typed", () => {
  assert.equal(gain.format(33.799999), "33.8");
  assert.equal(gain.parse("33.75"), 33.75);
  assert.equal(gain.format(undefined), "");
});

test("hex: with or without 0x; not hex is nothing", () => {
  assert.equal(hex.parse("293"), 0x293);
  assert.equal(hex.parse("0xbee00"), 0xbee00);
  assert.equal(hex.parse("29g"), undefined);
  assert.equal(hex.parse(""), undefined);
  assert.equal(hex.format(0xbee00), "BEE00");
  roundTrips(hex, [undefined, 0, 0x293, 0xbee00]);
});

test("mhz and mhzList", () => {
  assert.equal(mhz.parse("851.0125"), 851_012_500);
  assert.equal(mhz.parse(""), 0);
  assert.equal(mhz.format(0), "");
  assert.deepEqual(mhzList.parse("857.9875, 858.9875"), [857_987_500, 858_987_500]);
  assert.deepEqual(mhzList.parse("857.9875,"), [857_987_500]);
  roundTrips(mhzList, [[], [851_012_500, 851_037_500]]);
});

test("msps: not above 0 is 0, for the field to ignore", () => {
  assert.equal(msps.parse("2.4"), 2_400_000);
  assert.equal(msps.parse("2,048"), 2_048_000);
  for (const t of ["", "0", "-1", "x"]) assert.equal(msps.parse(t), 0, JSON.stringify(t));
  roundTrips(msps, [2_400_000, 3_200_000]);
});

test("lcnTable", () => {
  assert.deepEqual(lcnTable.parse("101=452.275, 102=452.3"), { "101": 452_275_000, "102": 452_300_000 });
  assert.deepEqual(lcnTable.parse("101=452275000"), { "101": 452_275_000 });
  assert.deepEqual(lcnTable.parse("101="), {});
  roundTrips(lcnTable, [{}, { "101": 452_275_000, "20": 451_012_500 }]);
});

test("minutes: kept in seconds", () => {
  assert.equal(minutes.parse("1.5"), 90);
  assert.equal(minutes.parse(""), 0);
  assert.equal(minutes.parse("-1"), 0);
  assert.equal(minutes.format(0), "");
  roundTrips(minutes, [0, 60, 90, 600]);
});

test("lists: a trailing separator or space adds nothing", () => {
  assert.deepEqual(textList(/,/).parse("New York, "), ["New York"]);
  assert.deepEqual(numberList(/,/).parse("1, 2,"), [1, 2]);
  assert.ok(Number.isNaN(numberList(/,/).parse("1, x")[1]));
  assert.deepEqual(talkgroupList.parse("1001 1000-1099; 507*,"), ["1001", "1000-1099", "507*"]);
  assert.deepEqual(talkgroupList.parse("1000 - 1099 2000 -2099, 5"), ["1000-1099", "2000-2099", "5"], "a range with spaces is one");
  assert.equal(talkgroupProblem(["1001", "1000-1099", "1099 - 1000", "507*", "12?45", 8001]), null);
  for (const bad of ["abc", "10-", "-5", "10-20-30", "5a*", "99999999999", "1000-x"]) assert.match(talkgroupProblem(["1", bad]) ?? "", new RegExp(`"${bad.replace(/[*?]/g, "\\$&")}" isn't a talkgroup`), bad);
  roundTrips(talkgroupList, [[], ["1001", "507*"]]);
});

test("asTyped and cleanShortName", () => {
  assert.equal(asTyped.parse(" NAC 293 "), "NAC 293");
  assert.equal(cleanShortName("county fire/2"), "countyfire2");
  assert.equal(cleanShortName("dcfd_p25.v-2"), "dcfd_p25.v-2");
});

test("a field typed in is kept once, when it's done", () => {
  assert.equal(finishDraft(null, "FIREGROUND 04", asTyped), null, "not being edited: nothing");
  assert.equal(finishDraft("FIREGROUND 04", "FIREGROUND 04", asTyped), null, "focused and left as it was: no save");
  assert.deepEqual(finishDraft("FIRE4A ", "FIREGROUND 04", asTyped), { value: "FIRE4A" }, "the word typed, tidied");
  assert.deepEqual(finishDraft("", 12, decimal), { value: undefined }, "cleared");
  assert.equal(finishDraft("12", 12, decimal), null);
  assert.deepEqual(finishDraft(" - ", "", verbatim), { value: " - " }, "verbatim keeps spaces");
});
