// P25 Phase 1 voice framing — the layers between a frame sync and the vocoder.
//
//   HDU   (DUID 0x0, 792 bits)  header: encryption sync (MI/ALGID/KID) + TGID
//   LDU1  (DUID 0x5, 1728 bits) 9 IMBE codewords + link control (TGID, source)
//   LDU2  (DUID 0xA, 1728 bits) 9 IMBE codewords + encryption sync
//   TDU   (DUID 0x3, 144 bits)  terminator
//   TDULC (DUID 0xF, 432 bits)  terminator with link control
//
// A "frame body" here is op25's: the frame's bits counted from the first bit of
// the 48-bit sync with the status symbols LEFT IN PLACE (every 36th dibit). All
// bit positions below are op25's tables (voiceTables.ts), so this reads exactly
// like p25p1_fdma.cc's process_HDU / process_LLDU / process_LDU2 / process_TDU15
// / process_LCW and op25_imbe_frame.h's imbe_header_decode — ported verbatim,
// with an encoder beside each decoder (op25's imbe_header_encode, plus the
// inverses of the LC/ES paths) for the loopback tests and the sim.

import { SYNC_DIBITS } from "./c4fm.ts";
import { encodeNid } from "./nid.ts";
import {
  golay23Decode,
  golay23Encode,
  golay24Decode,
  golay24Encode,
  hamming1063Decode,
  hamming1063Parity,
  hamming15Decode,
  hamming15Encode,
  rsDecode,
  rsEncode,
} from "./fec.ts";
import { HDU_CODEWORD_BITS, LDU_LS_DATA_BITS, VOICE_CODEWORD_BITS } from "./voiceTables.ts";

export const DUID_HDU = 0x0;
export const DUID_TDU = 0x3;
export const DUID_LDU1 = 0x5;
export const DUID_LDU2 = 0xa;
export const DUID_TDULC = 0xf;

/** Frame lengths in bits (status symbols included) — op25 p25_framer.cc. */
export const P1_FRAME_BITS: Record<number, number> = {
  [DUID_HDU]: 792,
  [DUID_TDU]: 144,
  [DUID_LDU1]: 1728,
  [DUID_LDU2]: 1728,
  [DUID_TDULC]: 432,
};

/** ALGID 0x80 = unencrypted (TIA-102.AAAD). Anything else is a cipher. */
export const ALGID_CLEAR = 0x80;

/** Common P25 algorithm IDs, for display only. */
export function algName(algid: number): string {
  switch (algid) {
    case -1: return "encrypted (cipher not yet known)";
    case 0x80: return "clear";
    case 0x81: return "DES-OFB";
    case 0x83: return "3DES";
    case 0x84: return "AES-256";
    case 0x85: return "AES-128";
    case 0x9f: return "DES-XL";
    case 0xa0: return "DVI-XL";
    case 0xa1: return "DVP-XL";
    case 0xaa: return "ADP (RC4)";
    default: return `ALGID 0x${algid.toString(16).padStart(2, "0")}`;
  }
}

// ── IMBE codeword FEC (op25_imbe_frame.h) ────────────────────────────────────

/** op25 pngen23 / pngen15: the IMBE PN modulator, seeded from u0. */
function pngen(state: { pr: number }, nbits: number): number {
  let n = 0;
  for (let i = nbits - 1; i >= 0; --i) {
    state.pr = (173 * state.pr + 13849) & 0xffff;
    if (state.pr & 32768) n += 1 << i;
  }
  return n;
}

function extract(cw: ArrayLike<number>, begin: number, end: number): number {
  let v = 0;
  for (let i = begin; i < end; i++) v = v * 2 + (cw[i] & 1);
  return v;
}
function store(cw: Uint8Array, begin: number, end: number, v: number): void {
  for (let i = end - 1; i >= begin; i--) {
    cw[i] = v & 1;
    v = Math.floor(v / 2);
  }
}

export interface ImbeParams {
  /** u0..u7: 12,12,12,12,11,11,11 bits and u7 (7 bits, stored <<1 as in op25). */
  u: number[];
  /** Bits corrected across the Golay/Hamming words (uncorrectable words count 4). */
  errs: number;
  /** Errors in u0 alone (op25's E0) — u0 carries the pitch and seeds the PN. */
  e0: number;
}

/** op25 imbe_header_decode: a 144-bit IMBE codeword → the 88 information bits. */
export function imbeHeaderDecode(cw: ArrayLike<number>): ImbeParams {
  const u = new Array<number>(8);
  const c = (r: { data: number; errs: number }) => (r.errs < 0 ? 4 : r.errs);
  const r0 = golay23Decode(extract(cw, 0, 23));
  u[0] = r0.data;
  const e0 = r0.errs;
  let errs = e0;
  const pn = { pr: u[0] << 4 };
  for (let k = 1; k <= 3; k++) {
    const m = pngen(pn, 23);
    const r = golay23Decode(extract(cw, 23 * k, 23 * (k + 1)) ^ m);
    u[k] = r.data;
    errs += c(r);
  }
  for (let k = 4; k <= 6; k++) {
    const m = pngen(pn, 15);
    const s = 92 + (k - 4) * 15;
    const r = hamming15Decode(extract(cw, s, s + 15) ^ m);
    u[k] = r.data;
    errs += c(r);
  }
  u[7] = extract(cw, 137, 144) << 1;
  return { u, errs, e0 };
}

/** op25 imbe_header_encode: 88 information bits → a 144-bit IMBE codeword. */
export function imbeHeaderEncode(u: ArrayLike<number>): Uint8Array {
  const cw = new Uint8Array(144);
  const pn = { pr: (u[0] & 0xfff) << 4 };
  store(cw, 0, 23, golay23Encode(u[0]));
  for (let k = 1; k <= 3; k++) store(cw, 23 * k, 23 * (k + 1), golay23Encode(u[k]) ^ pngen(pn, 23));
  for (let k = 4; k <= 6; k++) {
    const s = 92 + (k - 4) * 15;
    store(cw, s, s + 15, hamming15Encode(u[k]) ^ pngen(pn, 15));
  }
  store(cw, 137, 144, (u[7] >> 1) & 0x7f);
  return cw;
}

/** u0..u7 → mbelib's imbe_d[88] (the order mbe_eccImbe7200x4400Data produces). */
export function imbeParamsToBits(u: ArrayLike<number>): Uint8Array {
  const d = new Uint8Array(88);
  const widths = [12, 12, 12, 12, 11, 11, 11, 7];
  let p = 0;
  for (let k = 0; k < 8; k++) {
    const v = k === 7 ? u[7] >> 1 : u[k];
    for (let b = widths[k] - 1; b >= 0; b--) d[p++] = (v >> b) & 1;
  }
  return d;
}

/** Inverse of {@link imbeParamsToBits}. */
export function imbeBitsToParams(d: ArrayLike<number>): number[] {
  const widths = [12, 12, 12, 12, 11, 11, 11, 7];
  const u: number[] = [];
  let p = 0;
  for (let k = 0; k < 8; k++) {
    let v = 0;
    for (let b = 0; b < widths[k]; b++) v = (v << 1) | (d[p++] & 1);
    u.push(k === 7 ? v << 1 : v);
  }
  return u;
}

// ── Link control and encryption sync ─────────────────────────────────────────

export interface LinkControl {
  /** Link control opcode (0 = group voice, 3 = unit-to-unit). */
  lco: number;
  /** Protect flag: the LC itself is encrypted, so its fields are meaningless. */
  protected: boolean;
  mfid: number;
  /** Service options (group voice): 0x80 emergency, 0x40 encrypted. */
  svcOpts: number | null;
  tgid: number | null;
  /** Unit-to-unit destination. */
  target: number | null;
  source: number | null;
  /** The 9 LC bytes, for anything not interpreted above. */
  raw: number[];
}

export interface EncryptionSync {
  algid: number;
  keyid: number;
  /** 72-bit message indicator, hex. */
  mi: string;
}

/** op25 process_LCW (p25p1_fdma.cc) + the field layouts op25's tk_p25.py reads. */
export function parseLcw(lcw: number[]): LinkControl {
  const pf = (lcw[0] & 0x80) !== 0;
  const lco = lcw[0] & 0x3f;
  const lc: LinkControl = { lco, protected: pf, mfid: lcw[1], svcOpts: null, tgid: null, target: null, source: null, raw: lcw };
  if (pf) return lc;
  if (lco === 0x00) {
    lc.svcOpts = lcw[2];
    lc.tgid = (lcw[4] << 8) | lcw[5];
    lc.source = (lcw[6] << 16) | (lcw[7] << 8) | lcw[8];
  } else if (lco === 0x03) {
    lc.svcOpts = lcw[2];
    lc.target = (lcw[3] << 16) | (lcw[4] << 8) | lcw[5];
    lc.source = (lcw[6] << 16) | (lcw[7] << 8) | lcw[8];
  }
  return lc;
}

/** 12 hexbits → 9 bytes (op25's repeated `(HB[j]<<2)+(HB[j+1]>>4)…` idiom). */
function hexbitsToBytes(hb: ArrayLike<number>, from: number, nBytes: number): number[] {
  const out: number[] = [];
  let j = from;
  while (out.length < nBytes) {
    out.push(((hb[j] << 2) | (hb[j + 1] >> 4)) & 0xff);
    if (out.length < nBytes) out.push((((hb[j + 1] & 0x0f) << 4) | (hb[j + 2] >> 2)) & 0xff);
    if (out.length < nBytes) out.push((((hb[j + 2] & 0x03) << 6) | hb[j + 3]) & 0xff);
    j += 4;
  }
  return out;
}
function bytesToHexbits(bytes: number[]): number[] {
  const hb: number[] = [];
  for (let i = 0; i < bytes.length; i += 3) {
    const v = (bytes[i] << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
    hb.push((v >> 18) & 63, (v >> 12) & 63, (v >> 6) & 63, v & 63);
  }
  return hb;
}
const miHex = (mi: number[]) => mi.map((b) => b.toString(16).padStart(2, "0")).join("");

const bit = (fb: ArrayLike<number>, k: number) => fb[k] & 1;

/** LDU1/LDU2 low-speed-data hexbits: 24 Hamming(10,6) words (op25 process_LLDU). */
function lduHexbits(fb: ArrayLike<number>): Uint8Array {
  const HB = new Uint8Array(63);
  let k = 0;
  for (let i = 0; i < 24; i++) {
    let cw = 0;
    for (let j = 0; j < 10; j++) cw = (cw << 1) | bit(fb, LDU_LS_DATA_BITS[k++]);
    HB[39 + i] = hamming1063Decode(cw >> 4, cw & 0x0f);
  }
  return HB;
}

/** LDU1 link control: RS(24,12,13), ≤ 6 corrections (op25 process_LCW). */
export function decodeLdu1Lc(fb: ArrayLike<number>): LinkControl | null {
  const HB = lduHexbits(fb);
  const ec = rsDecode(HB, 12, [], 39);
  if (ec < 0 || ec > 6) return null;
  return parseLcw(hexbitsToBytes(HB, 39, 9));
}

/** LDU2 encryption sync: RS(24,16,9), ≤ 4 corrections (op25 process_LDU2). */
export function decodeLdu2Es(fb: ArrayLike<number>): EncryptionSync | null {
  const HB = lduHexbits(fb);
  const ec = rsDecode(HB, 8, [], 39);
  if (ec < 0 || ec > 4) return null;
  const mi = hexbitsToBytes(HB, 39, 9);
  const j = 51;
  const algid = ((HB[j] << 2) | (HB[j + 1] >> 4)) & 0xff;
  const keyid = ((HB[j + 1] & 0x0f) << 12) | (HB[j + 2] << 6) | HB[j + 3];
  return { algid, keyid, mi: miHex(mi) };
}

export interface HduInfo extends EncryptionSync {
  mfid: number;
  tgid: number;
}

/** HDU: 36 Golay(18,6) hexbits → RS(36,20,17), ≤ 8 corrections (op25 process_HDU). */
export function decodeHdu(fb: ArrayLike<number>): HduInfo | null {
  const HB = new Uint8Array(63);
  let k = 0;
  for (let i = 0; i < 36; i++) {
    let cw = 0;
    for (let j = 0; j < 18; j++) cw = (cw << 1) | bit(fb, HDU_CODEWORD_BITS[k++]);
    HB[27 + i] = golay24Decode(cw).data & 63;
  }
  const ec = rsDecode(HB, 16, [], 27);
  if (ec < 0 || ec > 8) return null;
  const mi = hexbitsToBytes(HB, 27, 9);
  const j = 39;
  return {
    mi: miHex(mi),
    mfid: ((HB[j] << 2) | (HB[j + 1] >> 4)) & 0xff,
    algid: (((HB[j + 1] & 0x0f) << 4) | (HB[j + 2] >> 2)) & 0xff,
    keyid: ((HB[j + 2] & 0x03) << 14) | (HB[j + 3] << 8) | (HB[j + 4] << 2) | (HB[j + 5] >> 4),
    tgid: ((HB[j + 5] & 0x0f) << 12) | (HB[j + 6] << 6) | HB[j + 7],
  };
}

/** TDULC: 12 Golay(24,12) words → LC (op25 process_TDU15). */
export function decodeTdulc(fb: ArrayLike<number>): LinkControl | null {
  const HB = new Uint8Array(63);
  let k = 0;
  for (let i = 0; i <= 22; i += 2) {
    let cw = 0;
    for (let j = 0; j < 12; j++) {
      cw = (cw << 1) | bit(fb, HDU_CODEWORD_BITS[k++]);
      cw = (cw << 1) | bit(fb, HDU_CODEWORD_BITS[k++]);
    }
    const d = golay24Decode(cw).data;
    HB[39 + i] = d >> 6;
    HB[40 + i] = d & 63;
  }
  const ec = rsDecode(HB, 12, [], 39);
  if (ec < 0 || ec > 6) return null;
  return parseLcw(hexbitsToBytes(HB, 39, 9));
}

/** The last frame-body bit IMBE codeword `f` (0..8) of an LDU occupies. */
export function lduCodewordEndBit(f: number): number {
  let m = 0;
  for (let j = 0; j < 144; j++) m = Math.max(m, VOICE_CODEWORD_BITS[f * 144 + j]);
  return m;
}

/** The nine IMBE codewords of an LDU (op25 imbe_deinterleave). */
export function lduCodewords(fb: ArrayLike<number>): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (let f = 0; f < 9; f++) {
    const cw = new Uint8Array(144);
    for (let j = 0; j < 144; j++) cw[j] = bit(fb, VOICE_CODEWORD_BITS[f * 144 + j]);
    out.push(cw);
  }
  return out;
}

// ── Encoders (loopback tests + the simulated voice channel) ──────────────────

/** Frame-body positions that are NOT status symbols, in order. */
function dataPositions(frameBits: number): number[] {
  const p: number[] = [];
  for (let k = 0; k < frameBits; k++) if (((k >> 1) + 1) % 36 !== 0) p.push(k);
  return p;
}

/** Sync + BCH-coded NID written into a fresh frame body; status dibits 0b01. */
function frameWithHeader(nac: number, duid: number): Uint8Array {
  const n = P1_FRAME_BITS[duid];
  const fb = new Uint8Array(n);
  for (let k = 0; k < n; k++) if (((k >> 1) + 1) % 36 === 0) fb[k] = k & 1; // status dibit 01
  const pos = dataPositions(n);
  let i = 0;
  for (const d of SYNC_DIBITS) {
    fb[pos[i++]] = (d >> 1) & 1;
    fb[pos[i++]] = d & 1;
  }
  const nid = encodeNid(nac, duid);
  for (let b = 63; b >= 0; b--) fb[pos[i++]] = Number((nid >> BigInt(b)) & 1n);
  return fb;
}

function writeLduHexbits(fb: Uint8Array, dataHexbits: number[], nroots: number): void {
  const msg = new Array(63 - nroots).fill(0);
  for (let i = 0; i < dataHexbits.length; i++) msg[39 + i] = dataHexbits[i];
  const par = rsEncode(msg, nroots);
  const hb = [...dataHexbits, ...par]; // 24 hexbits
  let k = 0;
  for (let i = 0; i < 24; i++) {
    const cw = (hb[i] << 4) | hamming1063Parity(hb[i]);
    for (let j = 9; j >= 0; j--) fb[LDU_LS_DATA_BITS[k++]] = (cw >> j) & 1;
  }
}

export interface LcSpec {
  lco?: number;
  mfid?: number;
  svcOpts?: number;
  tgid?: number;
  source?: number;
}
export function lcBytes(lc: LcSpec): number[] {
  const lco = lc.lco ?? 0;
  const tg = lc.tgid ?? 0;
  const src = lc.source ?? 0;
  return [lco & 0x3f, lc.mfid ?? 0, lc.svcOpts ?? 0, 0, (tg >> 8) & 0xff, tg & 0xff, (src >> 16) & 0xff, (src >> 8) & 0xff, src & 0xff];
}

function miBytes(mi: string | undefined): number[] {
  const out = new Array(9).fill(0);
  if (mi) for (let i = 0; i < 9; i++) out[i] = parseInt(mi.slice(2 * i, 2 * i + 2), 16) || 0;
  return out;
}

/** An LDU frame body. `imbe` is nine u0..u7 vectors (op25 layout). */
export function encodeLdu(
  nac: number,
  kind: "ldu1" | "ldu2",
  imbe: ArrayLike<number>[],
  meta: { lc?: LcSpec; es?: Partial<EncryptionSync> },
): Uint8Array {
  const fb = frameWithHeader(nac, kind === "ldu1" ? DUID_LDU1 : DUID_LDU2);
  for (let f = 0; f < 9; f++) {
    const cw = imbeHeaderEncode(imbe[f]);
    for (let j = 0; j < 144; j++) fb[VOICE_CODEWORD_BITS[f * 144 + j]] = cw[j];
  }
  if (kind === "ldu1") {
    writeLduHexbits(fb, bytesToHexbits(lcBytes(meta.lc ?? {})), 12);
  } else {
    const es = meta.es ?? {};
    const algid = es.algid ?? ALGID_CLEAR;
    const keyid = es.keyid ?? 0;
    const hb = bytesToHexbits(miBytes(es.mi)); // 12 hexbits
    // algid (8) + keyid (16) = 24 bits = 4 hexbits, op25's layout at HB[51..54].
    const v = (algid << 16) | keyid;
    hb.push((v >> 18) & 63, (v >> 12) & 63, (v >> 6) & 63, v & 63);
    writeLduHexbits(fb, hb, 8);
  }
  return fb;
}

/** An HDU frame body. */
export function encodeHdu(nac: number, h: { tgid?: number; algid?: number; keyid?: number; mfid?: number; mi?: string }): Uint8Array {
  const fb = frameWithHeader(nac, DUID_HDU);
  const hb = bytesToHexbits(miBytes(h.mi)); // 12
  // mfid 8, algid 8, keyid 16, tgid 16 = 48 bits = 8 hexbits.
  const hi = ((h.mfid ?? 0) << 16) | ((h.algid ?? ALGID_CLEAR) << 8) | (((h.keyid ?? 0) >> 8) & 0xff);
  const lo = (((h.keyid ?? 0) & 0xff) << 16) | (h.tgid ?? 0);
  hb.push((hi >> 18) & 63, (hi >> 12) & 63, (hi >> 6) & 63, hi & 63, (lo >> 18) & 63, (lo >> 12) & 63, (lo >> 6) & 63, lo & 63);
  const msg = new Array(47).fill(0);
  for (let i = 0; i < 20; i++) msg[27 + i] = hb[i];
  const all = [...hb, ...rsEncode(msg, 16)]; // 36 hexbits
  let k = 0;
  for (let i = 0; i < 36; i++) {
    const cw = golay24Encode(all[i]) & 0x3ffff; // (18,6): top 6 data bits zero
    for (let j = 17; j >= 0; j--) fb[HDU_CODEWORD_BITS[k++]] = (cw >> j) & 1;
  }
  return fb;
}

/** A TDU (plain terminator) frame body. */
export function encodeTdu(nac: number): Uint8Array {
  return frameWithHeader(nac, DUID_TDU);
}

/** A TDULC frame body. */
export function encodeTdulc(nac: number, lc: LcSpec): Uint8Array {
  const fb = frameWithHeader(nac, DUID_TDULC);
  const data = bytesToHexbits(lcBytes(lc));
  const msg = new Array(51).fill(0);
  for (let i = 0; i < 12; i++) msg[39 + i] = data[i];
  const hb = [...data, ...rsEncode(msg, 12)]; // 24 hexbits
  let k = 0;
  for (let i = 0; i <= 22; i += 2) {
    const cw = golay24Encode((hb[i] << 6) | hb[i + 1]);
    for (let j = 11; j >= 0; j--) {
      fb[HDU_CODEWORD_BITS[k++]] = (cw >> (2 * j + 1)) & 1;
      fb[HDU_CODEWORD_BITS[k++]] = (cw >> (2 * j)) & 1;
    }
  }
  return fb;
}

/** Frame body bits → dibits (the on-air symbol stream). */
export function frameBodyToDibits(fb: ArrayLike<number>): Uint8Array {
  const d = new Uint8Array(fb.length >> 1);
  for (let i = 0; i < d.length; i++) d[i] = ((fb[2 * i] & 1) << 1) | (fb[2 * i + 1] & 1);
  return d;
}
