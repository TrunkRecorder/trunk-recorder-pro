// Voice-layer equivalence: re-decode every frame `p25tool voice` printed (its
// raw bits, status symbols in place) with the TS functions and compare field by
// field against the C++ decode.
//
//   p25tool voice cap.cu8 … > frames.jsonl
//   node --experimental-strip-types ts_voice_check.ts frames.jsonl

import { readFileSync } from "node:fs";
import { decodeHdu, decodeLdu1Lc, decodeLdu2Es, decodeTdulc, imbeHeaderDecode, lduCodewords, type LinkControl } from "../../archive/ts-engine/src/vendor/ff/p25/voice.ts";

interface CppLc { lco: number; prot: number; svc: number; tgid: number; target: number; src: number }
interface CppFrame {
  t: number;
  duid: number;
  nbits: number;
  raw: string;
  imbe?: { u: number[]; errs: number; e0: number }[];
  lc?: CppLc | null;
  es?: { algid: number; keyid: number } | null;
  hdu?: { algid: number; keyid: number; mfid: number; tgid: number } | null;
  tdulc?: CppLc | null;
}

const bitsOf = (hex: string, n: number) => {
  const fb = new Uint8Array(n);
  for (let i = 0; i < n; i++) fb[i] = (parseInt(hex[i >> 2], 16) >> (3 - (i & 3))) & 1;
  return fb;
};
const lcKey = (lc: LinkControl | null) =>
  lc ? JSON.stringify([lc.lco, lc.protected ? 1 : 0, lc.svcOpts ?? -1, lc.tgid ?? -1, lc.target ?? -1, lc.source ?? -1]) : "null";
const cppLcKey = (lc: CppLc | null | undefined) => (lc ? JSON.stringify([lc.lco, lc.prot, lc.svc, lc.tgid, lc.target, lc.src]) : "null");

const n = { frames: 0, codewords: 0, cwMismatch: 0, lc: 0, lcMismatch: 0, es: 0, esMismatch: 0, hdu: 0, hduMismatch: 0, tdulc: 0, tdulcMismatch: 0 };
const byDuid: Record<number, number> = {};
const examples: string[] = [];
let fecErrs = 0;

for (const line of readFileSync(process.argv[2], "utf8").split("\n")) {
  if (!line.startsWith("{")) continue;
  const f = JSON.parse(line) as CppFrame;
  n.frames++;
  byDuid[f.duid] = (byDuid[f.duid] ?? 0) + 1;
  const fb = bitsOf(f.raw, f.nbits);
  if (f.imbe) {
    lduCodewords(fb).forEach((cw, k) => {
      const p = imbeHeaderDecode(cw);
      const c = f.imbe![k];
      n.codewords++;
      fecErrs += p.errs;
      if (JSON.stringify(p.u) !== JSON.stringify(c.u) || p.errs !== c.errs || p.e0 !== c.e0) {
        n.cwMismatch++;
        if (examples.length < 5) examples.push(`t=${f.t} cw${k}: TS ${JSON.stringify(p)} C++ ${JSON.stringify(c)}`);
      }
    });
  }
  if (f.lc !== undefined) {
    n.lc++;
    if (lcKey(decodeLdu1Lc(fb)) !== cppLcKey(f.lc)) n.lcMismatch++;
  }
  if (f.es !== undefined) {
    n.es++;
    const es = decodeLdu2Es(fb);
    if ((es ? `${es.algid},${es.keyid}` : "null") !== (f.es ? `${f.es.algid},${f.es.keyid}` : "null")) n.esMismatch++;
  }
  if (f.hdu !== undefined) {
    n.hdu++;
    const h = decodeHdu(fb);
    if ((h ? `${h.algid},${h.keyid},${h.mfid},${h.tgid}` : "null") !== (f.hdu ? `${f.hdu.algid},${f.hdu.keyid},${f.hdu.mfid},${f.hdu.tgid}` : "null")) n.hduMismatch++;
  }
  if (f.tdulc !== undefined) {
    n.tdulc++;
    if (lcKey(decodeTdulc(fb)) !== cppLcKey(f.tdulc)) n.tdulcMismatch++;
  }
}
console.log(JSON.stringify({ ...n, meanFecErrsPerCodeword: +(fecErrs / Math.max(1, n.codewords)).toFixed(2), byDuid }));
for (const e of examples) console.log(e);
