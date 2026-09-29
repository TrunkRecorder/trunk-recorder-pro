// π/4-DQPSK modulator — P25 Phase 1 simulcast CQPSK/LSM (4800 sym/s) and the
// Phase 2 outbound H-DQPSK (6000 sym/s). The inverse of the receiver the
// decoders use (`demodulate(…, "pi4dqpsk", baud)`): each dibit is a phase STEP,
// 00 → +45°, 01 → +135°, 10 → −45°, 11 → −135° (P25's dibit order, the same
// mapping as C4FM's +1/+3/−1/−3), root-raised-cosine shaped (α = 0.2, the P25
// value). For the sim and the loopback tests; nothing on the receive side
// depends on it.

const STEP = [Math.PI / 4, (3 * Math.PI) / 4, -Math.PI / 4, (-3 * Math.PI) / 4];

/** RRC taps spanning `span` symbols at `sps` samples/symbol (sps may be fractional), unit energy. */
function rrcAt(t: number, alpha: number): number {
  if (Math.abs(t) < 1e-9) return 1 - alpha + (4 * alpha) / Math.PI;
  if (Math.abs(Math.abs(t) - 1 / (4 * alpha)) < 1e-9) {
    const a = Math.PI / (4 * alpha);
    return (alpha / Math.SQRT2) * ((1 + 2 / Math.PI) * Math.sin(a) + (1 - 2 / Math.PI) * Math.cos(a));
  }
  return (Math.sin(Math.PI * t * (1 - alpha)) + 4 * alpha * t * Math.cos(Math.PI * t * (1 + alpha))) / (Math.PI * t * (1 - (4 * alpha * t) ** 2));
}

export interface Pi4DqpskOptions {
  carrierOffsetHz?: number;
  /** Uniform noise amplitude added per component (as dibitsToIq). */
  noise?: number;
  amp?: number;
  alpha?: number;
  /** Starting carrier phase, radians. */
  phase0?: number;
  /** Deterministic noise source (default Math.random). */
  rand?: () => number;
}

/** RRC-shaped π/4-DQPSK: dibits → interleaved IQ at `fs`, constant average power `amp`². */
export function dibitsToPi4Dqpsk(dibits: ArrayLike<number>, fs: number, baud: number, opts: Pi4DqpskOptions = {}): Float32Array {
  const sps = fs / baud;
  const alpha = opts.alpha ?? 0.2;
  const span = 8; // symbols each side
  const n = Math.ceil((dibits.length + 1) * sps);
  const re = new Float64Array(n);
  const im = new Float64Array(n);
  let ph = opts.phase0 ?? 0;
  for (let k = 0; k < dibits.length; k++) {
    ph += STEP[dibits[k] & 3];
    const c = Math.cos(ph);
    const s = Math.sin(ph);
    const at = (k + 0.5) * sps;
    const lo = Math.max(0, Math.ceil(at - span * sps));
    const hi = Math.min(n - 1, Math.floor(at + span * sps));
    for (let i = lo; i <= hi; i++) {
      const h = rrcAt((i - at) / sps, alpha);
      re[i] += c * h;
      im[i] += s * h;
    }
  }
  let p = 0;
  for (let i = 0; i < n; i++) p += re[i] * re[i] + im[i] * im[i];
  const scale = (opts.amp ?? 1) / Math.sqrt(p / n || 1);
  const w = (2 * Math.PI * (opts.carrierOffsetHz ?? 0)) / fs;
  const noise = opts.noise ?? 0;
  const rand = opts.rand ?? Math.random;
  const iq = new Float32Array(2 * n);
  for (let i = 0; i < n; i++) {
    const a = re[i] * scale;
    const b = im[i] * scale;
    const c = Math.cos(w * i);
    const s = Math.sin(w * i);
    iq[2 * i] = a * c - b * s + (noise ? (rand() - 0.5) * noise : 0);
    iq[2 * i + 1] = a * s + b * c + (noise ? (rand() - 0.5) * noise : 0);
  }
  return iq;
}
