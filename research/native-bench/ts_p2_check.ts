// Phase 2 equivalence: re-decode every slot `trunk-lite tool p2 --soft none`
// printed (its raw dibits) with the archived TS Phase 2 layer — descramble,
// DUID, AMBE codeword FEC, MAC PDU — and compare field by field. With a Rust
// audio file (`--audio x.f32 --slot N`), also run the TS AMBE+2 vocoder over
// that slot's codewords with the same RNG (32-bit LCG seeded 1) and compare
// sample by sample.
//
//   trunk-lite tool p2 cap.cu8 --center … --freq … --nac … --sysid … --wacn … --soft none [--audio rs.f32 --slot 0] > p2.jsonl
//   node --experimental-strip-types ts_p2_check.ts p2.jsonl nac sysid wacn [rs.f32 slot]

import { readFileSync } from "node:fs";
import {
  BURST_2V, BURST_4V, BURST_FACCH_S, BURST_FACCH_U, BURST_LCCH_S, BURST_SACCH_S, BURST_SACCH_U, P2_SLOT_DIBITS, SLOT_CHANNEL,
  decodeAcch, decodeVcw, duidDecode, p2XorMask,
} from "../../archive/ts-engine/src/vendor/ff/p25/phase2.ts";
import { MbeDecoder, mbeToUnit, MBE_FRAME_SAMPLES } from "../../archive/ts-engine/src/vendor/ff/mbe/mbe.ts";

const [file, nacS, sysS, wacnS, audioFile, slotS] = process.argv.slice(2);
const mask = p2XorMask(Number(nacS), Number(sysS), Number(wacnS));
const wantSlot = Number(slotS ?? 0);
let lcg = 1;
const rand = () => {
  lcg = (Math.imul(lcg, 1664525) + 1013904223) >>> 0;
  return lcg / 4294967296;
};
const vocoder = new MbeDecoder({ rand, profile: "enhanced" });
const chunks: Float32Array[] = [];
const n = { slots: 0, typeMismatch: 0, vcw: 0, vcwMismatch: 0, mac: 0, macMismatch: 0 };
const examples: string[] = [];

for (const line of readFileSync(file, "utf8").split("\n")) {
  if (!line.startsWith("{")) continue;
  const r = JSON.parse(line) as { slot: number; type: number; dibits: string; vcw?: { bits: string; errs: number }[]; mac?: { hex: string } | null };
  n.slots++;
  const d = Uint8Array.from(r.dibits, (c) => c.charCodeAt(0) - 48);
  const burst = d.subarray(10);
  const type = duidDecode(burst);
  if (type !== r.type) {
    n.typeMismatch++;
    continue;
  }
  const scrambled = [BURST_4V, BURST_2V, BURST_SACCH_S, BURST_LCCH_S, BURST_FACCH_S].includes(type);
  const x = new Uint8Array(170);
  for (let i = 0; i < 170; i++) x[i] = scrambled ? burst[i] ^ mask[r.slot * P2_SLOT_DIBITS + i] : burst[i];
  if (type === BURST_4V || type === BURST_2V) {
    const at = type === BURST_4V ? [11, 48, 96, 133] : [11, 48];
    at.forEach((a, k) => {
      const f = decodeVcw(x, a);
      n.vcw++;
      const bits = Array.from(f.bits).join("");
      const rs = r.vcw![k];
      if (bits !== rs.bits || f.errs !== rs.errs) {
        n.vcwMismatch++;
        if (examples.length < 5) examples.push(`vcw slot ${r.slot} k ${k}: ts ${bits}/${f.errs} rs ${rs.bits}/${rs.errs}`);
      }
      if (SLOT_CHANNEL[r.slot] === wantSlot) {
        const out = new Float32Array(MBE_FRAME_SAMPLES);
        vocoder.ambe(f.bits, f.errs, out);
        chunks.push(mbeToUnit(out));
      }
    });
  } else if ([BURST_SACCH_S, BURST_SACCH_U, BURST_FACCH_S, BURST_FACCH_U].includes(type)) {
    const pdu = decodeAcch(x, type === BURST_FACCH_S || type === BURST_FACCH_U);
    const hex = pdu ? Array.from(pdu.bytes, (b) => b.toString(16).padStart(2, "0")).join("") : null;
    n.mac++;
    if (hex !== (r.mac?.hex ?? null)) {
      n.macMismatch++;
      if (examples.length < 5) examples.push(`mac slot ${r.slot}: ts ${hex} rs ${r.mac?.hex ?? null}`);
    }
  }
}
const res: Record<string, unknown> = { ...n };
if (audioFile) {
  const ts = new Float32Array(chunks.length * MBE_FRAME_SAMPLES);
  chunks.forEach((c, i) => ts.set(c, i * MBE_FRAME_SAMPLES));
  const b = readFileSync(audioFile);
  const rs = new Float32Array(b.buffer, b.byteOffset, b.byteLength / 4);
  let maxAbs = 0;
  for (let i = 0; i < Math.min(ts.length, rs.length); i++) maxAbs = Math.max(maxAbs, Math.abs(ts[i] - rs[i]));
  res.audio = { ts: ts.length, rs: rs.length, seconds: +(ts.length / 8000).toFixed(1), maxAbsDiff: maxAbs };
}
console.log(JSON.stringify(res));
for (const e of examples) console.log(e);
