// C4FM or CQPSK? — the one P25 parameter another recorder needs that is NOT
// carried in any TSBK.
//
// Trunk Recorder (and SDRTrunk, and op25) must be told which physical layer a
// P25 Phase 1 site uses: `"modulation": "fsk4"` for standard C4FM, or `"qpsk"`
// for the linear-simulcast (LSM / CQPSK) waveform that simulcast sites
// transmit. The band plan doesn't say, the TSBKs don't say, and getting it
// wrong costs most of the decode. So it has to be measured off the air.
//
// The discriminator is the envelope. C4FM is a *constant-envelope* FM waveform:
// |IQ| is flat, and only noise moves it. LSM/CQPSK is a linear, pulse-shaped
// modulation whose symbol trajectory passes near the origin, so |IQ| swings
// hard and periodically dips toward zero. Two features fall out:
//
//   cv          robust spread of the envelope (half the 16→84 percentile
//               span, over the median) — a std/mean that outliers can't move.
//   dipFraction how much of the time the envelope sits below half the median.
//               C4FM never goes there; a QPSK trajectory does, every time it
//               passes the origin.
//
// Both are computed on a lightly smoothed envelope (a quarter-symbol boxcar),
// which knocks the AWGN contribution down without touching the modulation's own
// swing — the modulation moves at the symbol rate, the noise doesn't.
//
// This is a heuristic, and it is reported as one: the caller gets the raw
// features, a decision, and a confidence, and low-SNR windows push the answer
// to "unknown" rather than to a guess. The consumer (trunkRecorder.ts) defaults
// to fsk4 when unknown — the far more common case — and says so in a warning.

import { P25_SYMBOL_RATE } from "./c4fm.ts";

export type P25Modulation = "fsk4" | "qpsk" | "unknown";

export interface ModulationMeasurement {
  kind: P25Modulation;
  /** Robust envelope spread ÷ median: ~0 for a clean C4FM carrier. */
  cv: number;
  /** Fraction of the window whose envelope is below half the median. */
  dipFraction: number;
  /** Combined feature score, 0 = definitely C4FM, 1 = definitely CQPSK. */
  score: number;
  /** How far the score sits from the undecided middle, 0..1. */
  confidence: number;
}

/** Below this many symbols the percentiles are noise. */
const MIN_SYMBOLS = 64;
/** Envelope spread at or under this reads as constant-envelope. */
const CV_FLAT = 0.15;
/** …and at or over this reads as a linear modulation. */
const CV_SWINGING = 0.3;
/** Dip fractions bracketing the same decision. */
const DIP_FLAT = 0.01;
const DIP_SWINGING = 0.07;
/** Score bands. Between them the answer is "unknown", not a coin flip. */
const SCORE_FSK4 = 0.35;
const SCORE_QPSK = 0.65;

const clamp01 = (x: number): number => (x < 0 ? 0 : x > 1 ? 1 : x);

/** Value at fractional rank `q` of an already-sorted array. */
function quantile(sorted: Float32Array, q: number): number {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.round(q * (sorted.length - 1))));
  return sorted[i];
}

/**
 * Measure whether a channelized block of P25 IQ is C4FM or CQPSK/LSM. Returns
 * null when the block is too short, or carries no signal at all (a zero-ish
 * envelope has no meaningful spread).
 */
export function measureModulation(
  iq: Float32Array,
  sampleRateHz: number,
): ModulationMeasurement | null {
  const n = iq.length / 2;
  const sps = sampleRateHz / P25_SYMBOL_RATE;
  if (!Number.isFinite(sps) || sps < 2 || n < MIN_SYMBOLS * sps) return null;

  const env = new Float32Array(n);
  for (let i = 0; i < n; i++) env[i] = Math.hypot(iq[2 * i], iq[2 * i + 1]);

  // Quarter-symbol boxcar: averages noise down by √w, leaves the symbol-rate
  // envelope swing of a linear modulation essentially intact.
  const w = Math.max(1, Math.round(sps / 4));
  const sm = new Float32Array(Math.max(0, n - w));
  let acc = 0;
  for (let i = 0; i < n; i++) {
    acc += env[i];
    if (i >= w) acc -= env[i - w];
    if (i >= w) sm[i - w] = acc / w;
  }
  if (sm.length < MIN_SYMBOLS) return null;

  const sorted = Float32Array.from(sm).sort();
  const median = quantile(sorted, 0.5);
  if (!(median > 0)) return null;

  const cv = (quantile(sorted, 0.84) - quantile(sorted, 0.16)) / 2 / median;
  const dipThr = median * 0.5;
  let dips = 0;
  for (let i = 0; i < sm.length; i++) if (sm[i] < dipThr) dips++;
  const dipFraction = dips / sm.length;

  const score =
    0.5 * clamp01((cv - CV_FLAT) / (CV_SWINGING - CV_FLAT)) +
    0.5 * clamp01((dipFraction - DIP_FLAT) / (DIP_SWINGING - DIP_FLAT));

  return { ...classifyScore(score), cv, dipFraction, score };
}

/**
 * Score → decision + confidence. Split out so the run-long roll-up (system.ts)
 * can re-decide from the MEDIAN score across windows with the same thresholds
 * a single window used, rather than voting on per-window decisions.
 */
export function classifyScore(score: number): { kind: P25Modulation; confidence: number } {
  const kind: P25Modulation = score <= SCORE_FSK4 ? "fsk4" : score >= SCORE_QPSK ? "qpsk" : "unknown";
  // Distance from the undecided band, normalized so its edges read 0.
  const confidence =
    kind === "unknown"
      ? 0
      : kind === "fsk4"
        ? clamp01((SCORE_FSK4 - score) / SCORE_FSK4)
        : clamp01((score - SCORE_QPSK) / (1 - SCORE_QPSK));
  return { kind, confidence };
}
