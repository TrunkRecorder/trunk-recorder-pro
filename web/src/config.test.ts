// Which systems are sites of one system (config.ts: multiSiteGroup,
// siteSiblings, multiSitePartners, bySite), as Setup and the dashboard say.
// `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { bySite, defaultConfig, multiSiteGroup, multiSitePartners, newSystem, siteSiblings, type LiveSite } from "./config.ts";
import type { Config, Identity, System } from "./protocol.ts";

/** A config with these systems (each on a control channel of its own). */
function config(...systems: Partial<System>[]): Config {
  const c = defaultConfig();
  systems.forEach((x, i) => c.systems.push(newSystem(c, { controlChannelsHz: [851_000_000 + i * 25_000], ...x })));
  return c;
}

const id = (x: Partial<Identity>): Identity => ({ nac: null, wacn: null, sysId: null, rfss: null, site: null, ...x });
const live = (shortName: string, siteGroup: string | null, identity: Partial<Identity>, controlChannelHz: number | null = null): LiveSite => ({ shortName, siteGroup, identity: id(identity), controlChannelHz });
const names = (xs: System[]) => xs.map((x) => x.shortName);
const sys = (c: Config, name: string) => c.systems.find((x) => x.shortName === name)!;

test("P25 sites locked to one WACN + System ID are one system; another System ID isn't", () => {
  const c = config(
    { shortName: "a", expect: { wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: 1 } },
    { shortName: "b", expect: { wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: 2 } },
    { shortName: "other", expect: { wacn: 0xbee00, sysId: 0x1a3, rfss: 1, site: 1 } },
  );
  assert.equal(multiSiteGroup(sys(c, "a")), "p25:bee00.1a2");
  assert.deepEqual(names(siteSiblings(c, sys(c, "a"))), ["b"]);
  assert.deepEqual(names(siteSiblings(c, sys(c, "other"))), []);
  assert.equal(multiSitePartners(c, sys(c, "other")), null);
  const m = multiSitePartners(c, sys(c, "a"));
  assert.deepEqual(m?.siblings, ["b"]);
  assert.match(m!.why, /WACN BEE00 · SysID 1A2/);
});

test("P25 sites matched from the air, by what the running recorder's control channels say", () => {
  const c = config({ shortName: "a" }, { shortName: "b" }, { shortName: "c" });
  assert.deepEqual(names(siteSiblings(c, sys(c, "a"))), [], "nothing known while stopped");
  const heard = [live("a", "p25:bee00.1a2", { site: 1 }), live("b", "p25:bee00.1a2", { site: 2 }), live("c", "p25:bee00.999", { site: 1 })];
  assert.deepEqual(names(siteSiblings(c, sys(c, "a"), heard)), ["b"]);
  assert.match(multiSitePartners(c, sys(c, "a"), heard)!.why, /^The same system on the air \(P25 WACN BEE00 · SysID 1A2\)/);
});

test("two entries following one site, or one control channel, aren't sites of each other", () => {
  const c = config({ shortName: "a" }, { shortName: "b" }, { shortName: "c" });
  const heard = [live("a", "p25:bee00.1a2", { rfss: 1, site: 1 }, 851_000_000), live("b", "p25:bee00.1a2", { rfss: 1, site: 1 }), live("c", "p25:bee00.1a2", { site: 3 }, 851_000_000)];
  assert.deepEqual(names(siteSiblings(c, sys(c, "a"), heard)), []);
});

test("SmartNet sites by System ID; a P25 system with the same number isn't one of them", () => {
  const c = config({ shortName: "s1", type: "smartnet", expect: { sysId: 0x2c3 } }, { shortName: "s2", type: "smartnet", expect: { sysId: 0x2c3, site: 2 } }, { shortName: "p", expect: { wacn: 1, sysId: 0x2c3 } });
  assert.equal(multiSiteGroup(sys(c, "s1")), "smartnet:2c3");
  assert.deepEqual(names(siteSiblings(c, sys(c, "s1"))), ["s2"]);
  assert.deepEqual(names(siteSiblings(c, sys(c, "p"))), []);
});

test("DMR sites are joined only by a site group, and the tooltip names it", () => {
  const heard = [live("d1", "dmr:1", { site: 1 }), live("d2", "dmr:1", { site: 2 })];
  const plain = config({ shortName: "d1", type: "dmr" }, { shortName: "d2", type: "dmr" });
  assert.deepEqual(names(siteSiblings(plain, sys(plain, "d1"), heard)), []);
  const grouped = config({ shortName: "d1", type: "dmr", siteGroup: "capmax" }, { shortName: "d2", type: "dmr", siteGroup: " capmax " }, { shortName: "d3", type: "dmr", siteGroup: "other" });
  assert.equal(multiSiteGroup(sys(grouped, "d2")), "group:capmax");
  assert.deepEqual(multiSitePartners(grouped, sys(grouped, "d1"))?.siblings, ["d2"]);
  assert.equal(multiSitePartners(grouped, sys(grouped, "d1"))?.why, 'Site group "capmax" — calls saved once');
  grouped.recording.dropDuplicateCalls = false;
  assert.equal(multiSitePartners(grouped, sys(grouped, "d1"))?.why, `Site group "capmax" — every site's copy saved`);
});

test("bySite: the sites of one system together, where its first site was, the rest in order", () => {
  const g = (shortName: string, siteGroup?: string) => ({ shortName, type: "dmr" as const, ...(siteGroup ? { siteGroup } : {}) });
  const c = config(g("a1", "a"), g("x"), g("b1", "b"), g("a2", "a"), g("y"), g("b2", "b"), g("a3", "a"));
  assert.deepEqual(names(bySite(c, c.systems)), ["a1", "a2", "a3", "x", "b1", "b2", "y"]);
  // Only the systems given (e.g. the ones recording).
  assert.deepEqual(names(bySite(c, c.systems.filter((x) => x.shortName !== "a1"))), ["x", "b1", "b2", "a2", "a3", "y"]);
});
