// A conventional row's code as people write it (tones.ts: parseAccess, the
// recorder's Access::parse and Access::parts), and its CSV. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultTalkgroups, nextTalkgroup } from "./config.ts";
import type { Channel } from "./protocol.ts";
import { channelsToCsv, parseChannelCsv } from "./rules.ts";
import { parseAccess, sameAccess } from "./tones.ts";
import { MODES, type Mode } from "./modes.ts";

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
  // AM and auto have no code to split a frequency by.
  assert.deepEqual(parseAccess("auto", "any"), { code: "" });
  assert.deepEqual(parseAccess("auto", ""), { code: "" });
  const auto = parseAccess("auto", "100.0");
  assert.ok("error" in auto && auto.error.includes("auto channels carry no tone or code"), JSON.stringify(auto));
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

test("MODES: each mode says what it can take, and parseAccess agrees", () => {
  const modes = Object.keys(MODES) as Mode[];
  assert.deepEqual(modes, ["fm", "am", "p25", "dmr", "nxdn48", "nxdn96", "auto"], "the order Setup's menus list them");
  for (const m of modes) {
    const info = MODES[m];
    // A mode that takes no code says so when given one; one that does reads it or says what is wrong with it.
    const given = parseAccess(m, "100.0");
    assert.equal("error" in given && given.error.includes("carry no tone or code"), !info.takesCode, m);
    // A digital mode names its talkgroup on the air, and so does auto, which may settle on one.
    assert.ok(!info.matchesTalkgroup || info.airNamesTalkgroup, m);
    assert.ok(info.analog ? info.analogReceiver : true, m);
  }
  assert.deepEqual(modes.filter((m) => MODES[m].matchesTalkgroup), ["p25", "dmr", "nxdn48", "nxdn96"]);
  assert.deepEqual(modes.filter((m) => MODES[m].airNamesTalkgroup && !MODES[m].matchesTalkgroup), ["auto"]);
  assert.deepEqual(modes.filter((m) => MODES[m].analogReceiver), ["fm", "am", "auto"]);
  assert.equal(MODES.fm.chip("100.0"), "100.0 Hz");
  assert.equal(MODES.p25.chip("293"), "NAC 293");
});
