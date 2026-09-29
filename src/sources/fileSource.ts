// Replays an rtl_sdr capture (.cu8: unsigned 8-bit interleaved IQ) from a File,
// paced to real time (or as fast as the engine can take it). Lets anyone try
// the whole pipeline with no dongle, and makes a bug on the air reproducible.

import type { IqSource, SourceSettings } from "./iqSource.ts";

const CHUNK_BYTES = 1 << 17; // 65536 samples

export class FileSource implements IqSource {
  readonly kind = "file" as const;
  private pos = 0;
  private rate = 0;
  private t0 = 0;
  private readonly file: File;
  private readonly realtime: boolean;

  constructor(file: File, realtime = true) {
    this.file = file;
    this.realtime = realtime;
  }

  async open(s: SourceSettings): Promise<void> {
    this.rate = s.rateHz;
    this.pos = 0;
    this.t0 = performance.now();
  }

  async read(): Promise<Uint8Array | null> {
    if (this.pos >= this.file.size) return null;
    if (this.realtime) {
      const due = this.t0 + (this.pos / 2 / this.rate) * 1000;
      const wait = due - performance.now();
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
    }
    const end = Math.min(this.file.size, this.pos + CHUNK_BYTES);
    const buf = new Uint8Array(await this.file.slice(this.pos, end).arrayBuffer());
    this.pos = end;
    return buf.subarray(0, buf.length & ~1);
  }

  async close(): Promise<void> {
    this.pos = this.file.size;
  }
}
