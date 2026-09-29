// Synthetic P25 system at wideband, as an RTL-SDR would deliver it (u8 IQ):
// a C4FM control channel repeating IDEN_UP / NET_STS / RFSS_STS, with a
// GRP_V_CH_GRANT for each call while it is on the air, and each call as
// spec-shaped C4FM Phase 1 voice on its granted frequency. Built from
// freq-finder's encoders, which its decoders are verified against.

import { buildGrant, buildIdenUp, buildNetSts, buildRfssSts, buildTsduDibits, dibitsToC4fmShaped, dibitsToIq } from "../src/vendor/ff/p25/encode.ts";
import { imbeB0ForHz, melody, p1CallDibits, packImbe } from "../src/vendor/ff/p25/voiceSim.ts";

export const SYS = { nac: 0x4d8, wacn: 0xbee00, sysId: 0x123, rfss: 1, site: 3 };
export const IDEN = { iden: 1, baseHz: 762_000_000, stepHz: 12_500, offsetHz: 30_000_000 };
export const chanFor = (hz: number) => (IDEN.iden << 12) | Math.round((hz - IDEN.baseHz) / IDEN.stepHz);

export interface SynthCall {
  hz: number;
  tg: number;
  src: number;
  startS: number;
  /** IMBE frames (20 ms each); padded to whole LDU pairs. */
  frames: number;
  algid?: number;
}

export interface SynthOptions {
  fs: number;
  centerHz: number;
  ccHz: number;
  durationS: number;
  calls: SynthCall[];
  noise?: number;
}

function callDurationS(c: SynthCall): number {
  return (Math.ceil(c.frames / 18) * 18 * 0.02) + 0.3;
}

export function synthSystem(o: SynthOptions): Uint8Array {
  const n = Math.round(o.durationS * o.fs);
  const acc = new Float32Array(2 * n);

  // Control channel, TSDU by TSDU, granting whichever calls are on the air.
  const ccDib: number[] = [];
  const ccCh = chanFor(o.ccHz);
  let t = 0;
  let k = 0;
  while (t < o.durationS + 0.2) {
    const live = o.calls.filter((c) => t >= c.startS - 0.1 && t < c.startS + callDurationS(c));
    const blocks = [
      k % 2 === 0 ? buildIdenUp(IDEN) : buildNetSts(SYS.wacn, SYS.sysId, ccCh),
      live.length ? buildGrant(chanFor(live[k % live.length].hz), live[k % live.length].tg, live[k % live.length].src) : buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, ccCh),
      buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, ccCh, true),
    ];
    const d = buildTsduDibits(SYS.nac, blocks);
    for (const x of d) ccDib.push(x);
    t += d.length / 4800;
    k++;
  }
  const ccIq = dibitsToIq(Uint8Array.from(ccDib), o.fs, { carrierOffsetHz: o.ccHz - o.centerHz, amp: 0.25 });
  for (let i = 0; i < 2 * n; i++) acc[i] += ccIq[i];

  // Voice calls: shaped C4FM at 48 k, FM-upsampled onto the wideband.
  for (const c of o.calls) {
    const frames = melody(c.frames, [262, 330, 392, 523], 10, (hz: number) => packImbe(imbeB0ForHz(hz)));
    const dib = p1CallDibits({ nac: SYS.nac, tgid: c.tg, source: c.src, frames, algid: c.algid });
    const v = dibitsToC4fmShaped(dib, 48_000, {});
    const nv = v.length / 2;
    const R = o.fs / 48_000;
    const inst = new Float64Array(nv);
    for (let i = 1; i < nv; i++) {
      inst[i] = Math.atan2(v[2 * i + 1] * v[2 * i - 2] - v[2 * i] * v[2 * i - 1], v[2 * i] * v[2 * i - 2] + v[2 * i + 1] * v[2 * i - 1]) / R;
    }
    const start = Math.round(c.startS * o.fs);
    const w = (2 * Math.PI * (c.hz - o.centerHz)) / o.fs;
    let ph = 0;
    for (let i = 0; i < Math.floor(nv * R) && start + i < n; i++) {
      const j = i / R;
      const j0 = Math.floor(j);
      const fr = j - j0;
      ph += (inst[j0] ?? 0) * (1 - fr) + (inst[j0 + 1] ?? 0) * fr + w;
      acc[2 * (start + i)] += 0.25 * Math.cos(ph);
      acc[2 * (start + i) + 1] += 0.25 * Math.sin(ph);
    }
  }

  // Quantise like an RTL2832U: offset-binary u8, a little noise.
  const noise = o.noise ?? 0.02;
  const u8 = new Uint8Array(2 * n);
  let seed = 12345;
  const rnd = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32 - 0.5);
  for (let i = 0; i < 2 * n; i++) u8[i] = Math.max(0, Math.min(255, Math.round(127.5 + 127.5 * (acc[i] + noise * rnd()))));
  return u8;
}
