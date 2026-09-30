// The archived TS engine's whole Phase 2 chain (its H-DQPSK demod, framer,
// decoder) over channel IQ (`trunk-lite tool voice --iq ch.cf32`, 37.5 kHz),
// for comparing receivers: slots framed, burst types, AMBE codeword FEC load.
//
//   node --experimental-strip-types ts_p2_rx.ts ch.cf32 rate nac sysid wacn

import { readFileSync } from "node:fs";
import { findFrames } from "../../archive/ts-engine/src/vendor/ff/p25Voice.ts";
import { P2_SLOT_DIBITS, BURST_2V, BURST_4V, decodeVcw, duidDecode, p2XorMask } from "../../archive/ts-engine/src/vendor/ff/p25/phase2.ts";

const [file, rateS, nacS, sysS, wacnS] = process.argv.slice(2);
const b = readFileSync(file);
const iq = new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);
const fs = Number(rateS);
const mask = p2XorMask(Number(nacS), Number(sysS), Number(wacnS));
const t0 = performance.now();
const evs = findFrames({ demod: "hdqpsk", inverted: false, offsetHz: 0 }, iq, fs, 0, iq.length >> 1);
const cpu = (performance.now() - t0) / 1000;
const types: Record<number, number> = {};
let vcw = 0, errs = 0, clean = 0;
for (const ev of evs) {
  if (ev.kind !== "p2") continue;
  const { slot, dibits } = ev.packet;
  const burst = dibits.subarray(10);
  const t = duidDecode(burst);
  types[t] = (types[t] ?? 0) + 1;
  if (t !== BURST_4V && t !== BURST_2V) continue;
  const x = new Uint8Array(170);
  for (let i = 0; i < 170; i++) x[i] = burst[i] ^ mask[slot * P2_SLOT_DIBITS + i];
  for (const a of t === BURST_4V ? [11, 48, 96, 133] : [11, 48]) {
    const f = decodeVcw(x, a);
    vcw++;
    errs += f.errs;
    if (f.errs <= 1) clean++;
  }
}
const air = iq.length / 2 / fs;
console.log(JSON.stringify({ receiver: "ts", airS: +air.toFixed(1), pctCore: +(100 * cpu / air).toFixed(2), packets: evs.length, types, vcw, meanErrs: +(errs / vcw).toFixed(3), cleanFrac: +(clean / vcw).toFixed(4) }));
