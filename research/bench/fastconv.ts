// Multi-channel fast-convolution channelizer (overlap-save), after CyberEther's
// `filter_engine` block: ONE forward FFT of the wideband block is shared by every
// channel; each channel ("head") then costs a bin-window multiply by the filter
// spectrum, a small inverse FFT, and a phase correction. Decimation happens in
// the frequency domain (keep M = N/D bins around the channel), so the per-head
// IFFT is only M points.
//
// All buffers are allocated once, at construction (CyberEther's "allocate in
// create(), reuse every cycle" rule). `push()` accepts any amount of input and
// runs a block whenever L new samples have accumulated.
//
// Prototype for the feasibility study — not production code.

const FFT_CACHE = new Map<number, { cos: Float64Array; sin: Float64Array; rev: Uint32Array }>();

function plan(n: number) {
  let p = FFT_CACHE.get(n);
  if (p) return p;
  const cos = new Float64Array(n / 2), sin = new Float64Array(n / 2), rev = new Uint32Array(n);
  for (let i = 0; i < n / 2; i++) {
    cos[i] = Math.cos((-2 * Math.PI * i) / n);
    sin[i] = Math.sin((-2 * Math.PI * i) / n);
  }
  const bits = Math.log2(n);
  for (let i = 0; i < n; i++) {
    let r = 0;
    for (let b = 0, x = i; b < bits; b++, x >>= 1) r = (r << 1) | (x & 1);
    rev[i] = r;
  }
  p = { cos, sin, rev };
  FFT_CACHE.set(n, p);
  return p;
}

/** In-place radix-2 FFT; `inverse` flips the twiddle sign (no 1/n scaling). */
function fft(re: Float64Array, im: Float64Array, n: number, inverse: boolean): void {
  const { cos, sin, rev } = plan(n);
  for (let i = 0; i < n; i++) {
    const j = rev[i];
    if (j > i) {
      let t = re[i]; re[i] = re[j]; re[j] = t;
      t = im[i]; im[i] = im[j]; im[j] = t;
    }
  }
  const sg = inverse ? -1 : 1;
  for (let size = 2; size <= n; size <<= 1) {
    const half = size >> 1, step = n / size;
    for (let i = 0; i < n; i += size) {
      for (let j = i, k = 0; j < i + half; j++, k += step) {
        const c = cos[k], s = sg * sin[k];
        const a = j + half;
        const tr = re[a] * c - im[a] * s, ti = re[a] * s + im[a] * c;
        re[a] = re[j] - tr; im[a] = im[j] - ti;
        re[j] += tr; im[j] += ti;
      }
    }
  }
}

export interface Head {
  /** Channel centre, Hz offset from the tuned centre. */
  offsetHz: number;
  /** Called with each block of channel IQ (interleaved, output rate). The
   *  array is reused on the next block — copy it if you keep it. */
  onOutput: (iq: Float32Array) => void;
}

interface HeadState {
  bin: number; // channel centre, in FFT bins (rounded)
  residual: number; // remaining offset, rad/sample at the output rate
  blockPhase: number; // overlap-save shift correction, rad
  blockStep: number; // correction increment per block
  ncoPhase: number;
  re: Float64Array; im: Float64Array; out: Float32Array;
  onOutput: (iq: Float32Array) => void;
}

export class FastConvChannelizer {
  readonly n: number; // forward FFT size
  readonly m: number; // per-head IFFT size = n / decim
  readonly l: number; // new samples per block
  readonly decim: number;
  readonly outRate: number;
  private readonly p: number; // filter taps
  private readonly hRe: Float64Array; // filter spectrum, M bins centred on DC (FFT order)
  private readonly hIm: Float64Array;
  private readonly xRe: Float64Array; private readonly xIm: Float64Array; // FFT work
  private readonly histRe: Float64Array; private readonly histIm: Float64Array; // last P-1 inputs
  private fill = 0; // new samples accumulated in xRe/xIm after the history
  private heads = new Map<number, HeadState>();
  private nextId = 1;

  readonly fs: number;

  constructor(fs: number, opts: { fftSize: number; decim: number; taps: number; cutoffHz: number }) {
    this.fs = fs;
    const { fftSize: n, decim, taps: p } = opts;
    if ((p - 1) % decim || (n - p + 1) % decim) throw new Error("P-1 and L must be multiples of decim");
    this.n = n; this.decim = decim; this.p = p; this.m = n / decim; this.l = n - p + 1;
    this.outRate = fs / decim;
    // Windowed-sinc low-pass (Blackman: ~-74 dB stopband), then its N-point spectrum.
    const fre = new Float64Array(n), fim = new Float64Array(n);
    const fc = opts.cutoffHz / fs;
    let sum = 0;
    for (let i = 0; i < p; i++) {
      const k = i - (p - 1) / 2;
      const sinc = k === 0 ? 2 * fc : Math.sin(2 * Math.PI * fc * k) / (Math.PI * k);
      const w = 0.42 - 0.5 * Math.cos((2 * Math.PI * i) / (p - 1)) + 0.08 * Math.cos((4 * Math.PI * i) / (p - 1));
      fre[i] = sinc * w;
      sum += fre[i];
    }
    for (let i = 0; i < p; i++) fre[i] /= sum;
    fft(fre, fim, n, false);
    // Keep only the M bins around DC: [0, M/2) and [N-M/2, N). Folding the rest
    // in would be exact decimation; with a -74 dB stopband outside ±M/2 bins it
    // is below the noise, so dropping them is equivalent and M× cheaper.
    this.hRe = new Float64Array(this.m); this.hIm = new Float64Array(this.m);
    const half = this.m / 2;
    for (let k = 0; k < this.m; k++) {
      const src = k < half ? k : n - this.m + k;
      // y[nD] = (1/N)·Σ Y[k]·e^{j2πkn/M}: an unscaled M-point IFFT of the
      // windowed bins, times 1/N — folded into the filter here.
      this.hRe[k] = fre[src] / n;
      this.hIm[k] = fim[src] / n;
    }
    this.xRe = new Float64Array(n); this.xIm = new Float64Array(n);
    this.histRe = new Float64Array(p - 1); this.histIm = new Float64Array(p - 1);
  }

  addHead(h: Head): number {
    const bin = Math.round((h.offsetHz / this.fs) * this.n);
    const residualHz = h.offsetHz - (bin * this.fs) / this.n;
    const id = this.nextId++;
    this.heads.set(id, {
      bin: ((bin % this.n) + this.n) % this.n,
      residual: (-2 * Math.PI * residualHz) / this.outRate,
      blockPhase: 0,
      // Each block starts L samples later; a shift of `bin` bins advances
      // 2π·bin·L/N per block, which overlap-save doesn't track on its own.
      blockStep: (-2 * Math.PI * bin * this.l) / this.n,
      ncoPhase: 0,
      re: new Float64Array(this.m), im: new Float64Array(this.m),
      out: new Float32Array(2 * (this.l / this.decim)),
      onOutput: h.onOutput,
    });
    return id;
  }

  removeHead(id: number): void {
    this.heads.delete(id);
  }

  get headCount(): number {
    return this.heads.size;
  }

  /** Feed interleaved float IQ at `fs`. */
  push(iq: Float32Array): void {
    const P1 = this.p - 1;
    let i = 0;
    const total = iq.length >> 1;
    while (i < total) {
      const take = Math.min(this.l - this.fill, total - i);
      const base = P1 + this.fill;
      for (let k = 0; k < take; k++) {
        this.xRe[base + k] = iq[2 * (i + k)];
        this.xIm[base + k] = iq[2 * (i + k) + 1];
      }
      this.fill += take;
      i += take;
      if (this.fill === this.l) this.runBlock();
    }
  }

  private runBlock(): void {
    const { n, m, l, p, decim } = this;
    const P1 = p - 1;
    const xRe = this.xRe, xIm = this.xIm;
    // History in front, then save the new tail for the next block before the FFT clobbers it.
    xRe.set(this.histRe, 0); xIm.set(this.histIm, 0);
    this.histRe.set(xRe.subarray(n - P1)); this.histIm.set(xIm.subarray(n - P1));
    fft(xRe, xIm, n, false);
    const half = m / 2;
    const keepFrom = P1 / decim; // discard the circular-wrap outputs
    const nOut = l / decim;
    for (const h of this.heads.values()) {
      const re = h.re, im = h.im;
      // Window M bins around the channel, rotated to DC, times the filter.
      for (let k = 0; k < m; k++) {
        const off = k < half ? k : k - m;
        const src = (h.bin + off + n) % n;
        const a = xRe[src], b = xIm[src], c = this.hRe[k], d = this.hIm[k];
        re[k] = a * c - b * d;
        im[k] = a * d + b * c;
      }
      fft(re, im, m, true);
      // Overlap-save block phase + residual-offset NCO, then emit.
      let ph = h.blockPhase + h.ncoPhase;
      const out = h.out;
      for (let j = 0; j < nOut; j++) {
        const cr = Math.cos(ph), ci = Math.sin(ph);
        const a = re[keepFrom + j], b = im[keepFrom + j];
        out[2 * j] = a * cr - b * ci;
        out[2 * j + 1] = a * ci + b * cr;
        ph += h.residual;
      }
      h.ncoPhase = (h.ncoPhase + nOut * h.residual) % (2 * Math.PI);
      h.blockPhase = (h.blockPhase + h.blockStep) % (2 * Math.PI);
      h.onOutput(out);
    }
    this.fill = 0;
  }
}
