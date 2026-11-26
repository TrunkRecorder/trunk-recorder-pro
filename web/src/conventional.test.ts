// A conventional row's code as people write it (rules.ts: parseAccess, the
// recorder's Access::parse and Access::parts), and its CSV. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultTalkgroups, nextTalkgroup } from "./config.ts";
import type { Channel } from "./protocol.ts";
import { channelsToCsv, parseAccess, parseChannelCsv, sameAccess } from "./rules.ts";
import { allModes, modeInfo } from "./modes.ts";

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
  assert.ok(sameAccess("fm", { code: "D023N" }, { code: "D047I" }), "a DCS code and its alias");
  assert.ok(!sameAccess("p25", { code: "293", matchTalkgroup: 1 }, { code: "293" }));
  assert.ok(sameAccess("p25", { code: "293" }, { code: "$293" }) && sameAccess("fm", { code: "" }, { code: "" }));
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

test("modes: each says what it can take, as the recorder does, and parseAccess agrees", () => {
  const modes = allModes().map((m) => m.mode);
  assert.deepEqual(modes, ["fm", "am", "p25", "dmr", "nxdn48", "nxdn96", "auto"], "the order Setup's menus list them");
  for (const m of modes) {
    const info = modeInfo(m);
    // A mode that takes no code says so when given one; one that does reads it or says what is wrong with it.
    const given = parseAccess(m, "100.0");
    assert.equal("error" in given && given.error.includes("carry no tone or code"), !info.takesCode, m);
    // A row can be added on its frequency where there is a code to tell them apart by.
    assert.equal(!!info.addButton, info.takesCode, m);
  }
  const which = (f: (m: ReturnType<typeof modeInfo>) => boolean) => allModes().filter(f).map((m) => m.mode);
  assert.deepEqual(which((m) => m.matchesTalkgroup), ["p25", "dmr", "nxdn48", "nxdn96"]);
  assert.deepEqual(which((m) => m.airNamesTalkgroup && !m.matchesTalkgroup), ["auto"]);
  assert.deepEqual(which((m) => m.analogReceiver), ["fm", "am", "auto"]);
  assert.deepEqual(which((m) => m.slotted), ["dmr"]);
  assert.equal(modeInfo("fm").codeName, "tone");
  assert.equal(modeInfo("auto").codeName, null);
  assert.equal(modeInfo("fm").chip("100.0"), "100.0 Hz");
  assert.equal(modeInfo("p25").chip("293"), "NAC 293");
});

test("toneLabel: a tone as shown, and its FRS/GMRS privacy code on those channels only", async () => {
  const { toneLabel } = await import("./rules.ts");
  const gmrs = 462_562_500;
  assert.deepEqual(toneLabel("151.4", gmrs), { text: "151.4 Hz", privacy: { code: 24, motorola: false } });
  assert.deepEqual(toneLabel("023 DPL", gmrs), { text: "D023N", privacy: { code: 39, motorola: true } });
  assert.deepEqual(toneLabel("D047I", gmrs).privacy, { code: 39, motorola: true }, "an alias of D023N");
  assert.equal(toneLabel("69.3", gmrs).privacy, null, "not one of the 38");
  assert.deepEqual(toneLabel("151.4 Hz", 154_430_000), { text: "151.4 Hz", privacy: null }, "off FRS/GMRS");
  assert.deepEqual(toneLabel("bogus", gmrs), { text: "bogus", privacy: null }, "as written when it isn't one");
  assert.equal(modeInfo("fm").chip("D023"), "D023N");
});
