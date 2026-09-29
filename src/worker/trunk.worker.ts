/// <reference lib="webworker" />
// The trunk worker: every decoder and all trunking logic (TrunkEngine), fed
// from the SampleRings the radio worker fills. Concluded calls are written to
// OPFS here; the page gets status, a message log, and live audio.

import { SampleRing } from "../engine/ring.ts";
import { driverFor } from "../protocols/registry.ts";
import { TrunkEngine, type ChannelPort } from "../trunking/trunkEngine.ts";
import { parseTalkgroupCsv } from "../trunking/talkgroups.ts";
import type { Call } from "../trunking/callManager.ts";
import { encodeWav } from "../recording/wav.ts";
import { saveCall } from "../recording/opfsStore.ts";
import type { CallView, FromTrunk, LogLine, ToTrunk, TrunkToRadio } from "./messages.ts";

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: FromTrunk, t: Transferable[] = []) => ctx.postMessage(m, t);

/** Seconds of channel IQ each ring holds (≥ pre-roll burst + scheduling slack). */
const RING_S = 3;
const POLL_MS = 40;

interface Stream {
  ring: SampleRing;
  onIq: (iq: Float32Array) => void;
}

let engine: TrunkEngine | null = null;
/** OPFS sync handles are exclusive: one save at a time (they share the index). */
let saves: Promise<unknown> = Promise.resolve();
let running = false;
const streams = new Map<number, Stream>();

function view(c: Call): CallView {
  return {
    id: c.id,
    talkgroup: c.talkgroup,
    alphaTag: c.talkgroupInfo?.alphaTag ?? "",
    freqHz: c.freqHz,
    slot: c.phase2Tdma ? c.tdmaSlot : null,
    state: c.state,
    reason: c.reason,
    encrypted: c.encrypted,
    emergency: c.emergency,
    startS: c.startS,
    sources: c.sources.map((s) => s.src),
  };
}

async function start(m: Extract<ToTrunk, { type: "start" }>): Promise<void> {
  const cfg = m.config;
  const radio = m.radioPort;
  const send = (x: TrunkToRadio) => radio.postMessage(x);
  const driver = driverFor(cfg.system.type, { modulation: cfg.system.modulation });
  let nextId = 1;
  const port: ChannelPort = {
    open(offsetHz, cutoffHz, prerollS, onIq) {
      const id = nextId++;
      const ring = new SampleRing(Math.ceil(2 * m.channelRate * (RING_S + prerollS)));
      streams.set(id, { ring, onIq });
      send({ type: "open", id, offsetHz, cutoffHz, prerollS, ring: ring.buffer as SharedArrayBuffer });
      return id;
    },
    close(id) {
      streams.delete(id);
      send({ type: "close", id });
    },
  };

  let log: LogLine[] = [];
  engine = new TrunkEngine(
    {
      system: {
        shortName: cfg.system.shortName,
        type: cfg.system.type,
        controlChannels: cfg.system.controlChannels,
        talkgroups: cfg.system.talkgroupsCsv ? parseTalkgroupCsv(cfg.system.talkgroupsCsv) : undefined,
      },
      centerHz: cfg.source.centerHz,
      rateHz: cfg.source.rateHz,
      channelRate: m.channelRate,
      prerollS: cfg.recording.prerollS,
      maxRecorders: cfg.recording.maxRecorders,
      keepSilentCalls: cfg.recording.keepSilentCalls,
      calls: {
        callTimeoutS: cfg.recording.callTimeoutS,
        recordUnknown: cfg.recording.recordUnknown,
        recordEncrypted: cfg.recording.recordEncrypted,
        recordUnitToUnit: cfg.recording.recordUnitToUnit,
      },
      epochMsAtZero: m.epochMsAtZero,
    },
    driver,
    port,
    {
      onMessages: (msgs) => {
        for (const x of msgs) if (x.type !== "unknown" || x.meta) log.push({ timeS: x.timeS, kind: x.type, text: x.meta });
      },
      onControlChannel: (hz) => log.push({ timeS: engine?.status().nowS ?? 0, kind: "control", text: `Control channel ${(hz / 1e6).toFixed(5)} MHz` }),
      onAudio: (call, samples) => {
        const s = samples.slice();
        post({ type: "audio", callId: call.id, talkgroup: call.talkgroup, samples: s }, [s.buffer]);
      },
      onConcluded: (c) => {
        const wav = encodeWav(c.audio, c.audioRate);
        saves = saves
          .then(() => saveCall(cfg.system.shortName, c.baseName, c.record, wav))
          .then((entry) => post({ type: "concluded", entry }))
          .catch((e) => post({ type: "error", message: `Saving ${c.baseName} failed: ${e instanceof Error ? e.message : String(e)}` }));
      },
    },
  );

  try {
    engine.start();
  } catch (e) {
    post({ type: "error", message: e instanceof Error ? e.message : String(e) });
    return;
  }
  running = true;

  // Drain loop: read every ring, control channel first (it drives the clock).
  const scratch = new Float32Array(2 * Math.ceil(m.channelRate * 0.25));
  let lastStatus = 0;
  while (running && engine) {
    let backlog = 0;
    for (const [, st] of [...streams]) {
      backlog = Math.max(backlog, st.ring.size / 2 / m.channelRate);
      for (let guard = 0; guard < 64; guard++) {
        const n = st.ring.read(scratch);
        if (!n) break;
        st.onIq(scratch.subarray(0, n));
      }
    }
    const now = performance.now();
    if (now - lastStatus > 250) {
      lastStatus = now;
      post({ type: "status", status: engine.status(), calls: engine.calls.calls.map(view), backlogS: backlog });
      if (log.length) {
        post({ type: "log", lines: log.slice(-200) });
        log = [];
      }
    }
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
}

ctx.onmessage = (ev: MessageEvent<ToTrunk>) => {
  const m = ev.data;
  if (m.type === "start") {
    void start(m).catch((e) => post({ type: "error", message: e instanceof Error ? e.message : String(e) }));
  } else if (m.type === "stop") {
    running = false;
    engine?.stop();
    engine = null;
  }
};
