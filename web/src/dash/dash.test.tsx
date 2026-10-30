// The dashboard pages as rendered (react-dom/server), from what the recorder
// sends: what a user relies on seeing there stays there. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newConventional, newSystem } from "../config.ts";
import { setView, snapshot, transport } from "../controller.ts";
import { clearSeries, feedSeries } from "../series.ts";
import { coverage, dashSystems, pageHealth, systemHealth } from "./data.ts";
import type { CallView, Channel, ClockInfo, Config, EngineStatus, FromRecorder, PlatformInfo, PluginInfo, SiteIdentity, SiteStatus, SourceStatus, SystemStatus, ToneRow } from "../protocol.ts";
import { DecodePage } from "./Decode.tsx";
import { eventText, Overview } from "./Overview.tsx";
import { RadioPage } from "./Radio.tsx";
import { ToneTable } from "./Tones.tsx";
import { PlatformPage } from "./Platform.tsx";
import { PluginHealthPage } from "./PluginHealth.tsx";
import { axisTicks, TimeSeries } from "../charts.tsx";

const site = (index: number, name: string, identity: Partial<SiteIdentity>): SiteStatus => ({
  index, name, nowS: 10, controlChannelHz: 851_000_000 + index * 500_000,
  identity: { nac: 0x1f1, wacn: 0xbee00, sysId: 0x1a2, rfss: 1, site: null, ...identity },
  good: 100, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null,
});
const system = (index: number, shortName: string, sites: SiteStatus[]): SystemStatus => ({ index, shortName, activeCalls: 0, recording: 0, conventionalOpen: 0, callsConcluded: 0, sites });

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

test("Overview: enabled internet audio sources appear as independent systems", () => {
  const c = defaultConfig();
  c.audioStreams.push(
    { url: "https://example.test/live.mp3", shortName: "dispatch-stream", talkgroup: 1, enabled: true, silenceTimeoutS: 3 },
    { url: "https://example.test/off.mp3", shortName: "disabled-stream", talkgroup: 1, enabled: false, silenceTimeoutS: 3 },
  );
  running(c, [{ ...system(0, "dispatch-stream", []), connected: true }]);
  assert.deepEqual(dashSystems(snapshot()).map((x) => [x.name, x.kind]), [["dispatch-stream", "audio"]]);
  setView("overview");
  const page = text(renderToString(<Overview />));
  assert.match(page, /dispatch-stream Internet audio Calls — this run .*Internet audio/);
  assert.match(page, /Connected/);
  assert.doesNotMatch(page, /disabled-stream/);
});

test("internet audio connection failures are visible and lower system health", () => {
  const c = defaultConfig();
  c.audioStreams.push({ url: "https://example.test/live.mp3", shortName: "dispatch-stream", talkgroup: 1, enabled: true, silenceTimeoutS: 3 });
  running(c, [{ ...system(0, "dispatch-stream", []), connected: false, lastError: "ffmpeg exited with status 8" }]);
  const s = snapshot();
  const x = dashSystems(s)[0];
  assert.equal(systemHealth(s, x).level, "warn");
  setView("decode", x.name);
  assert.match(text(renderToString(<DecodePage />)), /Reconnecting ffmpeg exited with status 8/);
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

test("Decode: a conventional system's page has its calls, and each channel's mode and names", () => {
  const c = defaultConfig();
  const ch = (freqHz: number, mode: Channel["mode"], name: string) => ({ freqHz, mode, name, code: "", description: "", tag: "", group: "", enabled: true }) as Channel;
  c.conventional.push(newConventional(c, { shortName: "town", channels: [ch(155_200_000, "p25", "Town PD"), ch(155_400_000, "fm", "Town PW")] }));
  running(c, []);
  transport.onMessage({
    type: "status",
    status: { systems: [{ index: 0, shortName: "town", activeCalls: 1, recording: 0, conventionalOpen: 1, callsConcluded: 7, sites: [] }], recording: 0 } as unknown as EngineStatus,
    sources: [], load: 0, calls: [],
  });
  setView("decode");
  assert.match(text(renderToString(<DecodePage />)), /Calls 7 this run .*1 on the air now/);
  assert.match(text(renderToString(<Overview />)), /town Conventional .*Calls 7 this run .*2 channels · 1 open now/);
  setView("decode", "town");
  const snap = (freqHz: number, mode: Channel["mode"], names: string[]) => ({
    system: "town", site: null, freqHz, source: 0, kind: "conventional" as const, powerDb: -60, noiseDb: -80, snrDb: 20, offsetHz: null, quality: null, phaseErrDeg: null, calls: 0, mode, names,
  });
  transport.onMessage({ type: "decodeDetail", system: "town", channels: [snap(155_200_000, "p25", ["Town PD"]), snap(155_400_000, "fm", ["Town PW"])], sites: [], patches: [] });
  const page = text(renderToString(<DecodePage />));
  assert.match(page, /7 calls saved this run · 1 active/);
  assert.ok(page.includes("P25 · Town PD") && page.includes("FM · Town PW"), page);
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

test("Radio: a system's Tones card names its signals and goes to them in Setup; heard rows show what the recorder made of them", () => {
  twoSites();
  const c = snapshot().config!;
  c.systems[0].signals = [{ id: "NORWALK_EMS", pattern: "two_tone", tonesHz: [788.5, 879], talkgroups: ["20341"] }];
  transport.onMessage({ type: "config", config: c, version: 2 });
  setView("radio", "dcfd");
  const page = text(renderToString(<RadioPage />));
  assert.match(page, /Tones Signals of dcfd: NORWALK_EMS\. Signals in Setup/);
  const row = (r: Partial<ToneRow>): ToneRow => ({
    tg: 20341, alphaTag: "Norwalk EMS 1", pattern: "two_tone", tones: [781.25, 875], lengths: [0.98, 2.98], digits: "", knox: false, cycles: 0, format: "Quick Call II",
    text: "two-tone 781.25 → 875 Hz, 0.98 + 2.98 s (Quick Call II)", count: 3, first: 0, last: 0, radios: [{ unit: 7, name: "E7", n: 3 }], frame: true, ids: ["QC2 126/128"], signals: ["NORWALK_EMS"], ...r,
  });
  const rows = [row({}), row({ tg: 20342, alphaTag: "", pattern: "digits", tones: [], digits: "12#", text: "DTMF 12#", ids: [], signals: [] })];
  const html = renderToString(<ToneTable rows={rows} system="dcfd" at={0} />);
  const lines = [...html.matchAll(/<tr>(.*?)<\/tr>/g)].slice(1).map((m) => text(m[1]).trim());
  assert.match(lines[0], /^Norwalk EMS 1 20341 two-tone 781\.25 → 875 Hz, 0\.98 \+ 2\.98 s \(Quick Call II\) QC2 126\/128 3 .* E7 ▶ NORWALK_EMS$/, "the recorder's text and its signal");
  assert.match(lines[1], /^20342 DTMF 12# — 3 .* ▶ Add$/, "not a signal yet: Add");
  assert.equal(eventText({ type: "monitorEvent", t: 0, level: "info", kind: "tonePattern", system: "dcfd", talkgroup: 20341, text: "two-tone 781.25 → 875 Hz", signals: ["NORWALK_EMS"] }), "dcfd: NORWALK_EMS (two-tone 781.25 → 875 Hz) on talkgroup 20341");
});

test("Radio: a warning of calls out of band links to the frequencies, on the Overview and the system's page", () => {
  twoSites();
  feedSeries(Date.now() / 1000, { "sys/other/why/no_source": 1 });
  setView("overview");
  assert.match(renderToString(<Overview />), /<button class="linkish why-link">Calls are being missed: their frequencies are outside every source(?:'|&#x27;)s band \(Out of band\)(?:<!-- -->)? <span aria-hidden="true">→/);
  setView("radio", "other");
  const page = renderToString(<RadioPage />);
  assert.match(page, /why-link/, "What happened to calls links there too");
  assert.doesNotMatch(text(page), /Out of band \(/, "no list until there's one, or it's asked for");
  // Where the link goes: the system's page, with the list (empty until the recorder answers).
  setView("radio", "other", "outOfBand");
  const at = text(renderToString(<RadioPage />));
  assert.match(at, /Out of band \(0\) None in the last 24 hours/);
  assert.match(at, /What happened to calls/, "on the system's page, not a page of its own");
});

test("A frequency out of band: the nearest source, and how far past its usable edge", () => {
  twoSites();
  const src = (index: number, label: string, centerHz: number): SourceStatus => ({ index, label, centerHz, rateHz: 2_400_000, rateMeasured: 2_400_000, dropped: 0, errors: 0, lastError: null, ended: false });
  const s = { ...snapshot(), sources: [src(0, "rtl0", 851_000_000), src(1, "rtl1", 855_000_000)] };
  // (2.4 MS/s less the 75 kHz guard each side: 1.125 MHz either side is usable.)
  const inside = coverage(s, 851_200_000)!;
  assert.equal(inside.src.label, "rtl0");
  assert.equal(inside.beyondHz, 0);
  assert.ok(inside.depth > 0.8);
  const past = coverage(s, 853_500_000)!;
  assert.equal(past.src.label, "rtl1", "the nearer edge");
  assert.equal(past.beyondHz, 375_000);
  assert.equal(past.depth, 0);
  assert.equal(coverage({ ...s, sources: [] }, 851_000_000), null);
});

/** The computer as the recorder samples it, its clock as given (the rest as nothing known). */
function host(given: Partial<ClockInfo>, disks: PlatformInfo["disks"] = []): void {
  const clock: ClockInfo = { warnMs: 250, os: null, service: null, driftMs: null, radio: [], server: null, measured: null, error: null, problem: null, check: null, ...given };
  const platform: PlatformInfo = {
    os: "Linux", arch: "aarch64", host: "pi", cores: 4, container: null, cpuLimitCores: null, memTotal: 8e9, uptimeS: 3600, disks, probes: [], clock,
    perCore: null, interfaces: null, unavailable: [],
  };
  transport.onMessage({ type: "host", t: Date.now() / 1000, values: {}, platform });
}

const now = () => Date.now() / 1000;
const synced = { synced: true, maxErrorMs: 897 };

test("Platform: the clock as measured and as the OS says, and the light goes by the recorder's verdict", () => {
  twoSites();
  setView("platform");
  host({ os: synced, server: "pool.ntp.org", measured: { offsetMs: 35, delayMs: 20, stratum: 2, at: now() - 60, current: true }, error: null, problem: null, check: null });
  let page = text(renderToString(<PlatformPage />));
  assert.match(page, /Offset, measured 35 ms behind pool\.ntp\.org/);
  assert.match(page, /Operating system synchronized within 897 ms by its own account/);
  assert.match(page, /Round trip 20 ms stratum 2/);
  assert.notEqual(pageHealth(snapshot()).platform.level, "warn");
  const why = "The clock is 400 ms ahead of pool.ntp.org: call times are off by as much";
  host({ os: synced, server: "pool.ntp.org", measured: { offsetMs: -400, delayMs: 20, stratum: 2, at: now() - 60, current: true }, error: null, problem: { level: "warn", cause: "clock", why }, check: null });
  page = text(renderToString(<PlatformPage />));
  assert.match(page, /Clock .*400 ms ahead of pool\.ntp\.org/);
  assert.deepEqual(pageHealth(snapshot()).platform, { level: "warn", why }, "the navigation's light, in the recorder's words");
  // Nothing measured (UDP 123 blocked), and an OS that doesn't say.
  host({ os: null, server: "pool.ntp.org", measured: null, error: "pool.ntp.org: no answer", problem: null, check: { target: "pool.ntp.org", ok: false, rttMs: null, lastOk: null, lastFail: now(), downSince: now(), tries: 3, fails: 3 } });
  page = text(renderToString(<PlatformPage />));
  assert.match(page, /Offset, measured — pool\.ntp\.org: no answer/);
  assert.match(page, /3 of 3 queries unanswered .*UDP port 123/);
});

test("Platform: the Network card's Checks switch turns the internet checks and the time server off (#37)", () => {
  twoSites();
  setView("platform");
  host({ os: synced, server: "pool.ntp.org", measured: null, error: null, problem: null, check: null });
  assert.equal(snapshot().config!.monitor!.networkChecks, true, "on by default");
  assert.match(renderToString(<PlatformPage />), /role="switch" checked=""/);
  const c = snapshot().config!;
  c.monitor!.networkChecks = false;
  transport.onMessage({ type: "config", config: c, version: 2 });
  host({ os: synced, server: null, measured: null, error: null, problem: null, check: null });
  const page = text(renderToString(<PlatformPage />));
  assert.match(page, /Checks off: the internet and the time server aren(?:'|&#x27;)t asked/);
  assert.match(page, /Offset, measured — not asked: network checks are off/);
});

test("Platform: the time service, the recording's clock and the radio systems' clocks", () => {
  twoSites();
  setView("platform");
  host({
    os: synced, server: "pool.ntp.org", service: { name: "systemd-timesyncd", server: "2.debian.pool.ntp.org (45.33.53.84)", stratum: 2, lastSync: now() - 120 }, driftMs: 1800, warnMs: 500,
    radio: [{ site: "dcfd", offsetMs: -12, at: now() - 30, sane: true, valid: false }, { site: "other", offsetMs: -4 * 3600e3, at: now() - 30, sane: false, valid: true }],
    problem: { level: "warn", cause: "recording", why: "Calls are being stamped 1.8 s behind the right time: the clock was corrected after recording started. Stop and Start again to stamp them by it" },
  });
  const html = renderToString(<PlatformPage />);
  assert.match(html, /<p class="small warn"[^>]*>Calls are stamped by the clock as it was at Start/, "amber by the recorder's verdict (its cause), not a rule of its own");
  const page = text(html);
  assert.match(page, /Kept by systemd-timesyncd from 2\.debian\.pool\.ntp\.org \(45\.33\.53\.84\), stratum 2, last synchronized 2 m/);
  // (driftMs: the wall clock less the recording's: the recording's is behind.)
  assert.match(page, /Calls are stamped by the clock as it was at Start: 1\.8 s behind the clock now/);
  assert.match(page, /The radio systems(?:'|&#x27;) own clocks, against the calls(?:'|&#x27;) time: dcfd 12 ms behind them \(the system marks its time not valid: shown, not gone by\)/);
  assert.match(page, /other set to another time \(its local time\?\): not compared/);
  assert.match(page, /A warning at 500 ms off/);
  assert.equal(pageHealth(snapshot()).platform.level, "warn");
});

test("Platform: a RAM spool deleted while running (systemd's RemoveIPC at logout) says how to stop it, not that it's full", () => {
  twoSites();
  setView("platform");
  const spool = { name: "spool", path: "/dev/shm/trunk-pro-1000-abc", mount: "/dev/shm/trunk-pro-1000-abc", kind: "tmpfs", totalBytes: 256 << 20, freeBytes: 250 << 20, overflowed: 0 };
  host({ os: synced }, [{ ...spool, removed: 2, missing: false }]);
  let h = pageHealth(snapshot()).platform;
  assert.equal(h.level, "warn");
  assert.match(h.why, /deleted 2 times while running \(made again\)\. systemd deletes .*loginctl enable-linger .*RemoveIPC=no/);
  assert.match(text(renderToString(<PlatformPage />)), /deleted 2 times while running/);
  // Gone and not made again: calls go to the disk.
  host({ os: synced }, [{ ...spool, totalBytes: 0, freeBytes: 0, removed: 3, missing: true }]);
  h = pageHealth(snapshot()).platform;
  assert.equal(h.level, "bad");
  assert.match(h.why, /The RAM spool is gone, calls go to the recordings folder\. systemd deletes/);
  assert.doesNotMatch(h.why, /full/);
  assert.match(text(renderToString(<PlatformPage />)), /Gone: calls go to the recordings folder/);
});

/** A plugin that's on and running, with what it said of its work. */
function uploader(id: string): void {
  const c = defaultConfig();
  c.plugins = { [id]: { enabled: true, settings: {} } } as Config["plugins"];
  const p: PluginInfo = {
    id, path: `/plugins/${id}/${id}`, custom: false, problem: null,
    manifest: { id, name: "Broadcastify Calls", version: "0.1.7", description: "", api: 1, subscribe: ["call.concluded"], audio_formats: [] },
    runtime: { state: "ok", message: "", ok: 30, skipped: 0, failed: 0, lastFailure: "", log: [] },
  };
  running(c, []);
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [p] });
}

test("Plugins: a call's age at delivery, and its parts, the same for any plugin", () => {
  uploader("broadcastify");
  setView("plugins");
  // A delivery every 10 s, 2.5 s after the call's end: 0.4 s of it the upload, none held.
  const t = Date.now() / 1000;
  for (let i = 0; i < 5; i++) {
    feedSeries(t - 60 + i * 10, { "plg/broadcastify/delivered": 0.1, "plg/broadcastify/ageMs": 250, "plg/broadcastify/heldMs": 0, "plg/broadcastify/uploads": 0.1, "plg/broadcastify/uploadMs": 40, "plg/broadcastify/waitMs": 210, "plg/broadcastify/age": 2500 });
  }
  const page = text(renderToString(<PluginHealthPage manage={null} />));
  assert.match(page, /Age at delivery 2\.5 s last 5 min: waiting 2\.1 s · upload 400 ms/);
  assert.doesNotMatch(page, /held/, "nothing held by a delay rule: not said");
});

test("Plugins: a plugin that doesn't say its upload time gives the age alone", () => {
  uploader("broadcastify");
  setView("plugins");
  feedSeries(Date.now() / 1000, { "plg/broadcastify/delivered": 0.1, "plg/broadcastify/ageMs": 6200, "plg/broadcastify/heldMs": 6000, "plg/broadcastify/uploads": 0, "plg/broadcastify/uploadMs": 0, "plg/broadcastify/waitMs": 0 });
  const html = renderToString(<PluginHealthPage manage={null} />);
  assert.match(text(html), /Age at delivery 62 s last 5 min: held 60 s/);
  assert.match(html, /doesn&#x27;t say its own upload time/);
});

test("A chart of one line says what it is and in what unit, as one of several does", () => {
  const data = { t0: Date.now() / 1000 - 600, stepS: 60, v: [3, -2, 1, 0, 2, -1, 3, 0, 1, 2], lo: Array(10).fill(null), hi: Array(10).fill(null), n: Array(10).fill(1) };
  const html = renderToString(<TimeSeries lines={[{ label: "This computer's clock against pool.ntp.org, ms (+: behind)", color: "red", data }]} />);
  assert.match(text(html), /This computer(?:'|&#x27;)s clock against pool\.ntp\.org, ms \(\+: behind\)/);
});

test("A y axis never has two ticks that read the same", () => {
  const whole = (v: number) => v.toFixed(0);
  // The clock chart's: 36.2 to 36.9 ms under a whole-ms format (it read 37, 37, 36, 36).
  assert.deepEqual(axisTicks(36.2, 36.9, whole), [36, 37]);
  const tenth = (v: number) => (Math.abs(v) >= 100 ? v.toFixed(0) : v.toFixed(1));
  for (const [lo, hi, f] of [[36.2, 36.9, tenth], [-3.3, 0.8, whole], [0, 1500, whole], [99.7, 100.4, tenth], [0, 0.04, tenth], [-2, 2, (v: number) => `${v.toFixed(1)} s`]] as const) {
    const t = axisTicks(lo, hi, f);
    const labels = t.map(f);
    assert.equal(new Set(labels).size, labels.length, `${lo}..${hi}: ${labels.join(", ")}`);
    assert.ok(t.length >= 2 && t[0] <= lo + 1e-9 + (t[1] - t[0]) && t[t.length - 1] >= hi - (t[1] - t[0]) - 1e-9, `${lo}..${hi}: ${t.join(", ")} spans the data`);
  }
});
