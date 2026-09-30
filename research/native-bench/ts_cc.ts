// TS reference for the control-channel stage: run the engine's
// P25ControlDecoder on channel IQ (f32 interleaved, e.g. `p25tool cc --iq`) and
// print one JSON line per CRC-valid TSBK, in p25tool's format, plus a summary
// on stderr.
//
//   node --experimental-strip-types ts_cc.ts <chan.cf32> --rate 37500 [--modulation auto|fsk4|qpsk]

import { readFileSync } from "node:fs";
import { P25ControlDecoder, type P25Modulation } from "../../archive/ts-engine/src/protocols/p25/controlDecoder.ts";

const arg = (k: string, d: string) => {
  const i = process.argv.indexOf(`--${k}`);
  return i >= 0 ? process.argv[i + 1] : d;
};
const rate = Number(arg("rate", "37500"));
const buf = readFileSync(process.argv[2]);
const iq = new Float32Array(buf.buffer, buf.byteOffset, buf.byteLength / 4);

const dec = new P25ControlDecoder(rate, () => {}, arg("modulation", "auto") as P25Modulation);
// Capture the raw blocks: every block that reaches the parser passed its CRC.
const parse = dec.parser.parse.bind(dec.parser);
(dec.parser as { parse: typeof parse }).parse = (block, nac, timeS) => {
  const hex = Array.from(block, (b) => b.toString(16).padStart(2, "0")).join("");
  console.log(JSON.stringify({ t: +timeS.toFixed(3), nac, op: block[0] & 0x3f, hex }));
  return parse(block, nac, timeS);
};

const t0 = performance.now();
const CH = 2 * 192;
for (let i = 0; i < iq.length; i += CH) dec.push(iq.subarray(i, Math.min(iq.length, i + CH)));
const ms = performance.now() - t0;
const air = iq.length / 2 / rate;
const s = dec.stats();
console.error(JSON.stringify({ ts: true, airS: +air.toFixed(2), pctCore: +((100 * ms) / 1000 / air).toFixed(3), good: s.good, bad: s.bad, modulation: s.modulation }));
