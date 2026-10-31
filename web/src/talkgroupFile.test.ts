// A system's talkgroups as rows: a file read and written by the recorder's
// own code (through wasm), one row edited as the recorder edits it, and
// another list's talkgroups merged in without replacing the user's. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { editCells, ignoreEdit, mergeTalkgroups, readTalkgroupFile, talkgroup, talkgroupFileText, withEdit } from "./talkgroups.ts";

const names = (rows: { number: number; alphaTag: string }[]) => rows.map((t) => [t.number, t.alphaTag]);

test("a file: semicolons, a byte-order mark and header case read as Trunk Recorder's columns", () => {
  const r = readTalkgroupFile('﻿decimal;MODE;alpha tag;Description;Group\n101;D;"Dispatch; North";Main;Fire\n');
  assert.deepEqual(r.talkgroups, [talkgroup(101, { alphaTag: "Dispatch; North", description: "Main", category: "Fire" })]);
  assert.deepEqual([r.legacy, r.missing, r.unknownColumns, r.skipped], [false, [], [], []]);
});

test("a file's report: no header, columns missing or unknown, rows without a number", () => {
  const legacy = readTalkgroupFile("101,65,D,Dispatch,Desc,Fire,Ops\n");
  assert.equal(legacy.legacy, true);
  assert.deepEqual(names(legacy.talkgroups), [[101, "Dispatch"]]);
  const r = readTalkgroupFile("Decimal,Alpha Tag,Zone\n101,A,North\nabc,B,South\n");
  assert.deepEqual([r.missing, r.unknownColumns, r.skipped], [["Mode", "Description"], ["Zone"], ["abc,B,South"]]);
  assert.deepEqual(r.talkgroups[0].extra, [["Zone", "North"]], "kept for Download");
});

test("download: Trunk Recorder's headed CSV, with comments and columns of its own", () => {
  const rows = readTalkgroupFile("Decimal,Alpha Tag,Mode,Description,Zone,Priority\n# county\n101,Fire Disp,D,Main,North,-1\n102,Tac,D,,,\n").talkgroups;
  assert.equal(rows[0].comment, "# county");
  assert.equal(rows[0].ignore, true, "priority -1 is ignored");
  const text = talkgroupFileText(rows);
  assert.match(text, /^Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Priority,Zone\n# county\n101,65,Fire Disp,D,Main,,,-1,North\n102,66,Tac,D,,,,1,\n$/);
  assert.deepEqual(readTalkgroupFile(text).talkgroups, rows, "read back the same");
  assert.equal(talkgroupFileText([]), "");
});

// The same edits as trunk-core's: this copy of the rows stays the recorder's.
test("withEdit: one row's cells, as the recorder sets them", () => {
  const rows = [talkgroup(101, { alphaTag: "Fire Disp" }), talkgroup(102, { alphaTag: "Tac" })];
  const named = withEdit(rows, 101, { alphaTag: "FIRE4A", tag: "Fire, Tac" });
  assert.deepEqual([named[0].alphaTag, named[0].tag], ["FIRE4A", "Fire, Tac"]);
  assert.deepEqual(named[1], rows[1], "the other row as it was");
  assert.equal(withEdit(rows, 102, { ignore: true })[1].ignore, true);
  assert.deepEqual(names(withEdit(rows, 205, { ignore: true, alphaTag: "Data" })), [[101, "Fire Disp"], [102, "Tac"], [205, "Data"]], "a row added");
  assert.equal(withEdit(rows, 205, { ignore: false }).length, 2, "none for nothing set");
  const old = withEdit([talkgroup(104, { priority: -1, ignore: true })], 104, { ignore: false });
  assert.deepEqual([old[0].ignore, old[0].priority], [false, 1], "un-ignored: Trunk Recorder's priority -1 undone");
});

test("ignoreEdit: named too when the talkgroup has no row", () => {
  const rows = [talkgroup(101, { alphaTag: "Dispatch" })];
  assert.deepEqual(ignoreEdit(rows, 101, true, "Other"), { ignore: true });
  assert.deepEqual(ignoreEdit(rows, 7, true, "Data"), { alphaTag: "Data", ignore: true });
  assert.deepEqual(editCells({ alphaTag: " A ", ignore: false }), [["Alpha Tag", "A"], ["Ignore", ""]]);
});

test("merge: the talkgroups it lacks, after its own; its own as they are", () => {
  const mine = [talkgroup(716, { alphaTag: "E16 TAC (mine)", priority: 2 }), talkgroup(900, { alphaTag: "Mine only", ignore: true })];
  const theirs = [talkgroup(716, { alphaTag: "Engine 16 Tac" }), talkgroup(801, { alphaTag: "Dispatch" })];
  const m = mergeTalkgroups(mine, theirs);
  assert.equal(m.added, 1);
  assert.deepEqual(m.talkgroups, [...mine, theirs[1]]);
  assert.deepEqual(mergeTalkgroups([], theirs), { talkgroups: theirs, added: 2 });
  assert.equal(mergeTalkgroups(m.talkgroups, theirs).added, 0);
});
