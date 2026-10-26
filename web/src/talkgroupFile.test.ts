// Editing a system's talkgroup file in place: ignoring a talkgroup, and
// merging another file's talkgroups in, keep its columns, its delimiter and
// everything of the user's. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { mergeTalkgroupCsv, parseTalkgroupCsv, withIgnore, withTalkgroupFields } from "./talkgroups.ts";

test("ignore: a file of none gets a header and the row", () => {
  assert.equal(withIgnore("", 101, true, "Dispatch"), "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n101,65,Dispatch,D,,,,x\n");
});

test("ignore: its column added, the row marked; others untouched; and not ignored again", () => {
  const csv = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,Dispatch,D,,Fire,Ops\n102,66,Tac,D,,,\n";
  const on = withIgnore(csv, 102, true);
  assert.equal(on, "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n101,65,Dispatch,D,,Fire,Ops,\n102,66,Tac,D,,,,x\n");
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

test("fields: a talkgroup not in the file gets a row with just the given cells set", () => {
  const out = withTalkgroupFields("", 101, { alphaTag: "Dispatch", description: "Fire ops" });
  const t = parseTalkgroupCsv(out).get(101)!;
  assert.equal(t.alphaTag, "Dispatch");
  assert.equal(t.description, "Fire ops");
  assert.equal(t.tag, "");
});

test("fields: an existing row is patched in place — untouched fields and other rows keep their values", () => {
  const csv = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,Dispatch,D,,Fire,Ops\n102,66,Tac,D,,,\n";
  const out = withTalkgroupFields(csv, 101, { description: "Primary fire channel" });
  const all = parseTalkgroupCsv(out);
  assert.equal(all.get(101)!.alphaTag, "Dispatch", "alpha tag untouched");
  assert.equal(all.get(101)!.description, "Primary fire channel");
  assert.equal(all.get(101)!.tag, "Fire", "other field on the same row untouched");
  assert.equal(all.get(102)!.alphaTag, "Tac", "other row untouched");
});

test("fields: a column the file lacks is added, aligning every existing row", () => {
  const csv = "Decimal,Alpha Tag\n101,Dispatch\n102,Tac\n";
  const out = withTalkgroupFields(csv, 101, { tag: "Fire" });
  assert.equal(out, "Decimal,Alpha Tag,Tag\n101,Dispatch,Fire\n102,Tac,\n");
});

test("fields: setting the alpha tag and the Ignore column stay independent edits", () => {
  const withName = withTalkgroupFields("", 101, { alphaTag: "Dispatch" });
  const withIgn = withIgnore(withName, 101, true);
  const t = parseTalkgroupCsv(withIgn).get(101)!;
  assert.equal(t.alphaTag, "Dispatch");
  assert.equal(t.ignore, true);
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
