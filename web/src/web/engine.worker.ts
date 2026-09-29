// The recorder inside the browser: the Rust engine (WebAssembly) and the
// RTL-SDR driver (WebUSB), speaking the same protocol as the desktop app's
// server, so the interface is identical. Calls are stored in OPFS.

import init, { WebRtl, WebSession } from "./pkg/trunk_web.js";
import type { Config, FromRecorder } from "../protocol.ts";
import { resolvedCenters } from "../config.ts";
import { listCalls, readText, saveCall, writeText } from "./opfs.ts";

export type ToWorker =
  | { type: "init"; config: Config }
  | { type: "setConfig"; config: Config }
  | { type: "files"; files: (File | null)[] }
  | { type: "start" }
  | { type: "stop" }
  | { type: "devices" }
  | { type: "listen"; on: boolean; talkgroup: number | null };

/** Worker → page: a protocol message, or a live audio frame. */
export type FromWorker = { msg: FromRecorder } | { audio: ArrayBuffer; tg: number };

const post = (msg: FromRecorder) => postMessage({ msg } satisfies FromWorker);

let config: Config | null = null;
let files: (File | null)[] = [];
let session: WebSession | null = null;
let rtls: WebRtl[] = [];
let timer: ReturnType<typeof setInterval> | null = null;
let running = false;
let listen: { on: boolean; talkgroup: number | null } = { on: false, talkgroup: null };
let phase: FromRecorder & { type: "state" } = { type: "state", phase: "idle", error: null, ended: false };
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
function deliver(outs: { t: string; json?: string; tg?: number; frame?: Uint8Array; rel?: string; wav?: Uint8Array; entry?: string }[]): void {
  for (const o of outs) {
    if (o.t === "text") post(JSON.parse(o.json!) as FromRecorder);
    else if (o.t === "audio") {
      if (listen.on && (listen.talkgroup === null || listen.talkgroup === o.tg)) {
        const buf = o.frame!.slice().buffer;
        postMessage({ audio: buf, tg: o.tg! } satisfies FromWorker, { transfer: [buf] });
      }
    } else if (o.t === "file") {
      void saveCall(o.rel!, o.wav!, o.json!, JSON.parse(o.entry!)).catch((e) =>
        post({ type: "log", lines: [{ timeS: 0, kind: "error", text: `couldn't store ${o.rel}: ${e instanceof Error ? e.message : String(e)}` }] }),
      );
    }
  }
}

async function start(): Promise<void> {
  if (running || !config) return;
  setPhase("starting");
  try {
    await ready;
    const cfg = config;
    const plan = await readText(`bandplan-${cfg.system.shortName}.txt`);
    session = new WebSession(JSON.stringify(cfg), Date.now(), plan);
    session.set_want_audio(listen.on);
    const centers = resolvedCenters(cfg);
    running = true;
    rtls = [];
    for (let i = 0; i < cfg.sources.length; i++) {
      const s = cfg.sources[i];
      if (s.kind === "rtlsdr") {
        const rtl = await WebRtl.open(s.serial, centers[i] ?? s.centerHz, s.rateHz, s.gainDb ?? undefined, s.ppm);
        rtls.push(rtl);
        void pumpRtl(i, rtl);
      } else if (s.kind === "usrp" || s.kind === "airspy") {
        throw new Error(`Source ${i + 1}: USRP and Airspy need the desktop app.`);
      } else {
        if (s.format && s.format !== "cu8") throw new Error(`Source ${i + 1}: the browser version reads rtl_sdr (cu8) captures only.`);
        const f = files[i];
        if (!f) throw new Error(`Source ${i + 1}: choose the capture file again (the browser forgets it on reload).`);
        void pumpFile(i, f, s.rateHz, s.realtime);
      }
    }
    timer = setInterval(() => {
      if (session) deliver(session.poll(performance.now()));
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
  if (config) await writeText(`bandplan-${config.system.shortName}.txt`, s.bandplan()).catch(() => {});
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
      });
      break;
    case "setConfig":
      config = m.config;
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
      listen = { on: m.on, talkgroup: m.talkgroup };
      session?.set_want_audio(m.on);
      break;
  }
};

