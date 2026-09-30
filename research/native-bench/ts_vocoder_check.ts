// Vocoder equivalence: run the TS MbeDecoder over the IMBE parameters
// `p25tool voice --audio` printed, with the same RNG (32-bit LCG seeded 1), and
// compare its audio with the C++ output sample by sample.
//
//   node --experimental-strip-types ts_vocoder_check.ts frames.jsonl cpp.f32 [--profile enhanced|mbelib]

import { readFileSync } from "node:fs";
import { MbeDecoder, mbeToUnit, MBE_FRAME_SAMPLES, type MbeProfile } from "../../archive/ts-engine/src/vendor/ff/mbe/mbe.ts";
import { imbeParamsToBits } from "../../archive/ts-engine/src/vendor/ff/p25/voice.ts";

const profile = (process.argv.includes("--profile") ? process.argv[process.argv.indexOf("--profile") + 1] : "enhanced") as MbeProfile;
let lcg = 1;
const rand = () => {
  lcg = (Math.imul(lcg, 1664525) + 1013904223) >>> 0;
  return lcg / 4294967296;
};
const dec = new MbeDecoder({ rand, profile });
const chunks: Float32Array[] = [];
for (const line of readFileSync(process.argv[2], "utf8").split("\n")) {
  if (!line.startsWith("{")) continue;
  const f = JSON.parse(line) as { imbe?: { u: number[]; errs: number; e0: number }[] };
  for (const c of f.imbe ?? []) {
    const out = new Float32Array(MBE_FRAME_SAMPLES);
    dec.imbe(imbeParamsToBits(c.u), { e0: c.e0, et: c.errs }, out);
    chunks.push(mbeToUnit(out));
  }
}
const ts = new Float32Array(chunks.length * MBE_FRAME_SAMPLES);
chunks.forEach((c, i) => ts.set(c, i * MBE_FRAME_SAMPLES));
const b = readFileSync(process.argv[3]);
const cpp = new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);

let maxAbs = 0;
let firstBad = -1;
let sig = 0;
let err = 0;
for (let i = 0; i < Math.min(ts.length, cpp.length); i++) {
  const d = Math.abs(ts[i] - cpp[i]);
  if (d > maxAbs) maxAbs = d;
  if (d > 1e-4 && firstBad < 0) firstBad = i;
  sig += ts[i] * ts[i];
  err += d * d;
}
console.log(
  JSON.stringify({
    profile,
    samples: { ts: ts.length, cpp: cpp.length },
    seconds: +(ts.length / 8000).toFixed(2),
    maxAbsDiff: maxAbs,
    snrDb: err > 0 ? +(10 * Math.log10(sig / err)).toFixed(1) : "inf",
    firstSampleOver1e4: firstBad,
    rms: +Math.sqrt(sig / Math.max(1, ts.length)).toFixed(4),
  }),
);
