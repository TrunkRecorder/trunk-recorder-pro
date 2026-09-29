// P25 voice call generator — for the simulated voice channels and the loopback
// tests. Builds whole calls as dibit streams: Phase 1 (HDU, LDU1/LDU2 pairs, TDU)
// and Phase 2 (scrambled superframes with MAC_PTT / 4V / 2V / MAC_END_PTT).
//
// The vocoder frames are PARAMETER frames, not encoded speech: there is no IMBE
// or AMBE speech encoder here (that is an analysis problem — pitch tracking,
// voicing, spectral estimation — and is not needed to exercise a decoder). Each
// frame names a pitch, fully voiced, at a fixed gain, with the spectral-shape
// coefficients at their quantisers' mid-point — a steady "aah" at that pitch. A
// sequence of pitches is a melody, which makes a decoded channel recognisable by
// ear and checkable by measurement (the decoded fundamental must match).

import { bo, hoba, ImbeJi, ba } from "../mbe/tables.ts";
import { encodeHdu, encodeLdu, encodeTdu, frameBodyToDibits, imbeBitsToParams, type LcSpec } from "./voice.ts";
import {
  ambeBitsToU,
  buildMacHeader,
  buildMacPtt,
  encodeEss,
  encodeSlot,
  p2XorMask,
  P2_SLOT_DIBITS,
  type SlotContent,
} from "./phase2.ts";

// ── IMBE parameter frames ────────────────────────────────────────────────────

/** IMBE b0 for a fundamental in Hz: w0 = 4π/(b0 + 39.5) rad/sample at 8 kHz. */
export function imbeB0ForHz(f0: number): number {
  return Math.max(0, Math.min(207, Math.round(16000 / f0 - 39.5)));
}
export function imbeHzForB0(b0: number): number {
  return 16000 / (b0 + 39.5);
}

/**
 * Pack an IMBE frame: pitch index b0, every band voiced, gain index (b2, the
 * 6-bit G1 → mbelib B2[]), everything else at its quantiser mid-point. The bit
 * placement is the inverse of mbe_decodeImbe4400Parms (mbelib's bo[] map).
 */
export function packImbe(b0: number, gain = 56, voiced = true): Uint8Array {
  const d = new Uint8Array(88);
  for (let i = 0; i < 6; i++) d[i] = (b0 >> (7 - i)) & 1;
  d[85] = (b0 >> 1) & 1;
  d[86] = b0 & 1;
  const w0 = (4 * Math.PI) / (b0 + 39.5);
  const L = Math.trunc(0.9254 * Math.trunc(Math.PI / w0 + 0.25));
  const L9 = L - 9;
  const K = L < 37 ? Math.trunc((L + 2) / 3) : 12;
  const bb: Uint8Array[] = Array.from({ length: 58 }, () => new Uint8Array(12));
  for (let k = 0; k < K; k++) bb[1][k] = voiced ? 1 : 0;
  for (let j = 0; j < 6; j++) bb[2][j] = (gain >> j) & 1; // bb[2][5] is the MSB
  for (let i = 2; i < 7; i++) {
    const ba1 = Math.trunc(ba[L9 * 10 + (i - 2) * 2]);
    if (ba1 > 0) bb[i + 1][ba1 - 1] = 1; // bm = 2^(ba1-1): the mid-point
  }
  let m = 8;
  for (let i = 1; i <= 6; i++) {
    for (let k = 2; k <= ImbeJi[L9 * 6 + i - 1]; k++) {
      const Bm = hoba[L9 * 50 + m - 8];
      if (Bm > 0) bb[m][Bm - 1] = 1;
      m++;
    }
  }
  const base = L9 * 79 * 2;
  for (let i = 6, p = 0; i < 85; i++, p++) d[i] = bb[bo[base + 2 * p]][bo[base + 2 * p + 1]];
  return d;
}

// ── AMBE+2 parameter frames ──────────────────────────────────────────────────

/** AMBE b0 for a fundamental in Hz — inverse of mbelib's AmbeW0table (a log scale). */
export function ambeB0ForHz(f0: number, table: ArrayLike<number>): number {
  let best = 0;
  for (let b = 0; b < 120; b++) if (Math.abs(table[b] * 8000 - f0) < Math.abs(table[best] * 8000 - f0)) best = b;
  return best;
}

/** Pack an AMBE+2 frame (mbe_decodeAmbe2450Parms bit positions): pitch b0,
 *  V/UV codebook entry 0 (all voiced), gain step b2, spectral indices fixed. */
export function packAmbe(b0: number, gain = 14): Uint8Array {
  const d = new Uint8Array(49);
  const put = (v: number, pos: number[]) => pos.forEach((p, i) => (d[p] = (v >> (pos.length - 1 - i)) & 1));
  put(b0, [0, 1, 2, 3, 37, 38, 39]);
  put(0, [4, 5, 6, 7, 35]); // b1: V/UV vector 0 = all voiced
  put(gain, [8, 9, 10, 11, 36]); // b2: gain delta
  put(256, [12, 13, 14, 15, 16, 17, 18, 19, 40]); // b3 PRBA24
  put(64, [20, 21, 22, 23, 41, 42, 43]); // b4 PRBA58
  put(16, [24, 25, 26, 27, 44]); // b5
  put(8, [28, 29, 30, 45]); // b6
  put(8, [31, 32, 33, 46]); // b7
  put(4, [34, 47, 48]); // b8
  return d;
}

/** A melody: `notesHz` each held `framesPerNote` 20 ms frames, `nFrames` long. */
export function melody(nFrames: number, notesHz: number[], framesPerNote: number, pack: (hz: number) => Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (let f = 0; f < nFrames; f++) out.push(pack(notesHz[Math.floor(f / framesPerNote) % notesHz.length]));
  return out;
}

// ── Phase 1 call ─────────────────────────────────────────────────────────────

export interface P1CallSpec {
  nac: number;
  tgid: number;
  source: number;
  /** imbe_d[88] frames; padded with the last frame to a multiple of 18 (one LDU1+LDU2). */
  frames: Uint8Array[];
  algid?: number;
  keyid?: number;
  emergency?: boolean;
  /** Send the HDU (a late-joined call has none). Default true. */
  header?: boolean;
  /** Send the terminator. Default true. */
  terminator?: boolean;
}

/** A whole Phase 1 call as on-air dibits (status symbols included). */
export function p1CallDibits(c: P1CallSpec): Uint8Array {
  const parts: Uint8Array[] = [];
  const algid = c.algid ?? 0x80;
  const enc = algid !== 0x80;
  if (c.header !== false) parts.push(frameBodyToDibits(encodeHdu(c.nac, { tgid: c.tgid, algid, keyid: c.keyid ?? 0 })));
  const frames = c.frames.slice();
  while (frames.length % 18) frames.push(frames[frames.length - 1]);
  const lc: LcSpec = { tgid: c.tgid, source: c.source, svcOpts: (c.emergency ? 0x80 : 0) | (enc ? 0x40 : 0) };
  for (let f = 0; f < frames.length; f += 18) {
    const u1 = frames.slice(f, f + 9).map(imbeBitsToParams);
    const u2 = frames.slice(f + 9, f + 18).map(imbeBitsToParams);
    parts.push(frameBodyToDibits(encodeLdu(c.nac, "ldu1", u1, { lc })));
    parts.push(frameBodyToDibits(encodeLdu(c.nac, "ldu2", u2, { es: { algid, keyid: c.keyid ?? 0 } })));
  }
  if (c.terminator !== false) parts.push(frameBodyToDibits(encodeTdu(c.nac)));
  return concat(parts);
}

// ── Phase 2 channel ──────────────────────────────────────────────────────────

export interface P2CallSpec {
  tgid: number;
  source: number;
  /** ambe_d[49] frames; padded to a multiple of 18 (one superframe per channel). */
  frames: Uint8Array[];
  algid?: number;
  keyid?: number;
}

/**
 * Phase 2 superframes carrying up to two calls (logical channels 0 and 1).
 * Superframe 0 announces each call (MAC_PTT in the channel's FACCH/SACCH, and
 * every later SACCH repeats it),
 * voice runs 4V,4V,4V,4V,2V per channel per superframe, and the last
 * superframe carries MAC_END_PTT. Channels without a call idle (MAC_IDLE).
 */
export function p2Dibits(key: { nac: number; sysid: number; wacn: number }, calls: [P2CallSpec | null, P2CallSpec | null]): Uint8Array {
  const mask = p2XorMask(key.nac, key.sysid, key.wacn);
  const perCall = calls.map((c) => {
    if (!c) return null;
    const f = c.frames.slice();
    while (f.length % 18) f.push(f[f.length - 1]);
    return { ...c, frames: f, ess: encodeEss({ algid: c.algid ?? 0x80, keyid: c.keyid ?? 0 }) };
  });
  const voiceSf = Math.max(0, ...perCall.map((c) => (c ? c.frames.length / 18 : 0)));
  const nSf = voiceSf + 2; // PTT superframe + voice + END superframe
  const slots: Uint8Array[] = [];
  for (let sf = 0; sf < nSf; sf++) {
    for (let s = 0; s < 12; s++) {
      const chn = [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0][s];
      const c = perCall[chn];
      let content: SlotContent;
      const vsf = sf - 1; // voice superframe index
      if (s >= 10) {
        // SACCH (slot 10 → ch1, 11 → ch0)
        // The call's identity rides every SACCH, not just the first: a
        // receiver that tunes in mid-call still learns who is talking. (Real
        // systems carry it mid-call in MAC_ACTIVE's GRP_V_CH_USR message, which
        // this decoder does not parse yet; a repeated MAC_PTT stands in.)
        const pdu =
          !c ? buildMacHeader(3)
          : sf === nSf - 1 ? buildMacHeader(2)
          : buildMacPtt({ offset: 4, source: c.source, group: c.tgid, algid: c.algid ?? 0x80, keyid: c.keyid });
        content = { kind: "sacch", pdu };
      } else if (!c || vsf < 0 || vsf >= c.frames.length / 18) {
        // No voice in this slot: FACCH announcement / idle.
        const pdu = c && sf === 0 ? buildMacPtt({ offset: 4, source: c.source, group: c.tgid, algid: c.algid ?? 0x80, keyid: c.keyid }, 18) : buildMacHeader(c && sf === nSf - 1 ? 2 : 3, 0, 18);
        content = { kind: "sacch", pdu, fast: true };
      } else {
        const bid = s >> 1; // 0..3 → 4V, 4 → 2V
        const base = vsf * 18 + bid * 4;
        const fr = (k: number) => ambeBitsToU(c.frames[base + k]);
        content =
          bid < 4
            ? { kind: "4v", frames: [fr(0), fr(1), fr(2), fr(3)], essB: c.ess.essB.slice(4 * bid, 4 * bid + 4) }
            : { kind: "2v", frames: [fr(0), fr(1)], essA: c.ess.essA };
      }
      slots.push(encodeSlot(s, content, mask));
    }
  }
  const out = concat(slots);
  if (out.length !== nSf * 12 * P2_SLOT_DIBITS) throw new Error("superframe length");
  return out;
}

function concat(parts: Uint8Array[]): Uint8Array {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}
