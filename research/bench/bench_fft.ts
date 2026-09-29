// Raw cost of freq-finder's radix-2 JS FFT, the fixed cost of the fast-conv channelizer.
import { performance } from "node:perf_hooks";
const FF = process.argv[2] ?? "/Users/luke/Projects/SDR/freq-finder/web/src";
const { FFT } = await import(`${FF}/dsp/fft.ts`);
for (const n of [4096, 8192, 16384]) {
  const f = new FFT(n); const re = new Float64Array(n).map(Math.random), im = new Float64Array(n).map(Math.random);
  for (let i = 0; i < 200; i++) f.forward(re, im);
  const t0 = performance.now(); const R = 2000;
  for (let i = 0; i < R; i++) f.forward(re, im);
  const us = (performance.now() - t0) * 1000 / R;
  const perSec = 2_400_000 / (n * 0.75);
  console.log(`FFT ${n}: ${us.toFixed(1)} us -> at 2.4 MSPS with 25% overlap: ${(us * perSec / 1e4).toFixed(2)}% core`);
}
