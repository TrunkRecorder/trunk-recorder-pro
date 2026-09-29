// Channelizer variant: phasor-recurrence NCO (no per-sample trig) + mix fused
// into the decimating FIR so the FIR only runs at output points. How cheap can a
// naive per-channel JS channelizer get before reaching for an FFT channelizer?
import { performance } from "node:perf_hooks";
const FF = process.argv[2] ?? "/Users/luke/Projects/SDR/freq-finder/web/src";
const { designLowpass } = await import(`${FF}/dsp/filters.ts`);
const FS = 2_400_000, DECIM = 50, BLOCK = 65536, SECONDS = 4;
const inI = new Float32Array(BLOCK), inQ = new Float32Array(BLOCK);
for (let i = 0; i < BLOCK; i++) { inI[i] = Math.random() - 0.5; inQ[i] = Math.random() - 0.5; }
for (const taps of [101, 201]) {
  const h = designLowpass(taps, 7_000 / FS);
  const M = h.length;
  const wI = new Float32Array(M - 1 + BLOCK), wQ = new Float32Array(M - 1 + BLOCK);
  const outI = new Float32Array(BLOCK / DECIM + 2), outQ = new Float32Array(BLOCK / DECIM + 2);
  const dw = (2 * Math.PI * 312_500) / FS, cw = Math.cos(dw), sw = Math.sin(dw);
  let pr = 1, pi = 0, next = 0;
  const run = () => {
    wI.copyWithin(0, BLOCK); wQ.copyWithin(0, BLOCK);
    for (let i = 0; i < BLOCK; i++) {
      const x = inI[i], y = inQ[i];
      wI[M - 1 + i] = x * pr - y * pi; wQ[M - 1 + i] = x * pi + y * pr;
      const t = pr * cw - pi * sw; pi = pr * sw + pi * cw; pr = t;
    }
    const g = 1 / Math.hypot(pr, pi); pr *= g; pi *= g; // renormalise per block
    let o = 0, j = next;
    for (; j < BLOCK; j += DECIM) {
      let aI = 0, aQ = 0;
      for (let k = 0; k < M; k++) { const t = h[k]; aI += t * wI[j + k]; aQ += t * wQ[j + k]; }
      outI[o] = aI; outQ[o++] = aQ;
    }
    next = j - BLOCK;
  };
  for (let b = 0; b < 20; b++) run();
  const t0 = performance.now();
  for (let d = 0; d < FS * SECONDS; d += BLOCK) run();
  const ms = performance.now() - t0;
  console.log(`channelizer, rotator NCO, alloc-free (${taps} taps): ${ms.toFixed(1)} ms / ${SECONDS} s -> ${((ms / 1000 / SECONDS) * 100).toFixed(2)}% of one core per channel`);
}
