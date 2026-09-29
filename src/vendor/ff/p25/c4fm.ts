// P25 Phase 1 C4FM physical layer: IQ → dibit symbols.
//
// C4FM is 4-level FSK at 4800 symbols/s (9600 bit/s), deviations ±1800/±600 Hz.
// The dibit ↔ deviation mapping is the P25 standard:
//     dibit 00 → +1 (+600 Hz)     dibit 01 → +3 (+1800 Hz)
//     dibit 10 → -1 (-600 Hz)     dibit 11 → -3 (-1800 Hz)
// i.e. the high bit is the sign (0 = above centre) and the low bit selects the
// outer rail. A dibit's two bits go into the frame bit-vector MSB first.
//
// The front end is the same shape as `demodFsk4` (FLEX 3200/4): FM-discriminate,
// remove the tuning-offset centre, then point-sample at the symbol centres — a
// matched-filter boxcar compresses the four rails, so point-sampling recovers the
// clean ±1800/±600 levels. Timing is a fixed best-phase sampler (a captured burst
// is short enough that the crystal offset barely drifts the clock across it), the
// same decision made for the other demods in this project.
//
// Verified indirectly by the full BER=0 loopback in `web/test/p25.test.ts`
// (encode → C4FM IQ → this demod → frame/NID/TSBK → exact fields).

const TWO_PI = Math.PI * 2;
export const P25_SYMBOL_RATE = 4800;

export interface C4fmSymbols {
  /** Dibit per symbol (0..3), best-guess polarity resolved by the caller. */
  dibits: Uint8Array;
  /** Centre-removed instantaneous frequency at each symbol centre (Hz). */
  soft: Float32Array;
  centerHz: number;
}

/** FM discriminator: interleaved IQ → instantaneous frequency (Hz) per sample. */
function discriminate(iq: Float32Array, sampleRateHz: number): Float32Array {
  const n = iq.length / 2;
  const freq = new Float32Array(n);
  const k = sampleRateHz / TWO_PI;
  let pi = iq[0];
  let pq = iq[1];
  for (let i = 0; i < n; i++) {
    const ci = iq[2 * i];
    const cq = iq[2 * i + 1];
    freq[i] = i === 0 ? 0 : Math.atan2(cq * pi - ci * pq, ci * pi + cq * pq) * k;
    pi = ci;
    pq = cq;
  }
  return freq;
}

/** Boxcar moving average of length `w` (matched filter for the rectangular
 *  symbol pulses). */
function boxcar(x: Float32Array, w: number): Float32Array {
  if (w <= 1) return x;
  const n = x.length;
  const out = new Float32Array(n);
  let acc = 0;
  for (let i = 0; i < n; i++) {
    acc += x[i];
    if (i >= w) acc -= x[i - w];
    out[i] = acc / Math.min(i + 1, w);
  }
  return out;
}

/** Robust centre (tuning offset) estimate: median of the discriminator. */
function median(x: Float32Array): number {
  const s = Float32Array.from(x).sort();
  return s[s.length >> 1];
}

/**
 * Demodulate C4FM IQ to dibits. `sampleRateHz` may be any rate above ~2× the
 * symbol rate; symbol instants are found by linear interpolation, so a
 * non-integer samples-per-symbol is fine.
 */
export function demodC4fm(iq: Float32Array, sampleRateHz: number): C4fmSymbols {
  const raw = discriminate(iq, sampleRateHz);
  const center = median(raw);
  const sps = sampleRateHz / P25_SYMBOL_RATE;
  const filtered = boxcar(raw, Math.max(1, Math.round(sps * 0.9)));

  const nSyms = Math.max(0, Math.floor((filtered.length - 1) / sps) - 1);
  if (nSyms <= 0) return { dibits: new Uint8Array(0), soft: new Float32Array(0), centerHz: center };

  // Choose the sampling phase (0..sps) that maximises the summed |deviation| —
  // symbol centres carry the full rail, transitions are near the centre.
  const phaseSteps = Math.max(4, Math.round(sps));
  let bestPhase = 0;
  let bestEnergy = -1;
  for (let p = 0; p < phaseSteps; p++) {
    const phase = (p / phaseSteps) * sps;
    let energy = 0;
    for (let s = 0; s < nSyms; s++) {
      const t = phase + s * sps;
      const i0 = Math.floor(t);
      const frac = t - i0;
      const v = filtered[i0] * (1 - frac) + filtered[i0 + 1] * frac - center;
      energy += Math.abs(v);
    }
    if (energy > bestEnergy) { bestEnergy = energy; bestPhase = phase; }
  }

  const soft = new Float32Array(nSyms);
  const dibits = new Uint8Array(nSyms);
  for (let s = 0; s < nSyms; s++) {
    const t = bestPhase + s * sps;
    const i0 = Math.floor(t);
    const frac = t - i0;
    const v = filtered[i0] * (1 - frac) + filtered[i0 + 1] * frac - center;
    soft[s] = v;
    dibits[s] = softToDibit(v);
  }
  return { dibits, soft, centerHz: center };
}

/**
 * {@link demodC4fm} with timing that FOLLOWS the transmitter's clock: the best
 * sampling phase is found per block of `blockSyms` symbols, unwrapped across
 * blocks (so a phase walking past a whole symbol moves the symbol grid instead
 * of dropping or repeating a symbol) and interpolated between block centres.
 * For voice — seconds-long transmissions, where a 50–100 ppm crystal mismatch
 * walks the fixed-phase sampler off the eye. Returns each symbol's sample
 * instant too, so a caller can time frames.
 */
export function demodC4fmTracked(
  iq: Float32Array,
  sampleRateHz: number,
  opts: { blockSyms?: number; smooth?: number; adaptive?: boolean } = {},
): C4fmSymbols & { instants: Float32Array } {
  const blockSyms = opts.blockSyms ?? 240;
  const raw = discriminate(iq, sampleRateHz);
  const center = median(raw);
  const sps = sampleRateHz / P25_SYMBOL_RATE;
  const filtered = boxcar(raw, Math.max(1, Math.round(sps * (opts.smooth ?? 0.9))));
  const nApprox = Math.max(0, Math.floor((filtered.length - 1) / sps) - 1);
  const empty = { dibits: new Uint8Array(0), soft: new Float32Array(0), centerHz: center, instants: new Float32Array(0) };
  if (nApprox <= 0) return empty;
  const at = (t: number) => {
    const i0 = Math.floor(t);
    const frac = t - i0;
    return filtered[i0] * (1 - frac) + filtered[i0 + 1] * frac - center;
  };
  const steps = Math.max(8, Math.round(sps * 2));
  // Per-block best phase (samples, in [0, sps)).
  const nBlocks = Math.max(1, Math.ceil(nApprox / blockSyms));
  const phase = new Float64Array(nBlocks);
  for (let b = 0; b < nBlocks; b++) {
    const s0 = b * blockSyms;
    const s1 = Math.min(nApprox, s0 + blockSyms);
    let best = -1;
    for (let p = 0; p < steps; p++) {
      const ph = (p / steps) * sps;
      let e = 0;
      for (let s = s0; s < s1; s++) {
        const t = ph + s * sps;
        if (t + 1 >= filtered.length) break;
        e += Math.abs(at(t));
      }
      if (e > best) {
        best = e;
        phase[b] = ph;
      }
    }
    // Unwrap: stay within half a symbol of the previous block's phase.
    if (b > 0) {
      while (phase[b] - phase[b - 1] > sps / 2) phase[b] -= sps;
      while (phase[b] - phase[b - 1] < -sps / 2) phase[b] += sps;
    }
  }
  const phaseAt = (s: number) => {
    const x = s / blockSyms - 0.5; // block centres at (b + 0.5)·blockSyms
    if (x <= 0 || nBlocks === 1) return phase[0];
    if (x >= nBlocks - 1) return phase[nBlocks - 1];
    const b = Math.floor(x);
    return phase[b] + (phase[b + 1] - phase[b]) * (x - b);
  };
  const soft: number[] = [];
  const instants: number[] = [];
  for (let s = 0; ; s++) {
    const t = phaseAt(s) + s * sps;
    if (t < 0) continue;
    if (t + 1 >= filtered.length) break;
    soft.push(at(t));
    instants.push(t);
  }
  // Re-centre and slice on the RAILS, not the median. The median of the
  // symbols is the carrier only when the four levels are equiprobable; voice
  // frames are not (a run of identical vocoder frames, or silence, skews it by
  // hundreds of Hz). The ±3 rails are always present — every frame sync is made
  // of them — so their midpoint is the carrier and their spread the deviation.
  // The inner/outer threshold is then 2/3 of the outer rail (1200 Hz nominal),
  // which follows a channel filter or an off-nominal transmitter deviation.
  const sorted = Float32Array.from(soft).sort();
  const qLo = sorted[Math.floor(sorted.length * 0.02)] ?? 0;
  const qHi = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.98))] ?? 0;
  const recentre = (qHi + qLo) / 2;
  const outer = (qHi - qLo) / 2;
  const thr = opts.adaptive === false || !(outer > 300) ? 1200 : (2 / 3) * outer;
  const dibits = new Uint8Array(soft.length);
  for (let k = 0; k < soft.length; k++) {
    const v = (soft[k] -= recentre);
    dibits[k] = v >= thr ? 0b01 : v >= 0 ? 0b00 : v >= -thr ? 0b10 : 0b11;
  }
  return { dibits, soft: Float32Array.from(soft), centerHz: center + recentre, instants: Float32Array.from(instants) };
}

/** Map a centre-removed deviation to a dibit (P25 mapping above). Threshold at
 *  0 and ±1200 Hz (the midpoints between the ±600/±1800 rails). */
export function softToDibit(v: number): number {
  if (v >= 1200) return 0b01; // +3
  if (v >= 0) return 0b00; // +1
  if (v >= -1200) return 0b10; // -1
  return 0b11; // -3
}

/** Invert C4FM polarity (spectrum flip): +f ↔ -f flips the sign bit of a dibit. */
export function invertDibit(d: number): number {
  return d ^ 0b10;
}

/** The 48-bit P25 frame sync (op25 P25_FRAME_SYNC_MAGIC), as 24 dibits. */
export const FRAME_SYNC_MAGIC = 0x5575f5ff77ffn;
export const SYNC_DIBITS: Uint8Array = (() => {
  const d = new Uint8Array(24);
  let acc = FRAME_SYNC_MAGIC;
  for (let i = 23; i >= 0; i--) { d[i] = Number(acc & 0b11n); acc >>= 2n; }
  return d;
})();

/** Pack a dibit stream into a bit array, 2 bits per dibit, MSB first. */
export function dibitsToBits(dibits: ArrayLike<number>, start = 0, count = dibits.length - start): Uint8Array {
  const bits = new Uint8Array(count * 2);
  for (let i = 0; i < count; i++) {
    const d = dibits[start + i];
    bits[i * 2] = (d >> 1) & 1;
    bits[i * 2 + 1] = d & 1;
  }
  return bits;
}
