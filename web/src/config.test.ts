// Systems with sites (config.ts): the sites recorded, their names, and
// systems set up apart that are one on the air. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { activeSites, defaultConfig, foundRadio, deviceId, DRIVER_KINDS, importTrunkRecorderConfig, nameProblem, newSiteName, newSystem, normalizeSystem, planCenters, radioName, rateFor, rateOk, ratesText, resolvedCenters, restartKey, usableHalfWidth, sameOnTheAir, siteNameOf, SOURCE_KINDS, sourceFor, sourceKey, startProblem } from "./config.ts";
import type { Config, FoundRadio, Radios, SiteIdentity, SiteStatus, Source, SystemStatus } from "./protocol.ts";

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
  const id = (x: Partial<SiteIdentity>): SiteIdentity => ({ nac: null, wacn: null, sysId: null, rfss: null, site: null, ...x });
  const site = (name: string, identity: Partial<SiteIdentity>): SiteStatus =>
    ({ index: 0, name, nowS: 0, controlChannelHz: null, identity: id(identity), good: 0, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null });
  const sys = (shortName: string, sites: SiteStatus[]): SystemStatus => ({ index: 0, shortName, activeCalls: 0, recording: 0, conventionalOpen: 0, callsConcluded: 0, sites });
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

test("the first-run guide places radios with the recorder's planner: the busiest channels, a capture file left alone", () => {
  const rtl = { rateHz: 2_400_000 };
  const voice = [
    { hz: 854_000_000, weight: 50 },
    { hz: 854_500_000, weight: 40 },
  ];
  const [first, second, file] = planCenters([rtl, rtl, { ...rtl, fixedHz: 860_000_000 }], 0, 851_400_000, [851_000_000], voice);
  assert.equal(first, 851_400_000);
  assert.ok(second !== null && voice.every((v) => Math.abs(v.hz - second) <= usableHalfWidth(rtl.rateHz) && Math.abs(v.hz - second) >= 25_000), String(second));
  assert.equal(file, 860_000_000);
});

test("SOURCE_KINDS: each kind's new source is that kind, and names its device where it has one", () => {
  for (const [type, k] of Object.entries(SOURCE_KINDS) as [keyof typeof SOURCE_KINDS, (typeof SOURCE_KINDS)[keyof typeof SOURCE_KINDS]][]) {
    assert.equal(k.fresh().type, type);
    if (k.device) assert.equal(deviceId(sourceFor(type, "x1")), "x1", type);
  }
  assert.deepEqual(DRIVER_KINDS, ["usrp", "airspy", "bladerf", "soapy"]);
  assert.equal(sourceKey(sourceFor("bladerf", "f12ce103")), "bladerf:f12ce103");
});

test("importing a bladeRF already set up keeps one source", () => {
  const tr = JSON.stringify({ sources: [{ center: 857e6, rate: 8e6, gain: 40, device: "bladerf=f12ce103" }], systems: [] });
  const once = importTrunkRecorderConfig(tr, defaultConfig()).config;
  const twice = importTrunkRecorderConfig(tr, once, {}, true).config;
  assert.equal(twice.sources.filter((x) => x.type === "bladerf").length, 1);
});

test("restartKey: a radio's gain doesn't need a restart on the desktop recorder; other settings do", () => {
  const base = defaultConfig();
  const c = (src: Partial<Source> & { type: Source["type"] }) => ({ ...base, sources: [{ ...SOURCE_KINDS[src.type].fresh(), ...src } as Source] });
  const rtl = c({ type: "rtlsdr", gainDb: 30 });
  assert.equal(restartKey(c({ type: "rtlsdr", gainDb: 20 }), true), restartKey(rtl, true), "an RTL-SDR's gain");
  assert.equal(restartKey(c({ type: "rtlsdr", agc: true }), true), restartKey(rtl, true), "an RTL-SDR's AGC");
  assert.notEqual(restartKey(c({ type: "rtlsdr", gainDb: 20 }), false), restartKey(rtl, false), "the browser build restarts for it");
  assert.notEqual(restartKey(c({ type: "rtlsdr", gainDb: 30, centerHz: 851e6 }), true), restartKey(rtl, true), "a retune");
  const blade = c({ type: "bladerf", gainDb: 30 });
  assert.equal(restartKey(c({ type: "bladerf", gainDb: 42 }), true), restartKey(blade, true), "a bladeRF's gain");
  assert.notEqual(restartKey(c({ type: "bladerf", gainDb: 30, agc: true }), true), restartKey(blade, true), "a bladeRF's AGC waits for the start");
});

test("foundRadio: what the picked radio runs at, once its driver has asked it; a new source starts at one of them", () => {
  const r2 = { serial: "735863DC2F650AE3", label: "Airspy R2 · SN 735863DC2F650AE3", model: "R2", rates: [10_000_000, 2_500_000] };
  const mini = { serial: "B", label: "Airspy Mini · SN B", model: "Mini", rates: [6_000_000, 3_000_000] };
  const busy = { serial: "C", label: "Airspy · SN C" };
  const blade = { serial: "f12ce1037830a1b27f3ceeba1f521413", label: "bladeRF 2.0 · SN f12ce1037830…", model: "bladeRF 2.0", rateRange: [520_834, 61_440_000] as [number, number] };
  const driver = (devices: FoundRadio[] | null) => ({ available: true, detail: "", devices });
  const radios: Radios = { usrp: driver(null), airspy: driver([r2, mini, busy]), bladerf: driver([blade]) };
  const airspy = (serial: string) => ({ ...(SOURCE_KINDS.airspy.fresh() as Extract<Source, { type: "airspy" }>), serial });
  assert.equal(foundRadio(radios, airspy(r2.serial)), r2);
  assert.equal(foundRadio(radios, airspy("")), r2, "the first found");
  assert.equal(foundRadio(radios, airspy("Z")), undefined, "one not connected");
  assert.equal(foundRadio(undefined, airspy(r2.serial)), undefined);
  assert.equal(foundRadio(radios, { ...(SOURCE_KINDS.bladerf.fresh() as Extract<Source, { type: "bladerf" }>), serial: "F12CE103" }), blade, "a bladeRF by the start of its serial");
  assert.equal(foundRadio(radios, SOURCE_KINDS.rtlsdr.fresh()), undefined, "no driver's");

  assert.ok(!rateOk(6_000_000, r2) && rateOk(2_500_000, r2) && rateOk(6_000_000, busy), "unknown: fine");
  assert.equal(rateFor(6_000_000, r2), 10_000_000, "a rate it can't do: its first");
  assert.equal(rateFor(2_500_000, r2), 2_500_000, "one it can: kept");
  assert.equal(rateFor(6_000_000, busy), 6_000_000, "unknown: kept");
  assert.equal(rateFor(80_000_000, blade), 61_440_000, "past a range: its end");
  assert.equal(ratesText(r2), "10 or 2.5");
  assert.equal(ratesText(blade), "0.52–61.44");
  assert.equal(ratesText(busy), null);
  assert.equal(radioName("airspy", r2), "Airspy R2");
  assert.equal(radioName("bladerf", blade), "bladeRF 2.0");
  assert.equal(radioName("airspy", busy), "Airspy");

  assert.equal(sourceFor("airspy", r2.serial, r2).rateHz, 10_000_000);
  assert.equal(sourceFor("airspy", mini.serial, mini).rateHz, 6_000_000);
  assert.equal(sourceFor("airspy", "C", busy).rateHz, 6_000_000, "not asked: the default");
  assert.equal(sourceFor("bladerf", blade.serial, blade).rateHz, 10_000_000, "in its range: the default");
});
