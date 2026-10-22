// The dashboard pages as rendered (react-dom/server), from what the recorder
// sends: what a user relies on seeing there stays there. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem } from "../config.ts";
import { setView, snapshot, transport } from "../controller.ts";
import { clearSeries, feedSeries } from "../series.ts";
import { pageHealth } from "./data.ts";
import type { CallView, Config, EngineStatus, FromRecorder, SiteIdentity, SiteStatus, SystemStatus } from "../protocol.ts";
import { DecodePage } from "./Decode.tsx";
import { Overview } from "./Overview.tsx";
import { RadioPage } from "./Radio.tsx";

const site = (index: number, name: string, identity: Partial<SiteIdentity>): SiteStatus => ({
  index, name, nowS: 10, controlChannelHz: 851_000_000 + index * 500_000,
  identity: { nac: 0x1f1, wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: null, ...identity },
  good: 100, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null,
});
const system = (index: number, shortName: string, sites: SiteStatus[]): SystemStatus => ({ index, shortName, activeCalls: 0, recording: 0, callsConcluded: 0, sites });

/** The recorder says hello with `c` and is running these systems, these calls on the air. */
function running(c: Config, systems: SystemStatus[], calls: Partial<CallView>[] = []): void {
  // The greeting: how things are, then hello.
  const greeting: FromRecorder[] = [
    { type: "state", phase: "running", error: null, ended: false },
    { type: "config", config: c, version: 1 },
    { type: "history", entries: [] },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service", "drivers", "plugins", "interfaces", "host", "keys"] },
  ];
  clearSeries();
  for (const m of greeting) transport.onMessage(m);
  transport.onMessage({ type: "status", status: { systems, recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: calls as CallView[] });
}

/** dcfd, a system of two sites (dcfd and south), and another of one. */
function twoSites(calls: Partial<CallView>[] = []): void {
  const c = defaultConfig();
  const dcfd = newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_000_000] });
  dcfd.sites.push({ ...dcfd.sites[0], name: "south", controlChannelsHz: [851_500_000] });
  c.systems.push(dcfd, newSystem(c, { shortName: "other", controlChannelsHz: [852_000_000] }));
  running(c, [system(0, "dcfd", [site(0, "dcfd", { site: 1 }), site(1, "south", { site: 2 })]), system(1, "other", [site(2, "other", { sysId: 0x999, site: 1 })])], calls);
}

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
const titles = (html: string) => [...html.matchAll(/<h3>(.*?)<\/h3>/g)].map((m) => text(m[1]).trim());

test("Overview: a card per system; one of several sites has a row per site", () => {
  twoSites();
  setView("overview");
  const html = renderToString(<Overview />);
  assert.deepEqual(titles(html).filter((x) => ["dcfd", "south", "other"].includes(x)), ["dcfd", "other"], "a site isn't a card of its own");
  const page = text(html);
  assert.match(page, /Control channels .* 2 sites/);
  assert.match(page, /dcfd 851\.0+ .* south 851\.5+/);
  assert.match(page, /other P25 Control channel — msg\/s … · 852\.0+ MHz/, "a system of one site, as before");
});

test("Decode: a card per site; a system's page picks the site", () => {
  twoSites();
  setView("decode");
  assert.deepEqual(titles(renderToString(<DecodePage />)), ["dcfd / dcfd", "dcfd / south", "other"]);
  setView("decode", "dcfd", "south");
  const page = text(renderToString(<DecodePage />));
  assert.match(page, /All systems dcfd south dcfd \/ south .* 851\.50+ MHz/, "the site picker, then south's control channel");
  assert.match(page, /Site 1-2/);
  assert.doesNotMatch(page, /No system called/);
});

test("Decode: a system's page shows what its sites announce (decodeDetail), not before", () => {
  twoSites();
  setView("decode", "dcfd", "south");
  const before = text(renderToString(<DecodePage />));
  assert.ok(!before.includes("Patches") && !before.includes("Neighbouring sites"));
  const tg = (talkgroup: number, alphaTag = "") => ({ talkgroup, alphaTag });
  transport.onMessage({
    type: "decodeDetail", system: "dcfd", channels: [],
    sites: [{ index: 1, name: "south", adjacent: [{ sysId: 0x1a2, rfss: 1, site: 9, freqHz: 853_000_000 }], dmr: null, nxdn: null }],
    patches: [{ supergroup: tg(60001), members: [tg(100, "Fire Dispatch"), tg(200)] }],
  });
  const page = text(renderToString(<DecodePage />));
  assert.ok(page.includes("SG 60001") && page.includes("Fire Dispatch (100) + 200"), page);
  assert.ok(page.includes("site 1-9"), page);
});

test("Decode: a site's FCC call sign, once its control channel has said it", () => {
  const c = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_000_000] }));
  running(c, [system(0, "dcfd", [site(0, "dcfd", { site: 1 })])]);
  setView("decode", "dcfd");
  assert.ok(!text(renderToString(<DecodePage />)).includes("Call sign"));
  running(c, [system(0, "dcfd", [{ ...site(0, "dcfd", { site: 1 }), callsigns: [{ callSign: "KNJA391", freqHz: 859_087_500 }, { callSign: "KNJE616", freqHz: 859_987_500 }] }])]);
  assert.ok(text(renderToString(<DecodePage />)).includes("Call signs KNJA391 859.0875 , KNJE616 859.9875"), text(renderToString(<DecodePage />)));
});

test("Radio: a system's page has its calls on the air now, saying the site and the others carrying it, and its log; not another's", () => {
  twoSites([
    { id: 7, system: 0, systemName: "dcfd", site: "south", alsoOn: ["dcfd"], talkgroup: 100, alphaTag: "Fire", freqHz: 851_600_000, slot: null, state: "recording", reason: null, sources: [], startS: 1 },
    { id: 8, system: 1, systemName: "other", site: "other", talkgroup: 555, alphaTag: "Elsewhere", freqHz: 852_600_000, slot: null, state: "recording", reason: null, sources: [], startS: 1 },
  ]);
  setView("radio", "dcfd");
  const page = text(renderToString(<RadioPage />));
  assert.match(page, /Active calls Listen live .*100 Fire · south also on dcfd/);
  assert.doesNotMatch(page, /Elsewhere/, "another system's call isn't here");
  assert.match(page, /Control channel log/);
});

test("Radio: each system's card opens its system from anywhere on it, without an Open link", () => {
  twoSites();
  setView("radio");
  const html = renderToString(<RadioPage />);
  assert.equal((html.match(/class="card[^"]* card-opens sys-section"/g) ?? []).length, 2, "both systems' cards open them");
  assert.doesNotMatch(text(html), /Open dcfd/);
});

test("Radio: a system's page goes back as Decode's does, with the systems beside; a talkgroup's goes back to its system", () => {
  twoSites();
  setView("radio", "dcfd");
  const page = text(renderToString(<RadioPage />));
  assert.match(page, /^ ?← All systems dcfd other dcfd P25/, "the way back first, the systems beside it, then the system");
  setView("radio", "dcfd", "tg", 100);
  const tg = text(renderToString(<RadioPage />));
  assert.match(tg, /^ ?← dcfd dcfd P25/, "back to its system, no picker");
});

test("Radio: calls missed on one of several systems name it, in the navigation's light and on its card", () => {
  twoSites();
  feedSeries(Date.now() / 1000, { "sys/other/why/no_source": 1 });
  const h = pageHealth(snapshot()).radio;
  assert.equal(h.level, "warn");
  assert.match(h.why, /^other: .*outside every source's band/);
  setView("radio");
  const html = renderToString(<RadioPage />);
  assert.equal((html.match(/class="card card-warn card-opens sys-section"/g) ?? []).length, 1, "only other's card warns");
  assert.match(text(html), /other P25 Calls are being missed: their frequencies are outside every source/);
  setView("radio", "dcfd");
  assert.match(renderToString(<RadioPage />), /other<span class="light light-warn"/, "the picker marks the other system");
  setView("overview");
  const overview = renderToString(<Overview />);
  assert.match(overview, /<h3><span class="sys-tag">.*?other<\/span><\/h3>/, "the Overview's system card names it by its tag");
  assert.equal((overview.match(/class="card card-warn[^"]*"/g) ?? []).length, 1, "and only other's card warns");
});
