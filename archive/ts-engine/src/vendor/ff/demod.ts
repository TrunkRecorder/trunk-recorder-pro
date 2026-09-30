// Demodulation to symbols — docs/WORKBENCH.md §6. Turns a captured channel and
// a chosen modulation + baud into a stream of symbol indices (and a soft metric
// per symbol, for the bit stage). Pure functions over interleaved complex I/Q;
// the analysis worker calls them, and the node test suite drives them with
// synthetic signals to a BER=0 bar (see test/demod.test.ts).
//
// Two tiers, deliberately different in difficulty:
//   • loop-free — OOK/ASK and 2FSK need no recovery loops: detect (envelope or
//     discriminator), matched-filter, recover symbol timing (Gardner), slice.
//   • loops — BPSK/QPSK need carrier recovery (Costas) before the same timing +
//     slicing. Costas locks with a phase ambiguity (180°/90°), so the symbol
//     stage offers differential decoding and the bit stage can resolve absolute
//     phase against a known sequence (phase 4).
//   • π/4-DQPSK (P25 CQPSK/LSM, TETRA, IS-54, NXDN's linear mode) needs no
//     carrier lock at all — the data is in the phase CHANGE — but it does need
//     real timing tracking: it is always RRC-shaped, so a symbol instant off by
//     a fraction of a symbol is a smeared decision. It gets a genuine Gardner
//     loop rather than the one-shot phase pick the other complex path uses.
//
// The timing recovery is Gardner: a decision-free TED that needs no carrier and
// tolerates a fractional samples-per-symbol, so one implementation serves every
// mode. It tracks slow timing drift, which a fixed best-phase decimation can't.

export type DemodMod = "ook" | "ask" | "fsk2" | "gfsk" | "bpsk" | "qpsk" | "pi4dqpsk" | "fsk4";

export interface DemodResult {
  /** Symbol indices, 0..2^bitsPerSymbol−1. */
  symbols: Uint8Array;
  /** One soft value per symbol (real decision statistic) — for the bit stage
   *  and for a soft eye. OOK/ASK: envelope; FSK: discriminator; PSK: the arm. */
  soft: Float32Array;
  /** Absolute sample index each symbol was sampled at — for overlaying the
   *  recovered symbol instants on the time views. */
  sampleInstants: Float32Array;
  bitsPerSymbol: number;
}

const TWO_PI = 2 * Math.PI;

// ---------------------------------------------------------------------------
// Public entry point.
// ---------------------------------------------------------------------------

export interface DemodOptions {
  /** Costas/timing loop bandwidth scale (0..1 of nominal); default is sane. */
  loopBw?: number;
  /** PSK differential decoding, to sidestep the constellation phase ambiguity. */
  differential?: boolean;
  /** Seed the Costas NCO with this carrier offset (Hz) — the analysis stage
   *  measures it, so lock is near-instant instead of a symbol-eating transient. */
  carrierOffsetHz?: number;
  /** Amplitude slicing threshold in |x| units, overriding the midpoint of the
   *  sample range. URH's `detect_center` (dsp/autoInterpret.ts) measures this as
   *  a histogram, which survives the overshoot that drags a midpoint. FSK's
   *  equivalent is `carrierOffsetHz` — the discriminator already slices at 0. */
  sliceCenter?: number;
  /** Envelope floor in |x| units: samples at or below it are silence, not a low
   *  symbol. URH's `afp_demod` noise gate; the autodetect measures it. */
  noiseFloor?: number;
  /**
   * The pulse shaping the transmitter used, so the PSK receiver can match it.
   *
   * `"rect"` (the default) keeps the one-symbol boxcar applied after the Costas
   * loop. It IS the matched filter for the hard-phase-step PSK the sim and the
   * unit tests generate, and it stays the default for that reason.
   *
   * `"rrc"` applies a root-raised-cosine matched filter and coarse-derotates by
   * the measured carrier offset first, so the signal sits inside the filter's
   * passband and the Costas loop is left only the residual. Use it for a real
   * link, which is almost always RRC-shaped: cascaded with the transmitter's
   * RRC it is a raised cosine, so the ISI at the symbol instants is zero, and
   * the boxcar's one-symbol average cannot undo a pulse that spans eleven.
   *
   * Which one wins depends on what dominates. Measured on real captures with
   * calibrated noise added, CRC-valid frames out of 60 (docs/OTA-TESTING.md):
   *
   *     shaped BPSK    noise x2:  boxcar 64  rrc 93     x3:  boxcar 0  rrc 48
   *     shaped QPSK    noise x1:  boxcar  5  rrc 18     x2:  boxcar 0  rrc  7
   *     unshaped BPSK  noise x2:  boxcar 46  rrc 91     x3:  boxcar 0  rrc 61
   *
   * On UNSHAPED pulses the picture reverses when noise is not the limit: an RRC
   * over a rectangular pulse is not Nyquist, and the ISI it adds costs QPSK its
   * BER-0 at 20 dB SNR (web/test/demod.test.ts). So this is a real choice, not
   * a strict upgrade — hence an option with a conservative default rather than
   * a silent change of behaviour.
   *
   * The exact roll-off barely matters (0.2/0.35/0.5 score within noise of each
   * other), so a receiver does not need to know the transmitter's alpha.
   */
  pulseShape?: "rect" | "rrc";
  /** RRC roll-off (excess bandwidth), 0..1. Ignored unless `pulseShape` is
   *  `"rrc"`. 0.35 is the most common choice and the default. */
  excessBw?: number;
}

export function demodulate(
  iq: Float32Array,
  sampleRateHz: number,
  mod: DemodMod,
  baudHz: number,
  opts: DemodOptions = {}
): DemodResult {
  const sps = sampleRateHz / baudHz;
  if (!(sps >= 2) || baudHz <= 0 || iq.length < 4) {
    return { symbols: new Uint8Array(0), soft: new Float32Array(0), sampleInstants: new Float32Array(0), bitsPerSymbol: 1 };
  }
  switch (mod) {
    case "ook":
    case "ask":
      return demodAmplitude(iq, sps, opts);
    case "fsk2":
    case "gfsk":
      return demodFsk2(iq, sampleRateHz, sps, opts.carrierOffsetHz);
    case "fsk4":
      return demodFsk4(iq, sampleRateHz, sps, opts.carrierOffsetHz);
    case "bpsk":
      return demodPsk(iq, sampleRateHz, sps, 2, opts);
    case "qpsk":
      return demodPsk(iq, sampleRateHz, sps, 4, opts);
    case "pi4dqpsk":
      return demodPi4Dqpsk(iq, sampleRateHz, sps, opts);
  }
}

/** Default symbol→bit mapping. Gray/differential/inversion are pipeline decode
 *  stages (phase 4); this is the raw binary unpacking, MSB first. */
export function symbolsToBits(symbols: Uint8Array, bitsPerSymbol: number, gray = false): Uint8Array {
  const bits = new Uint8Array(symbols.length * bitsPerSymbol);
  let o = 0;
  for (let i = 0; i < symbols.length; i++) {
    let s = symbols[i];
    if (gray) s = grayDecode(s);
    for (let b = bitsPerSymbol - 1; b >= 0; b--) bits[o++] = (s >> b) & 1;
  }
  return bits;
}

// ---------------------------------------------------------------------------
// OOK / ASK.
// ---------------------------------------------------------------------------

function demodAmplitude(iq: Float32Array, sps: number, opts: DemodOptions = {}): DemodResult {
  const n = iq.length / 2;
  const env = new Float32Array(n);
  // URH's afp_demod noise gate, when the Identify stage measured a floor: a
  // sample below it is silence, so it must read as a hard zero rather than
  // dragging the level statistics of a bursty capture.
  const floor = opts.noiseFloor ?? 0;
  for (let i = 0; i < n; i++) {
    const m = Math.hypot(iq[2 * i], iq[2 * i + 1]);
    env[i] = m <= floor ? 0 : m;
  }
  const filtered = boxcar(env, Math.round(sps));
  const { samples, instants } = gardnerReal(filtered, sps);

  // Threshold between the two amplitude levels. The midpoint of the sample
  // range is robust for clean OOK; ASK with >2 levels would need more, but v1
  // slices two. A measured centre (URH's histogram) overrides it.
  let mn = Infinity;
  let mx = -Infinity;
  for (const v of samples) {
    if (v < mn) mn = v;
    if (v > mx) mx = v;
  }
  const thresh = opts.sliceCenter ?? (mn + mx) / 2;
  const symbols = new Uint8Array(samples.length);
  for (let i = 0; i < samples.length; i++) symbols[i] = samples[i] > thresh ? 1 : 0;
  return { symbols, soft: samples, sampleInstants: instants, bitsPerSymbol: 1 };
}

// ---------------------------------------------------------------------------
// 2FSK / GFSK.
// ---------------------------------------------------------------------------

function demodFsk2(iq: Float32Array, sampleRateHz: number, sps: number, centerHz?: number): DemodResult {
  const n = iq.length / 2;
  // Polar discriminator (exact atan2 form). The decision threshold is 0, so the
  // FSK centre has to be removed first. Prefer the MEASURED centre (the midpoint
  // of the two rails) when given: a bursty capture's sample mean is polluted by
  // idle-noise, and unbalanced data biases it too. Fall back to the mean.
  const freq = new Float32Array(n);
  let pi = iq[0];
  let pq = iq[1];
  let mean = 0;
  const k = sampleRateHz / TWO_PI;
  for (let i = 0; i < n; i++) {
    const ci = iq[2 * i];
    const cq = iq[2 * i + 1];
    freq[i] = i === 0 ? 0 : Math.atan2(cq * pi - ci * pq, ci * pi + cq * pq) * k;
    mean += freq[i];
    pi = ci;
    pq = cq;
  }
  mean /= n;
  const center = centerHz ?? mean;
  for (let i = 0; i < n; i++) freq[i] -= center;

  const filtered = boxcar(freq, Math.round(sps));
  const { samples, instants } = gardnerReal(filtered, sps);
  const symbols = new Uint8Array(samples.length);
  for (let i = 0; i < samples.length; i++) symbols[i] = samples[i] > 0 ? 1 : 0;
  return { symbols, soft: samples, sampleInstants: instants, bitsPerSymbol: 1 };
}

// ---------------------------------------------------------------------------
// 4-FSK (FLEX 3200/4 & 6400/4 data). Four frequency levels carry 2 bits each,
// Gray-coded so a single-level slip flips only one bit.
// ---------------------------------------------------------------------------

// Gray map: level (lowest→highest freq) → 2-bit symbol, per the FLEX spec
// (0,0)=lowest, (0,1), (1,1), (1,0)=highest. Index by level 0..3 → symbol.
const FSK4_GRAY = [0b00, 0b01, 0b11, 0b10];

function demodFsk4(iq: Float32Array, sampleRateHz: number, sps: number, centerHz?: number): DemodResult {
  const n = iq.length / 2;
  const freq = new Float32Array(n);
  let pi = iq[0];
  let pq = iq[1];
  let mean = 0;
  const k = sampleRateHz / TWO_PI;
  for (let i = 0; i < n; i++) {
    const ci = iq[2 * i];
    const cq = iq[2 * i + 1];
    freq[i] = i === 0 ? 0 : Math.atan2(cq * pi - ci * pq, ci * pi + cq * pq) * k;
    mean += freq[i];
    pi = ci;
    pq = cq;
  }
  mean /= n;
  const center = centerHz ?? mean;
  for (let i = 0; i < n; i++) freq[i] -= center;

  const filtered = boxcar(freq, Math.round(sps));
  const { samples, instants } = gardnerRealOneShot(filtered, sps);
  if (samples.length === 0) {
    return { symbols: new Uint8Array(0), soft: samples, sampleInstants: instants, bitsPerSymbol: 2 };
  }

  // Establish the four decision levels from the robust range of the sampled
  // frequency (the outer rails are populated by random data), then place the
  // three slicing thresholds at the quarter points.
  const sorted = Float32Array.from(samples).sort();
  const lo = sorted[Math.floor(sorted.length * 0.02)];
  const hi = sorted[Math.floor(sorted.length * 0.98)];
  const span = hi - lo || 1;
  const symbols = new Uint8Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    let level = Math.round(((samples[i] - lo) / span) * 3);
    level = level < 0 ? 0 : level > 3 ? 3 : level;
    symbols[i] = FSK4_GRAY[level];
  }
  return { symbols, soft: samples, sampleInstants: instants, bitsPerSymbol: 2 };
}

// ---------------------------------------------------------------------------
// BPSK / QPSK.
// ---------------------------------------------------------------------------

function demodPsk(iq: Float32Array, sampleRateHz: number, sps: number, order: number, opts: DemodOptions): DemodResult {
  const bitsPerSymbol = order === 2 ? 1 : 2;
  const seedFreq = (TWO_PI * (opts.carrierOffsetHz ?? 0)) / sampleRateHz;
  const shaped = opts.pulseShape === "rrc";

  let bi: Float32Array;
  let bq: Float32Array;
  if (shaped) {
    // RRC-shaped link. The filter has to come BEFORE carrier recovery: Costas
    // is decision-directed at full rate, and between symbols a shaped waveform
    // sits at arbitrary amplitude and phase. For QPSK the axis choice
    // (|I| vs |Q|) then flips on the transitions and injects error the loop
    // chases, so it never settles — measured: QPSK stayed at zero frames with
    // the filter after the loop and decodes with it before.
    //
    // Coarse-derotate by the MEASURED offset first, so the signal is centred
    // inside the matched filter's passband (an offset near the RRC band edge
    // would otherwise be attenuated by the very filter meant to pass it), and
    // leave only the residual for the loop.
    const centred = derotate(iq, seedFreq);
    const mf = rrcTaps(sps, opts.excessBw ?? 0.35, 11);
    const mi = fir(evens(centred), mf);
    const mq = fir(odds(centred), mf);
    const filtered = new Float32Array(mi.length * 2);
    for (let i = 0; i < mi.length; i++) {
      filtered[2 * i] = mi[i];
      filtered[2 * i + 1] = mq[i];
    }
    // The matched filter is the receive filter, so the arms are already the
    // symbol waveform — no boxcar on top of it.
    const base = costas(filtered, order, opts.loopBw ?? 1, 0);
    bi = evens(base);
    bq = odds(base);
  } else {
    // 1. carrier recovery — Costas removes the residual carrier and locks phase,
    //    seeded with the measured offset so acquisition doesn't eat symbols.
    const base = costas(iq, order, opts.loopBw ?? 1, seedFreq);
    // 2. matched filter (boxcar for the rectangular sim pulses) on both arms.
    bi = boxcar(evens(base), Math.round(sps));
    bq = boxcar(odds(base), Math.round(sps));
  }
  // 3. timing recovery on the complex baseband.
  const { i: si, q: sq, instants } = gardnerComplex(bi, bq, sps);

  // Label symbols by PHASE INDEX (nearest of the M constellation angles). This
  // makes the Costas ambiguity a constant ADDITION mod M — so differential
  // decoding removes it, and a known-sequence resolves the residual constant.
  const symbols = new Uint8Array(si.length);
  const soft = new Float32Array(si.length);
  const step = TWO_PI / order;
  for (let k = 0; k < si.length; k++) {
    const ang = Math.atan2(sq[k], si[k]);
    symbols[k] = (((Math.round(ang / step) % order) + order) % order) as number;
    soft[k] = order === 2 ? si[k] : Math.hypot(si[k], sq[k]);
  }
  const out = opts.differential ? differentialDecode(symbols, order) : symbols;
  return { symbols: out, soft, sampleInstants: instants, bitsPerSymbol };
}

// ---------------------------------------------------------------------------
// π/4-DQPSK.
// ---------------------------------------------------------------------------
//
// Each symbol is a phase STEP of ±45° or ±135° from the one before, so the
// constellation alternates between two QPSK sets rotated 45° apart and never
// sits still. That is exactly what defeats `demodPsk`'s Costas loop (it wants a
// fixed four points) and what fooled the classifier into "ASK": the steps pass
// near the origin, so the envelope of this constant-amplitude-per-symbol
// modulation swings by tens of dB. Measured on a real P25 simulcast control
// channel (docs/WORKBENCH.md §13): envelope CV 0.54, and 83% of symbol-spaced
// phase steps within 15° of ±45°/±135°.
//
// The receiver is the textbook one: derotate by the measured offset, RRC
// matched filter, Gardner timing loop, then differential detection
// s[k]·conj(s[k−1]) with a slow decision-directed loop removing what is left
// of the carrier offset (a residual Δf turns every step by 2π·Δf/baud).

/** Phase step → symbol, TIA-102 CQPSK / IS-54 Gray order: +45° = 00,
 *  +135° = 01, −45° = 10, −135° = 11. P25's dibit mapping is the same table
 *  (+1, +3, −1, −3), so these symbols ARE P25 dibits. */
function pi4Symbol(step: number): number {
  if (step >= 0) return step < Math.PI / 2 ? 0b00 : 0b01;
  return step > -Math.PI / 2 ? 0b10 : 0b11;
}

function demodPi4Dqpsk(iq: Float32Array, sampleRateHz: number, sps: number, opts: DemodOptions): DemodResult {
  // The offset has to be right to within baud/8 before differential detection:
  // beyond that a turned +45° step reads as +135° on every symbol, and the 4th-
  // power residual estimate below is ambiguous by exactly that 90°. Measured on
  // the real capture: 1 kHz off at 4800 Bd (77° per symbol) decoded no frames
  // at all. So the caller's offset is only a seed — the spectral centroid of
  // what is left after it is measured here and removed too. For a symmetric
  // spectrum (every linear modulation with equiprobable data) the mean phase
  // increment per sample IS the carrier offset.
  const seeded = derotate(iq, (TWO_PI * (opts.carrierOffsetHz ?? 0)) / sampleRateHz);
  const centred = derotate(seeded, meanPhaseIncrement(seeded));
  // RRC unless the caller asked for the boxcar. Unlike BPSK/QPSK the default is
  // the shaped filter: a π/4-DQPSK transmitter is pulse-shaped by definition
  // (the modulation exists to be linear and band-limited). The roll-off barely
  // matters — measured on the real P25 LSM capture, 0.20/0.35/0.50 gave 190/
  // 192/189 CRC-valid TSBKs — so it takes the app-wide 0.35 default.
  let bi: Float32Array;
  let bq: Float32Array;
  if (opts.pulseShape === "rect") {
    bi = boxcar(evens(centred), Math.round(sps));
    bq = boxcar(odds(centred), Math.round(sps));
  } else {
    const mf = rrcTaps(sps, opts.excessBw ?? 0.35, 11);
    bi = fir(evens(centred), mf);
    bq = fir(odds(centred), mf);
  }
  const { i: si, q: sq, instants } = gardnerLoopComplex(bi, bq, sps, opts.loopBw ?? 1);
  const n = si.length;
  if (n < 2) return { symbols: new Uint8Array(0), soft: new Float32Array(0), sampleInstants: new Float32Array(0), bitsPerSymbol: 2 };

  // Differential products. Their phase is the data step plus a constant turn
  // from any residual offset.
  const dr = new Float32Array(n - 1);
  const di = new Float32Array(n - 1);
  for (let k = 1; k < n; k++) {
    dr[k - 1] = si[k] * si[k - 1] + sq[k] * sq[k - 1];
    di[k - 1] = sq[k] * si[k - 1] - si[k] * sq[k - 1];
  }
  // Seed the residual turn from the 4th power: every ideal step raised to the
  // 4th is e^{jπ}, so arg(Σd⁴) = π + 4θ. Unambiguous for |θ| < 45°, i.e. a
  // residual below baud/8 — the measured offset is far better than that.
  let theta = pi4ResidualTurn(dr, di, 0, Math.min(dr.length, 2000));

  const symbols = new Uint8Array(n - 1);
  const soft = new Float32Array(n - 1);
  const gain = 0.005 * (opts.loopBw ?? 1);
  for (let k = 0; k < dr.length; k++) {
    let step = Math.atan2(di[k], dr[k]) - theta;
    if (step > Math.PI) step -= TWO_PI;
    else if (step < -Math.PI) step += TWO_PI;
    const sym = pi4Symbol(step);
    const ideal = sym === 0 ? Math.PI / 4 : sym === 1 ? (3 * Math.PI) / 4 : sym === 2 ? -Math.PI / 4 : (-3 * Math.PI) / 4;
    theta += gain * (step - ideal);
    symbols[k] = sym;
    soft[k] = step;
  }
  return { symbols, soft, sampleInstants: instants.subarray(1), bitsPerSymbol: 2 };
}

/** arg Σ x[n]·conj(x[n−1]) — the power-weighted mean frequency, rad/sample. */
function meanPhaseIncrement(iq: Float32Array): number {
  let ar = 0;
  let ai = 0;
  for (let i = 1; i < iq.length / 2; i++) {
    const a = iq[2 * i];
    const b = iq[2 * i + 1];
    const c = iq[2 * i - 2];
    const d = iq[2 * i - 1];
    ar += a * c + b * d;
    ai += b * c - a * d;
  }
  return ar === 0 && ai === 0 ? 0 : Math.atan2(ai, ar);
}

/** θ such that the steps in [from, to) sit at ±45°/±135° once turned by −θ. */
function pi4ResidualTurn(dr: Float32Array, di: Float32Array, from: number, to: number): number {
  let ar = 0;
  let ai = 0;
  for (let k = from; k < to; k++) {
    // (d/|d|)^4 weighted by |d| — unit-circle power, so a fade cannot dominate.
    const m = Math.hypot(dr[k], di[k]);
    if (!(m > 0)) continue;
    const a = dr[k] / m;
    const b = di[k] / m;
    const a2 = a * a - b * b;
    const b2 = 2 * a * b;
    ar += (a2 * a2 - b2 * b2) * m;
    ai += 2 * a2 * b2 * m;
  }
  if (ar === 0 && ai === 0) return 0;
  // arg(Σ) = π + 4θ  ⇒  θ = (arg(−Σ)) / 4.
  return Math.atan2(-ai, -ar) / 4;
}

/**
 * Gardner timing recovery on a complex, pulse-shaped baseband — a real
 * tracking loop, unlike {@link gardnerComplex}'s one-shot phase pick.
 *
 * Error e = Re{ (y[k] − y[k−1]) · conj(y[k−½]) }, normalised by the running
 * symbol power so the loop gain does not depend on the signal level. It is
 * decision-free and carrier-independent, which is why it can sit before the
 * differential detector with a residual offset still on the signal.
 */
function gardnerLoopComplex(
  xi: Float32Array,
  xq: Float32Array,
  sps: number,
  bwScale: number
): { i: Float32Array; q: Float32Array; instants: Float32Array } {
  const N = Math.min(xi.length, xq.length);
  // Acquire with the energy search — it is right on average, and the loop then
  // only has to follow drift, not pull in from half a symbol away.
  const start = bestPhase(sps, N, (p) => {
    let energy = 0;
    let n = 0;
    for (let pos = p; pos + 1 < N && n < 4000; pos += sps) {
      const i = lerp(xi, pos);
      const q = lerp(xq, pos);
      energy += i * i + q * q;
      n++;
    }
    return n > 0 ? energy / n : 0;
  });
  const oi: number[] = [];
  const oq: number[] = [];
  const instants: number[] = [];
  // Proportional + integral, in samples. The integral term absorbs a constant
  // clock ratio (the transmitter's and the dongle's crystals disagree by ppm).
  const kp = 0.02 * bwScale;
  const ki = kp * kp * 0.25;
  let pos = start;
  let rate = 0;
  let power = 0;
  let pi = lerp(xi, pos);
  let pq = lerp(xq, pos);
  oi.push(pi);
  oq.push(pq);
  instants.push(pos);
  while (pos + sps + 1 < N) {
    const next = pos + sps + rate;
    const mid = pos + (next - pos) / 2;
    const ci = lerp(xi, next);
    const cq = lerp(xq, next);
    const mi = lerp(xi, mid);
    const mq = lerp(xq, mid);
    const e = (ci - pi) * mi + (cq - pq) * mq;
    const p = ci * ci + cq * cq;
    power = power === 0 ? p : power + 0.01 * (p - power);
    const en = power > 0 ? clampAbs(e / power, 1) : 0;
    // e > 0 means the strobes are late: across a − → + transition the midpoint
    // sample has already climbed past the zero crossing. So retard.
    rate = clampAbs(rate - ki * en * sps, sps * 0.05);
    pos = next - kp * en * sps;
    const i = lerp(xi, pos);
    const q = lerp(xq, pos);
    oi.push(i);
    oq.push(q);
    instants.push(pos);
    pi = i;
    pq = q;
  }
  return { i: Float32Array.from(oi), q: Float32Array.from(oq), instants: Float32Array.from(instants) };
}

// ---------------------------------------------------------------------------
// Costas carrier recovery.
// ---------------------------------------------------------------------------

/** Multiply by exp(-j·w·n) — a fixed-frequency NCO, used to remove a MEASURED
 *  carrier offset before filtering, leaving the loop only the residual. */
function derotate(iq: Float32Array, w: number): Float32Array {
  const n = iq.length / 2;
  const out = new Float32Array(n * 2);
  for (let i = 0; i < n; i++) {
    const ph = -w * i;
    const c = Math.cos(ph);
    const sn = Math.sin(ph);
    const ii = iq[2 * i];
    const qq = iq[2 * i + 1];
    out[2 * i] = ii * c - qq * sn;
    out[2 * i + 1] = ii * sn + qq * c;
  }
  return out;
}

/**
 * Costas loop for M-PSK (order 2 or 4). Returns the derotated baseband as
 * interleaved I/Q. A second-order (PI) loop tracks a constant frequency offset;
 * `bwScale` multiplies the nominal loop bandwidth for signals that need faster
 * pull-in at the cost of more jitter.
 */
function costas(iq: Float32Array, order: number, bwScale: number, seedFreq: number): Float32Array {
  const n = iq.length / 2;
  const out = new Float32Array(n * 2);
  // Nominal loop constants: damping 1/√2, normalised bandwidth ~1% × scale.
  const bw = 0.01 * bwScale;
  const damping = Math.SQRT1_2;
  const denom = 1 + 2 * damping * bw + bw * bw;
  const alpha = (4 * damping * bw) / denom;
  const beta = (4 * bw * bw) / denom;

  let phase = 0;
  let freq = seedFreq; // measured carrier offset → near-instant lock

  for (let i = 0; i < n; i++) {
    const c = Math.cos(-phase);
    const s = Math.sin(-phase);
    const ii = iq[2 * i];
    const qq = iq[2 * i + 1];
    const vi = ii * c - qq * s;
    const vq = ii * s + qq * c;
    out[2 * i] = vi;
    out[2 * i + 1] = vq;
    // Decision-directed phase error e = Im(v · conj(decision)), where the
    // decision is the nearest constellation point. This is zero at a correctly
    // locked symbol for BOTH the BPSK constellation {±1} and the ON-AXIS QPSK
    // constellation {1, j, −1, −j} the modulators use — the earlier
    // 45°-diagonal QPSK detector was non-zero at valid on-axis symbols and made
    // the loop wander.
    let e: number;
    if (order === 2) {
      e = sign(vi) * vq;
    } else if (Math.abs(vi) >= Math.abs(vq)) {
      e = sign(vi) * vq; // nearest point on the I axis
    } else {
      e = -sign(vq) * vi; // nearest point on the Q axis
    }
    freq += beta * e;
    phase += freq + alpha * e;
    // Keep phase bounded.
    if (phase > Math.PI) phase -= TWO_PI;
    else if (phase < -Math.PI) phase += TWO_PI;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Symbol timing recovery.
// ---------------------------------------------------------------------------
//
// The baud is a MEASURED input, not a spec — and for the cheap RC-oscillator
// encoder chips behind most OOK remotes/sensors (no crystal), the true rate is
// commonly 5-10% off whatever was measured, and can wander within a single
// burst besides. A single best-phase pick sampled at one fixed period (the
// previous approach here) has no way to correct for that: over a ~75-symbol
// packet, even a "matched" baud measured to a percent or two drifts the
// sampling instant most of the way to the WRONG symbol by the end — verified
// against a real 315 MHz remote (sample/315.007421M-*), where it decoded 11 of
// 12 identical repeats of the same transmission to 11 different wrong answers.
//
// `gardnerReal` fixes this the way real NRZ clock/data recovery does: it can
// only correct timing where the data itself transitions, so instead of a
// continuously-adjusting loop (tried first — a decision-free Gardner TED is
// noisy enough on a boxcar-filtered envelope that its own gain either drifted
// or, tuned safer, barely helped) it re-anchors the NEXT symbol centre to
// sit exactly sps/2 after every transition edge it can find, and only falls
// back to the nominal fixed step across a run with no transition in it (where
// there is, genuinely, no timing information available). Measured: a 315 MHz
// capture with runs no longer than 2 clocks — which is exactly why this works
// for it — dropped from BER 0.33 to 0.01 at the device's actual ~6% rate
// mismatch (test/demod.test.ts, "OOK survives a measured-baud mismatch").
// `gardnerComplex` (PSK's timing stage, downstream of Costas) is UNCHANGED —
// this is real/envelope-detector specific, and PSK devices in this app's scope
// are crystal-clocked sims, not the class of device this was measured against.

const PHASE_STEPS = 32;

/**
 * Sample a real detector (OOK/ASK envelope, FSK discriminator) with edge-
 * corrected timing: acquire a starting phase with the existing coarse
 * variance search (unchanged — still the right way to find where to start),
 * then step by `sps` per symbol, but whenever the signal crosses its own
 * midpoint inside the gap before the next expected centre, re-anchor there
 * (edge time + sps/2) instead of trusting the nominal step. A `guardFrac`
 * margin at each end of the gap keeps a crossing that belongs to the
 * CURRENT or NEXT symbol's own centre from being mistaken for the boundary
 * between them.
 */
function gardnerReal(x: Float32Array, sps: number): { samples: Float32Array; instants: Float32Array } {
  const n = x.length;
  const acquire = bestPhase(sps, n, (p) => {
    let sum = 0;
    let sumSq = 0;
    let cnt = 0;
    for (let pos = p; pos + 1 < n; pos += sps) {
      const v = lerp(x, pos);
      sum += v;
      sumSq += v * v;
      cnt++;
    }
    return cnt > 1 ? sumSq / cnt - (sum / cnt) ** 2 : 0; // variance
  });
  if (n < sps * 2) {
    // Too short to say anything about timing beyond the acquired phase.
    const samples: number[] = [];
    const instants: number[] = [];
    for (let pos = acquire; pos + 1 < n; pos += sps) {
      samples.push(lerp(x, pos));
      instants.push(pos);
    }
    return { samples: Float32Array.from(samples), instants: Float32Array.from(instants) };
  }

  // The midpoint used to find transitions is a robust range estimate, purely
  // for locating edges — independent of the caller's own slicing threshold
  // (e.g. `sliceCenter`), which is applied afterwards on the returned samples.
  const sorted = Float32Array.from(x).sort();
  const mid = (sorted[Math.floor(sorted.length * 0.02)] + sorted[Math.floor(sorted.length * 0.98)]) / 2;
  const guard = sps * 0.1;

  const samples: number[] = [lerp(x, acquire)];
  const instants: number[] = [acquire];
  let pos = acquire;
  while (pos + sps + 1 < n) {
    const nominal = pos + sps;
    const scanStart = Math.max(Math.floor(pos + guard), Math.floor(pos) + 1);
    const scanEnd = Math.min(Math.floor(nominal - guard) + 1, n - 1);
    let edgeTime: number | null = null;
    if (scanEnd > scanStart) {
      let prevSign = Math.sign(x[scanStart] - mid) || 1;
      for (let i = scanStart + 1; i < scanEnd; i++) {
        const s = Math.sign(x[i] - mid) || prevSign;
        if (s !== prevSign) {
          const a = x[i - 1] - mid;
          const b = x[i] - mid;
          const frac = a === b ? 0 : a / (a - b);
          edgeTime = i - 1 + clampRange(frac, 0, 1);
          break;
        }
        prevSign = s;
      }
    }
    pos = edgeTime !== null ? edgeTime + sps / 2 : nominal;
    samples.push(lerp(x, pos));
    instants.push(pos);
  }
  return { samples: Float32Array.from(samples), instants: Float32Array.from(instants) };
}

/**
 * The original one-shot sampler: acquire a single best phase and sample at a
 * fixed period for the whole record. `gardnerReal`'s edge correction assumes a
 * BINARY signal — a transition crosses the midpoint at the symbol boundary,
 * which is what makes "re-anchor on a crossing" meaningful. 4-FSK's four
 * frequency levels break that assumption (a level-2↔level-3 transition never
 * crosses the midpoint at all, and a level-0↔level-3 jump doesn't cross it
 * at the boundary the same way a binary edge does), and forcing it through
 * the same edge logic measurably hurt a clean, correctly-matched signal
 * (test/demod.test.ts's 4-FSK case) rather than helping — so 4-FSK keeps the
 * one-shot pick it already works with.
 */
function gardnerRealOneShot(x: Float32Array, sps: number): { samples: Float32Array; instants: Float32Array } {
  const phase = bestPhase(sps, x.length, (p) => {
    let sum = 0;
    let sumSq = 0;
    let n = 0;
    for (let pos = p; pos + 1 < x.length; pos += sps) {
      const v = lerp(x, pos);
      sum += v;
      sumSq += v * v;
      n++;
    }
    return n > 1 ? sumSq / n - (sum / n) ** 2 : 0; // variance
  });
  const samples: number[] = [];
  const instants: number[] = [];
  for (let pos = phase; pos + 1 < x.length; pos += sps) {
    samples.push(lerp(x, pos));
    instants.push(pos);
  }
  return { samples: Float32Array.from(samples), instants: Float32Array.from(instants) };
}

/** Complex twin: best phase by maximum mean sample energy (the matched-filter
 *  output peaks at the symbol centre for constant-envelope PSK). */
function gardnerComplex(
  xi: Float32Array,
  xq: Float32Array,
  sps: number
): { i: Float32Array; q: Float32Array; instants: Float32Array } {
  const N = Math.min(xi.length, xq.length);
  const phase = bestPhase(sps, N, (p) => {
    let energy = 0;
    let n = 0;
    for (let pos = p; pos + 1 < N; pos += sps) {
      const i = lerp(xi, pos);
      const q = lerp(xq, pos);
      energy += i * i + q * q;
      n++;
    }
    return n > 0 ? energy / n : 0;
  });
  const oi: number[] = [];
  const oq: number[] = [];
  const instants: number[] = [];
  for (let pos = phase; pos + 1 < N; pos += sps) {
    oi.push(lerp(xi, pos));
    oq.push(lerp(xq, pos));
    instants.push(pos);
  }
  return { i: Float32Array.from(oi), q: Float32Array.from(oq), instants: Float32Array.from(instants) };
}

/** Coarse grid search over [0, sps) then a parabolic refine, maximising `score`. */
function bestPhase(sps: number, _len: number, score: (phase: number) => number): number {
  let bestP = 0;
  let bestS = -Infinity;
  for (let k = 0; k < PHASE_STEPS; k++) {
    const p = (k / PHASE_STEPS) * sps;
    const s = score(p);
    if (s > bestS) {
      bestS = s;
      bestP = p;
    }
  }
  // Parabolic refine against the two neighbours.
  const d = sps / PHASE_STEPS;
  const a = score(bestP - d);
  const c = score(bestP + d);
  const denom = a - 2 * bestS + c;
  const delta = denom !== 0 ? (0.5 * (a - c)) / denom : 0;
  return bestP + clampAbs(delta, 0.5) * d;
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/** Centred boxcar (moving-average) matched filter for rectangular symbols. */
function boxcar(x: Float32Array, width: number): Float32Array {
  const w = Math.max(1, width);
  const out = new Float32Array(x.length);
  const half = w >> 1;
  let acc = 0;
  // Running sum with the window centred on each sample.
  for (let i = 0; i < x.length + half; i++) {
    if (i < x.length) acc += x[i];
    if (i - w >= 0) acc -= x[i - w];
    const out_i = i - half;
    if (out_i >= 0 && out_i < x.length) {
      const count = Math.min(i, x.length - 1) - Math.max(0, i - w + 1) + 1;
      out[out_i] = acc / Math.max(1, count);
    }
  }
  return out;
}

function lerp(x: Float32Array, pos: number): number {
  const i = Math.floor(pos);
  if (i < 0) return x[0];
  if (i + 1 >= x.length) return x[x.length - 1];
  const f = pos - i;
  return x[i] + (x[i + 1] - x[i]) * f;
}

/**
 * Root-raised-cosine impulse response, `spanSymbols` long and centred, in the
 * same normalisation GNU Radio's `firdes.root_raised_cosine` uses (unit DC
 * gain). Cascaded with an identical transmit filter this is a raised cosine, so
 * the ISI at the symbol instants is zero — which is the whole reason to match.
 */
function rrcTaps(sps: number, alpha: number, spanSymbols: number): Float32Array {
  // The span is in SYMBOLS, but the cost is in taps, and a crop much wider than
  // the signal can hand us 200 samples per symbol — an 11-symbol filter would
  // then be 2200 taps per output sample. Trade span for cost above the cap; the
  // tails of an RRC are small, so a shorter span loses little, and 4 symbols is
  // the floor below which the matched filter stops being one.
  const MAX_TAPS = 512;
  const persym = Math.max(1, Math.round(sps));
  const span = Math.max(4, Math.min(spanSymbols, Math.floor(MAX_TAPS / persym)));
  const n = persym * span;
  const len = n % 2 === 0 ? n + 1 : n; // odd, so the peak sits on a sample
  const taps = new Float32Array(len);
  const mid = (len - 1) / 2;
  for (let i = 0; i < len; i++) {
    // t in symbol times.
    const t = (i - mid) / sps;
    let v: number;
    if (Math.abs(t) < 1e-8) {
      v = 1 - alpha + (4 * alpha) / Math.PI;
    } else if (alpha > 0 && Math.abs(Math.abs(t) - 1 / (4 * alpha)) < 1e-8) {
      // Removable singularity at t = ±T/(4a).
      const a = Math.PI / (4 * alpha);
      v = (alpha / Math.SQRT2) * ((1 + 2 / Math.PI) * Math.sin(a) + (1 - 2 / Math.PI) * Math.cos(a));
    } else {
      const pt = Math.PI * t;
      const num = Math.sin(pt * (1 - alpha)) + 4 * alpha * t * Math.cos(pt * (1 + alpha));
      const den = pt * (1 - (4 * alpha * t) * (4 * alpha * t));
      v = num / den;
    }
    taps[i] = v;
  }
  // Unit DC gain, so a matched-filtered symbol keeps the constellation's scale
  // and the phase-index slicer sees the same magnitudes it always did.
  let sum = 0;
  for (let i = 0; i < len; i++) sum += taps[i];
  if (sum !== 0) for (let i = 0; i < len; i++) taps[i] /= sum;
  return taps;
}

/** Centred FIR convolution — same alignment convention as `boxcar`, so the
 *  symbol instants Gardner finds do not shift when the kernel changes. */
function fir(x: Float32Array, taps: Float32Array): Float32Array {
  const out = new Float32Array(x.length);
  const half = (taps.length - 1) >> 1;
  for (let i = 0; i < x.length; i++) {
    let acc = 0;
    for (let k = 0; k < taps.length; k++) {
      const j = i + k - half;
      if (j >= 0 && j < x.length) acc += x[j] * taps[k];
    }
    out[i] = acc;
  }
  return out;
}

function evens(interleaved: Float32Array): Float32Array {
  const out = new Float32Array(interleaved.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = interleaved[2 * i];
  return out;
}
function odds(interleaved: Float32Array): Float32Array {
  const out = new Float32Array(interleaved.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = interleaved[2 * i + 1];
  return out;
}
function sign(v: number): number {
  return v >= 0 ? 1 : -1;
}
function clampAbs(v: number, lim: number): number {
  return v > lim ? lim : v < -lim ? -lim : v;
}
function clampRange(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}
function grayDecode(v: number): number {
  let r = v;
  for (let m = v >> 1; m; m >>= 1) r ^= m;
  return r;
}
function differentialDecode(symbols: Uint8Array, order: number): Uint8Array {
  const out = new Uint8Array(symbols.length);
  let prev = 0;
  for (let i = 0; i < symbols.length; i++) {
    out[i] = (symbols[i] - prev + order) % order;
    prev = symbols[i];
  }
  return out;
}
