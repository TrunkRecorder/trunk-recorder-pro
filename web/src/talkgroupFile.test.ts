// Editing a system's talkgroup file in place: ignoring a talkgroup, and
// merging another file's talkgroups in, keep its columns, its delimiter and
// everything of the user's. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { editCells, mergeTalkgroupCsv, parseTalkgroupCsv, withCells, withIgnore } from "./talkgroups.ts";

test("ignore: a file of none gets a header and the row", () => {
  assert.equal(withIgnore("", 101, true, "Dispatch"), "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n101,65,Dispatch,D,,,,x\n");
});

test("ignore: its column added, the row marked; others untouched; and not ignored again", () => {
  const csv = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,Dispatch,D,,Fire,Ops\n102,66,Tac,D,,,\n";
  const on = withIgnore(csv, 102, true);
  assert.equal(on, "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n101,65,Dispatch,D,,Fire,Ops\n102,66,Tac,D,,,,x\n", "the other row as it was");
  assert.equal(parseTalkgroupCsv(on).get(102)!.ignore, true);
  assert.equal(parseTalkgroupCsv(withIgnore(on, 102, false)).get(102)!.ignore ?? false, false);
});

test("ignore: Trunk Recorder's priority -1 is undone when un-ignored", () => {
  const csv = "Decimal,Mode,Alpha Tag,Priority\n7,D,Old,-1\n";
  assert.match(withIgnore(csv, 7, false), /^7,D,Old,1,$/m);
});

test("ignore: a legacy (headerless) file is given its header", () => {
  const out = withIgnore("101,65,D,Dispatch,Desc,Fire,Ops\n", 101, true);
  assert.equal(out.split("\n")[0], "Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category,Ignore");
  assert.equal(parseTalkgroupCsv(out).get(101)!.alphaTag, "Dispatch");
});

test("ignore: a semicolon file stays a semicolon file", () => {
  const out = withIgnore("Decimal;Mode;Alpha Tag\n101;D;Dispatch, North\n", 101, true);
  assert.equal(out, "Decimal;Mode;Alpha Tag;Ignore\n101;D;Dispatch, North;x\n");
});

test("merge: the talkgroups it lacks, appended in its columns and delimiter; its own as they are", () => {
  const mine = "Decimal;Mode;Alpha Tag;Description;Priority;Ignore\n716;D;E16 TAC (mine);;2;\n900;D;Mine only;;;1\n";
  const theirs = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Priority\n716,2cc,Engine 16 Tac,D,,Fire Dispatch,,\n801,321,Dispatch,D,,,,\n";
  const m = mergeTalkgroupCsv(mine, theirs);
  assert.equal(m.added, 1);
  assert.equal(m.csv, mine + "801;D;Dispatch;;;\n");
  assert.deepEqual(mergeTalkgroupCsv("", theirs), { csv: theirs, added: 2 });
  assert.equal(mergeTalkgroupCsv(m.csv, theirs).added, 0);
});

// The same edits as trunk-core's `a_row_edited_in_place`: this copy of a file stays the recorder's.
test("withCells: one row's cells, made as the recorder makes them", () => {
  const csv = '# county\nDecimal;Alpha Tag;Description\n101;Fire Disp;Main\n102;"Tac; 2";\n';
  const out = withCells(csv, 102, [["Ignore", "x"]]);
  assert.equal(out, '# county\nDecimal;Alpha Tag;Description;Ignore\n101;Fire Disp;Main\n102;"Tac; 2";;x\n');
  const named = withCells(out, 101, editCells({ alphaTag: "FIRE4A", tag: "Fire, Tac" }));
  assert.deepEqual([parseTalkgroupCsv(named).get(101)?.alphaTag, parseTalkgroupCsv(named).get(101)?.tag], ["FIRE4A", "Fire, Tac"]);
  assert.equal(withCells("Decimal,Alpha Tag\n101,A\n", 205, [["Ignore", "x"], ["Alpha Tag", "Data"]]), "Decimal,Alpha Tag,Ignore\n101,A\n205,Data,x\n");
  assert.equal(withCells("Decimal,Alpha Tag\n101,A\n", 205, [["Ignore", ""]]), "Decimal,Alpha Tag,Ignore\n101,A\n");
  assert.equal(parseTalkgroupCsv(withCells("Decimal,Alpha Tag,Priority\n104,Old,-1\n", 104, [["Ignore", ""]])).get(104)?.ignore ?? false, false);
  assert.equal(withCells("202,ca,E,Police,Tac 2,Law,City\n", 202, [["Alpha Tag", "PD Tac"]]), "Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category\n202,ca,E,PD Tac,Tac 2,Law,City\n");
  assert.equal(withCells("", 7, [["Ignore", "x"]]), "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n7,7,,D,,,,x\n");
});
