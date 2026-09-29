// Shared fast-convolution channelizer (overlap-save), after CyberEther's
// multi-head `filter_engine`. ONE forward FFT of each wideband block serves
// every channel ("head"); a head costs an M-bin window × filter multiply, an
// M-point inverse FFT (which also decimates by D = N/M), and a phase rotation.
// Measured in research/bench: 32 channels at 2.4 MSPS ≈ 12.5 % of one core.
//
// Buffers are allocated once (constructor / addHead) and reused every block.
//
// Timing is absolute. Block b holds wideband input samples [b·L, (b+1)·L);
// output sample k of a head that started at block b0 corresponds to input
// sample b0·L + k·D. Head phase corrections are computed from the absolute
// block index, so a head's output is the same whether it was added live or
// replayed from history.
//
// Pre-roll: the last `historyBlocks` spectra are kept (as f32). A head added
// with pre-roll first replays up to that many stored spectra — so a voice
// channel opened by a grant starts from BEFORE the grant arrived — then runs
// live. The replay costs only M-point work per block, no forward FFTs.

import { fft } from "./fft.ts";

export interface ChannelizerOptions {
  /** Wideband sample rate, Hz. */
  fs: number;
  /** Lowest per-channel output rate any decoder needs, Hz. */
  minOutputRate: number;
  /** Seconds of spectra to keep for pre-roll. */
  historyS?: number;
  fftSize?: number;
  taps?: number;
}

/** The per-channel output rate a Channelizer with these settings will produce. */
export function channelizerOutputRate(fs: number, minOutputRate: number, fftSize = 16384): number {
  let d = 1;
  while (d * 2 <= fftSize / 64 && fs / (d * 2) >= minOutputRate) d *= 2;
  return fs / d;
}

export type HeadSink = (iq: Float32Array) => void;

interface Filter {
  re: Float64Array;
  im: Float64Array;
}

interface Head {
  id: number;
  offsetHz: number;
  bin: number;
  /** Output-rate NCO for the sub-bin residual, rad/sample. */
  residual: number;
  filter: Filter;
  /** Absolute index of the head's first block; for its output timing. */
  startBlock: number;
  re: Float64Array;
  im: Float64Array;
  out: Float32Array;
  sink: HeadSink;
}

export class Channelizer {
  readonly fs: number;
  readonly n: number;
  readonly m: number;
  readonly l: number;
  readonly decim: number;
  readonly outputRate: number;
  private readonly p: number;
  private readonly xRe: Float64Array;
  private readonly xIm: Float64Array;
  private readonly histRe: Float64Array;
  private readonly histIm: Float64Array;
  private readonly spectra: Float32Array; // ring of `historyCap` blocks × 2N
  private readonly historyCap: number;
  private historyCount = 0;
  private fill = 0;
  private block = 0; // absolute index of the NEXT block to run
  private readonly heads = new Map<number, Head>();
  private readonly filters = new Map<number, Filter>();
  private nextId = 1;

  constructor(opts: ChannelizerOptions) {
    this.fs = opts.fs;
    this.n = opts.fftSize ?? 16384;
    this.p = opts.taps ?? 4097;
    // Largest power-of-two decimation that still meets the output rate.
    this.decim = Math.round(opts.fs / channelizerOutputRate(opts.fs, opts.minOutputRate, this.n));
    const d = this.decim;
    this.m = this.n / d;
    this.l = this.n - this.p + 1;
    if ((this.p - 1) % d || this.l % d || this.l <= 0) throw new Error("channelizer: taps/FFT size incompatible with decimation");
    this.outputRate = opts.fs / d;
    this.xRe = new Float64Array(this.n);
    this.xIm = new Float64Array(this.n);
    this.histRe = new Float64Array(this.p - 1);
    this.histIm = new Float64Array(this.p - 1);
    this.historyCap = Math.max(1, Math.ceil(((opts.historyS ?? 1) * opts.fs) / this.l));
    this.spectra = new Float32Array(this.historyCap * 2 * this.n);
  }

  /** Seconds of air a block of input represents. */
  get blockSeconds(): number {
    return this.l / this.fs;
  }

  /** Absolute input sample index at which the next block begins. */
  get samplePosition(): number {
    return this.block * this.l + this.fill;
  }

  get headCount(): number {
    return this.heads.size;
  }

  /** Seconds of pre-roll currently available. */
  get historySeconds(): number {
    return (this.historyCount * this.l) / this.fs;
  }

  /**
   * Start a channel. `prerollS` replays up to that much stored air first (as
   * much as the history holds). Returns the head id and the absolute input
   * sample its first output sample corresponds to.
   */
  addHead(offsetHz: number, cutoffHz: number, sink: HeadSink, prerollS = 0): { id: number; startSample: number } {
    const bin = Math.round((offsetHz / this.fs) * this.n);
    const residualHz = offsetHz - (bin * this.fs) / this.n;
    const replay = Math.min(this.historyCount, Math.ceil((prerollS * this.fs) / this.l));
    const h: Head = {
      id: this.nextId++,
      offsetHz,
      bin: ((bin % this.n) + this.n) % this.n,
      residual: (-2 * Math.PI * residualHz) / this.outputRate,
      filter: this.filterFor(cutoffHz),
      startBlock: this.block - replay,
      re: new Float64Array(this.m),
      im: new Float64Array(this.m),
      out: new Float32Array(2 * (this.l / this.decim)),
      sink,
    };
    for (let k = replay; k >= 1; k--) this.runHead(h, this.block - k);
    this.heads.set(h.id, h);
    return { id: h.id, startSample: h.startBlock * this.l };
  }

  /**
   * Power spectrum of the most recent block, fft-shifted (bin 0 = −fs/2),
   * averaged down to `bins` bins, in dBFS. Free for the waterfall: it reads the
   * spectrum the channelizer already computed.
   */
  powerSpectrum(bins: number): Float32Array {
    const out = new Float32Array(bins);
    if (!this.historyCount) return out.fill(-120);
    const { n } = this;
    const slot = ((this.block - 1) % this.historyCap) * 2 * n;
    const s = this.spectra;
    const per = n / bins;
    // Rectangular window over N samples: a full-scale tone is |X| = N.
    const norm = 1 / (n * n);
    for (let b = 0; b < bins; b++) {
      let acc = 0;
      for (let k = 0; k < per; k++) {
        const i = (b * per + k + n / 2) % n;
        acc += s[slot + 2 * i] ** 2 + s[slot + 2 * i + 1] ** 2;
      }
      out[b] = 10 * Math.log10((acc / per) * norm + 1e-14);
    }
    return out;
  }

  removeHead(id: number): void {
    this.heads.delete(id);
  }

  /** Feed interleaved float IQ. */
  push(iq: Float32Array): void {
    const total = iq.length >> 1;
    const base = this.p - 1;
    let i = 0;
    while (i < total) {
      const take = Math.min(this.l - this.fill, total - i);
      const o = base + this.fill;
      for (let k = 0; k < take; k++) {
        this.xRe[o + k] = iq[2 * (i + k)];
        this.xIm[o + k] = iq[2 * (i + k) + 1];
      }
      this.fill += take;
      i += take;
      if (this.fill === this.l) this.runBlock();
    }
  }

  /** Feed RTL-SDR native unsigned 8-bit interleaved IQ. */
  pushU8(u8: Uint8Array): void {
    const total = u8.length >> 1;
    const base = this.p - 1;
    let i = 0;
    while (i < total) {
      const take = Math.min(this.l - this.fill, total - i);
      const o = base + this.fill;
      for (let k = 0; k < take; k++) {
        this.xRe[o + k] = (u8[2 * (i + k)] - 127.5) / 127.5;
        this.xIm[o + k] = (u8[2 * (i + k) + 1] - 127.5) / 127.5;
      }
      this.fill += take;
      i += take;
      if (this.fill === this.l) this.runBlock();
    }
  }

  private filterFor(cutoffHz: number): Filter {
    const key = Math.round(cutoffHz);
    let f = this.filters.get(key);
    if (f) return f;
    // Blackman windowed-sinc (≈ −74 dB stopband), then its N-point spectrum,
    // keeping only the M bins around DC — outside them the stopband is below
    // any noise we would alias in. 1/N is the overlap-save IFFT scale.
    const { n, p, m } = this;
    const re = new Float64Array(n);
    const im = new Float64Array(n);
    const fc = cutoffHz / this.fs;
    let sum = 0;
    for (let i = 0; i < p; i++) {
      const k = i - (p - 1) / 2;
      const sinc = k === 0 ? 2 * fc : Math.sin(2 * Math.PI * fc * k) / (Math.PI * k);
      const w = 0.42 - 0.5 * Math.cos((2 * Math.PI * i) / (p - 1)) + 0.08 * Math.cos((4 * Math.PI * i) / (p - 1));
      re[i] = sinc * w;
      sum += re[i];
    }
    for (let i = 0; i < p; i++) re[i] /= sum;
    fft(re, im, n);
    f = { re: new Float64Array(m), im: new Float64Array(m) };
    for (let k = 0; k < m; k++) {
      const src = k < m / 2 ? k : n - m + k;
      f.re[k] = re[src] / n;
      f.im[k] = im[src] / n;
    }
    this.filters.set(key, f);
    return f;
  }

  private runBlock(): void {
    const { n, p } = this;
    const P1 = p - 1;
    const xRe = this.xRe;
    const xIm = this.xIm;
    xRe.set(this.histRe, 0);
    xIm.set(this.histIm, 0);
    this.histRe.set(xRe.subarray(n - P1));
    this.histIm.set(xIm.subarray(n - P1));
    fft(xRe, xIm, n);
    // Store the spectrum (interleaved f32) in the history ring.
    const slot = (this.block % this.historyCap) * 2 * n;
    const s = this.spectra;
    for (let k = 0; k < n; k++) {
      s[slot + 2 * k] = xRe[k];
      s[slot + 2 * k + 1] = xIm[k];
    }
    if (this.historyCount < this.historyCap) this.historyCount++;
    for (const h of this.heads.values()) this.runHead(h, this.block);
    this.block++;
    this.fill = 0;
  }

  /** Run one head over the stored spectrum of absolute block `b`. */
  private runHead(h: Head, b: number): void {
    const { n, m, l, decim } = this;
    const slot = (b % this.historyCap) * 2 * n;
    const s = this.spectra;
    const re = h.re;
    const im = h.im;
    const fr = h.filter.re;
    const fi = h.filter.im;
    const half = m / 2;
    for (let k = 0; k < m; k++) {
      const off = k < half ? k : k - m;
      const src = slot + 2 * ((h.bin + off + n) % n);
      const a = s[src];
      const c = s[src + 1];
      re[k] = a * fr[k] - c * fi[k];
      im[k] = a * fi[k] + c * fr[k];
    }
    fft(re, im, m, true);
    // Overlap-save shift correction e^{-j2π·bin·L·b/N} (integer arithmetic mod
    // N keeps it exact forever) plus the residual NCO from the absolute output
    // sample index.
    const shiftTurns = ((((h.bin * l) % n) * (b % n)) % n) / n;
    const nOut = l / decim;
    const firstOut = b * nOut; // absolute output index: same phase however the head started
    const keepFrom = (this.p - 1) / decim;
    const ph = -2 * Math.PI * shiftTurns + ((h.residual * firstOut) % (2 * Math.PI));
    const out = h.out;
    const rc = Math.cos(h.residual);
    const rs = Math.sin(h.residual);
    let cr = Math.cos(ph);
    let ci = Math.sin(ph);
    for (let j = 0; j < nOut; j++) {
      const a = re[keepFrom + j];
      const c = im[keepFrom + j];
      out[2 * j] = a * cr - c * ci;
      out[2 * j + 1] = a * ci + c * cr;
      const t = cr * rc - ci * rs;
      ci = cr * rs + ci * rc;
      cr = t;
    }
    h.sink(out);
  }
}
