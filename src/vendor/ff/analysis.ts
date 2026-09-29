// Trimmed from freq-finder web/src/dsp/analysis.ts: only the STFT and the
// averaged spectrum that p25/tuning.ts needs. Code below is verbatim.

import { FFT } from "./fft.ts";

const TWO_PI = 2 * Math.PI;

export interface Stft {
  /** dBFS magnitudes, row-major [frame][bin]; bin 0 = −fs/2 (fft-shifted). */
  data: Float32Array;
  frames: number;
  bins: number;
  /** Sample index at the centre of each frame, for the time axis. */
  hop: number;
  fftSize: number;
}

/**
 * Overlapping windowed FFTs across the whole segment. `fftSize` sets frequency
 * resolution; `overlap` (0..1) trades time resolution for a smoother image.
 * `maxFrames` bounds the output: a long manual capture would otherwise produce
 * an image far taller than any screen (and megabytes to transfer), so the hop
 * is widened to span the whole segment within the cap when needed.
 */
export function stft(iq: Float32Array, fftSize: number, overlap = 0.5, maxFrames = Infinity): Stft {
  const total = iq.length / 2;
  const minHop = Math.max(1, Math.floor(fftSize * (1 - overlap)));
  const spanHop = total > fftSize ? Math.ceil((total - fftSize) / Math.max(1, maxFrames - 1)) : minHop;
  const hop = Math.max(minHop, spanHop);
  const frames = total < fftSize ? 1 : Math.floor((total - fftSize) / hop) + 1;
  const fft = new FFT(fftSize);
  const win = hann(fftSize);
  const re = new Float64Array(fftSize);
  const im = new Float64Array(fftSize);
  const data = new Float32Array(frames * fftSize);
  const half = fftSize >> 1;
  // Same coherent-gain normalisation as PowerSpectrum, so a full-scale tone
  // reads ~0 dBFS and the segment STFT and the live trace share a dB scale.
  const g = fftSize * (sum(win) / fftSize);
  const norm = 1 / (g * g);

  for (let f = 0; f < frames; f++) {
    const start = f * hop;
    for (let i = 0; i < fftSize; i++) {
      const s = start + i;
      if (2 * s + 1 < iq.length) {
        re[i] = iq[2 * s] * win[i];
        im[i] = iq[2 * s + 1] * win[i];
      } else {
        re[i] = 0;
        im[i] = 0;
      }
    }
    fft.forward(re, im);
    const row = f * fftSize;
    // fft-shift: negative-freq half to the front.
    for (let i = 0; i < half; i++) {
      const rh = re[half + i];
      const ih = im[half + i];
      data[row + i] = 10 * Math.log10((rh * rh + ih * ih) * norm + 1e-12);
      const rl = re[i];
      const il = im[i];
      data[row + half + i] = 10 * Math.log10((rl * rl + il * il) * norm + 1e-12);
    }
  }
  return { data, frames, bins: fftSize, hop, fftSize };
}

/** Average magnitude spectrum of a whole segment, dBFS, fft-shifted. Used for
 *  the FSK peak-pair test and as the "zoomed FFT" the user reads by eye. */
export function averageSpectrum(iq: Float32Array, fftSize: number): Float32Array {
  const s = stft(iq, fftSize, 0.5);
  const out = new Float32Array(s.bins);
  for (let f = 0; f < s.frames; f++) {
    const row = f * s.bins;
    for (let b = 0; b < s.bins; b++) out[b] += s.data[row + b];
  }
  for (let b = 0; b < s.bins; b++) out[b] /= s.frames;
  return out;
}

function hann(n: number): Float64Array {
  const w = new Float64Array(n);
  for (let i = 0; i < n; i++) w[i] = 0.5 * (1 - Math.cos((TWO_PI * i) / n));
  return w;
}
function sum(a: ArrayLike<number>): number {
  let s = 0;
  for (let i = 0; i < a.length; i++) s += a[i];
  return s;
}
