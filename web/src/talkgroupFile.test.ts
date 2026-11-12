// A system's talkgroups as rows: a file read and written by the recorder's
// own code (through wasm), one row edited as the recorder edits it, and
// another list's talkgroups merged in without replacing the user's. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { talkgroupsReplace } from "./rules.ts";
import { ignoreEdit, isEncryptedMode, mergeTalkgroups, parseRadioReferencePaste, sameEdit, readTalkgroupFile, talkgroup, talkgroupFileText, withEdit } from "./talkgroups.ts";

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

const BY_HAND = { kind: "edited" as const, at: 1_762_500_000 };

// The same edits as trunk-core's: this copy of the rows stays the recorder's.
test("withEdit: one row's cells, as the recorder sets them", () => {
  const rows = [talkgroup(101, { alphaTag: "Fire Disp" }), talkgroup(102, { alphaTag: "Tac" })];
  const named = withEdit(rows, 101, { alphaTag: "FIRE4A", tag: "Fire, Tac" }, BY_HAND);
  assert.deepEqual([named[0].alphaTag, named[0].tag], ["FIRE4A", "Fire, Tac"]);
  assert.deepEqual(named[1], rows[1], "the other row as it was");
  assert.equal(withEdit(rows, 102, { ignore: true }, BY_HAND)[1].ignore, true);
  assert.deepEqual(names(withEdit(rows, 205, { ignore: true, alphaTag: "Data" }, BY_HAND)), [[101, "Fire Disp"], [102, "Tac"], [205, "Data"]], "a row added");
  assert.equal(withEdit(rows, 205, { ignore: false }, BY_HAND).length, 2, "none for nothing set");
  const old = withEdit([talkgroup(104, { priority: -1, ignore: true })], 104, { ignore: false }, BY_HAND);
  assert.deepEqual([old[0].ignore, old[0].priority], [false, 1], "un-ignored: Trunk Recorder's priority -1 undone");
});

test("ignoreEdit: named too when the talkgroup has no row", () => {
  const rows = [talkgroup(101, { alphaTag: "Dispatch" })];
  assert.deepEqual(ignoreEdit(rows, 101, true, "Other"), { ignore: true });
  assert.deepEqual(ignoreEdit(rows, 7, true, "Data"), { alphaTag: "Data", ignore: true });
});

test("sameEdit: the same cells to the same values, as sent and as the recorder answers", () => {
  assert.ok(sameEdit({ alphaTag: " A ", ignore: false }, { ignore: false, alphaTag: "A" }));
  assert.ok(!sameEdit({ alphaTag: "A" }, { alphaTag: "A", ignore: true }));
});

// Where each row's names came from: the last change, by the recorder's own code (names.rs).
test("origins: a names edit makes it the row's, an Ignore doesn't; a list loaded keeps the rows it doesn't change", () => {
  const rr = { kind: "radioReference" as const, at: 100 };
  const rows = talkgroupsReplace([], [talkgroup(101, { alphaTag: "Fire Disp" }), talkgroup(102, { alphaTag: "Tac" })], rr);
  assert.deepEqual(rows.map((t) => t.origin?.kind), ["radioReference", "radioReference"]);
  const ignored = withEdit(rows, 101, { ignore: true }, BY_HAND);
  assert.equal(ignored[0].origin?.kind, "radioReference", "Ignore isn't a name");
  const named = withEdit(rows, 101, { alphaTag: "FIRE4A" }, BY_HAND);
  assert.deepEqual(named[0].origin, BY_HAND);
  const file = { kind: "file" as const, name: "dc.csv", at: 200 };
  const loaded = talkgroupsReplace(named, [talkgroup(101, { alphaTag: "FIRE4A" }), talkgroup(102, { alphaTag: "Tac 2" })], file);
  assert.deepEqual(loaded.map((t) => t.origin?.kind), ["edited", "file"], "the edit stands where the file agrees");
  assert.deepEqual(talkgroupsReplace([], named, undefined), named, "moved: each its own");
  assert.equal(readTalkgroupFile(talkgroupFileText(named)).talkgroups[0].origin, undefined, "not written to the file");
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

// A table copied from RadioReference's page (rows from a Berks County, PA listing, 2026-11): its
// key says "Enc" full encryption, "enc" partial (some clear traffic, recorded).
test("RadioReference paste: Enc is always encrypted, enc only sometimes; badges glued on read too", () => {
  const paste = [
    "County Law Enforcement",
    "DEC\tHEX\tMode\tAlpha Tag\tDescription\tTag",
    "1001\t3e9\tT Enc\tPD Disp A\tPolice Dispatch Zone A\tLaw Dispatch",
    "1010\t3f2\tT\tPD SCOPE2\tPolice SCOPE Interactions\tLaw Talk",
    "4004\tfa4\tDEnc\tBrksJail ERT\tCo Jail Emergency Response Team\tCorrections",
    "Emergency Management",
    "8001\t1f41\tT enc\tBerks DES\tDES Primary\tEmergency Ops",
    "8009Online\t1f49\tT\tFire/EMSDisp\tFire/EMS Dispatch - Patched to 151.220\tFire Dispatch",
    "8010\t1f4a\tTenc\tDES Tac\tDES Tactical\tEmergency Ops",
  ].join("\n");
  const rows = parseRadioReferencePaste(paste);
  assert.deepEqual(rows.map((t) => [t.number, t.mode, t.alphaTag, t.category]), [
    [1001, "TE", "PD Disp A", "County Law Enforcement"],
    [1010, "T", "PD SCOPE2", "County Law Enforcement"],
    [4004, "DE", "BrksJail ERT", "County Law Enforcement"],
    [8001, "Te", "Berks DES", "Emergency Management"],
    [8009, "T", "Fire/EMSDisp", "Emergency Management"],
    [8010, "Te", "DES Tac", "Emergency Management"],
  ]);
  assert.deepEqual(rows.map((t) => isEncryptedMode(t.mode)), [true, false, true, false, false, false], "only the fully encrypted ones aren't recorded");
  assert.deepEqual(parseRadioReferencePaste("101\tD ENC\tA\n102\tDE\tB\n103\tDe\tC\n104\td\tD").map((t) => t.mode), ["DE", "DE", "De", "D"], "the older ENC, and the letters as RadioReference wrote them before");
});
