// FIR design and streaming decimators shared by the DSP chains (audio demod,
// channel capture). Moved verbatim out of Demodulator.ts — the behaviour here
// is what the demod's numeric verification ran against, so treat changes as
// changes to verified DSP.

const TWO_PI = 2 * Math.PI;

/** Windowed-sinc low-pass. `fc` is the cutoff as a fraction of the sample rate
 *  (0..0.5). DC gain normalised to 1. */
export function designLowpass(numTaps: number, fc: number): Float32Array {
  const taps = new Float32Array(numTaps);
  const m = numTaps - 1;
  let sum = 0;
  for (let i = 0; i < numTaps; i++) {
    const n = i - m / 2;
    const sinc = n === 0 ? 2 * fc : Math.sin(TWO_PI * fc * n) / (Math.PI * n);
    const hamming = 0.54 - 0.46 * Math.cos((TWO_PI * i) / m);
    taps[i] = sinc * hamming;
    sum += taps[i];
  }
  for (let i = 0; i < numTaps; i++) taps[i] /= sum;
  return taps;
}

export function oddClamp(v: number, lo: number, hi: number): number {
  const c = Math.max(lo, Math.min(hi, Math.round(v)));
  return c % 2 === 0 ? c + 1 : c;
}

/**
 * Stateful biquad (RBJ cookbook), Direct Form I. Filters a mono stream in place,
 * carrying its state across blocks so consecutive calls form one continuous
 * filter. Cheap enough to cascade a few for a steeper skirt.
 */
export class Biquad {
  private b0 = 1;
  private b1 = 0;
  private b2 = 0;
  private a1 = 0;
  private a2 = 0;
  private x1 = 0;
  private x2 = 0;
  private y1 = 0;
  private y2 = 0;

  private set(b0: number, b1: number, b2: number, a0: number, a1: number, a2: number): void {
    this.b0 = b0 / a0;
    this.b1 = b1 / a0;
    this.b2 = b2 / a0;
    this.a1 = a1 / a0;
    this.a2 = a2 / a0;
  }

  static lowpass(fs: number, fc: number, q = Math.SQRT1_2): Biquad {
    const bq = new Biquad();
    const w0 = (TWO_PI * fc) / fs;
    const cw = Math.cos(w0);
    const alpha = Math.sin(w0) / (2 * q);
    bq.set((1 - cw) / 2, 1 - cw, (1 - cw) / 2, 1 + alpha, -2 * cw, 1 - alpha);
    return bq;
  }

  static highpass(fs: number, fc: number, q = Math.SQRT1_2): Biquad {
    const bq = new Biquad();
    const w0 = (TWO_PI * fc) / fs;
    const cw = Math.cos(w0);
    const alpha = Math.sin(w0) / (2 * q);
    bq.set((1 + cw) / 2, -(1 + cw), (1 + cw) / 2, 1 + alpha, -2 * cw, 1 - alpha);
    return bq;
  }

  /** Filter `x` in place, `n` samples. */
  process(x: Float32Array, n = x.length): void {
    let x1 = this.x1;
    let x2 = this.x2;
    let y1 = this.y1;
    let y2 = this.y2;
    const { b0, b1, b2, a1, a2 } = this;
    for (let i = 0; i < n; i++) {
      const xi = x[i];
      const yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
      x2 = x1;
      x1 = xi;
      y2 = y1;
      y1 = yi;
      x[i] = yi;
    }
    this.x1 = x1;
    this.x2 = x2;
    this.y1 = y1;
    this.y2 = y2;
  }
}

/**
 * Decimating FIR over a complex stream. Keeps its own tail so consecutive blocks
 * form one continuous filter, and tracks the decimation phase across blocks so
 * output samples land on the same grid regardless of where a block boundary fell.
 */
export class ComplexFirDecimator {
  private readonly taps: Float32Array;
  private readonly m: number; // taps.length
  private readonly decim: number;
  private tailI: Float32Array;
  private tailQ: Float32Array;
  /** Input samples to skip from the start of the next block before an output. */
  private nextOut = 0;

  constructor(taps: Float32Array, decim: number) {
    this.taps = taps;
    this.m = taps.length;
    this.decim = decim;
    this.tailI = new Float32Array(this.m - 1);
    this.tailQ = new Float32Array(this.m - 1);
  }

  /** Consume `nIn` interleaved-into-two-arrays samples, return decimated I/Q. */
  process(inI: Float32Array, inQ: Float32Array, nIn: number): { i: Float32Array; q: Float32Array } {
    const M = this.m;
    const wI = new Float32Array(M - 1 + nIn);
    const wQ = new Float32Array(M - 1 + nIn);
    wI.set(this.tailI);
    wQ.set(this.tailQ);
    for (let j = 0; j < nIn; j++) {
      wI[M - 1 + j] = inI[j];
      wQ[M - 1 + j] = inQ[j];
    }

    const numOut = this.nextOut >= nIn ? 0 : Math.floor((nIn - 1 - this.nextOut) / this.decim) + 1;
    const outI = new Float32Array(numOut);
    const outQ = new Float32Array(numOut);
    const taps = this.taps;
    let o = 0;
    for (let j = this.nextOut; j < nIn; j += this.decim) {
      let accI = 0;
      let accQ = 0;
      for (let k = 0; k < M; k++) {
        const t = taps[k];
        accI += t * wI[j + k];
        accQ += t * wQ[j + k];
      }
      outI[o] = accI;
      outQ[o] = accQ;
      o++;
    }

    // Carry the decimation phase and the filter tail into the next block.
    this.nextOut = numOut === 0 ? this.nextOut - nIn : this.nextOut + numOut * this.decim - nIn;
    this.tailI.set(wI.subarray(nIn));
    this.tailQ.set(wQ.subarray(nIn));
    return { i: outI, q: outQ };
  }
}

/** Real-valued twin of ComplexFirDecimator, for post-detector audio stages. */
export class RealFirDecimator {
  private readonly taps: Float32Array;
  private readonly m: number;
  private readonly decim: number;
  private tail: Float32Array;
  private nextOut = 0;

  constructor(taps: Float32Array, decim: number) {
    this.taps = taps;
    this.m = taps.length;
    this.decim = decim;
    this.tail = new Float32Array(this.m - 1);
  }

  process(input: Float32Array, nIn: number): Float32Array {
    const M = this.m;
    const w = new Float32Array(M - 1 + nIn);
    w.set(this.tail);
    w.set(input.subarray(0, nIn), M - 1);

    const numOut = this.nextOut >= nIn ? 0 : Math.floor((nIn - 1 - this.nextOut) / this.decim) + 1;
    const out = new Float32Array(numOut);
    const taps = this.taps;
    let o = 0;
    for (let j = this.nextOut; j < nIn; j += this.decim) {
      let acc = 0;
      for (let k = 0; k < M; k++) acc += taps[k] * w[j + k];
      out[o++] = acc;
    }

    this.nextOut = numOut === 0 ? this.nextOut - nIn : this.nextOut + numOut * this.decim - nIn;
    this.tail.set(w.subarray(nIn));
    return out;
  }
}
