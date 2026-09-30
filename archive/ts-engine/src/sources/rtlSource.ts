// RTL-SDR over WebUSB (freq-finder's WebRtlSource), read with several transfers
// in flight: the RTL2832U's on-chip FIFO is only milliseconds deep, so a queue
// of ~4 × 27 ms is what rides out a stall on this thread.

import { WebRtlSource } from "./WebRtlSource.ts";
import type { IqSource, SourceSettings } from "./iqSource.ts";

const CHUNK_SAMPLES = 65536;
const DEPTH = 4;
/** A transfer that hasn't landed in this long is stuck, not slow. */
const STALL_MS = 3000;

export class UsbStallError extends Error {}

export class RtlSource implements IqSource {
  readonly kind = "usb" as const;
  private dev: WebRtlSource | null = null;
  private pending: Promise<Uint8Array>[] = [];
  private closed = false;

  async open(s: SourceSettings): Promise<void> {
    const dev = new WebRtlSource(s.serial || undefined);
    await dev.open();
    await dev.setSampleRate(s.rateHz);
    await dev.setFreqCorrection(Math.round(s.ppm));
    await dev.setCenterFreq(s.centerHz);
    await dev.setGain(s.gainDb === null ? { kind: "auto" } : { kind: "manual", tenthsDb: Math.round(s.gainDb * 10) });
    await dev.prepareRead();
    this.dev = dev;
  }

  async read(): Promise<Uint8Array | null> {
    const dev = this.dev;
    if (!dev || this.closed) return null;
    while (this.pending.length < DEPTH) {
      const p = dev.readIQ(CHUNK_SAMPLES);
      p.catch(() => {});
      this.pending.push(p);
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stall = new Promise<never>((_, rej) => {
      timer = setTimeout(() => rej(new UsbStallError(`USB read stalled for ${STALL_MS / 1000} s`)), STALL_MS);
    });
    try {
      return await Promise.race([this.pending.shift()!, stall]);
    } finally {
      clearTimeout(timer);
    }
  }

  async close(): Promise<void> {
    this.closed = true;
    // Don't wait forever on transfers that are stuck (that may be why we're closing).
    await Promise.race([Promise.allSettled(this.pending), new Promise((r) => setTimeout(r, 500))]);
    this.pending = [];
    await this.dev?.close();
    this.dev = null;
  }
}
