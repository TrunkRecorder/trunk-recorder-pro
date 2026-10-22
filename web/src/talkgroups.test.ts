// The talkgroup file's Send To column, as Setup and the talkgroup page edit it. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseSendTo, sendToOf, withSendTo } from "./talkgroups.ts";

const file = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n801,321,KingstonDisp,A,Dispatch,Law Dispatch,Kingston PD\n863,35f,KingstonCh.1,A,Fire,Fire Dispatch,Kingston FD\n";

test("a Send To cell reads as the recorder reads it", () => {
  assert.equal(parseSendTo(""), undefined);
  assert.equal(parseSendTo("All"), undefined);
  assert.deepEqual(parseSendTo("none"), []);
  assert.deepEqual(parseSendTo("OpenMHz; Rdio Scanner"), ["openmhz", "rdioscanner"]);
  assert.deepEqual(parseSendTo("rdio-scanner|rdioscanner"), ["rdioscanner"]);
});

test("setting a talkgroup's Send To adds the column, changes only its cell, and reads back", () => {
  const one = withSendTo(file, 801, ["rdioscanner"]);
  assert.equal(one.split("\n")[0], "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Send To");
  assert.deepEqual(sendToOf(one, 801), ["rdioscanner"]);
  assert.equal(sendToOf(one, 863), undefined, "the others: every plugin");
  assert.ok(one.includes("\n863,35f,KingstonCh.1,A,Fire,Fire Dispatch,Kingston FD\n"), "other rows as they were");
  const two = withSendTo(one, 863, []);
  assert.deepEqual(sendToOf(two, 863), [], "none");
  assert.equal(sendToOf(withSendTo(two, 801, undefined), 801), undefined, "back to every plugin");
});

test("a talkgroup not in the file gets a row; every plugin on a file without the column changes nothing", () => {
  const added = withSendTo(file, 2047, ["openmhz"], "All call");
  assert.ok(added.trimEnd().endsWith("2047,7ff,All call,,,,,openmhz"));
  assert.equal(withSendTo(file, 801, undefined), file);
  assert.equal(withSendTo(file, 999, undefined), file);
  assert.deepEqual(sendToOf(withSendTo("", 5, ["openmhz"]), 5), ["openmhz"], "an empty file");
});
