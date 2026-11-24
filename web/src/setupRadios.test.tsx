// Setup's source editors as rendered (react-dom/server): a HackRF's stages and a
// USRP's wire format. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { gainLine } from "./dash/Rf.tsx";
import { renderToString } from "react-dom/server";
import { defaultConfig, kindNeedsDriver, SOURCE_KINDS } from "./config.ts";
import { transport } from "./controller.ts";
import type { Config, EngineStatus, FromRecorder, NetworkRadio, Radios } from "./protocol.ts";
import { SourceCard } from "./Setup.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

function setup(c: Config): void {
  const none = { available: true, detail: "", devices: [] };
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, access: "control", surveyBands: [], capabilities: ["service", "drivers"] },
    { type: "radios", radios: { usrp: none, airspy: none, hydrasdr: none, bladerf: none, hackrf: { available: true, detail: "built in", devices: [{ serial: "0000a1", label: "HackRF One · SN 0000a1" }] } } },
  ];
  for (const m of greeting) transport.onMessage(m);
}

test("Setup: a HackRF's editor has its stages and bias-T, and needs no driver", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.hackrf.fresh()];
  setup(c);
  const html = renderToString(<SourceCard c={c} i={0} />);
  const page = text(html);
  assert.match(page, /LNA \(IF\) gain/);
  assert.match(page, /VGA \(baseband\) gain/);
  assert.match(page, /Amp .*\+14 dB; overloads near strong signals/);
  assert.match(page, /Bias-T/);
  assert.match(page, /HackRF One · SN 0000a1/, "the radio its driver found");
  assert.doesNotMatch(page, /brew install|apt install/, "nothing to install");
  // The LNA offers 0 to 40 in 8 dB steps, the VGA 0 to 62 in 2.
  const lna = html.match(/<select[^>]*aria-label="LNA \(IF\) gain"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.deepEqual([...lna.matchAll(/value="(\d+)"/g)].map((m) => m[1]), ["0", "8", "16", "24", "32", "40"]);
  const vga = html.match(/<select[^>]*aria-label="VGA \(baseband\) gain"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.equal([...vga.matchAll(/value="(\d+)"/g)].length, 32);
});

test("Setup: a driver that won't load says why in full, with the steps for the computer it runs on (#219)", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.hydrasdr.fresh()];
  setup(c);
  const why = "libhydrasdr found but couldn't be loaded: LoadLibraryExW failed (os error 126) — a DLL it needs is missing: hydrasdr.dll needs libusb-1.0.dll beside it";
  const radios = (platform: string) => {
    transport.onMessage({ type: "hello", version: "test", platform, api: 1, access: "control", surveyBands: [], capabilities: ["service", "drivers"] });
    transport.onMessage({ type: "radios", radios: { hydrasdr: { available: false, detail: why, devices: null } } as unknown as Radios });
    return text(renderToString(<SourceCard c={c} i={0} />));
  };
  const win = radios("windows");
  assert.ok(win.includes(`HydraSDR needs ${why}`), "the recorder's reason, shown outright");
  assert.match(win, /Windows put hydrasdr\.dll and libusb-1\.0\.dll .* next to trunk-pro\.exe Then Restart the app/);
  assert.doesNotMatch(win, /dylib|host-tools/, "not another computer's steps");
  // A computer it can't place: every way to install it.
  assert.match(radios("test"), /libhydrasdr\.1\.dylib .* hydrasdr-host-tools .* put hydrasdr\.dll/);
});

test("Setup: an Airspy's driver is built in — nothing to install, its radios listed (#225)", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.airspy.fresh()];
  setup(c);
  transport.onMessage({ type: "radios", radios: { airspy: { available: true, detail: "built in", devices: [{ serial: "735863DC2F650AE3", label: "Airspy R2 735863DC2F650AE3" }] } } as unknown as Radios });
  const page = text(renderToString(<SourceCard c={c} i={0} />));
  assert.equal(SOURCE_KINDS.airspy.driver, undefined, "nothing to install");
  assert.ok(kindNeedsDriver("airspy"), "the desktop app's all the same");
  assert.match(page, /Airspy R2 735863DC2F650AE3/, "the radio its driver found");
  assert.doesNotMatch(page, /needs|brew install|apt install|airspy\.dll/, "nothing to install");
});

test("Setup: the browser build offers only the radios it opens (no driver, no HackRF)", () => {
  const c = defaultConfig();
  setup(c);
  const kinds = (html: string) => [...(html.match(/role="radiogroup"[^>]*kind"[^>]*>(.*?)<\/div>/)?.[1] ?? "").matchAll(/role="radio"[^>]*>([^<]+)</g)].map((m) => m[1]);
  assert.ok(kinds(renderToString(<SourceCard c={c} i={0} />)).includes("HackRF"), "the desktop app");
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, access: "control", surveyBands: [], capabilities: [] });
  assert.deepEqual(kinds(renderToString(<SourceCard c={c} i={0} />)), ["RTL-SDR", "Capture file"]);
});

test("Setup: a radio's coverage is drawn under its guard band, by the guard band shown", () => {
  const c = defaultConfig();
  c.sources = [{ ...SOURCE_KINDS.rtlsdr.fresh(), centerHz: 851_000_000, rateHz: 2_400_000, guardHz: 100_000 }];
  setup(c);
  const covers = (html: string) => html.match(/aria-label="(Covers [^"]*)"/)?.[1];
  const html = renderToString(<SourceCard c={c} i={0} />);
  assert.equal(covers(html), "Covers 849.900 to 852.100 MHz");
  assert.ok(html.indexOf('aria-label="Guard band at each edge, kHz"') < html.indexOf('aria-label="Covers'), "under the guard band field");
  c.sources[0].guardHz = 200_000;
  assert.equal(covers(renderToString(<SourceCard c={c} i={0} />)), "Covers 850.000 to 852.000 MHz");
});

test("Setup: a USRP's editor offers the wire format", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.usrp.fresh()];
  setup(c);
  const html = renderToString(<SourceCard c={c} i={0} />);
  const sel = html.match(/<select[^>]*aria-label="Wire format"[^>]*>(.*?)<\/select>/)?.[1] ?? "";
  assert.deepEqual([...sel.matchAll(/value="([^"]*)"/g)].map((m) => m[1]), ["", "sc16", "sc12", "sc8"]);
  assert.match(text(sel), /Auto — sc16, or sc12 for a B2xx on USB 2\.0 above 8 MSPS sc16 sc12 \(B2xx\) sc8/);
});

test("Setup's AutoTune: the ppm to set is measured against the radio's as running, so setting it twice doesn't move it twice", () => {
  const rtl = (serial: string, ppm: number) => ({ ...SOURCE_KINDS.rtlsdr.fresh(), serial, ppm, autoTune: true }) as Config["sources"][number];
  const c = defaultConfig();
  c.sources = [rtl("200", 2), rtl("91", 0)];
  setup(c);
  // Recording: SN 200 measures +3.6 ppm against its 2, so −2 removes it; SN 91 nothing yet.
  const status = (settings: Config["sources"]) =>
    transport.onMessage({
      type: "status",
      status: { systems: [], recording: 0 } as unknown as EngineStatus,
      sources: settings.map((s, index) => ({ index, label: `RTL-SDR SN ${"serial" in s ? s.serial : ""}`, centerHz: 851e6, rateHz: 2.4e6, rateMeasured: 2.4e6, dropped: 0, errors: 0, lastError: null, ended: false, settings: s, ...(index === 0 ? { errorPpm: 3.6, ppmApply: -2, tunePpm: 3.6 } : {}) })),
      load: 0,
      calls: [],
    });
  transport.onMessage({ type: "state", phase: "running", error: null, ended: false, restartNeeded: false });
  status(c.sources);
  assert.match(text(renderToString(<SourceCard c={c} i={0} />)), /Measured \+3\.60 ppm Corrected \+3\.60 ppm Set correction to -2 ppm/);
  // Set: the radio still runs at 2 and measures the same, and nothing more is offered — it applies at the next Start.
  const set = { ...c, sources: [rtl("200", -2), rtl("91", 0)] };
  let page = text(renderToString(<SourceCard c={set} i={0} />));
  assert.match(page, /Correction -2 ppm from next Start/);
  assert.doesNotMatch(page, /Set correction to/);
  // Sources moved in Setup since Start: each card shows its own radio's measurement, not the one at its place.
  const moved = { ...c, sources: [rtl("91", 0), rtl("200", 2)] };
  assert.doesNotMatch(text(renderToString(<SourceCard c={moved} i={0} />)), /Measured \+/);
  page = text(renderToString(<SourceCard c={moved} i={1} />));
  assert.match(page, /Set correction to -2 ppm/);
});

test("A radio's gain says what it's set to when that isn't what Setup asks", () => {
  const rtl = { type: "rtlsdr" as const, serial: "", centerHz: 851_000_000, rateHz: 2_400_000, gainDb: 34, agc: false, biasTee: false, ppm: 0, bufLen: 0, buffers: 0 };
  assert.equal(gainLine(rtl, "33.8 dB"), "34 dB · set to 33.8 dB");
  assert.equal(gainLine(rtl, null), "34 dB", "the same: said once");
});

test("Setup: a SpyServer's sample rates are the ones the server said, not a fixed list", () => {
  const c = defaultConfig();
  c.sources = [{ ...SOURCE_KINDS.spyserver.fresh(), address: "pi:5555", rateHz: 1_250_000 } as Config["sources"][number]];
  setup(c);
  assert.match(renderToString(<SourceCard c={c} i={0} />), /<datalist/, "until it has said, any rate can be typed");
  const none = { available: true, detail: "", devices: [] };
  const radios = (device: { label: string } & Partial<NetworkRadio>): Radios => ({ usrp: none, airspy: none, hydrasdr: none, bladerf: none, hackrf: none, network: { available: true, detail: "built in", devices: [{ kind: "spyserver", address: "pi:5555", ...device }] } });
  transport.onMessage({ type: "radios", radios: radios({ label: "Airspy R2 / Mini at pi:5555", model: "Airspy R2 / Mini", rates: [2_500_000, 1_250_000, 625_000], gains: [0, 1] }) });
  const html = renderToString(<SourceCard c={c} i={0} />);
  const opts = [...html.matchAll(/<option[^>]*value="(\d+)"[^>]*>[\d.]+(?:<!-- -->)? MSPS/g)].map((m) => Number(m[1]));
  assert.ok(opts.includes(625_000) && !opts.includes(10_000_000), `the server's rates: ${opts}`);
  assert.match(text(html), /2\.5 MSPS 1\.25 MSPS 0\.625 MSPS/, "three decimals, the zeros that mean nothing left off");
  assert.match(text(html), /This SpyServer Airspy R2 \/ Mini: 2\.5 or 1\.25 or 0\.625/);
  // One that can't be reached says so under its address.
  transport.onMessage({ type: "radios", radios: radios({ label: "pi:5555", error: "SpyServer pi:5555: can't connect" }) });
  assert.match(text(renderToString(<SourceCard c={c} i={0} />)), /Not answering/);
});

test("Setup: an RTL-SDR on USB or through rtl_tcp: the dongle list for one, the server's address for the other", () => {
  const c = defaultConfig();
  c.sources = [SOURCE_KINDS.rtlsdr.fresh(), { ...SOURCE_KINDS.rtlsdr.fresh(), address: "pi.local" } as Config["sources"][number]];
  setup(c);
  const usb = text(renderToString(<SourceCard c={c} i={0} />));
  assert.match(usb, /Connection .* USB rtl_tcp Dongle/);
  assert.match(usb, /Dongle/);
  assert.doesNotMatch(usb, /Server address/);
  const net = renderToString(<SourceCard c={c} i={1} />);
  assert.match(text(net), /Server address/);
  assert.doesNotMatch(text(net), /Dongle|USB transfers/, "nothing of USB");
  assert.match(net, /value="pi.local"/);
  assert.match(text(net), /Bias-T/, "its bias-T, as on USB");
});
