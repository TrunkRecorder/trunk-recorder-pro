// A conventional row's code as people write it (tones.ts: parseAccess, the
// recorder's Access::parse and Access::parts), and its CSV. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultTalkgroups, nextTalkgroup } from "./config.ts";
import type { Channel } from "./protocol.ts";
import { channelsToCsv, parseChannelCsv } from "./rules.ts";
import { parseAccess, sameAccess } from "./tones.ts";

test("parseAccess: code, slot and talkgroup, as the recorder reads them", () => {
  assert.deepEqual(parseAccess("fm", "151.4 PL"), { code: "151.4" });
  assert.deepEqual(parseAccess("p25", "293 NAC"), { code: "293" });
  assert.deepEqual(parseAccess("p25", "NAC 293 TG 1"), { code: "293", matchTalkgroup: 1 });
  assert.deepEqual(parseAccess("p25", "TG 46"), { code: "", matchTalkgroup: 46 });
  assert.deepEqual(parseAccess("p25", "F7E"), { code: "" });
  assert.ok("error" in parseAccess("p25", "NAC") && "error" in parseAccess("p25", "TG 70000"));
  assert.deepEqual(parseAccess("dmr", "CC1 TS2 TG201"), { code: "1", slot: 2, matchTalkgroup: 201 });
  assert.deepEqual(parseAccess("nxdn48", "RAN 5 TG 201"), { code: "5", matchTalkgroup: 201 });
  assert.deepEqual(parseAccess("nxdn48", "RAN 0"), { code: "" });
  assert.ok(sameAccess({ code: "D023N" }, { code: "D047I" }), "a DCS code and its alias");
  assert.ok(!sameAccess({ code: "293", matchTalkgroup: 1 }, { code: "293" }));
});

test("default talkgroups: by place on the frequency; a digital row's own number is its own", () => {
  const row = (freqHz: number, mode: Channel["mode"], talkgroup?: number): Channel => ({ freqHz, mode, name: "", enabled: true, ...(talkgroup ? { talkgroup } : {}) });
  const chans = [row(154325000, "fm"), row(154325000, "fm", 1543251), row(453500000, "p25", 45351), row(453500000, "p25")];
  assert.deepEqual(defaultTalkgroups(chans), [154325, 1543251, 453500, 4535001]);
  assert.equal(nextTalkgroup(chans, 154325000), 1543252);
});

test("the CSV: Code, Slot and Match TG, round trip", () => {
  const { channels, notes } = parseChannelCsv("TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag\n9001,451.5,CC1,1,1,dmr,Dispatch\n45351,453.5,293,,1,p25,PW\n");
  assert.deepEqual(notes, []);
  assert.deepEqual(channels.map((c) => [c.talkgroup, c.code, c.slot, c.matchTalkgroup]), [[9001, "1", 1, 1], [45351, "293", undefined, 1]]);
  const csv = channelsToCsv(channels);
  assert.ok(csv.startsWith("TG Number,Frequency,Code,Slot,Match TG,Mode,"), csv);
  assert.deepEqual(parseChannelCsv(csv).channels, channels);
});
