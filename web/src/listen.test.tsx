// The Listen page's scanner (listen.ts): a system's calls heard whichever of
// its sites they were kept from, and talkgroups switched on are heard. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import { ListenPage, Talkgroups } from "./dash/Listen.tsx";
import { avoid, readSelection, setAll, setFeed, setTalkgroupsOn, wanted } from "./listen.ts";
import type { CallEntry, EngineStatus, FromRecorder, Identity, SiteStatus } from "./protocol.ts";

const CSV = (rows: string) => `Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n${rows}`;
const call = (site: string, talkgroup: number, tag = "", at = Date.now()): CallEntry =>
  ({ path: `dcfd/${talkgroup}-${at}`, record: { short_name: "dcfd", site, talkgroup, talkgroup_tag: tag, start_time_ms: at, call_length_ms: 4000, encrypted: 0 } }) as unknown as CallEntry;

test("readSelection: what this browser kept, or none", () => {
  const kept = { offSystems: ["other"], offTalkgroups: ["dcfd:200", 7], hold: { system: "dcfd", talkgroup: 9 }, avoid: { "dcfd:4": 0 } };
  const sel = readSelection(kept);
  assert.deepEqual(sel, { offSystems: ["other"], offTalkgroups: ["dcfd:200"], hold: { system: "dcfd", talkgroup: 9 }, avoid: { "dcfd:4": 0 } });
  assert.deepEqual(readSelection(null), { offSystems: [], offTalkgroups: [], hold: null, avoid: {} });
  assert.ok(!wanted({ ...sel, hold: null }, call("south", 200)), "kept from either site, the system's choice");
});

/** The recorder says hello with dcfd, a system of two sites, and is running them. */
function twoSites(): void {
  const c = defaultConfig();
  const dcfd = newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_000_000] });
  dcfd.sites.push({ ...dcfd.sites[0], name: "south", controlChannelsHz: [851_500_000] });
  c.systems.push(dcfd);
  transport.onConnection(true);
  // The greeting: how things are, then hello.
  const greeting: FromRecorder[] = [
    { type: "state", phase: "running", error: null, ended: false },
    { type: "config", config: c, version: 1 },
    { type: "history", entries: [] },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [] },
  ];
  for (const m of greeting) transport.onMessage(m);
  transport.onMessage({ type: "talkgroups", system: "dcfd", csv: CSV("100,64,Fire Dispatch,D,Fire,,\n200,c8,EMS,D,EMS,,\n300,12c,Tac 3,D,Tac,,") });
  const id = (site: number): Identity => ({ nac: null, wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site });
  const site = (index: number, name: string): SiteStatus =>
    ({ index, name, nowS: 1, controlChannelHz: 851_000_000 + index * 500_000, identity: id(index + 1), good: 1, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null, adjacent: [] });
  const systems = [{ index: 0, shortName: "dcfd", activeCalls: 0, recording: 0, callsConcluded: 0, sites: [site(0, "dcfd"), site(1, "south")] }];
  transport.onMessage({ type: "status", status: { systems, recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: [] });
}
const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
const tgList = () => text(renderToString(<Talkgroups s={snapshot()} />));
const concluded = (c: CallEntry) => transport.onMessage({ type: "concluded", entry: c });
const showing = () => text(renderToString(<ListenPage />));

test("Listen: a system of sites has one talkgroup list, and a talkgroup switched on plays from either site", () => {
  twoSites();
  const list = tgList();
  assert.match(list, /dcfd 2 sites 3 of 3 talkgroups/);
  assert.equal(list.match(/of \d+ talkgroups/g)?.length, 1, "one system, not one per site");

  // Everything off, then only Fire Dispatch on.
  const known = ["dcfd:100", "dcfd:200", "dcfd:300"];
  setAll(false, ["dcfd"], known);
  setTalkgroupsOn(["dcfd:100"], true, known);
  setFeed(true);
  concluded(call("south", 200, "EMS", 1));
  assert.match(showing(), /Scanning…/, "EMS is off");
  concluded(call("south", 100, "Fire Dispatch", 2));
  assert.match(showing(), /Fire Dispatch .*TGID 100/, "kept from south, plays");

  // Avoided, it doesn't play; switched on again, it does.
  avoid(0);
  concluded(call("dcfd", 100, "Fire Dispatch", 3));
  assert.match(showing(), /Scanning…/);
  setTalkgroupsOn(["dcfd:100"], true, known);
  concluded(call("dcfd", 100, "Fire Dispatch", 4));
  assert.match(showing(), /Fire Dispatch .*TGID 100/);
  setFeed(false);
});
