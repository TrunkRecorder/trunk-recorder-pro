// The Listen page's scanner (listen.ts): the sites of a multi-site system
// heard as one, and talkgroups switched on are heard. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem, type LiveSite } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import { ListenPage, Talkgroups } from "./dash/Listen.tsx";
import { avoid, regrouped, setAll, setFeed, setTalkgroupsOn, sitesNow, wanted, type Selection } from "./listen.ts";
import type { CallEntry, Config, EngineStatus, FromRecorder, Identity, System, SystemStatus } from "./protocol.ts";

const G = "p25:bee00.1a2";
const CSV = (rows: string) => `Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n${rows}`;

function config(...systems: Partial<System>[]): Config {
  const c = defaultConfig();
  systems.forEach((x, i) => c.systems.push(newSystem(c, { controlChannelsHz: [851_000_000 + i * 25_000], ...x })));
  return c;
}
const id = (x: Partial<Identity>): Identity => ({ nac: null, wacn: null, sysId: null, rfss: 1, site: null, ...x });
const live = (shortName: string, siteGroup: string | null, site: number): LiveSite => ({ shortName, siteGroup, identity: id({ site }), controlChannelHz: null });
const sel = (x: Partial<Selection> = {}): Selection => ({ sites: {}, offSystems: [], offTalkgroups: [], hold: null, avoid: {}, ...x });
const call = (system: string, talkgroup: number, tag = "", at = Date.now()): CallEntry =>
  ({ path: `${system}/${talkgroup}-${at}`, record: { short_name: system, talkgroup, talkgroup_tag: tag, start_time_ms: at, call_length_ms: 4000, encrypted: 0 } }) as unknown as CallEntry;

test("sitesNow: sites of one system are heard under its name; a system of one site under its own", () => {
  const c = config({ shortName: "site1" }, { shortName: "site2" }, { shortName: "solo" });
  const heard = [live("site1", G, 1), live("site2", G, 2), live("solo", "p25:bee00.999", 1)];
  assert.deepEqual(sitesNow(c, heard, {}), { site1: G, site2: G });
  // A site switched off isn't one to hear.
  const off = config({ shortName: "site1" }, { shortName: "site2", enabled: false });
  assert.deepEqual(sitesNow(off, heard, {}), {});
});

test("sitesNow: stopped, or a control channel not decoded yet, the sites stay together", () => {
  const c = config({ shortName: "site1" }, { shortName: "site2" });
  const was = { site1: G, site2: G };
  assert.deepEqual(sitesNow(c, [], was), was);
  assert.deepEqual(sitesNow(c, [live("site1", G, 1), live("site2", null, 2)], was), was);
  // Both heard, and not one system after all.
  assert.deepEqual(sitesNow(c, [live("site1", G, 1), live("site2", "p25:bee00.999", 2)], was), {});
});

test("regrouped: a talkgroup on at any site is on for the system; off only if off at every site", () => {
  const before = sel({ offTalkgroups: ["site1:200", "site2:100", "site2:200", "site2:300", "other:5"] });
  const after = regrouped(before, { site1: G, site2: G });
  assert.deepEqual(after.sites, { site1: G, site2: G });
  assert.deepEqual(after.offTalkgroups.sort(), ["other:5", `${G}:200`]);
  assert.ok(wanted(after, call("site2", 100)), "turned on at site1, kept from site2: plays");
  assert.ok(!wanted(after, call("site1", 200)));
  assert.ok(wanted(after, call("site1", 300)));
});

test("regrouped: systems, holds and avoids move to the system; apart again, each site gets the system's", () => {
  const before = sel({ offSystems: ["site1", "site2"], offTalkgroups: ["site1:7"], hold: { system: "site2", talkgroup: 9 }, avoid: { "site1:4": 0, "site2:4": Date.now() + 60_000 } });
  const g = regrouped(before, { site1: G, site2: G });
  assert.deepEqual(g.offSystems, [G]);
  assert.deepEqual(g.hold, { system: G, talkgroup: 9 });
  assert.deepEqual(g.avoid, { [`${G}:4`]: 0 });
  const apart = regrouped(g, {});
  assert.deepEqual(apart.sites, {});
  assert.deepEqual(apart.offSystems.sort(), ["site1", "site2"]);
  assert.deepEqual(apart.avoid, { "site1:4": 0, "site2:4": 0 });
  assert.equal(apart.hold?.system, "site1");
  // One site's system off and the other's on: the system is on, the off site's talkgroups count as off there.
  const half = regrouped(sel({ offSystems: ["site2"], offTalkgroups: ["site1:7", "site1:8"], avoid: {} }), { site1: G, site2: G });
  assert.deepEqual(half.offSystems, []);
  assert.deepEqual(half.offTalkgroups.sort(), [`${G}:7`, `${G}:8`]);
});

/** The recorder says hello with two sites of one system and is running them. */
function twoSites(): void {
  const c = config({ shortName: "site1", talkgroupsCsv: CSV("100,64,Fire Dispatch,D,Fire,,\n300,12c,Tac 3,D,Tac,,") }, { shortName: "site2", talkgroupsCsv: CSV("100,64,Fire Disp,D,Fire,,\n200,c8,EMS,D,EMS,,") });
  transport.onConnection(true);
  const hello = { type: "hello", version: "test", platform: "test", config: c, devices: [], history: [], phase: { type: "state", phase: "running", error: null, ended: false } };
  transport.onMessage(hello as unknown as FromRecorder);
  const status = (shortName: string, index: number, site: number): SystemStatus => ({
    index, shortName, nowS: 1, controlChannelHz: 851_000_000 + index * 25_000, identity: id({ wacn: 0xbee00, sysId: 0x1a2, site }),
    good: 1, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null, adjacent: [], siteGroup: G,
  });
  transport.onMessage({ type: "status", status: { systems: [status("site1", 0, 1), status("site2", 1, 2)], recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: [] });
}
const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
const tgList = () => text(renderToString(<Talkgroups s={snapshot()} />));
const concluded = (c: CallEntry) => transport.onMessage({ type: "concluded", entry: c });
const showing = () => text(renderToString(<ListenPage />));

test("Listen: one talkgroup list for the sites of a system, and a talkgroup switched on plays from either site", () => {
  twoSites();
  const list = tgList();
  assert.match(list, /site1 \+ site2 multi-site/);
  assert.equal(list.match(/Fire Dispatch/g)?.length, 1, "TG 100 once, the first site's name for it");
  assert.match(list, /EMS/);
  assert.match(list, /Tac 3/);
  assert.equal(list.match(/of \d+ talkgroups/g)?.length, 1, "one system, not one per site");
  assert.match(list, /3 of 3 talkgroups/);

  // Everything off, then only Fire Dispatch on.
  setAll(false, [G], [`${G}:100`, `${G}:200`, `${G}:300`]);
  setTalkgroupsOn([`${G}:100`], true, [`${G}:100`, `${G}:200`, `${G}:300`]);
  setFeed(true);
  concluded(call("site2", 200, "EMS", 1));
  assert.match(showing(), /Scanning…/, "EMS is off");
  concluded(call("site2", 100, "Fire Disp", 2));
  assert.match(showing(), /Fire Disp .*TGID 100/, "kept from site2, plays");

  // Avoided, it doesn't play; switched on again, it does.
  avoid(0);
  concluded(call("site1", 100, "Fire Dispatch", 3));
  assert.match(showing(), /Scanning…/);
  setTalkgroupsOn([`${G}:100`], true, [`${G}:100`, `${G}:200`, `${G}:300`]);
  concluded(call("site1", 100, "Fire Dispatch", 4));
  assert.match(showing(), /Fire Dispatch .*TGID 100/);
  setFeed(false);
});
