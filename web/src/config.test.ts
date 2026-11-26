// Systems with sites (config.ts): the sites recorded, their names, and
// systems set up apart that are one on the air. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { kindLabel, siteLockFields, systemType, activeSites, adjacentIdentity, airspyGains, defaultConfig, desktopOnly, desktopOnlyWhy, hasBiasT, highestRate, kindDesktopOnly, kindNeedsDriver, networkAddress, networkRadio, onNetwork, detectRadios, pickable, foundRadio, deviceId, DRIVER_KINDS, importTrunkRecorderConfig, nameProblem, newHydrasdr, newSiteName, newSystem, normalizeSystem, placeSources, radioName, rateFor, rateOk, ratesText, resolvedCenters, roaming, usableHalfWidth, sameOnTheAir, sameSite, siteAddress, siteNameOf, sourceAdvice, SOURCE_KINDS, sourceFor, sourceKey, startProblem } from "./config.ts";
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

test("a neighbouring site: by the fields its protocol states, and how a radio roams to it", () => {
  const p25 = adjacentIdentity({ wacn: 0xbee00, sysId: 0x954, rfss: 1, site: 1, freqHz: 854_000_000 });
  assert.equal(siteAddress(p25), "BEE00:954:1-1");
  assert.equal(siteAddress(adjacentIdentity({ wacn: 1, sysId: 0x97, rfss: 1, site: 1, freqHz: 0 })), "00001:097:1-1");
  const here = { nac: 0x1f1, wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: 2 };
  assert.equal(roaming(here, p25), "inter-system");
  assert.equal(roaming(here, { ...p25, wacn: 1 }), "inter-WACN");
  assert.equal(roaming(here, { ...p25, sysId: 0x1a2 }), "intra-system");
  assert.ok(sameSite(here, { wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: 2 }) && !sameSite(here, { wacn: 1, sysId: 0x1a2, rfss: 1, site: 2 }));
  const nxdn = adjacentIdentity({ wacn: 0, sysId: 0x1a, rfss: 0, site: 5, freqHz: 0 }, "nxdn");
  assert.deepEqual(nxdn, { sysId: 0x1a, site: 5 }, "no WACN or RFSS to lock on");
  assert.equal(siteAddress(nxdn), "SysID 1A · site 5");
  assert.equal(roaming({ sysId: 0x1a, site: 4 }, nxdn), "intra-system");
});

test("sameOnTheAir: systems set up apart whose sites announce one system", () => {
  const c: Config = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "dcfd" }), newSystem(c, { shortName: "dcfd2" }), newSystem(c, { shortName: "other", expect: { wacn: 0xbee00, sysId: 0x9 } }), newSystem(c, { shortName: "cm", type: "dmr" }));
  const id = (x: Partial<SiteIdentity>): SiteIdentity => ({ nac: null, wacn: null, sysId: null, rfss: null, site: null, ...x });
  const site = (name: string, identity: Partial<SiteIdentity>): SiteStatus =>
    ({ index: 0, name, nowS: 0, controlChannelHz: null, source: null, identity: id(identity), good: 0, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null, adjacent: [] });
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
  assert.deepEqual(talkgroups.county.map((t) => [t.number, t.alphaTag]), [[100, "Fire"]], "its talkgroups, apart from the config");
  assert.ok(!JSON.stringify(config).includes("Fire"));
  assert.ok(todo.some((t) => t.kind === "siteLock" && t.system === "county" && t.site === 1), JSON.stringify(todo));
});

test("importTrunkRecorderConfig: talkgroup allow / deny lists come with the plugins' talkgroup menu set", () => {
  const tr = {
    sources: [{ center: 851e6, rate: 2.4e6, driver: "osmosdr", device: "rtl=0" }],
    systems: [
      { shortName: "a", type: "p25", control_channels: [851012500], broadcastifyApiKey: "k", broadcastifySystemId: 4, broadcastifyWhitelist: ["10*", 205], broadcastifyDeny: [109] },
      { shortName: "b", type: "p25", control_channels: [852012500] },
    ],
    plugins: [{ name: "rdioscanner", library: "librdioscanner_uploader.so", server: "http://rdio", systems: [{ shortName: "b", apiKey: "r", systemId: 2, talkgroupDeny: ["300-399"] }] }],
  };
  const { plugins } = importTrunkRecorderConfig(JSON.stringify(tr), defaultConfig());
  const sys = (id: string, s: string) => plugins.find((p) => p.id === id)?.systems[s];
  assert.deepEqual(sys("broadcastify", "a"), { apiKey: "k", systemId: 4, talkgroupMode: "only", talkgroupAllow: ["10*", "205"], talkgroupDeny: ["109"] });
  assert.deepEqual(sys("rdioscanner", "b"), { apiKey: "r", systemId: 2, talkgroupMode: "allBut", talkgroupDeny: ["300-399"] });
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
  const [first, second, file] = placeSources([{ ...rtl, fixedHz: 851_400_000 }, rtl, { ...rtl, fixedHz: 860_000_000 }], { groups: [{ channels: voice, need: [851_000_000] }] });
  assert.equal(first, 851_400_000);
  assert.ok(second !== null && voice.every((v) => Math.abs(v.hz - second) <= usableHalfWidth(rtl.rateHz) && Math.abs(v.hz - second) >= 25_000), String(second));
  assert.equal(file, 860_000_000);
});

test("SOURCE_KINDS: each kind's new source is that kind, and names its device where it has one", () => {
  for (const [type, k] of Object.entries(SOURCE_KINDS) as [keyof typeof SOURCE_KINDS, (typeof SOURCE_KINDS)[keyof typeof SOURCE_KINDS]][]) {
    assert.equal(k.fresh().type, type);
    if (k.device) assert.equal(deviceId(sourceFor(type, "x1")), "x1", type);
  }
  assert.deepEqual(DRIVER_KINDS, ["usrp", "airspy", "hydrasdr", "bladerf", "hackrf", "soapy"]);
  assert.equal(sourceKey(sourceFor("bladerf", "f12ce103")), "bladerf:f12ce103");
});

const none = () => ({ available: true, detail: "", devices: [] });

test("rtl_tcp and SpyServer: radios on the network, found by their address, in the desktop app only", () => {
  // An RTL-SDR on the network is an RTL-SDR with an address: the same kind, gain and bias-T as on USB.
  const dongle = SOURCE_KINDS.rtlsdr.fresh();
  const net = { ...dongle, address: " Pi.local " } as Config["sources"][number];
  assert.deepEqual(SOURCE_KINDS.spyserver.fresh(), { type: "spyserver", address: "", centerHz: 0, rateHz: 2_500_000, iqFormat: "cu8", gain: 14, ppm: 0 });
  assert.equal(SOURCE_KINDS.spyserver.driver, undefined, "nothing to install");
  assert.ok(SOURCE_KINDS.spyserver.network && !kindNeedsDriver("spyserver") && kindDesktopOnly("spyserver"), "a radio on the network, not a driver's");
  assert.deepEqual([networkAddress(net), networkAddress(dongle), networkAddress(SOURCE_KINDS.spyserver.fresh())], ["Pi.local", undefined, ""]);
  assert.ok(onNetwork(net) && !onNetwork(dongle) && onNetwork(SOURCE_KINDS.spyserver.fresh()) && !onNetwork(SOURCE_KINDS.hackrf.fresh()));
  assert.ok(desktopOnly(net) && !desktopOnly(dongle), "the browser opens a dongle on USB, not one on the network");
  assert.equal(desktopOnlyWhy(net), "An RTL-SDR on the network needs the desktop app.");
  assert.equal(desktopOnlyWhy(SOURCE_KINDS.spyserver.fresh()), "SpyServer needs the desktop app.");
  assert.equal(deviceId(sourceFor("spyserver", "10.0.0.5:5555")), "10.0.0.5:5555");
  assert.deepEqual([sourceKey(net), sourceKey(dongle)], ["rtlsdr:Pi.local", "rtlsdr:"], "by its address, not a serial");
  // What a radio on the network said is in the recorder's list of radios (`network`), by its address as given.
  const sp = { ...SOURCE_KINDS.spyserver.fresh(), address: " pi:5555 " } as Config["sources"][number];
  assert.equal(foundRadio(null, sp), undefined, "not listed yet");
  const radios: Radios = {
    usrp: none(),
    hackrf: none(),
    airspy: none(),
    hydrasdr: none(),
    bladerf: none(),
    network: {
      available: true,
      detail: "built in",
      devices: [
        { kind: "spyserver", address: "pi:5555", label: "Airspy R2 / Mini at pi:5555", model: "Airspy R2 / Mini", rates: [2_500_000, 1_250_000], gains: [0, 1, 2] },
        { kind: "spyserver", address: "gone:5555", label: "gone:5555", error: "can't connect" },
        { kind: "rtlsdr", address: "Pi.local", label: "R820T at Pi.local", model: "R820T", rates: [2_400_000] },
      ],
    },
  };
  assert.deepEqual(foundRadio(radios, sp)?.rates, [2_500_000, 1_250_000]);
  assert.equal(foundRadio(radios, net)?.model, "R820T", "an RTL-SDR on the network, by its address");
  assert.equal(foundRadio(radios, { ...net, type: "rtlsdr", address: "" } as Config["sources"][number]), undefined, "one on USB isn't a network radio");
  assert.equal(networkRadio(radios, { ...sp, address: "gone:5555" } as Config["sources"][number])?.error, "can't connect");
  assert.equal(highestRate(foundRadio(radios, sp)), 2_500_000, "the rate a source starts at once its server has said");
  assert.equal(highestRate({ label: "", rates: [1_250_000, 10_000_000, 2_500_000] }), 10_000_000);
  assert.equal(highestRate(undefined), undefined);
  assert.equal(ratesText(foundRadio(radios, sp)), "2.5 or 1.25");
  assert.equal(ratesText({ label: "", rates: [2_048_000, 625_000, 312_500] }), "2.048 or 0.625 or 0.313", "to the kHz, the zeros that mean nothing left off");
  assert.ok(hasBiasT(net) && hasBiasT(dongle) && !hasBiasT(SOURCE_KINDS.spyserver.fresh()), "a dongle's bias-T, on USB or the network; not a server's");
});

test("a new HackRF source: the driver's gain defaults, a native kind with a device list", () => {
  const h = SOURCE_KINDS.hackrf.fresh();
  assert.deepEqual(h, { type: "hackrf", serial: "", centerHz: 0, rateHz: 8_000_000, lnaDb: 16, vgaDb: 20, amp: false, biasTee: false, ppm: 0 });
  assert.equal(SOURCE_KINDS.hackrf.label, "HackRF");
  assert.equal(SOURCE_KINDS.hackrf.driver, undefined, "nothing to install");
  assert.equal(sourceKey(sourceFor("hackrf", "0000a1")), "hackrf:0000a1");
  const none = { available: true, detail: "", devices: [] };
  const radios: Radios = { usrp: none, hackrf: { available: true, detail: "built in", devices: [{ serial: "0000a1", label: "HackRF One · SN 0000a1" }] }, airspy: none, hydrasdr: none, bladerf: none };
  assert.equal(foundRadio(radios, h)?.serial, "0000a1", "the first, when none is named");
  assert.equal(detectRadios([], radios)[0].kind, "hackrf");
});

test("a HydraSDR: found by its driver, started at a rate it runs at, its gain as an Airspy's; imported from gr-osmosdr's hydrasdr", () => {
  const none = { available: true, detail: "", devices: [] };
  const rfone = { serial: "1A2B3C4D5E6F7081", label: "HydraSDR RFOne · SN 1A2B3C4D5E6F7081", model: "RFOne", rates: [10_000_000, 5_000_000, 2_500_000] };
  const radios: Radios = { usrp: none, hackrf: none, airspy: none, hydrasdr: { available: true, detail: "built in", devices: [rfone] }, bladerf: none };
  const [found] = detectRadios([], radios);
  assert.equal(found.kind, "hydrasdr");
  assert.equal(found.key, "hydrasdr:1A2B3C4D5E6F7081");
  assert.deepEqual(found.source, { ...newHydrasdr(), serial: "1A2B3C4D5E6F7081" });
  assert.equal(radioName("hydrasdr", rfone), "HydraSDR RFOne");
  const tr = JSON.stringify({ sources: [{ center: 857e6, rate: 2.5e6, device: "hydrasdr=0,bias=1", lnaGain: 12, mixGain: 8, ifGain: 9 }], systems: [] });
  const s = importTrunkRecorderConfig(tr, defaultConfig()).config.sources.find((x) => x.type === "hydrasdr");
  assert.ok(s && airspyGains(s));
  assert.deepEqual([s.rateHz, s.gainMode, s.lnaStep, s.mixerStep, s.vgaStep, s.biasTee, s.type === "hydrasdr" && s.rfPort], [2_500_000, "manual", 12, 8, 9, true, 0]);
});

test("importing a bladeRF already set up keeps one source", () => {
  const tr = JSON.stringify({ sources: [{ center: 857e6, rate: 8e6, gain: 40, device: "bladerf=f12ce103" }], systems: [] });
  const once = importTrunkRecorderConfig(tr, defaultConfig()).config;
  const twice = importTrunkRecorderConfig(tr, once, {}, true).config;
  assert.equal(twice.sources.filter((x) => x.type === "bladerf").length, 1);
});

test("foundRadio: what the picked radio runs at, once its driver has asked it; a new source starts at one of them", () => {
  const r2 = { serial: "735863DC2F650AE3", label: "Airspy R2 · SN 735863DC2F650AE3", model: "R2", rates: [10_000_000, 2_500_000] };
  const mini = { serial: "B", label: "Airspy Mini · SN B", model: "Mini", rates: [6_000_000, 3_000_000] };
  const busy = { serial: "C", label: "Airspy · SN C" };
  const blade = { serial: "f12ce1037830a1b27f3ceeba1f521413", label: "bladeRF 2.0 · SN f12ce1037830…", model: "bladeRF 2.0", rateRange: [520_834, 61_440_000] as [number, number] };
  const driver = (devices: FoundRadio[] | null) => ({ available: true, detail: "", devices });
  const radios: Radios = { usrp: driver(null), hackrf: driver([]), airspy: driver([r2, mini, busy]), hydrasdr: driver([]), bladerf: driver([blade]) };
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
  assert.equal(ratesText(blade), "0.521–61.44");
  assert.equal(ratesText(busy), null);
  assert.equal(radioName("airspy", r2), "Airspy R2");
  assert.equal(radioName("bladerf", blade), "bladeRF 2.0");
  assert.equal(radioName("airspy", busy), "Airspy");

  assert.equal(sourceFor("airspy", r2.serial, r2).rateHz, 10_000_000);
  assert.equal(sourceFor("airspy", mini.serial, mini).rateHz, 6_000_000);
  assert.equal(sourceFor("airspy", "C", busy).rateHz, 6_000_000, "not asked: the default");
  assert.equal(sourceFor("bladerf", blade.serial, blade).rateHz, 10_000_000, "in its range: the default");
});

test("advice for the sources (the recorder's Config::advice): a typed-in centre that misses the site's voice channels moves, and can go back to auto", () => {
  const c = defaultConfig();
  c.sources = [{ type: "rtlsdr", serial: "", centerHz: 849_500_000, rateHz: 2_400_000, gainDb: 25, agc: false, biasTee: false, ppm: 0, bufLen: 0, buffers: 0 }];
  c.systems.push(newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_012_500] }));
  c.systems[0].sites[0].voiceChannelsHz = [851_512_500, 852_012_500];
  const a = sourceAdvice(c, [undefined]);
  assert.ok(a, "advice");
  assert.ok(a.sources[0].changed && a.sources[0].auto && a.after.reached > a.before.reached, JSON.stringify(a));
  c.sources[0].centerHz = a.sources[0].centerHz!;
  c.sources[0].rateHz = a.sources[0].rateHz;
  assert.equal(sourceAdvice(c, [undefined]), null, "taken: nothing more to say");
});

test("each kind of system is as the recorder's table says: named, and locked by its fields", () => {
  assert.deepEqual([kindLabel("edacs"), kindLabel("smartnet"), kindLabel(undefined)], ["EDACS", "SmartNet", "P25"]);
  assert.deepEqual([systemType("edacs"), systemType("nope"), systemType(3)], ["edacs", "p25", "p25"]);
  assert.deepEqual(siteLockFields({ type: "p25" }), ["nac", "wacn", "sysId", "rfss", "site"]);
  assert.deepEqual([siteLockFields({ type: "nxdn" }), siteLockFields({ type: "nxdn", nxdnType: "typeD" }), siteLockFields({ type: "dmr" })], [["sysId", "site"], [], []]);
});

test("importing a Trunk Recorder source on rtl_tcp: an RTL-SDR with its server's address, bias-T as gr-osmosdr says", () => {
  const tr = JSON.stringify({ sources: [{ center: 851e6, rate: 2.4e6, gain: 30, device: "rtl_tcp=pi.local:1234,bias=1" }, { center: 853e6, rate: 2.4e6, device: "rtl=00000002" }], systems: [] });
  const [net, usb] = importTrunkRecorderConfig(tr, defaultConfig()).config.sources;
  assert.ok(net.type === "rtlsdr" && usb.type === "rtlsdr");
  assert.deepEqual([net.address, net.serial, net.biasTee, onNetwork(net)], ["pi.local:1234", "", true, true]);
  assert.deepEqual([usb.address, usb.serial, usb.biasTee, onNetwork(usb)], [undefined, "00000002", false, false]);
});

test("found radios as a source picks them, every kind alike (Config::radio_picks): by its driver's list, twins kept and marked, not dropped", () => {
  const d = (index: number, serial: string) => ({ index, serial, product: "RTL2838UHIDIR" });
  const radios = { usrp: { available: false, detail: "" }, hackrf: { available: true, detail: "built in", devices: [{ serial: "0000a1", label: "HackRF One" }, { serial: "0000A1", label: "HackRF One" }] }, airspy: { available: false, detail: "" }, hydrasdr: { available: false, detail: "" }, bladerf: { available: false, detail: "" } } as unknown as Radios;
  const c = defaultConfig();
  c.sources = [{ ...SOURCE_KINDS.rtlsdr.fresh(), serial: "00000001" } as Config["sources"][number], SOURCE_KINDS.rtlsdr.fresh()];
  const devices = [d(0, "00000001"), d(1, ""), d(2, "")];
  assert.equal(detectRadios(devices, radios).length, 5, "two dongles with no serial: both listed");
  const p = pickable(c, 1, devices, radios);
  assert.deepEqual(p.of("rtlsdr").map((x) => x.unpickable), [{ why: "source", index: 0, title: "Source 1" }, { why: "noSerial", twin: null }, { why: "noSerial", twin: 1 }]);
  assert.deepEqual(p.of("hackrf").map((x) => x.unpickable?.why), ["sameSerial", "sameSerial"], "a hex serial in any case is one serial");
  assert.equal(p.freeSerial, "00000002");
  // Source 1's picks: Source 2 opens the first RTL-SDR found, so it may take any.
  assert.equal(pickable(c, 0, devices, radios).of("rtlsdr")[0].first?.title, "Source 2");
  assert.deepEqual(pickable(c, 0, [d(0, "")], null).of("rtlsdr"), [{ first: { index: 1, title: "Source 2" } }], "a lone dongle with no serial is the first found, and fine");
});
