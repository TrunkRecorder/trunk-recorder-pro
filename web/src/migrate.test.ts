// Upgrading what an older version left (migrate.ts): goes with it. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { siteNameOf } from "./config.ts";
import { migrateSelection, migrateTalkgroups } from "./migrate.ts";
import { readConfig } from "./rules.ts";

test("an older config is read as the recorder reads it: systems joined by a site group or a site lock become one", () => {
  const { config, notes } = readConfig({
    systems: [
      { shortName: "cm1", type: "dmr", controlChannelsHz: [452175000], siteGroup: "capmax", talkgroupsCsv: "tg" },
      { shortName: "p1", controlChannelsHz: [851012500], expect: { wacn: 0xbee00, sysId: 0x1a2, site: 1 } },
      { shortName: "cm2", type: "dmr", controlChannelsHz: [452375000], siteGroup: "capmax", talkgroupsCsv: "other", enabled: false },
      { shortName: "p2", controlChannelsHz: [851512500], expect: { wacn: 0xbee00, sysId: 0x1a2, site: 2 } },
      { shortName: "p3", controlChannelsHz: [852012500], expect: { wacn: 0xbee00, sysId: 0x1a3 } },
    ],
  });
  assert.deepEqual(
    config.systems.map((s) => [s.shortName, s.sites.map((x) => siteNameOf(s, x))]),
    [["cm1", ["cm1", "cm2"]], ["p1", ["p1", "p2"]], ["p3", ["p3"]]],
  );
  assert.ok(!JSON.stringify(config).includes("talkgroupsCsv"), "talkgroup files leave the config");
  assert.deepEqual(config.systems[0].sites.map((x) => x.enabled), [true, false]);
  assert.ok(notes.some((n) => n.includes("cm2's talkgroup file weren't kept")), notes.join(" | "));
  assert.deepEqual(readConfig(config).notes, [], "systems with sites: nothing to do");
  assert.equal(config.recording.m4a?.encoder, "auto", "and what it lacked is at its default");
});

test("migrateSelection: a system of sites kept under its group's name is its first site's", () => {
  const G = "p25:bee00.1a2";
  const { name, key } = migrateSelection({ sites: { dcfd: G, dcfd2: G } });
  assert.deepEqual([name(G), name("other"), key(`${G}:200`), key("other:5")], ["dcfd", "other", "dcfd:200", "other:5"]);
  const none = migrateSelection({});
  assert.equal(none.key("dcfd:1"), "dcfd:1");
});

test("migrateTalkgroups: what an older version kept, read in as rows", () => {
  const old = { systems: [
    { shortName: "a", controlChannelsHz: [1], siteGroup: "g", talkgroupsCsv: "1,a" },
    { shortName: "b", controlChannelsHz: [2], siteGroup: "g", talkgroupsCsv: "2,b" },
    { shortName: "c", sites: [{ controlChannelsHz: [3] }], talkgroupsCsv: "3,c" },
  ] };
  const numbers = (m: Record<string, { number: number }[]>) => Object.fromEntries(Object.entries(m).map(([k, v]) => [k, v.map((t) => t.number)]));
  assert.deepEqual(numbers(migrateTalkgroups(old, {})), { a: [1], c: [3] }, "an older config's, joined systems keeping the first's");
  const kept = { d: "Decimal,Alpha Tag\n7,Tac\n", e: [{ ...migrateTalkgroups({}, { x: "9,n" }).x[0] }] };
  assert.deepEqual(numbers(migrateTalkgroups({}, kept)), { d: [7], e: [9] }, "a file's text kept before; rows as they are");
  assert.deepEqual(migrateTalkgroups({}, {}), {});
});

test("a conventional channel's tone becomes its code, slot and talkgroup; P25 rows sharing a frequency keep their number", () => {
  const { config, notes } = readConfig({
    conventional: [{ shortName: "conv", channels: [
      { freqHz: 451500000, mode: "dmr", tone: "CC 1 TS 2 TG 201", name: "Dispatch", enabled: true },
      { freqHz: 453500000, mode: "p25", tone: "NAC 293", name: "", enabled: true },
      { freqHz: 453500000, mode: "p25", tone: "NAC 4F1", name: "", enabled: true },
    ] }],
  });
  const rows = config.conventional[0].channels.map((c) => [c.code, c.slot, c.matchTalkgroup, c.talkgroup]);
  assert.deepEqual(rows, [["1", 2, 201, undefined], ["293", undefined, undefined, 453500], ["4F1", undefined, undefined, 4535001]]);
  assert.ok(notes.some((n) => n.includes("2 P25 channel row(s)")), notes.join(" | "));
  assert.ok(!JSON.stringify(config).includes('"tone"'));
});
