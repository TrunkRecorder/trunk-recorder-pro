// Where is the signal actually sitting in the capture window?
//
// The decoder is handed a channelized block of IQ whose DC is wherever the user
// (or a pipeline) tuned — NOT necessarily where the transmitter is. This module
// measures the *signal's* centre inside that window: average the spectrum, find
// the occupied patch around the strongest bin, and take its noise-subtracted
// power-weighted centroid. That is deliberately independent of the demodulator —
// it works whether or not a single TSBK decodes, and it does not care that the
// C4FM discriminator has its own (coarser, symbol-statistics-driven) centre
// estimate.
//
// Add the capture's absolute centre and you have the transmitter's measured
// frequency; compare that against the control-channel frequencies the system
// itself advertises (system.ts) and the difference is the tuning error — a
// crystal offset, so it is reported in ppm as well as Hz.

import { averageSpectrum } from "../analysis.ts";

export interface SignalCenter {
  /** Measured centre of the signal, Hz relative to the capture's DC. */
  offsetHz: number;
  /** Width of the occupied patch the centroid was taken over, Hz. */
  bandwidthHz: number;
  /** Peak above the noise floor, dB — how much to trust the measurement. */
  snrDb: number;
  /** FFT bin spacing, Hz (the resolution floor of a single-bin estimate; the
   *  centroid interpolates well below it). */
  binHz: number;
}

export interface SignalCenterOptions {
  /** FFT size; clamped down to the largest power of two the block can fill. */
  fftSize?: number;
  /** Reject the measurement below this peak-above-floor, dB. */
  minSnrDb?: number;
}

const DEFAULT_FFT = 4096;
const DEFAULT_MIN_SNR_DB = 6;
/** Bins this far below the peak are skirt/noise and are left out of the patch. */
const PATCH_DEPTH_DB = 12;
/** Bridge this many sub-threshold bins before calling the patch finished — an FM
 *  carrier's spectrum has nulls in it, and a null must not truncate the band. */
const MAX_GAP_BINS = 3;

function medianOf(x: Float32Array): number {
  const s = Float32Array.from(x).sort();
  return s[s.length >> 1];
}

/**
 * Locate the signal in a block of interleaved I/Q and return its centre as an
 * offset from the block's DC, or null when nothing stands far enough out of the
 * noise to call a signal.
 */
export function measureSignalCenter(
  iq: Float32Array,
  sampleRateHz: number,
  opts: SignalCenterOptions = {},
): SignalCenter | null {
  const samples = iq.length / 2;
  let fftSize = Math.min(opts.fftSize ?? DEFAULT_FFT, 1 << Math.floor(Math.log2(Math.max(1, samples))));
  if (fftSize < 256) return null;

  const spec = averageSpectrum(iq, fftSize);
  const bins = spec.length;
  const binHz = sampleRateHz / fftSize;
  const floorDb = medianOf(spec);

  let peak = -Infinity;
  let peakBin = 0;
  for (let i = 0; i < bins; i++) {
    if (spec[i] > peak) { peak = spec[i]; peakBin = i; }
  }
  const snrDb = peak - floorDb;
  const minSnr = opts.minSnrDb ?? DEFAULT_MIN_SNR_DB;
  if (!Number.isFinite(snrDb) || snrDb < minSnr) return null;

  // The occupied patch: bins near the peak that stay within PATCH_DEPTH_DB of
  // it (and above the floor), grown outward from the peak so a second, stronger
  // signal elsewhere in the window can never merge into this one.
  const thrDb = Math.max(peak - PATCH_DEPTH_DB, floorDb + snrDb * 0.5);
  let lo = peakBin;
  for (let i = peakBin - 1, gap = 0; i >= 0; i--) {
    if (spec[i] >= thrDb) { lo = i; gap = 0; } else if (++gap > MAX_GAP_BINS) break;
  }
  let hi = peakBin;
  for (let i = peakBin + 1, gap = 0; i < bins; i++) {
    if (spec[i] >= thrDb) { hi = i; gap = 0; } else if (++gap > MAX_GAP_BINS) break;
  }

  // Noise-subtracted power-weighted centroid over the patch. Working in linear
  // power (not dB) is what makes this a centre of mass rather than a centre of
  // log-magnitude, which would over-weight the skirts.
  const floorLin = Math.pow(10, floorDb / 10);
  let wsum = 0;
  let fsum = 0;
  for (let i = lo; i <= hi; i++) {
    const w = Math.pow(10, spec[i] / 10) - floorLin;
    if (w <= 0) continue;
    // bin 0 is −fs/2 (averageSpectrum is fft-shifted).
    const f = (i - bins / 2) * binHz;
    wsum += w;
    fsum += w * f;
  }
  if (wsum <= 0) return null;

  return {
    offsetHz: fsum / wsum,
    bandwidthHz: (hi - lo + 1) * binHz,
    snrDb,
    binHz,
  };
}
