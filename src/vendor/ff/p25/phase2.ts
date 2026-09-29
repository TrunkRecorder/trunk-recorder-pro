// P25 Phase 2 TDMA — the layers between an H-DQPSK dibit stream and the AMBE+2
// vocoder. A port of boatbod/op25 gr-op25_repeater (GPLv3): p25p2_framer.cc,
// p25p2_sync.cc, p25p2_isch.cc, p25p2_duid.cc, p25p2_vf.cc (the AMBE voice
// codeword layout + FEC), p25p2_tdma.cc (burst handling, ESS, ACCH/MAC PDUs,
// CRC-12) and apps/tdma/lfsr.py (the scrambler) — names kept for side-by-side
// reading. Verified in web/test/p25p2.test.ts: the scrambler against op25's own
// lfsr.py output, the AMBE PN against op25's pr_n[] table, everything else by
// encode → decode loopback (each encoder is the inverse of op25's decoder).
//
// The shape of the channel (TIA-102.BBAC):
//   · 6000 sym/s; a 30 ms timeslot is 180 dibits; 12 slots are a 360 ms
//     superframe. Two logical voice channels alternate: slots 0..9 are
//     ch0,ch1,ch0,…; slots 10/11 are the SACCHs of ch1/ch0.
//   · Each slot starts with a 20-dibit ISCH: S-ISCH (the sync word) in slots
//     2,3,6,7,10,11, I-ISCH (slot location) in 0,1,4,5,8,9.
//   · op25 frames a "packet" of 180 dibits from the ISCH; the burst is packet
//     dibits 10..179 (170 dibits), and is XOR-scrambled with a mask seeded from
//     WACN / System ID / NAC. **Without those three numbers a Phase 2 voice
//     channel cannot be descrambled** — they come from the control channel
//     (the P25 system harvest) or from the user.

import { golay23Decode, golay23Encode, golay24Decode, golay24Encode, rsDecode, rsEncode } from "./fec.ts";
import { DUID_LOOKUP, ISCH_CODEWORDS, LFSR_SEED_MATRIX } from "./phase2Tables.ts";

export const P2_SYMBOL_RATE = 6000;
export const P2_SLOT_DIBITS = 180;
export const P2_SUPERFRAME_SLOTS = 12;
export const P2_BURST_DIBITS = 170;
/** 40-bit S-ISCH / frame sync (op25 P25P2_FRAME_SYNC_MAGIC). */
export const P2_SYNC_HI = 0x575d5;
export const P2_SYNC_LO = 0x7f7ff;
export const P2_SYNC_DIBITS: Uint8Array = (() => {
  const d = new Uint8Array(20);
  const v = (BigInt(P2_SYNC_HI) << 20n) | BigInt(P2_SYNC_LO);
  for (let i = 0; i < 20; i++) d[i] = Number((v >> BigInt(2 * (19 - i))) & 3n);
  return d;
})();

/** Burst types (op25 duid_lookup values). */
export const BURST_4V = 0;
export const BURST_SACCH_S = 3; // scrambled
export const BURST_LCCH_S = 4;
export const BURST_2V = 6;
export const BURST_FACCH_S = 9;
export const BURST_SACCH_U = 12; // unscrambled
export const BURST_LCCH_U = 13;
export const BURST_FACCH_U = 15;

/** Which logical channel (0/1) each of the 12 slots belongs to (op25 which_slot). */
export const SLOT_CHANNEL = [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0];

// ── Scrambler (apps/tdma/lfsr.py) ────────────────────────────────────────────

const xorCache = new Map<string, Uint8Array>();

/**
 * op25 p25p2_lfsr(nac, sysid, wacn).xorsyms: the 2160-dibit (one superframe)
 * XOR mask. Slot s, burst dibit i is scrambled with mask[s·180 + i].
 */
export function p2XorMask(nac: number, sysid: number, wacn: number): Uint8Array {
  const key = `${nac}|${sysid}|${wacn}`;
  const hit = xorCache.get(key);
  if (hit) return hit;
  // reg = mk_array(16777216·wacn + 4096·sysid + nac, 44), MSB first.
  const seed = BigInt(wacn & 0xfffff) * 16777216n + BigInt(sysid & 0xfff) * 4096n + BigInt(nac & 0xfff);
  const v = new Uint8Array(44);
  for (let i = 0; i < 44; i++) v[i] = Number((seed >> BigInt(43 - i)) & 1n);
  // reg = mk_int(v · M) over GF(2).
  const r = new Uint8Array(44);
  for (let j = 0; j < 44; j++) {
    let acc = 0;
    for (let i = 0; i < 44; i++) if (v[i] && LFSR_SEED_MATRIX[i].charCodeAt(j) === 49) acc ^= 1;
    r[j] = acc;
  }
  const field = (from: number, len: number) => {
    let x = 0;
    for (let i = 0; i < len; i++) x = (x << 1) | r[from + i];
    return x;
  };
  // disasm_reg: s1 4 bits, s2 5, s3 6, s4 5, s5 14, s6 10 (MSB first).
  let s1 = field(0, 4);
  let s2 = field(4, 5);
  let s3 = field(9, 6);
  let s4 = field(15, 5);
  let s5 = field(20, 14);
  let s6 = field(34, 10);
  const bits = new Uint8Array(4320);
  for (let i = 0; i < 4320; i++) {
    bits[i] = (s1 >> 3) & 1; // (reg >> 43) & 1
    // cyc_reg
    const cy1 = (s1 >> 3) & 1;
    const cy2 = (s2 >> 4) & 1;
    const cy3 = (s3 >> 5) & 1;
    const cy4 = (s4 >> 4) & 1;
    const cy5 = (s5 >> 13) & 1;
    const cy6 = (s6 >> 9) & 1;
    s1 = ((s1 << 1) & 0xf) | (cy1 ^ cy2);
    s2 = ((s2 << 1) & 0x1f) | (cy1 ^ cy3);
    s3 = ((s3 << 1) & 0x3f) | (cy1 ^ cy4);
    s4 = ((s4 << 1) & 0x1f) | (cy1 ^ cy5);
    s5 = ((s5 << 1) & 0x3fff) | (cy1 ^ cy6);
    s6 = ((s6 << 1) & 0x3ff) | cy1;
  }
  const mask = new Uint8Array(2160);
  for (let i = 0; i < 2160; i++) mask[i] = (bits[2 * i] << 1) | bits[2 * i + 1];
  xorCache.set(key, mask);
  return mask;
}

// ── ISCH / DUID ──────────────────────────────────────────────────────────────

interface Cw40 { hi: number; lo: number; value: number }
const ISCH: Cw40[] = ISCH_CODEWORDS.map(([hex, value]) => {
  const v = BigInt("0x" + hex);
  return { hi: Number(v >> 20n), lo: Number(v & 0xfffffn), value };
});
function pop20(x: number): number {
  let c = 0;
  while (x) {
    x &= x - 1;
    c++;
  }
  return c;
}

/** op25 isch_lookup: 20 dibits → I-ISCH value (0..127), -2 (S-ISCH), or -1. */
export function ischLookup(d: ArrayLike<number>, at = 0): number {
  let hi = 0;
  let lo = 0;
  for (let i = 0; i < 10; i++) hi = (hi << 2) | (d[at + i] & 3);
  for (let i = 10; i < 20; i++) lo = (lo << 2) | (d[at + i] & 3);
  let best = -1;
  let bestDist = 8; // (40, 9, 16) code: corrects ≤ 7
  for (const c of ISCH) {
    const dist = pop20(hi ^ c.hi) + pop20(lo ^ c.lo);
    if (dist === 0) return c.value;
    if (dist < bestDist) {
      bestDist = dist;
      best = c.value;
    }
  }
  return best;
}

/** The 20 ISCH dibits for superframe slot `slot` (I-ISCH or S-ISCH). */
export function ischDibits(slot: number): Uint8Array {
  const chn = slot & 1;
  const loc = slot >> 2;
  const isI = [0, 1, 4, 5, 8, 9].includes(slot);
  const value = isI ? (chn << 5) | (loc << 3) : -2;
  const c = ISCH.find((x) => x.value === value)!;
  const d = new Uint8Array(20);
  for (let i = 0; i < 10; i++) d[i] = (c.hi >> (2 * (9 - i))) & 3;
  for (let i = 0; i < 10; i++) d[10 + i] = (c.lo >> (2 * (9 - i))) & 3;
  return d;
}

/** Burst DUID dibits sit at burst positions 10, 47, 132, 169. */
const DUID_POS = [10, 47, 132, 169];

export function duidDecode(burst: ArrayLike<number>): number {
  const v = ((burst[10] & 3) << 6) | ((burst[47] & 3) << 4) | ((burst[132] & 3) << 2) | (burst[169] & 3);
  return DUID_LOOKUP[v];
}

let duidCodes: Int16Array | null = null;
/** The error-free 8-bit DUID codeword for a burst type: the one whose every
 *  single-bit neighbour still decodes to it (the (8,4) code's centre). */
function duidCodeword(type: number): number {
  if (!duidCodes) {
    duidCodes = new Int16Array(16).fill(-1);
    for (let v = 0; v < 256; v++) {
      const t = DUID_LOOKUP[v];
      if (t < 0) continue;
      let centre = true;
      for (let b = 0; b < 8; b++) if (DUID_LOOKUP[v ^ (1 << b)] !== t) centre = false;
      if (centre) duidCodes[t] = v;
    }
  }
  const c = duidCodes[type];
  if (c < 0) throw new Error(`no DUID codeword for burst type ${type}`);
  return c;
}

// ── AMBE voice codeword (p25p2_vf.cc) ────────────────────────────────────────

// interleave_vcw / extract_vcw: vf bit k ← (which of c0..c3, bit index).
const VCW_MAP: [number, number][] = [
  [0, 23], [0, 5], [1, 10], [2, 3], [0, 22], [0, 4], [1, 9], [2, 2], [0, 21], [0, 3], [1, 8], [2, 1],
  [0, 20], [0, 2], [1, 7], [2, 0], [0, 19], [0, 1], [1, 6], [3, 13], [0, 18], [0, 0], [1, 5], [3, 12],
  [0, 17], [1, 22], [1, 4], [3, 11], [0, 16], [1, 21], [1, 3], [3, 10], [0, 15], [1, 20], [1, 2], [3, 9],
  [0, 14], [1, 19], [1, 1], [3, 8], [0, 13], [1, 18], [1, 0], [3, 7], [0, 12], [1, 17], [2, 10], [3, 6],
  [0, 11], [1, 16], [2, 9], [3, 5], [0, 10], [1, 15], [2, 8], [3, 4], [0, 9], [1, 14], [2, 7], [3, 3],
  [0, 8], [1, 13], [2, 6], [3, 2], [0, 7], [1, 12], [2, 5], [3, 1], [0, 6], [1, 11], [2, 4], [3, 0],
];

/** op25 extract_vcw: 36 dibits → c0 (24 bits), c1 (23), c2 (11), c3 (14). */
export function extractVcw(d: ArrayLike<number>, at = 0): [number, number, number, number] {
  const c = [0, 0, 0, 0];
  for (let k = 0; k < 72; k++) {
    const dib = d[at + (k >> 1)];
    const b = k & 1 ? dib & 1 : (dib >> 1) & 1;
    const [w, i] = VCW_MAP[k];
    if (b) c[w] |= 1 << i;
  }
  return [c[0], c[1], c[2], c[3]];
}

/** op25 interleave_vcw: the inverse of {@link extractVcw}. */
export function interleaveVcw(c: [number, number, number, number]): Uint8Array {
  const out = new Uint8Array(36);
  for (let k = 0; k < 72; k++) {
    const [w, i] = VCW_MAP[k];
    const b = (c[w] >> i) & 1;
    out[k >> 1] |= k & 1 ? b : b << 1;
  }
  return out;
}

/** The AMBE c1 modulator seeded from u0 (p25p2_vf.cc process_vcw; mbelib
 *  mbe_demodulateAmbe3600x2450Data): 23 bits, first PN bit in the MSB. */
export function ambePn23(u0: number): number {
  let pr = 16 * u0;
  let m1 = 0;
  for (let n = 1; n < 24; n++) {
    pr = (173 * pr + 13849) % 65536;
    m1 = (m1 << 1) | ((pr >> 15) & 1);
  }
  return m1;
}

export interface AmbeFrame {
  /** mbelib's ambe_d[49]: u0 (12) u1 (12) u2 (11) u3 (14), MSB first. */
  bits: Uint8Array;
  u: [number, number, number, number];
  /** Bits corrected in c0 + c1 (uncorrectable counts 4). */
  errs: number;
}

/** op25 process_vcw: FEC-decode one 36-dibit voice codeword. */
export function decodeVcw(d: ArrayLike<number>, at = 0): AmbeFrame {
  const [c0, c1, c2, c3] = extractVcw(d, at);
  const r0 = golay24Decode(c0);
  const u0 = r0.data;
  const r1 = golay23Decode(c1 ^ ambePn23(u0));
  const u: [number, number, number, number] = [u0, r1.data, c2, c3];
  const bits = new Uint8Array(49);
  let p = 0;
  for (const [v, w] of [[u[0], 12], [u[1], 12], [u[2], 11], [u[3], 14]] as const) for (let b = w - 1; b >= 0; b--) bits[p++] = (v >> b) & 1;
  return { bits, u, errs: (r0.errs < 0 ? 4 : r0.errs) + r1.errs };
}

/** Inverse of {@link decodeVcw}: u0..u3 → 36 dibits (op25 encode_vcw's FEC). */
export function encodeVcw(u: [number, number, number, number]): Uint8Array {
  const c0 = golay24Encode(u[0] & 0xfff);
  const c1 = golay23Encode(u[1] & 0xfff) ^ ambePn23(u[0] & 0xfff);
  return interleaveVcw([c0, c1, u[2] & 0x7ff, u[3] & 0x3fff]);
}

/** ambe_d[49] → u0..u3. */
export function ambeBitsToU(bits: ArrayLike<number>): [number, number, number, number] {
  const u = [0, 0, 0, 0];
  let p = 0;
  [12, 12, 11, 14].forEach((w, k) => {
    for (let b = 0; b < w; b++) u[k] = (u[k] << 1) | (bits[p++] & 1);
  });
  return u as [number, number, number, number];
}

// ── ACCH / MAC PDUs (p25p2_tdma.cc handle_acch_frame, crc12) ────────────────

const CRC12_POLY = [1, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1];
/** op25 crc12 (over `len` bits, xorout 0xfff). */
export function crc12(bits: ArrayLike<number>, len: number): number {
  const buf = new Uint8Array(len + 12);
  for (let i = 0; i < len; i++) buf[i] = bits[i] & 1;
  for (let i = 0; i < len; i++) if (buf[i]) for (let j = 0; j < 13; j++) buf[i + j] ^= CRC12_POLY[j];
  let crc = 0;
  for (let i = 0; i < 12; i++) crc = (crc << 1) | buf[len + i];
  return crc ^ 0xfff;
}

/** CRC-CCITT over bits as op25's crc16 (LCCH only — unused for voice, kept for parity). */

// Where the ACCH bits live in the (descrambled) burst.
const SACCH_RUNS: [number, number][] = [[11, 36], [48, 84], [133, 36]];
const FACCH_RUNS: [number, number][] = [[11, 36], [48, 31], [100, 32], [133, 36]];

export interface MacPdu {
  opcode: number;
  offset: number;
  bytes: Uint8Array;
  rsErrs: number;
}

/** op25 handle_acch_frame (SACCH/FACCH, not LCCH): RS(63,35) with erasures + CRC-12. */
export function decodeAcch(burst: ArrayLike<number>, fast: boolean): MacPdu | null {
  const bits: number[] = [];
  for (const [s, n] of fast ? FACCH_RUNS : SACCH_RUNS) {
    for (let i = s; i < s + n; i++) bits.push((burst[i] >> 1) & 1, burst[i] & 1);
  }
  const HB = new Uint8Array(63);
  let j = fast ? 9 : 5;
  const len = fast ? 270 : 312;
  const erasures = fast ? [0, 1, 2, 3, 4, 5, 6, 7, 8, 54, 55, 56, 57, 58, 59, 60, 61, 62] : [0, 1, 2, 3, 4, 57, 58, 59, 60, 61, 62];
  for (let i = 0; i < len; i += 6) {
    HB[j++] = (bits[i] << 5) | (bits[i + 1] << 4) | (bits[i + 2] << 3) | (bits[i + 3] << 2) | (bits[i + 4] << 1) | bits[i + 5];
  }
  let rsErrs = rsDecode(HB, 28, erasures);
  if (rsErrs < 0) return null;
  if (rsErrs >= erasures.length) rsErrs -= erasures.length;
  const payload = fast ? 144 : 168;
  j = fast ? 9 : 5;
  for (let i = 0; i < payload; i += 6) {
    for (let b = 0; b < 6; b++) bits[i + b] = (HB[j] >> (5 - b)) & 1;
    j++;
  }
  // op25 reads the CRC from bits[payload..+11] — still the received (uncorrected)
  // bits there, since only `payload` bits were rewritten. Kept as op25 does it.
  let rx = 0;
  for (let i = 0; i < 12; i++) rx = (rx << 1) | bits[payload + i];
  if (rx !== crc12(bits, payload)) return null;
  const bytes = new Uint8Array(payload / 8);
  for (let i = 0; i < bytes.length; i++) {
    let v = 0;
    for (let b = 0; b < 8; b++) v = (v << 1) | bits[i * 8 + b];
    bytes[i] = v;
  }
  return { opcode: (bytes[0] >> 5) & 7, offset: (bytes[0] >> 2) & 7, bytes, rsErrs };
}

/** Inverse of {@link decodeAcch}: a MAC PDU (21 bytes SACCH / 18 FACCH) → the
 *  burst dibits it occupies (other burst positions untouched). */
export function encodeAcch(burst: Uint8Array, pdu: Uint8Array, fast: boolean): void {
  const payload = fast ? 144 : 168;
  const bits = new Array<number>(payload + 12).fill(0);
  for (let i = 0; i < payload; i++) bits[i] = (pdu[i >> 3] >> (7 - (i & 7))) & 1;
  const crc = crc12(bits, payload);
  for (let i = 0; i < 12; i++) bits[payload + i] = (crc >> (11 - i)) & 1;
  const first = fast ? 9 : 5;
  const msg = new Array(35).fill(0);
  for (let h = 0; h < (payload + 12) / 6; h++) {
    let v = 0;
    for (let b = 0; b < 6; b++) v = (v << 1) | bits[h * 6 + b];
    msg[first + h] = v;
  }
  const HB = [...msg, ...rsEncode(msg, 28)];
  const len = fast ? 270 : 312;
  const out: number[] = [];
  for (let h = first; h < first + len / 6; h++) for (let b = 5; b >= 0; b--) out.push((HB[h] >> b) & 1);
  let k = 0;
  for (const [s, n] of fast ? FACCH_RUNS : SACCH_RUNS) {
    for (let i = s; i < s + n; i++) {
      burst[i] = (out[k] << 1) | out[k + 1];
      k += 2;
    }
  }
}

export interface MacPtt {
  algid: number;
  keyid: number;
  mi: string;
  source: number;
  group: number;
}
/** MAC_PTT (opcode 1) fields — op25 handle_mac_ptt. */
export function parseMacPtt(b: Uint8Array): MacPtt {
  return {
    mi: Array.from(b.subarray(1, 10), (x) => x.toString(16).padStart(2, "0")).join(""),
    algid: b[10],
    keyid: (b[11] << 8) | b[12],
    source: (b[13] << 16) | (b[14] << 8) | b[15],
    group: (b[16] << 8) | b[17],
  };
}
export function buildMacPtt(p: { offset?: number; algid?: number; keyid?: number; source: number; group: number }, len = 21): Uint8Array {
  const b = new Uint8Array(len);
  b[0] = (1 << 5) | ((p.offset ?? 0) << 2);
  b[10] = p.algid ?? 0x80;
  b[11] = ((p.keyid ?? 0) >> 8) & 0xff;
  b[12] = (p.keyid ?? 0) & 0xff;
  b[13] = (p.source >> 16) & 0xff;
  b[14] = (p.source >> 8) & 0xff;
  b[15] = p.source & 0xff;
  b[16] = (p.group >> 8) & 0xff;
  b[17] = p.group & 0xff;
  return b;
}
/** MAC_END_PTT (opcode 2) / MAC_IDLE (3) / MAC_ACTIVE (4): header byte only. */
export function buildMacHeader(opcode: number, offset = 0, len = 21): Uint8Array {
  const b = new Uint8Array(len);
  b[0] = ((opcode & 7) << 5) | ((offset & 7) << 2);
  return b;
}

// ── ESS (4V ESS-B + 2V ESS-A, RS(44,16,29)) ──────────────────────────────────

export interface Ess {
  algid: number;
  keyid: number;
  mi: string;
}

/** 12 dibits (4 hexbits) of ESS-B from a 4V burst at 84. */
export function readEssB(x: ArrayLike<number>): number[] {
  const hb: number[] = [];
  for (let i = 0; i < 12; i += 3) hb.push(((x[84 + i] & 3) << 4) | ((x[85 + i] & 3) << 2) | (x[86 + i] & 3));
  return hb;
}
/** 28 hexbits of ESS-A from a 2V burst at 84 (skipping the DUID dibit at 132). */
export function readEssA(x: ArrayLike<number>): number[] {
  const hb: number[] = [];
  let j = 84;
  for (let i = 0; i < 28; i++) {
    hb.push(((x[j] & 3) << 4) | ((x[j + 1] & 3) << 2) | (x[j + 2] & 3));
    j = i === 15 ? j + 4 : j + 3;
  }
  return hb;
}
/** op25 handle_4V2V_ess: rs28.decode(ESS_B, ESS_A) — ≤ 14 corrections. */
export function decodeEss(essB: number[], essA: number[]): Ess | null {
  const cw = new Uint8Array(63);
  for (let i = 0; i < 16; i++) cw[19 + i] = essB[i] & 63;
  for (let i = 0; i < 28; i++) cw[35 + i] = essA[i] & 63;
  const ec = rsDecode(cw, 28, [], 19);
  if (ec < 0 || ec > 14) return null;
  const B = cw.subarray(19, 35);
  const algid = ((B[0] << 2) | (B[1] >> 4)) & 0xff;
  const keyid = ((B[1] & 15) << 12) | (B[2] << 6) | B[3];
  const mi: number[] = [];
  for (let j = 0; mi.length < 9; j += 4) {
    mi.push(((B[j + 4] << 2) | (B[j + 5] >> 4)) & 0xff, (((B[j + 5] & 0x0f) << 4) | (B[j + 6] >> 2)) & 0xff, (((B[j + 6] & 0x03) << 6) | B[j + 7]) & 0xff);
  }
  return { algid, keyid, mi: mi.map((b) => b.toString(16).padStart(2, "0")).join("") };
}
/** Inverse: the 16 ESS-B hexbits + 28 ESS-A hexbits for an ESS. */
export function encodeEss(e: Partial<Ess>): { essB: number[]; essA: number[] } {
  const algid = e.algid ?? 0x80;
  const keyid = e.keyid ?? 0;
  const mi = new Array(9).fill(0);
  if (e.mi) for (let i = 0; i < 9; i++) mi[i] = parseInt(e.mi.slice(2 * i, 2 * i + 2), 16) || 0;
  const bytes = [algid, (keyid >> 8) & 0xff, keyid & 0xff, ...mi]; // 12 bytes = 16 hexbits
  const essB: number[] = [];
  for (let i = 0; i < 12; i += 3) {
    const v = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
    essB.push((v >> 18) & 63, (v >> 12) & 63, (v >> 6) & 63, v & 63);
  }
  const msg = new Array(35).fill(0);
  for (let i = 0; i < 16; i++) msg[19 + i] = essB[i];
  return { essB, essA: [...rsEncode(msg, 28)] };
}

// ── Framer + slot sync (p25p2_framer.cc, p25p2_sync.cc) ─────────────────────

const EXPECTED_SYNC = [0, 1, -2, -2, 4, 5, -2, -2, 8, 9, -2, -2];

export interface P2Packet {
  /** Dibit index (in the caller's stream) where the packet's ISCH starts. */
  at: number;
  /** Superframe slot 0..11. */
  slot: number;
  /** The 180 packet dibits. */
  dibits: Uint8Array;
}

/**
 * Cut a dibit stream into slot packets, the op25 way: a 40-bit S-ISCH (≤ 4
 * bit errors) starts a packet and renews a 10-packet allowance; I-ISCH
 * packets anchor the slot count (p25p2_sync check_confidence). Only packets
 * whose slot is known with confidence are returned.
 */
export function framePackets(d: ArrayLike<number>): P2Packet[] {
  const out: P2Packet[] = [];
  let hi = 0;
  let lo = 0;
  let inSync = 0;
  let pktStart = -1;
  let slotId = 0;
  let confident = false;
  const finish = (start: number) => {
    const pkt = new Uint8Array(P2_SLOT_DIBITS);
    for (let i = 0; i < P2_SLOT_DIBITS; i++) pkt[i] = d[start + i] & 3;
    // check_confidence
    slotId = (slotId + 1) % 12;
    const rc = ischLookup(pkt, 0);
    let checkval = rc;
    let chn = -1;
    if (rc >= 0) {
      chn = (rc >> 5) & 3;
      const loc = (rc >> 3) & 3;
      checkval = loc * 4 + chn;
    }
    if (EXPECTED_SYNC[slotId] !== checkval && checkval !== -1) confident = false;
    if (chn >= 0) {
      confident = true;
      slotId = checkval;
    }
    if (confident) out.push({ at: start, slot: slotId, dibits: pkt });
  };
  for (let n = 0; n < d.length; n++) {
    // 40-bit sliding window of the last 20 dibits.
    hi = ((hi << 2) | (lo >>> 18)) & 0xfffff;
    lo = ((lo << 2) | (d[n] & 3)) & 0xfffff;
    if (n >= 19 && pop20(hi ^ P2_SYNC_HI) + pop20(lo ^ P2_SYNC_LO) <= 4) {
      pktStart = n - 19;
      inSync = 10;
      continue;
    }
    if (inSync && pktStart >= 0 && n - pktStart + 1 >= P2_SLOT_DIBITS) {
      finish(pktStart);
      inSync--;
      pktStart = inSync ? pktStart + P2_SLOT_DIBITS : -1;
    }
  }
  return out;
}

// ── Encoder: a superframe of slots (loopback tests + the sim) ───────────────

export type SlotContent =
  | { kind: "4v"; frames: [number, number, number, number][]; essB: number[] }
  | { kind: "2v"; frames: [number, number, number, number][]; essA: number[] }
  /** A MAC PDU in a SACCH (21 bytes) or, with `fast`, a FACCH (18 bytes). */
  | { kind: "sacch"; pdu: Uint8Array; scrambled?: boolean; fast?: boolean };

/**
 * One 180-dibit slot: ISCH + a (scrambled) burst. `mask` is {@link p2XorMask}.
 */
export function encodeSlot(slot: number, content: SlotContent, mask: Uint8Array): Uint8Array {
  const burst = new Uint8Array(P2_BURST_DIBITS);
  let type: number;
  if (content.kind === "4v" || content.kind === "2v") {
    type = content.kind === "4v" ? BURST_4V : BURST_2V;
    const at = content.kind === "4v" ? [11, 48, 96, 133] : [11, 48];
    content.frames.forEach((u, i) => burst.set(encodeVcw(u), at[i]));
    if (content.kind === "4v") {
      for (let h = 0; h < 4; h++) {
        const v = content.essB[h];
        burst[84 + 3 * h] = (v >> 4) & 3;
        burst[85 + 3 * h] = (v >> 2) & 3;
        burst[86 + 3 * h] = v & 3;
      }
    } else {
      let j = 84;
      for (let i = 0; i < 28; i++) {
        const v = content.essA[i];
        burst[j] = (v >> 4) & 3;
        burst[j + 1] = (v >> 2) & 3;
        burst[j + 2] = v & 3;
        j = i === 15 ? j + 4 : j + 3;
      }
    }
  } else {
    type = content.fast
      ? content.scrambled === false ? BURST_FACCH_U : BURST_FACCH_S
      : content.scrambled === false ? BURST_SACCH_U : BURST_SACCH_S;
    encodeAcch(burst, content.pdu, !!content.fast);
  }
  const code = duidCodeword(type);
  DUID_POS.forEach((p, i) => (burst[p] = (code >> (6 - 2 * i)) & 3));
  const scrambled = type !== BURST_SACCH_U && type !== BURST_FACCH_U;
  const pkt = new Uint8Array(P2_SLOT_DIBITS);
  pkt.set(ischDibits(slot), 0);
  // The burst region starts at packet dibit 10 and overlaps the ISCH's second
  // half (op25 handle_packet: burstp = &dibits[10]); only dibits 20.. carry it.
  // The DUID dibits are never scrambled — op25 reads the burst type from the
  // raw burst to decide whether (and how) to descramble the rest.
  for (let i = 10; i < P2_BURST_DIBITS; i++) {
    const clear = !scrambled || DUID_POS.includes(i);
    pkt[10 + i] = clear ? burst[i] : burst[i] ^ mask[slot * P2_SLOT_DIBITS + i];
  }
  return pkt;
}
