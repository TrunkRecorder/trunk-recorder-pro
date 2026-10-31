// Importing a system from the systems directory: its bundle, the link that
// brings it, finding a system the config already has, merging talkgroups
// without replacing anyone's names, and adding or updating. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, normalizeSystem } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import { addSystem, findExisting, freeShortName, ImportDialog, importUrl, parseBundle, updateSystem, type Bundle } from "./directoryImport.tsx";
import type { Config, Talkgroup, ToRecorder } from "./protocol.ts";
import { mergeTalkgroups, readTalkgroupFile, talkgroup } from "./talkgroups.ts";

/** DC Fire and EMS as the directory hands it over (GET /systems/{id}/config). */
const bundle = (): Bundle => ({
  format: "trp-system",
  v: 1,
  source: { directory: "https://trunkrecorder.pro/directory", id: "sys_dcfd", rev: 12, url: "https://trunkrecorder.pro/directory/systems/sys_dcfd" },
  system: {
    shortName: "dc-fire-and-ems", name: "DC Fire and EMS", type: "p25", enabled: true, directory: { id: "sys_dcfd" },
    fcc: { "857987500": { callSign: "WPXT459", from: "air" } },
    sites: [
      { enabled: true, controlChannelsHz: [857_987_500, 855_462_500], modulation: "auto", expect: { nac: 0x443, wacn: 0xbee00, sysId: 0x445, rfss: 1, site: 3 }, voiceChannelsHz: [], directoryId: "site_13" },
      { enabled: true, controlChannelsHz: [851_000_000], modulation: "auto", expect: { wacn: 0xbee00, sysId: 0x445, rfss: 1, site: 4 }, voiceChannelsHz: [], directoryId: "site_14" },
    ],
  },
  talkgroupsCsv: "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Priority\n716,2cc,Engine 16 Tac,D,,Fire Dispatch,,\n801,321,Dispatch,D,,,,\n",
});

let version = 1000;
/** An edit is sent 300 ms after the last change (and the config isn't taken till then): wait it out. */
const sent300 = () => new Promise((r) => setTimeout(r, 350));
/** Connected with config `c` (a newer version than any before); what the page sends is kept in `sent`. */
function connected(c: Config, talkgroups: Record<string, Talkgroup[]> = {}): ToRecorder[] {
  const sent: ToRecorder[] = [];
  transport.send = (m: ToRecorder) => {
    sent.push(m);
    // (The recorder takes a config edit: as it would answer.)
    if (m.type === "setConfig") {
      version += 1;
      transport.onMessage({ type: "configSaved", version } as never);
      transport.onMessage({ type: "config", config: m.config, version } as never);
    }
  };
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: c, version: (version += 100) });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service"] });
  for (const [system, rows] of Object.entries(talkgroups)) transport.onMessage({ type: "talkgroups", system, talkgroups: rows });
  return sent;
}

test("a bundle is the directory's trp-system v1; anything else isn't one", () => {
  assert.ok(parseBundle(bundle()));
  assert.equal(parseBundle({ ...bundle(), format: "trunk-recorder" }), null);
  assert.equal(parseBundle({ ...bundle(), v: 2 }), null);
  assert.equal(parseBundle({ systems: [] }), null);
  assert.equal(parseBundle(null), null);
});

test("a link's import: the bundle's URL, or (an older link) a directory system ID and its sites", () => {
  const url = "https://trunkrecorder.pro/directory/api/v1/systems/sys_dcfd/config?sites=site_13";
  assert.equal(importUrl(`#import=${encodeURIComponent(url)}`, "https://trunkrecorder.pro"), url);
  assert.equal(importUrl("#import=sys_dcfd&sites=site_13,site_14", "https://trunkrecorder.pro/"), "https://trunkrecorder.pro/directory/api/v1/systems/sys_dcfd/config?sites=site_13,site_14");
  assert.equal(importUrl("#import=javascript:alert(1)", "https://trunkrecorder.pro"), null);
  assert.equal(importUrl("#something", "https://trunkrecorder.pro"), null);
});

test("the system the config already has: by its directory record, its identity, or a control channel", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "other", type: "p25", controlChannelsHz: [770_000_000] })];
  assert.equal(findExisting(c, bundle()), -1);
  c.systems.push(normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [855_462_500] }));
  assert.equal(findExisting(c, bundle()), 1, "a control channel");
  c.systems[1] = normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [860_000_000], expect: { wacn: 0xbee00, sysId: 0x445 } });
  assert.equal(findExisting(c, bundle()), 1, "its identity");
  c.systems[0].directory = { id: "sys_dcfd" };
  assert.equal(findExisting(c, bundle()), 0, "its directory record first");
  assert.equal(findExisting({ ...c, systems: [normalizeSystem({ shortName: "x", type: "smartnet", controlChannelsHz: [855_462_500] })] }, bundle()), -1, "another type isn't it");
});

test("a short name none has", () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "dc-fire-and-ems", type: "p25" }), normalizeSystem({ shortName: "dc-fire-and-ems-2", type: "p25" })];
  assert.equal(freeShortName(c, "dc-fire-and-ems"), "dc-fire-and-ems-3");
  assert.equal(freeShortName(c, "WMATA Radio!"), "wmata-radio");
});

test("talkgroups merged: only those it lacks added, after its own; its names, priorities and ignores stay", () => {
  const mine = [talkgroup(716, { alphaTag: "E16 TAC (mine)", priority: 2 }), talkgroup(900, { alphaTag: "Mine only", ignore: true })];
  const theirs = readTalkgroupFile(bundle().talkgroupsCsv).talkgroups;
  const m = mergeTalkgroups(mine, theirs);
  assert.equal(m.added, 1);
  assert.deepEqual(m.talkgroups.slice(0, 2), mine, "its rows as they were");
  assert.deepEqual(m.talkgroups.map((t) => [t.number, t.alphaTag]), [[716, "E16 TAC (mine)"], [900, "Mine only"], [801, "Dispatch"]]);
  assert.deepEqual(mergeTalkgroups([], theirs), { talkgroups: theirs, added: 2 });
  assert.equal(mergeTalkgroups(m.talkgroups, theirs).added, 0);
});

test("added: the sites chosen, its licenses and link, and its talkgroups as its file", async () => {
  const sent = connected(defaultConfig());
  addSystem(bundle(), "dcfd", [0], {});
  const s = snapshot().config!.systems.find((x) => x.shortName === "dcfd")!;
  assert.equal(s.sites.length, 1);
  assert.equal(s.sites[0].directoryId, "site_13");
  assert.deepEqual(s.directory, { id: "sys_dcfd" }, "the link only: the version is sharing's (sharing.json)");
  assert.equal(s.fcc?.["857987500"].callSign, "WPXT459");
  const tg = sent.find((m) => m.type === "setTalkgroups");
  assert.ok(tg && tg.type === "setTalkgroups" && tg.system === "dcfd" && tg.talkgroups.some((t) => t.alphaTag === "Engine 16 Tac"));
  await sent300();
  assert.ok(sent.some((m) => m.type === "setConfig"), "saved");
});

test("updated: the link, a site it has gets its directory ID and channels, the other is added; nothing of the user's replaced", async () => {
  const c = defaultConfig();
  c.systems = [normalizeSystem({ shortName: "dcfd", name: "My DCFD", type: "p25", controlChannelsHz: [855_462_500], fcc: { "857987500": { callSign: "KTYPED1" } } })];
  const sent = connected(c, { dcfd: [talkgroup(716, { alphaTag: "Mine" })] });
  const added = updateSystem(bundle(), 0, [0, 1], snapshot().talkgroups);
  const s = snapshot().config!.systems[0];
  assert.equal(s.name, "My DCFD");
  assert.deepEqual(s.directory, { id: "sys_dcfd" }, "the link only: the version is sharing's (sharing.json)");
  assert.equal(s.sites.length, 2);
  assert.equal(s.sites[0].directoryId, "site_13");
  assert.deepEqual(s.sites[0].controlChannelsHz, [855_462_500, 857_987_500]);
  assert.equal(s.sites[0].expect.sysId, 0x445);
  assert.equal(s.sites[1].directoryId, "site_14");
  assert.equal(s.fcc?.["857987500"].callSign, "KTYPED1", "a license typed stays");
  assert.equal(added, 1);
  const tg = sent.filter((m) => m.type === "setTalkgroups").pop();
  assert.ok(tg && tg.type === "setTalkgroups");
  assert.deepEqual(tg.talkgroups.map((t) => [t.number, t.alphaTag]), [[716, "Mine"], [801, "Dispatch"]]);
  await sent300();
});

test("the dialog: what comes, its sites; and, when the config has it, updating it", () => {
  const c = defaultConfig();
  connected(c);
  const text = (h: string) => h.replace(/<!-- -->/g, "");
  let html = text(renderToString(<ImportDialog bundle={bundle()} onClose={() => {}} />));
  assert.ok(html.includes("2 named talkgroups") && html.includes("1 FCC license") && html.includes("857.9875"), html);
  assert.ok(html.includes("Short name") && html.includes("Add it"));
  c.systems = [normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [857_987_500] })];
  connected(c);
  html = text(renderToString(<ImportDialog bundle={bundle()} onClose={() => {}} />));
  assert.ok(html.includes("You have it already: dcfd") && html.includes("Update dcfd"), html);
});
