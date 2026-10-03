// The recorder inside the browser: the Rust engine (WebAssembly) and the
// RTL-SDR driver (WebUSB), speaking the same protocol as the desktop app's
// server, so the interface is identical. Calls are stored in OPFS.

import init, { survey_bands, WebRtl, WebSession, WebSurvey } from "./pkg/trunk_web.js";
import type { Config, FromRecorder, HeardCode } from "../protocol.ts";
import { activeSystems, resolvedCenters } from "../config.ts";
import { listCalls, readText, saveCall, writeText } from "./opfs.ts";

export type ToWorker =
  | { type: "init"; config: Config }
  | { type: "setConfig"; config: Config }
  | { type: "files"; files: (File | null)[] }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "listen"; on: boolean; system: string | null; talkgroup: number | null }
  | { type: "surveyStart"; source: number; bands: string[]; findGain: boolean }
  | { type: "surveyListen"; freqHz: number }
  | { type: "surveyRescan" }
  | { type: "surveyStop" }
  | { type: "subscribe"; topics: string[] }
  | { type: "statsQuery"; id: number; series: string[]; range?: string; from?: number; to?: number; points?: number }
  | { type: "radioQuery"; id: number; what: string; system?: string; key?: number; hours?: number; limit?: number };

/** Worker → page: a protocol message, or a live audio frame. */
export type FromWorker = { msg: FromRecorder } | { audio: ArrayBuffer; tg: number };

const post = (msg: FromRecorder) => postMessage({ msg } satisfies FromWorker);

let config: Config | null = null;
let files: (File | null)[] = [];
let session: WebSession | null = null;
let rtls: WebRtl[] = [];
let timer: ReturnType<typeof setInterval> | null = null;
let running = false;
/** `system`: a short name. */
let listen: { on: boolean; system: string | null; talkgroup: number | null } = { on: false, system: null, talkgroup: null };
let phase: FromRecorder & { type: "state" } = { type: "state", phase: "idle", error: null, ended: false };
/** What the page watches (applied to each new session). */
let topics: string[] = [];
/** The radio registry, saved between runs. */
const REGISTRY_FILE = "radio-registry.json";
let registrySaved = 0;

async function saveRegistry(s: WebSession): Promise<void> {
  const changed = JSON.parse(s.registry_unsaved()) as Record<string, string>;
  if (!Object.keys(changed).length) return;
  let all: Record<string, string> = {};
  try {
    all = JSON.parse((await readText(REGISTRY_FILE)) ?? "{}");
  } catch {
    // Start afresh.
  }
  await writeText(REGISTRY_FILE, JSON.stringify({ ...all, ...changed })).catch(() => {});
}
const ready = init();

function setPhase(p: "idle" | "starting" | "running" | "stopping", error: string | null = null, ended = false): void {
  phase = { type: "state", phase: p, error, ended };
  post(phase);
}

async function devices() {
  try {
    await ready;
    return (await WebRtl.devices()) as { index: number; serial: string; product: string }[];
  } catch {
    return [];
  }
}

/** Hand the session's outputs to the page / storage. */
function deliver(outs: { t: string; json?: string; system?: number; shortName?: string; tg?: number; frame?: Uint8Array; rel?: string; wav?: Uint8Array; entry?: string }[]): void {
  for (const o of outs) {
    if (o.t === "text") post(JSON.parse(o.json!) as FromRecorder);
    else if (o.t === "audio") {
      if (listen.on && (listen.system === null || listen.system === o.shortName) && (listen.talkgroup === null || listen.talkgroup === o.tg)) {
        const buf = o.frame!.slice().buffer;
        postMessage({ audio: buf, tg: o.tg! } satisfies FromWorker, { transfer: [buf] });
      }
    } else if (o.t === "file") {
      // Announced once it's stored, as the desktop app does.
      const entry = JSON.parse(o.entry!);
      void saveCall(o.rel!, o.wav!, o.json!, entry).then(
        () => post({ type: "concluded", entry }),
        (e) => post({ type: "log", lines: [{ timeS: 0, kind: "error", text: `couldn't store ${o.rel}: ${e instanceof Error ? e.message : String(e)}` }] }),
      );
    }
  }
}

// ── first-run survey: one source, retuned as the survey asks ─────────────────

let survey: WebSurvey | null = null;
let surveyRtl: WebRtl | null = null;
let surveyTimer: ReturnType<typeof setInterval> | null = null;
let surveying = false;

function surveyPoll(): void {
  if (survey) deliver(survey.poll(performance.now()));
}

async function surveyStart(req: { source: number; bands: string[]; findGain: boolean }): Promise<void> {
  if (running) return post({ type: "error", message: "Stop recording first — the scan needs the radio to itself." });
  await surveyStop(false);
  if (!config) return;
  try {
    await ready;
    const src = config.sources[req.source];
    if (!src) throw new Error("No such source.");
    if (src.kind === "usrp" || src.kind === "airspy" || src.kind === "soapy") throw new Error("USRP, Airspy and SoapySDR need the desktop app.");
    const s = new WebSurvey(JSON.stringify(config), JSON.stringify(req));
    survey = s;
    surveying = true;
    surveyTimer = setInterval(surveyPoll, 100);
    if (src.kind === "rtlsdr") {
      const first = s.command() as { tune?: number } | undefined;
      const rtl = await WebRtl.open(src.serial, first?.tune ?? src.centerHz, src.rateHz, src.agc ? undefined : src.gainDb, src.ppm);
      surveyRtl = rtl;
      s.tuned(first?.tune ?? src.centerHz);
      void pumpSurveyRtl(s, rtl);
    } else {
      if (src.format && src.format !== "cu8") throw new Error("The browser version reads rtl_sdr (cu8) captures only.");
      const f = files[req.source];
      if (!f) throw new Error("Choose the capture file again (the browser forgets it on reload).");
      void pumpSurveyFile(s, f, resolvedCenters(config)[req.source] ?? src.centerHz);
    }
  } catch (e) {
    await surveyStop();
    post({ type: "error", message: e instanceof Error ? e.message : String(e) });
  }
}

async function pumpSurveyRtl(s: WebSurvey, rtl: WebRtl): Promise<void> {
  while (surveying && survey === s) {
    try {
      // Retunes happen between blocks (the dongle's promises go one at a time).
      for (let c = s.command() as { tune?: number; gain?: number } | undefined; c; c = s.command() as typeof c) {
        const hz = c.tune !== undefined ? ((await rtl.retune(c.tune)) as number) : ((await rtl.set_gain(c.gain!)) as number);
        s.tuned(hz);
      }
      const b = (await rtl.next()) as { bytes: Uint8Array; dropped: number } | undefined;
      if (!b || survey !== s) break;
      s.push(b.bytes);
    } catch (e) {
      if (!surveying || survey !== s) break;
      s.source_error(e instanceof Error ? e.message : String(e));
      break;
    }
  }
}

async function pumpSurveyFile(s: WebSurvey, f: File, centerHz: number): Promise<void> {
  const chunk = 1 << 17;
  for (let off = 0; surveying && survey === s && off < f.size; off += chunk) {
    for (let c = s.command(); c; c = s.command()) s.tuned(centerHz);
    s.push(new Uint8Array(await f.slice(off, Math.min(f.size, off + chunk)).arrayBuffer()));
    await new Promise((r) => setTimeout(r, 0));
  }
  if (survey === s) {
    s.finish();
    surveyPoll();
  }
}

async function surveyStop(tell = true): Promise<void> {
  const was = survey !== null;
  surveying = false;
  if (surveyTimer) clearInterval(surveyTimer);
  surveyTimer = null;
  const rtl = surveyRtl;
  surveyRtl = null;
  if (rtl) await rtl.close().catch(() => {});
  survey?.free();
  survey = null;
  if (was && tell) post({ type: "survey", stage: "idle" });
}

/** Each system's radios' talker aliases (Trunk Recorder's unitTagsOTA CSV), as saved. */
async function savedUnits(cfg: Config): Promise<Record<string, string>> {
  const units: Record<string, string> = {};
  for (const name of new Set([...cfg.systems.map((x) => x.shortName), ...cfg.conventional.map((x) => x.shortName)])) {
    const csv = await readText(`units-${name}.csv`);
    if (csv) units[name] = csv;
  }
  return units;
}

/** The codes conventional frequencies carried (trunk-app heard.rs). */
const heardFile = (_cfg: Config) => "heard-conventional.json";

async function savedHeard(cfg: Config): Promise<Record<string, HeardCode[]>> {
  try {
    return JSON.parse((await readText(heardFile(cfg))) ?? "{}");
  } catch {
    return {};
  }
}

async function saveHeard(s: WebSession, cfg: Config): Promise<void> {
  const json = s.heard_unsaved();
  if (json) await writeText(heardFile(cfg), json).catch(() => {});
}

async function saveUnits(changed: string): Promise<void> {
  if (changed === "{}") return;
  for (const [name, csv] of Object.entries(JSON.parse(changed) as Record<string, string>)) {
    await writeText(`units-${name}.csv`, csv).catch(() => {});
  }
}

async function start(): Promise<void> {
  if (running || !config) return;
  await surveyStop();
  setPhase("starting");
  try {
    await ready;
    const cfg = config;
    // Each system's band plan from the last run.
    const plans: Record<string, string> = {};
    for (const x of activeSystems(cfg)) {
      const p = await readText(`bandplan-${x.shortName}.txt`);
      if (p) plans[x.shortName] = p;
    }
    session = new WebSession(JSON.stringify(cfg), Date.now(), JSON.stringify(plans));
    session.load_units(JSON.stringify(await savedUnits(cfg)));
    session.load_heard((await readText(heardFile(cfg))) ?? "");
    session.load_registry((await readText(REGISTRY_FILE)) ?? "{}");
    session.set_topics(JSON.stringify(topics));
    session.set_want_audio(listen.on);
    const centers = resolvedCenters(cfg);
    running = true;
    rtls = [];
    for (let i = 0; i < cfg.sources.length; i++) {
      const s = cfg.sources[i];
      if (s.kind === "rtlsdr") {
        const rtl = await WebRtl.open(s.serial, centers[i] ?? s.centerHz, s.rateHz, s.agc ? undefined : s.gainDb, s.ppm);
        rtls.push(rtl);
        void pumpRtl(i, rtl);
      } else if (s.kind === "usrp" || s.kind === "airspy" || s.kind === "soapy") {
        throw new Error(`Source ${i + 1}: USRP, Airspy and SoapySDR need the desktop app.`);
      } else {
        if (s.format && s.format !== "cu8") throw new Error(`Source ${i + 1}: the browser version reads rtl_sdr (cu8) captures only.`);
        const f = files[i];
        if (!f) throw new Error(`Source ${i + 1}: choose the capture file again (the browser forgets it on reload).`);
        void pumpFile(i, f, s.rateHz, s.realtime);
      }
    }
    timer = setInterval(() => {
      if (!session) return;
      deliver(session.poll(performance.now()));
      void saveUnits(session.units_changed());
      void saveHeard(session, cfg);
      // The registry is rewritten whole: once a minute at most.
      if (performance.now() - registrySaved > 60_000) {
        registrySaved = performance.now();
        void saveRegistry(session);
      }
    }, 50);
    setPhase("running");
  } catch (e) {
    await teardown();
    setPhase("idle", e instanceof Error ? e.message : String(e));
  }
}

async function pumpRtl(source: number, rtl: WebRtl): Promise<void> {
  while (running) {
    try {
      const b = (await rtl.next()) as { bytes: Uint8Array; dropped: number } | undefined;
      if (!b || !session) break;
      const t = performance.now();
      session.push(source, b.bytes, b.dropped);
      session.add_busy_ms(performance.now() - t);
    } catch (e) {
      if (!running) break;
      session?.source_error(source, e instanceof Error ? e.message : String(e));
      break;
    }
  }
}

async function pumpFile(source: number, f: File, rateHz: number, realtime: boolean): Promise<void> {
  const chunk = 1 << 17; // 64 k samples
  const t0 = performance.now();
  for (let off = 0; running && off < f.size; off += chunk) {
    const bytes = new Uint8Array(await f.slice(off, Math.min(f.size, off + chunk)).arrayBuffer());
    if (!running || !session) return;
    const t = performance.now();
    session.push(source, bytes, 0);
    session.add_busy_ms(performance.now() - t);
    const due = ((off + bytes.length) / 2 / rateHz) * 1000;
    const wait = realtime ? due - (performance.now() - t0) : 0;
    await new Promise((r) => setTimeout(r, Math.max(0, wait)));
  }
  if (running && session?.source_ended(source)) await stop(true);
}

async function teardown(): Promise<void> {
  running = false;
  if (timer) clearInterval(timer);
  timer = null;
  for (const r of rtls) await r.close().catch(() => {});
  rtls = [];
}

async function stop(ended = false): Promise<void> {
  if (!session) return;
  setPhase("stopping");
  await teardown();
  const s = session;
  session = null;
  deliver(s.finish());
  await saveUnits(s.units_changed());
  await saveRegistry(s);
  if (config) await saveHeard(s, config);
  for (const [name, plan] of Object.entries(JSON.parse(s.bandplans()) as Record<string, string>)) {
    await writeText(`bandplan-${name}.txt`, plan).catch(() => {});
  }
  s.free();
  setPhase("idle", null, ended);
}

onmessage = async (ev: MessageEvent<ToWorker>) => {
  const m = ev.data;
  switch (m.type) {
    case "init":
      config = m.config;
      post({
        type: "hello",
        version: __APP_VERSION__,
        platform: "web",
        config: m.config,
        configPath: "this browser",
        devices: await devices(),
        phase,
        history: await listCalls(300),
        units: await savedUnits(m.config),
        heard: await savedHeard(m.config),
        surveyBands: await ready.then(() => JSON.parse(survey_bands())).catch(() => []),
        survey: { type: "survey", stage: "idle" },
      });
      break;
    case "setConfig": {
      // A talkgroup file changed while recording (an Ignore flag): the session takes it now.
      const old = config;
      config = m.config;
      if (session && old) {
        for (const x of m.config.systems) {
          const was = old.systems.find((o) => o.shortName === x.shortName);
          if (was && was.talkgroupsCsv !== x.talkgroupsCsv) session.set_talkgroups(x.shortName, x.talkgroupsCsv);
        }
      }
      break;
    }
    case "subscribe":
      topics = m.topics;
      session?.set_topics(JSON.stringify(topics));
      post({ type: "subscribed", topics });
      break;
    case "statsQuery": {
      const now = Math.floor(Date.now() / 1000);
      post(session ? (JSON.parse(session.stats_query(JSON.stringify(m))) as FromRecorder) : { type: "statsResult", id: m.id, from: now - 3600, to: now, loading: false, series: {} });
      break;
    }
    case "radioQuery":
      post(session ? (JSON.parse(session.radio_query(JSON.stringify(m))) as FromRecorder) : ({ type: "radioResult", id: m.id, what: m.what as "summary", error: "Start recording to see the radio system." } as FromRecorder));
      break;
    case "files":
      files = m.files;
      break;
    case "start":
      await start();
      break;
    case "stop":
      await stop();
      break;
    case "devices":
      post({ type: "devices", devices: await devices() });
      break;
    case "listen":
      listen = { on: m.on, system: m.system, talkgroup: m.talkgroup };
      session?.set_want_audio(m.on);
      break;
    case "surveyStart":
      await surveyStart(m);
      break;
    case "surveyListen":
      survey?.listen(m.freqHz);
      break;
    case "surveyRescan":
      survey?.rescan();
      break;
    case "surveyStop":
      await surveyStop();
      break;
  }
};

