// Systems with sites (config.ts): the sites recorded, their names, and
// systems set up apart that are one on the air. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { activeSites, defaultConfig, importTrunkRecorderConfig, nameProblem, newSiteName, newSystem, normalizeSystem, resolvedCenters, sameOnTheAir, siteNameOf, startProblem } from "./config.ts";
import type { Config, Identity, SiteStatus, SystemStatus } from "./protocol.ts";

test("normalizeSystem: an older config's system gets its site; a system with sites keeps them", () => {
  const old = normalizeSystem({ shortName: "a", type: "dmr", controlChannelsHz: [452175000], colorCode: 3, modulation: "qpsk", enabled: false, future: 1 } as never);
  assert.equal(old.sites.length, 1);
  assert.deepEqual([old.sites[0].controlChannelsHz, old.sites[0].colorCode, old.sites[0].modulation, old.sites[0].enabled], [[452175000], 3, "qpsk", false]);
  assert.equal((old as unknown as Record<string, unknown>).future, 1, "a key this version doesn't know is kept");
  assert.ok(!("controlChannelsHz" in old), "the site's fields are the site's");
  const now = normalizeSystem({ shortName: "b", sites: [{ controlChannelsHz: [1] }, { name: "b2", controlChannelsHz: [2], enabled: false }] });
  assert.deepEqual(now.sites.map((x) => [x.name, x.enabled, x.modulation]), [[undefined, true, "auto"], ["b2", false, "auto"]]);
});

test("activeSites: the recorder's order; site names unique in a system; new sites named so", () => {
  const c = defaultConfig();
  const a = newSystem(c, { shortName: "a", controlChannelsHz: [1] });
  a.sites.push({ ...a.sites[0], name: "a-south" }, { ...a.sites[0], name: "off", enabled: false });
  c.systems.push(a, newSystem(c, { shortName: "b", controlChannelsHz: [2] }), newSystem(c, { shortName: "c" }));
  assert.deepEqual(activeSites(c).map(({ sys, site }) => `${sys.shortName}/${siteNameOf(sys, site)}`), ["a/a", "a/a-south", "b/b"]);
  assert.equal(newSiteName(a), "a-4");
  assert.equal(newSiteName(a, { rfss: 1, site: 3 }), "a-1-3");
  assert.equal(nameProblem(c), null);
  a.sites[1].name = "a";
  assert.match(nameProblem(c) ?? "", /Two sites of a are named "a"/);
});

test("sameOnTheAir: systems set up apart whose sites announce one system", () => {
  const c: Config = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "dcfd" }), newSystem(c, { shortName: "dcfd2" }), newSystem(c, { shortName: "other", expect: { wacn: 0xbee00, sysId: 0x9 } }), newSystem(c, { shortName: "cm", type: "dmr" }));
  const id = (x: Partial<Identity>): Identity => ({ nac: null, wacn: null, sysId: null, rfss: null, site: null, ...x });
  const site = (name: string, identity: Partial<Identity>): SiteStatus =>
    ({ index: 0, name, nowS: 0, controlChannelHz: null, identity: id(identity), good: 0, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null });
  const sys = (shortName: string, sites: SiteStatus[]): SystemStatus => ({ index: 0, shortName, activeCalls: 0, recording: 0, callsConcluded: 0, sites });
  assert.deepEqual(sameOnTheAir(c, []), []);
  const live = [sys("dcfd", [site("dcfd", { wacn: 0xbee00, sysId: 0x1a2, site: 1 })]), sys("dcfd2", [site("dcfd2", { wacn: 0xbee00, sysId: 0x1a2, site: 2 })])];
  assert.deepEqual(sameOnTheAir(c, live).map(([a, b]) => [a.shortName, b.shortName]), [["dcfd", "dcfd2"]]);
});

test("importTrunkRecorderConfig: multiSite systems of one multiSiteSystemName are one system, a site each", () => {
  const tr = {
    sources: [{ center: 851e6, rate: 2.4e6, driver: "osmosdr", device: "rtl=0" }],
    systems: [
      { shortName: "north", type: "p25", control_channels: [851012500], multiSite: true, multiSiteSystemName: "county", talkgroupsFile: "tg.csv" },
      { shortName: "south", type: "p25", control_channels: [851512500], multiSite: true, multiSiteSystemName: "county", talkgroupsFile: "tg.csv", siteId: "2" },
      { shortName: "city", type: "p25", control_channels: [852012500] },
    ],
  };
  const { config, todo, talkgroups } = importTrunkRecorderConfig(JSON.stringify(tr), defaultConfig(), { "tg.csv": "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n100,64,Fire,D,Fire,,\n" });
  assert.deepEqual(config.systems.map((s) => [s.shortName, s.sites.map((x) => siteNameOf(s, x))]), [["county", ["north", "south"]], ["city", ["city"]]]);
  assert.match(talkgroups.county, /Fire/, "its talkgroup file, apart from the config");
  assert.ok(!JSON.stringify(config).includes("Fire"));
  assert.ok(todo.some((t) => t.kind === "siteLock" && t.system === "county" && t.site === 1), JSON.stringify(todo));
});

test("startProblem and resolvedCenters are the recorder's own (rules.ts)", () => {
  const c = defaultConfig();
  assert.match(startProblem(c) ?? "", /Add a system with a control channel, or a conventional channel/);
  c.sources[0].centerHz = 0;
  c.systems.push(newSystem(c, { shortName: "a", controlChannelsHz: [851_012_500] }));
  const centers = resolvedCenters(c);
  assert.ok(centers[0] && Math.abs(centers[0] - 851_012_500) < 1_000_000, `placed over the control channel: ${centers}`);
  assert.equal(startProblem(c), null);
  // A rule only the recorder had before: the RTL-SDR's USB transfer size.
  (c.sources[0] as { bufLen?: number }).bufLen = 1000;
  assert.match(startProblem(c) ?? "", /multiple of 512/);
});
