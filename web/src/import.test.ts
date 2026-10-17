// Importing Trunk Recorder configs: replacing the setup, and adding to it
// (a setup split over several configs, imported one by one). `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, importTrunkRecorderConfig } from "./config.ts";
import type { Config, Source } from "./protocol.ts";

const rtl = (serial: string, center: number) => ({ device: `rtl=${serial}`, center, rate: 2_400_000, gain: 30 });
const p25 = (shortName: string, cc: number, more: object = {}) => ({ type: "p25", shortName, control_channels: [cc], ...more });

// One setup, split as a user had it: 800 MHz, 700 MHz and VHF, each its own config.
const tr800 = JSON.stringify({
  captureDir: "/tr/audio",
  callTimeout: 3,
  logLevel: "debug",
  sources: [rtl("800a", 855_000_000)],
  systems: [p25("county800", 851_012_500, { minDuration: 1, talkgroupsFile: "county.csv" })],
});
const tr700 = JSON.stringify({
  captureDir: "/tr/elsewhere",
  logLevel: "error",
  sources: [rtl("700a", 770_000_000), rtl("800a", 855_000_000)],
  systems: [p25("state700", 769_256_250, { minDuration: 2, talkgroupsFile: "state.csv" })],
});
const trVhf = JSON.stringify({
  sources: [rtl("vhf", 155_000_000)],
  systems: [{ type: "conventional", shortName: "vhf", channels: [154_100_000, 155_220_000] }],
});
const csv = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,Fire Disp,D,Dispatch,Fire Dispatch,Fire\n";

const serials = (c: Config) => c.sources.map((s: Source) => (s.type === "rtlsdr" ? `${s.serial}@${s.centerHz / 1e6}` : s.type));
const names = (c: Config) => [...c.systems.map((x) => x.shortName), ...c.conventional.map((x) => x.shortName)];

/** The three configs one after another, each added to the last. */
function mergedAll(): Config {
  let c = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv }).config;
  c = importTrunkRecorderConfig(tr700, c, { "state.csv": csv }, true).config;
  return importTrunkRecorderConfig(trVhf, c, {}, true).config;
}

test("replace: a first import takes the config's radios, systems and settings", () => {
  const r = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv });
  assert.deepEqual(serials(r.config), ["800a@855"]);
  assert.deepEqual(names(r.config), ["county800"]);
  assert.equal(r.config.systems[0].talkgroupsName, "county.csv");
  assert.equal(r.config.recording.captureDir, "/tr/audio");
  assert.equal(r.config.recording.callTimeoutS, 3);
  assert.equal(r.config.log?.level, "debug");
  // The same on every system: the Recording tab's.
  assert.equal(r.config.recording.minCallS, 1);
  assert.equal(r.config.systems[0].recording, undefined);
  assert.deepEqual(r.added, { sources: [0], systems: ["county800"] });
});

test("replace: an import over a setup replaces its radios, systems and settings", () => {
  const before = mergedAll();
  const r = importTrunkRecorderConfig(tr700, before, { "state.csv": csv });
  assert.deepEqual(serials(r.config), ["700a@770", "800a@855"]);
  assert.deepEqual(names(r.config), ["state700"], "the 800 MHz system and the VHF channels are gone");
  assert.equal(r.config.recording.captureDir, "/tr/elsewhere");
  assert.equal(r.config.recording.minCallS, 2);
  assert.equal(r.config.log?.level, "error");
  assert.deepEqual(r.added.sources, [0, 1]);
});

test("replace: a config with no trunked systems keeps those there", () => {
  const before = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv }).config;
  const r = importTrunkRecorderConfig(trVhf, before, {});
  assert.deepEqual(names(r.config), ["county800", "vhf"]);
  assert.deepEqual(serials(r.config), ["vhf@155"], "its radios replace those there");
});

test("merge: split configs imported one by one make one setup", () => {
  const c = mergedAll();
  assert.deepEqual(serials(c), ["800a@855", "700a@770", "vhf@155"], "the 800 MHz dongle both name is there once");
  assert.deepEqual(names(c), ["county800", "state700", "vhf"]);
  assert.equal(c.conventional[0].channels.length, 2);
  assert.equal(c.systems[1].talkgroupsName, "state.csv");
  // The first config's settings stay.
  assert.equal(c.recording.captureDir, "/tr/audio");
  assert.equal(c.recording.callTimeoutS, 3);
  assert.equal(c.log?.level, "debug");
});

test("merge: what it added, and the notes on what it didn't", () => {
  const first = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv }).config;
  const r = importTrunkRecorderConfig(tr700, first, { "state.csv": csv }, true);
  assert.deepEqual(r.added, { sources: [1], systems: ["state700"] });
  assert.ok(r.notes.some((n) => n.includes("1 of its radios is already here")), r.notes.join("\n"));
  assert.ok(r.notes.some((n) => n.includes("captureDir") && n.includes("your Recording settings stay")), r.notes.join("\n"));
});

test("merge: a setting that differs from the Recording tab's is the system's own", () => {
  const c = mergedAll();
  assert.equal(c.recording.minCallS, 1);
  assert.equal(c.systems[0].recording, undefined, "county800 had the global's");
  assert.deepEqual(c.systems[1].recording, { minCallS: 2 });
  // The same as the global: nothing of its own.
  const same = JSON.stringify({ systems: [p25("other", 852_000_000, { minDuration: 1 })] });
  const r = importTrunkRecorderConfig(same, c, {}, true);
  assert.equal(r.config.systems.find((x) => x.shortName === "other")?.recording, undefined);
});

test("merge: a top-level filenameFormat is its systems' only", () => {
  const c = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv }).config;
  const global = c.recording.filenameFormat;
  const fmt = JSON.stringify({ filenameFormat: "{talkgroup}-{time}", systems: [p25("other", 852_000_000)] });
  const r = importTrunkRecorderConfig(fmt, c, {}, true).config;
  assert.equal(r.recording.filenameFormat, global);
  assert.equal(r.systems[0].recording?.filenameFormat, undefined);
  assert.equal(r.systems[1].recording?.filenameFormat, "{talkgroup}-{time}");
});

test("merge: importing the same config again changes nothing", () => {
  const c = mergedAll();
  for (const [text, files] of [[tr800, { "county.csv": csv }], [tr700, { "state.csv": csv }], [trVhf, {}]] as const) {
    const r = importTrunkRecorderConfig(text, c, files, true);
    assert.deepEqual(r.config, c);
    assert.deepEqual(r.added, { sources: [], systems: [] });
    assert.ok(r.notes.some((n) => n.startsWith("Already here, left as it is")), r.notes.join("\n"));
    assert.deepEqual(r.todo.filter((t) => t.kind === "talkgroups"), [], "no file asked for a system left as it is");
  }
});

test("merge: another system of a name already taken is added, renamed", () => {
  const c = mergedAll();
  // Same short name, another control channel: not the same system.
  const other = JSON.stringify({ systems: [p25("county800", 860_000_000)] });
  const r = importTrunkRecorderConfig(other, c, {}, true);
  assert.deepEqual(names(r.config), ["county800", "state700", "county2", "vhf"]);
  assert.deepEqual(r.added.systems, ["county2"]);
  // Same short name, another kind: not the same either.
  const conv = JSON.stringify({ systems: [{ type: "conventional", shortName: "state700", channels: [460_000_000] }] });
  assert.deepEqual(importTrunkRecorderConfig(conv, c, {}, true).added.systems, ["state2"]);
});

test("merge: a dongle already here at another centre is added, with a note", () => {
  const c = mergedAll();
  const moved = JSON.stringify({ sources: [rtl("vhf", 160_000_000)], systems: [] });
  const r = importTrunkRecorderConfig(moved, c, {}, true);
  assert.deepEqual(serials(r.config), ["800a@855", "700a@770", "vhf@155", "vhf@160"]);
  assert.deepEqual(r.added.sources, [3]);
  assert.ok(r.notes.some((n) => n.includes("rtlsdr vhf is already one of your radios")), r.notes.join("\n"));
});

test("merge: a missing talkgroup file is asked for the system it added", () => {
  const first = importTrunkRecorderConfig(tr800, defaultConfig(), { "county.csv": csv }).config;
  const r = importTrunkRecorderConfig(tr700, first, {}, true);
  assert.deepEqual(r.todo.filter((t) => t.kind === "talkgroups"), [{ kind: "talkgroups", system: "state700", file: "state.csv" }]);
});

test("merge: the setup it was given is left untouched", () => {
  const c = mergedAll();
  const copy = structuredClone(c);
  importTrunkRecorderConfig(JSON.stringify({ sources: [rtl("new", 450_000_000)], systems: [p25("x", 852_000_000)] }), c, {}, true);
  importTrunkRecorderConfig(tr700, c, {});
  assert.deepEqual(c, copy);
});

test("an RTL-SDR's buflen and buffers come along", () => {
  const tr = JSON.stringify({
    sources: [
      { device: "rtl=00000101,buflen=65536", center: 855_000_000, rate: 2_400_000, gain: 30 },
      { device: "rtl=00000102,buflen=65536,buffers=24", center: 857_000_000, rate: 2_400_000, gain: 30 },
      { device: "rtl=00000103,buflen=1000", center: 859_000_000, rate: 2_400_000, gain: 30 },
      rtl("00000104", 861_000_000),
    ],
    systems: [p25("county800", 855_012_500)],
  });
  const r = importTrunkRecorderConfig(tr, defaultConfig());
  const got = r.config.sources.map((s: Source) => (s.type === "rtlsdr" ? [s.serial, s.bufLen, s.buffers] : s.type));
  assert.deepEqual(got, [
    ["00000101", 65_536, 0],
    ["00000102", 65_536, 24],
    ["00000103", 0, 0],
    ["00000104", 0, 0],
  ]);
  assert.ok(r.notes.some((n) => n.includes("buflen=1000")));
});
