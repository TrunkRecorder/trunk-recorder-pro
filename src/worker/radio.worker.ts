/// <reference lib="webworker" />
// The radio worker: owns the SDR and the shared channelizer, and nothing else.
// Its loop is USB read → channelize → write each channel into its SampleRing;
// all decoding lives in the trunk worker, so a slow decode can never stall USB
// reads (freq-finder measured exactly that failure: dropped samples, choppy
// audio). The trunk worker opens/closes channels over a MessagePort.

import { Channelizer } from "../engine/channelizer.ts";
import { SampleRing } from "../engine/ring.ts";
import { FileSource } from "../sources/fileSource.ts";
import type { IqSource } from "../sources/iqSource.ts";
import { RtlSource, UsbStallError } from "../sources/rtlSource.ts";
import type { FromRadio, ToRadio, TrunkToRadio } from "./messages.ts";

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: FromRadio) => ctx.postMessage(m);

let chz: Channelizer | null = null;
let source: IqSource | null = null;
let running = false;
const heads = new Map<number, { headId: number; ring: SampleRing }>();
const pendingOpens: TrunkToRadio[] = [];

function onTrunk(m: TrunkToRadio): void {
  if (!chz) {
    pendingOpens.push(m);
    return;
  }
  if (m.type === "open") {
    const ring = new SampleRing(m.ring);
    const { id: headId } = chz.addHead(m.offsetHz, m.cutoffHz, (iq) => ring.write(iq), m.prerollS);
    heads.set(m.id, { headId, ring });
  } else {
    const h = heads.get(m.id);
    if (h) chz.removeHead(h.headId);
    heads.delete(m.id);
  }
}

async function run(msg: Extract<ToRadio, { type: "start" }>): Promise<void> {
  msg.trunkPort.onmessage = (ev: MessageEvent<TrunkToRadio>) => onTrunk(ev.data);
  chz = new Channelizer({ fs: msg.settings.rateHz, minOutputRate: msg.minOutputRate, historyS: msg.historyS });
  for (const m of pendingOpens.splice(0)) onTrunk(m);
  source = msg.source.kind === "usb" ? new RtlSource() : new FileSource(msg.source.file, msg.source.realtime);
  try {
    await source.open(msg.settings);
  } catch (e) {
    post({ type: "error", message: e instanceof Error ? e.message : String(e) });
    return;
  }
  post({ type: "started", outputRate: chz.outputRate });
  running = true;

  let recoveries = 0;
  let samples = 0;
  let busyMs = 0;
  let windowSamples = 0;
  let windowStart = performance.now();
  let lastStats = windowStart;
  while (running) {
    let u8: Uint8Array | null;
    try {
      u8 = await source.read();
    } catch (e) {
      if (!running) break;
      // A stuck USB transfer: reopen the dongle and carry on. The sample clock
      // simply skips the lost air; calls in progress see a gap, not a crash.
      if (e instanceof UsbStallError && msg.source.kind === "usb" && recoveries < 20) {
        recoveries++;
        await source.close().catch(() => {});
        source = new RtlSource();
        try {
          await source.open(msg.settings);
          continue;
        } catch (e2) {
          post({ type: "error", message: `The dongle stopped responding and couldn't be reopened: ${e2 instanceof Error ? e2.message : String(e2)}` });
          break;
        }
      }
      post({ type: "error", message: `Read failed: ${e instanceof Error ? e.message : String(e)}` });
      break;
    }
    if (!u8) {
      post({ type: "ended" });
      break;
    }
    const t0 = performance.now();
    chz.pushU8(u8);
    const t1 = performance.now();
    busyMs += t1 - t0;
    samples += u8.length >> 1;
    windowSamples += u8.length >> 1;
    if (t1 - lastStats >= 250) {
      const airMs = (windowSamples / msg.settings.rateHz) * 1000;
      let ringDrops = 0;
      for (const h of heads.values()) ringDrops += h.ring.stats().dropped;
      post({
        type: "stats",
        stats: {
          rateMeasured: (windowSamples / (t1 - windowStart)) * 1000,
          airS: samples / msg.settings.rateHz,
          load: airMs > 0 ? busyMs / airMs : 0,
          heads: chz.headCount,
          ringDrops,
          usbRecoveries: recoveries,
          spectrum: chz.powerSpectrum(1024),
          centerHz: msg.settings.centerHz,
          rateHz: msg.settings.rateHz,
        },
      });
      busyMs = 0;
      windowSamples = 0;
      windowStart = t1;
      lastStats = t1;
    }
  }
  await source.close().catch(() => {});
}

ctx.onmessage = (ev: MessageEvent<ToRadio>) => {
  const m = ev.data;
  if (m.type === "start") {
    void run(m);
  } else if (m.type === "stop") {
    running = false;
    void source?.close().catch(() => {});
  }
};
