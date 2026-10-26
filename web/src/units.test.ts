// Editing a unit names file in place: setting one radio's name. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { unitName, withUnitName } from "./units.ts";

test("withUnitName: an empty file gets one literal row", () => {
  const out = withUnitName("", 4810187, "Engine 7");
  assert.equal(out, "4810187,Engine 7\n");
  assert.equal(unitName(out, 4810187), "Engine 7");
});

test("withUnitName: an existing literal row is updated in place, others untouched", () => {
  const csv = "7,Old Seven\n8,Engine 8\n";
  const out = withUnitName(csv, 7, "New Seven");
  assert.equal(out, "7,New Seven\n8,Engine 8\n");
});

test("withUnitName: a new id is added ahead of every existing pattern, so it wins first-match", () => {
  const csv = "/^48(\\d+)$/,Fleet $1\n";
  const out = withUnitName(csv, 4810187, "Chief 1");
  assert.equal(out, "4810187,Chief 1\n/^48(\\d+)$/,Fleet $1\n");
  assert.equal(unitName(out, 4810187), "Chief 1", "the literal row wins over the broader pattern below it");
  assert.equal(unitName(out, 4810188), "Fleet 10188", "another id still falls through to the pattern");
});

test("withUnitName: comment lines and blank lines are left alone", () => {
  const csv = "# radios\n\n7,Engine 7\n";
  const out = withUnitName(csv, 7, "Engine Seven");
  assert.equal(out, "# radios\n\n7,Engine Seven\n");
});

test("withUnitName: a name with a comma or a quote is quoted, and read back whole", () => {
  const out = withUnitName("", 7, 'Engine 5, Station "2"');
  assert.equal(out, '7,"Engine 5, Station ""2"""\n');
  assert.equal(unitName(out, 7), 'Engine 5, Station "2"');
});
