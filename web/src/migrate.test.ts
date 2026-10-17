// Upgrading what an older version left (migrate.ts): goes with it. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { siteNameOf } from "./config.ts";
import { migrateSelection, migrateSystems } from "./migrate.ts";

test("migrateSystems: systems joined by a site group or a site lock become one, the first's settings its own", () => {
  const { systems, notes } = migrateSystems([
    { shortName: "cm1", type: "dmr", controlChannelsHz: [452175000], siteGroup: "capmax", talkgroupsCsv: "tg" },
    { shortName: "p1", controlChannelsHz: [851012500], expect: { wacn: 0xbee00, sysId: 0x1a2, site: 1 } },
    { shortName: "cm2", type: "dmr", controlChannelsHz: [452375000], siteGroup: "capmax", talkgroupsCsv: "other", enabled: false },
    { shortName: "p2", controlChannelsHz: [851512500], expect: { wacn: 0xbee00, sysId: 0x1a2, site: 2 } },
    { shortName: "p3", controlChannelsHz: [852012500], expect: { wacn: 0xbee00, sysId: 0x1a3 } },
  ]);
  assert.deepEqual(
    systems.map((s) => [s.shortName, s.sites.map((x) => siteNameOf(s, x))]),
    [["cm1", ["cm1", "cm2"]], ["p1", ["p1", "p2"]], ["p3", ["p3"]]],
  );
  assert.equal(systems[0].talkgroupsCsv, "tg");
  assert.deepEqual(systems[0].sites.map((x) => x.enabled), [true, false]);
  assert.equal(systems[0].sites[0].name, undefined, "a site named as its system leaves its name out");
  assert.ok(notes.some((n) => n.includes("cm2's talkgroup file weren't kept")), notes.join(" | "));
  assert.deepEqual(migrateSystems(systems).notes, [], "systems with sites: nothing to do");
});

test("migrateSelection: a system of sites kept under its group's name is its first site's", () => {
  const G = "p25:bee00.1a2";
  const { name, key } = migrateSelection({ sites: { dcfd: G, dcfd2: G } });
  assert.deepEqual([name(G), name("other"), key(`${G}:200`), key("other:5")], ["dcfd", "other", "dcfd:200", "other:5"]);
  const none = migrateSelection({});
  assert.equal(none.key("dcfd:1"), "dcfd:1");
});
