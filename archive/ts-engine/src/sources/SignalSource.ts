// Abstraction over an IQ sample source so the sweep engine can run against
// either a real RTL-SDR dongle (WebUSB) or a synthetic signal (for dev / no HW).
// Ported from Sources/TVPower/SDR/SignalSource.swift. Everything is async because
// WebUSB is promise-based — this is the single most invasive change vs. native.

export type GainConfig = { kind: "auto" } | { kind: "manual"; tenthsDb: number };

export const GainConfig = {
  auto(): GainConfig {
    return { kind: "auto" };
  },
  // RTL-SDR gain in tenths of a dB (e.g. 297 = 29.7 dB).
  manual(tenthsDb: number): GainConfig {
    return { kind: "manual", tenthsDb };
  },
  equal(a: GainConfig, b: GainConfig): boolean {
    if (a.kind !== b.kind) return false;
    if (a.kind === "manual" && b.kind === "manual") return a.tenthsDb === b.tenthsDb;
    return true;
  },
};

export interface SignalSource {
  readonly name: string;
  open(): Promise<void>;
  setSampleRate(hz: number): Promise<void>;
  setCenterFreq(hz: number): Promise<void>;
  setGain(config: GainConfig): Promise<void>;
  setFreqCorrection(ppm: number): Promise<void>;
  /** Called on the engine thread right after retuning, before reading. */
  prepareRead(): Promise<void>;
  /**
   * Fill/return interleaved 8-bit IQ for `sampleCount` samples
   * (2*sampleCount bytes). Resolves with the bytes actually read.
   */
  readIQ(sampleCount: number): Promise<Uint8Array>;
  close(): Promise<void>;
  /** Manual gains in dB the tuner actually supports (empty until known). */
  availableGainsDb?(): number[];
  /**
   * Signal whether samples are wanted right now. Only meaningful for a source
   * that streams over a shared/metered link (the remote agent): when off, the
   * agent stops reading the dongle and sending IQ. Local sources ignore it.
   */
  setStreaming?(on: boolean): void;
  /** Where the samples come from — drives the link indicator's glyph, and
   *  whether loss can be inferred from timing (a simulated source never loses). */
  readonly kind?: "usb" | "network" | "sim";
  /** How many reads the engine may keep in flight. A USB dongle wants several
   *  (its on-chip FIFO is only milliseconds deep); a source that parks one
   *  reader at a time must leave this at 1. */
  readonly readAheadDepth?: number;
  /** Cumulative samples the source itself discarded (e.g. a remote queue
   *  trimmed because the engine fell behind). */
  droppedSamples?(): number;
}
