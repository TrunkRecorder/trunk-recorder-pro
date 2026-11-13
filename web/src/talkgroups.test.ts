// A talkgroup table pasted from RadioReference's web page, in each shape the page's copy takes.
// The talkgroups here are made up; only the layout is the page's. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseRadioReferencePaste } from "./talkgroups.ts";

type Row = Pick<ReturnType<typeof parseRadioReferencePaste>[number], "number" | "mode" | "alphaTag" | "description" | "tag" | "category">;
const parse = (lines: string[]): Row[] =>
  parseRadioReferencePaste(lines.join("\n")).map(({ number, mode, alphaTag, description, tag, category }) => ({ number, mode, alphaTag, description, tag, category }));
const tg = (number: number, mode: string, alphaTag: string, tag: string, category: string, description = `${alphaTag} desc`): Row =>
  ({ number, mode, alphaTag, description, tag, category });

test("a row with the Category column and the date it was updated: the date isn't a field", () => {
  assert.deepEqual(parse(["1201\t4b1\tT\tLkv PD Disp\tLkv PD Disp desc\tLaw Dispatch\tLakeview\t2020-01-01"]), [
    tg(1201, "T", "Lkv PD Disp", "Law Dispatch", "Lakeview"),
  ]);
});

test("a live feed's ONLINE badge stuck to its number (4407Online)", () => {
  assert.deepEqual(parse(["1203Online\t4b3\tT\tLkv FD Disp\tLkv FD Disp desc\tFire Dispatch\tLakeview\t2020-01-01"]), [
    tg(1203, "T", "Lkv FD Disp", "Fire Dispatch", "Lakeview"),
  ]);
});

test("ENC breaking a row after its mode: the rest is on the next line, after the badge; Enc fully encrypted, enc partly", () => {
  const paste = [
    "1205\t4b5\tT",
    "Enc\tLkv PD Tac\tLkv PD Tac desc\tLaw Tac\tLakeview\t2020-01-01",
    "1207\t4b7\tT",
    "enc\tLkv PD Det\tLkv PD Det desc\tLaw Talk\tLakeview\t2020-01-01",
  ];
  assert.deepEqual(parse(paste), [tg(1205, "TE", "Lkv PD Tac", "Law Tac", "Lakeview"), tg(1207, "Te", "Lkv PD Det", "Law Talk", "Lakeview")]);
});

test("both badges: ONLINE stuck to the number, and ENC breaking the row", () => {
  assert.deepEqual(parse(["1209Online\t4b9\tT", "Enc\tLkv EMS Tac\tLkv EMS Tac desc\tEMS-Tac\tLakeview\t2020-01-01"]), [
    tg(1209, "TE", "Lkv EMS Tac", "EMS-Tac", "Lakeview"),
  ]);
});

test("a row stopping at its mode with no ENC line after it, mid-paste and at the end: kept as it is", () => {
  const paste = ["1211\t4bb\tT", "1213\t4bd\tT\tLkv DPW\tLkv DPW desc\tPublic Works\tLakeview\t2020-01-01", "1215\t4bf\tD"];
  assert.deepEqual(parse(paste), [
    tg(1211, "T", "", "", "", ""),
    tg(1213, "T", "Lkv DPW", "Public Works", "Lakeview"),
    tg(1215, "D", "", "", "", ""),
  ]);
});

test("a mixed paste: the header skipped, every row read in order", () => {
  const paste = [
    "DEC\tHEX\tMode\tAlpha Tag\tDescription\tTag\tCategory\tUpdated",
    "1201\t4b1\tT\tLkv PD Disp\tLkv PD Disp desc\tLaw Dispatch\tLakeview\t2020-01-01",
    "1203Online\t4b3\tT\tLkv FD Disp\tLkv FD Disp desc\tFire Dispatch\tLakeview\t2020-01-01",
    "1205\t4b5\tT",
    "Enc\tLkv PD Tac\tLkv PD Tac desc\tLaw Tac\tLakeview\t2020-01-01",
    "2301Online\t8fd\tT",
    "Enc\tHlt PD Disp\tHlt PD Disp desc\tLaw Dispatch\tHillton\t2020-01-01",
    "2303\t8ff\tT\tHlt FD Disp\tHlt FD Disp desc\tFire Dispatch\tHillton\t2020-01-01",
  ];
  assert.deepEqual(parse(paste), [
    tg(1201, "T", "Lkv PD Disp", "Law Dispatch", "Lakeview"),
    tg(1203, "T", "Lkv FD Disp", "Fire Dispatch", "Lakeview"),
    tg(1205, "TE", "Lkv PD Tac", "Law Tac", "Lakeview"),
    tg(2301, "TE", "Hlt PD Disp", "Law Dispatch", "Hillton"),
    tg(2303, "T", "Hlt FD Disp", "Fire Dispatch", "Hillton"),
  ]);
});

test("the older layouts still read: a heading for the category, ONLINE beside the number or on the next line, ENC after the mode", () => {
  const paste = [
    "Lakeview Fire",
    "167 ONLINE\t0a7\tD\tFD Disp\tFD Disp desc\tFire Dispatch",
    "168",
    "ONLINE\t0a8\tD ENC\tFD Tac\tFD Tac desc\tFire-Tac",
  ];
  assert.deepEqual(parse(paste), [tg(167, "D", "FD Disp", "Fire Dispatch", "Lakeview Fire"), tg(168, "DE", "FD Tac", "Fire-Tac", "Lakeview Fire")]);
});
