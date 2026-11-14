// A system's own names for its radios as rows: a unit names file read and
// written by the recorder's own code (through wasm), a radio named as the
// recorder names it, and its calls' talkers named by the recorder's lookup. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { unitNameOf, unitNamesReplace } from "./rules.ts";
import type { CallEntry, CallRecord } from "./protocol.ts";
import { mdcHeard, ownUnitName, readUnitNamesFile, unitIdMatches, unitIdText, unitNamesFileText, withUnitName, type MdcUnits } from "./units.ts";

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

test("an MDC1200 radio's ID in hex, 4 digits, as radios are programmed; any other in decimal; found either way", () => {
  const call = (short_name: string, srcs: [number, string][]): CallEntry => ({ path: `${short_name}/x`, record: { short_name, srcList: srcs.map(([src, signal_system]) => ({ src, signal_system })) } as unknown as CallRecord });
  const mdc: MdcUnits = mdcHeard({}, [call("conv", [[7187, "MDC1200"], [1795, "MDC1200"], [1011270, "FLEETSYNC"]]), call("p25", [[7187, ""]])]);
  assert.deepEqual(mdc, { conv: { 7187: true, 1795: true } } as MdcUnits);
  assert.equal(mdcHeard(mdc, [call("conv", [[7187, "MDC1200"]])]), mdc, "nothing new: the same");
  assert.equal(unitIdText(mdc.conv, 7187), "1C13");
  assert.equal(unitIdText(mdc.conv, 1795), "0703");
  assert.equal(unitIdText(mdc.conv, 1011270), "1011270", "FleetSync stays decimal");
  assert.equal(unitIdText(mdc.p25, 7187), "7187", "the same number on another system");
  for (const f of ["1c13", "0x1c13", "7187", "c1"]) assert.ok(unitIdMatches(mdc.conv, 7187, f), f);
  assert.ok(!unitIdMatches(mdc.p25, 7187, "1c13"), "hex only where it's shown in hex");
});
