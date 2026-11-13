// A system's own names for its radios as rows: a unit names file read and
// written by the recorder's own code (through wasm), a radio named as the
// recorder names it, and its calls' talkers named by the recorder's lookup. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { unitNameOf, unitNamesReplace } from "./rules.ts";
import { ownUnitName, readUnitNamesFile, unitNamesFileText, withUnitName } from "./units.ts";

const BY_HAND = { kind: "edited" as const, at: 1_762_500_000 };

test("a unit names file: unit IDs and patterns, in order; the first of a unit twice; patterns that aren't regular expressions said", () => {
  const r = readUnitNamesFile("# names\n1234,Engine 12\n/^7(\\d{3})$/,Medic $1\n/[/,broken\n1234,Again\n");
  assert.deepEqual(r.unitNames.map((u) => [u.unit, u.name]), [["1234", "Engine 12"], ["/^7(\\d{3})$/", "Medic $1"], ["/[/", "broken"]]);
  assert.deepEqual(r.bad, ["/[/"]);
  assert.deepEqual(readUnitNamesFile(unitNamesFileText(r.unitNames)).unitNames, r.unitNames, "read back the same");
});

test("a radio named by hand: its own row, ahead of the patterns, its origin; cleared, a pattern names it again", () => {
  const rows = unitNamesReplace([], readUnitNamesFile("/^7(\\d{3})$/,Medic $1\n").unitNames, { kind: "file", name: "units.csv", at: 100 });
  assert.equal(unitNameOf(rows, "user", 7042, undefined), "Medic 042");
  const named = withUnitName(rows, 7042, "Bus 14", BY_HAND);
  assert.deepEqual(ownUnitName(named, 7042), { unit: "7042", name: "Bus 14", origin: BY_HAND });
  assert.equal(named[1].origin?.kind, "file", "the pattern's own origin");
  assert.equal(unitNameOf(named, "user", 7042, "HEARD"), "Bus 14", "its own name first");
  assert.equal(unitNameOf(named, "ota", 7042, "HEARD"), "HEARD", "the alias heard first");
  assert.equal(unitNameOf(named, "none", 7042, "HEARD"), undefined);
  assert.equal(unitNameOf(named, "user_only", 5, "HEARD"), undefined);
  assert.equal(unitNameOf(withUnitName(named, 7042, "", BY_HAND), "user", 7042, undefined), "Medic 042");
});
