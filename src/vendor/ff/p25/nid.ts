// P25 Network ID (NID) — the 64-bit word after the frame sync. It carries the
// 12-bit NAC (Network Access Code) and 4-bit DUID (Data Unit ID), protected by a
// BCH(63,16) code plus one even-parity bit.
//
// `bchDecode` is a verbatim port of op25's `bch.cc::bchDec` (© 2010 KA1RBI): the
// GF(2^6) syndrome fill, Berlekamp-Massey error-locator, and Chien search. It
// corrects up to 11 symbol errors mathematically; op25 accepts ≤4 for the NID
// (the code's guaranteed distance), and we mirror that. `bchEncode` is a
// systematic encoder over the same generator polynomial (`BCH_G`), used by the
// sim/tests, verified against `bchDecode` (clean codeword → syndrome 0; a 1-bit
// error is corrected) in `web/test/p25.test.ts`.
//
// DUID 0x7 is a TSDU (trunking) — the control-channel data unit this decoder
// cares about.

// GF(2^6) exp/log tables (op25 bch.cc).
const GF_EXP = [
  1, 2, 4, 8, 16, 32, 3, 6, 12, 24, 48, 35, 5, 10, 20, 40,
  19, 38, 15, 30, 60, 59, 53, 41, 17, 34, 7, 14, 28, 56, 51, 37,
  9, 18, 36, 11, 22, 44, 27, 54, 47, 29, 58, 55, 45, 25, 50, 39,
  13, 26, 52, 43, 21, 42, 23, 46, 31, 62, 63, 61, 57, 49, 33, 0,
];
const GF_LOG = [
  -1, 0, 1, 6, 2, 12, 7, 26, 3, 32, 13, 35, 8, 48, 27, 18,
  4, 24, 33, 16, 14, 52, 36, 54, 9, 45, 49, 38, 28, 41, 19, 56,
  5, 62, 25, 11, 34, 31, 17, 47, 15, 23, 53, 51, 37, 44, 55, 40,
  10, 61, 46, 30, 50, 22, 39, 43, 29, 60, 42, 21, 20, 59, 57, 58,
];
// Generator polynomial coefficients g_0..g_47 (degree 47), op25 bch.cc `bchG`.
const BCH_G = [
  1, 1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1, 0, 0,
  1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 0, 1, 0, 0, 0, 0,
  1, 1, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 0, 1, 1,
];

/** Port of op25 `bchDec`. `cw` is a length-64 bit array (bits 0..62 the BCH
 *  codeword, bit 63 ignored), corrected in place. Returns the number of
 *  corrected errors, or a negative value if it cannot decode. */
export function bchDecode(cw: Uint8Array | number[]): number {
  const elp = Array.from({ length: 24 }, () => new Array<number>(22).fill(0));
  const S = new Array<number>(23).fill(0);
  const D = new Array<number>(23).fill(0);
  const L = new Array<number>(24).fill(0);
  const uLu = new Array<number>(24).fill(0);
  const locn = new Array<number>(11).fill(0);
  const reg = new Array<number>(12).fill(0);
  let i: number, j: number, U: number, q: number, count: number;
  let synError = 0;
  let cantDecode = 0;

  for (i = 1; i <= 22; i++) {
    S[i] = 0;
    for (j = 0; j <= 62; j++) if (cw[j]) S[i] ^= GF_EXP[(i * j) % 63];
    if (S[i]) synError = 1;
    S[i] = GF_LOG[S[i]];
  }

  if (synError) {
    L[0] = 0; uLu[0] = -1; D[0] = 0; elp[0][0] = 0;
    L[1] = 0; uLu[1] = 0; D[1] = S[1]; elp[1][0] = 1;
    for (i = 1; i <= 21; i++) { elp[0][i] = -1; elp[1][i] = 0; }
    U = 0;
    do {
      U = U + 1;
      if (D[U] === -1) {
        L[U + 1] = L[U];
        for (i = 0; i <= L[U]; i++) { elp[U + 1][i] = elp[U][i]; elp[U][i] = GF_LOG[elp[U][i]]; }
      } else {
        q = U - 1;
        while (D[q] === -1 && q > 0) q = q - 1;
        if (q > 0) {
          j = q;
          do { j = j - 1; if (D[j] !== -1 && uLu[q] < uLu[j]) q = j; } while (j > 0);
        }
        if (L[U] > L[q] + U - q) L[U + 1] = L[U]; else L[U + 1] = L[q] + U - q;
        for (i = 0; i <= 21; i++) elp[U + 1][i] = 0;
        for (i = 0; i <= L[q]; i++) if (elp[q][i] !== -1) elp[U + 1][i + U - q] = GF_EXP[(D[U] + 63 - D[q] + elp[q][i]) % 63];
        for (i = 0; i <= L[U]; i++) { elp[U + 1][i] ^= elp[U][i]; elp[U][i] = GF_LOG[elp[U][i]]; }
      }
      uLu[U + 1] = U - L[U + 1];
      if (U < 22) {
        if (S[U + 1] !== -1) D[U + 1] = GF_EXP[S[U + 1]]; else D[U + 1] = 0;
        for (i = 1; i <= L[U + 1]; i++) if (S[U + 1 - i] !== -1 && elp[U + 1][i] !== 0) D[U + 1] ^= GF_EXP[(S[U + 1 - i] + GF_LOG[elp[U + 1][i]]) % 63];
        D[U + 1] = GF_LOG[D[U + 1]];
      }
    } while (U < 22 && L[U + 1] <= 11);
    U = U + 1;
    if (L[U] <= 11) {
      for (i = 0; i <= L[U]; i++) elp[U][i] = GF_LOG[elp[U][i]];
      for (i = 1; i <= L[U]; i++) reg[i] = elp[U][i];
      count = 0;
      for (i = 1; i <= 63; i++) {
        q = 1;
        for (j = 1; j <= L[U]; j++) if (reg[j] !== -1) { reg[j] = (reg[j] + j) % 63; q ^= GF_EXP[reg[j]]; }
        if (q === 0) { locn[count] = 63 - i; count = count + 1; }
      }
      if (count === L[U]) { for (i = 0; i <= L[U] - 1; i++) cw[locn[i]] ^= 1; cantDecode = count; }
      else cantDecode = -1;
    } else {
      cantDecode = -2;
    }
  }
  return cantDecode;
}

/** Systematic BCH(63,16) encode of a 16-bit value → a length-63 bit array
 *  (bit k = coefficient of x^k) consistent with {@link bchDecode}. The message
 *  occupies the 16 high positions (x^62..x^47); parity fills x^46..x^0. */
export function bchEncode(value16: number): Uint8Array {
  const c = new Uint8Array(63);
  for (let k = 0; k < 16; k++) c[62 - k] = (value16 >> (15 - k)) & 1; // c[62]=MSB
  // remainder = (message at high powers) mod g(x)
  for (let pos = 62; pos >= 47; pos--) {
    if (c[pos]) {
      const shift = pos - 47;
      for (let k = 0; k <= 47; k++) c[k + shift] ^= BCH_G[k];
    }
  }
  for (let k = 0; k < 16; k++) c[62 - k] = (value16 >> (15 - k)) & 1; // restore message
  return c;
}

export interface NidResult {
  nac: number;
  duid: number;
  parity: number;
  errors: number; // BCH errors corrected
}

/**
 * Decode a 64-bit NID word (as a bigint, MSB = bit 63). Runs BCH correction and
 * returns {nac, duid} — or null if the BCH decode fails or the DUID/parity
 * validity check (TIA-102-BAAC) does not hold, exactly as op25's
 * `p25_framer::nid_codeword`.
 */
export function decodeNid(acc: bigint): NidResult | null {
  const parity = Number(acc & 1n);
  // Split into the LSB-first codeword bchDecode expects (cw[i] = bit i of acc>>1).
  let a = acc >> 1n;
  const cw = new Uint8Array(64);
  for (let i = 0; i <= 63; i++) { cw[i] = Number(a & 1n); a >>= 1n; }
  const ec = bchDecode(cw);
  if (ec < 0 || ec > 4) return null; // op25: reject beyond guaranteed distance

  // Reassemble the corrected word (MSB first) and re-attach the parity LSB.
  let word = 0n;
  for (let i = 63; i >= 0; i--) { word |= BigInt(cw[i]); word <<= 1n; }
  word |= BigInt(parity);

  if (word >> 1n === 0n) return null; // drop empty NIDs

  const nac = Number((word >> 52n) & 0xfffn);
  const duid = Number((word >> 48n) & 0xfn);

  // DUID/parity validity (TIA-102-BAAC): these DUIDs require parity 0, LDU1/LDU2
  // require parity 1.
  if ((duid === 0 || duid === 3 || duid === 7 || duid === 12 || duid === 15) && parity) return null;
  if ((duid === 5 || duid === 10) && !parity) return null;

  return { nac, duid, parity, errors: ec };
}

/** Build a 64-bit NID word (bigint) for {nac, duid}. Inverse of {@link decodeNid}
 *  for the sim/tests: BCH(63,16)-encodes (nac<<4|duid) and sets the even-parity
 *  LSB the way {@link decodeNid} validates. */
export function encodeNid(nac: number, duid: number): bigint {
  const value16 = ((nac & 0xfff) << 4) | (duid & 0xf);
  const cw = bchEncode(value16); // c[k] coeff of x^k, 63 bits
  // Reassemble as a 63-bit word MSB-first: bit i of (word>>1) == cw[i].
  let word = 0n;
  for (let i = 0; i <= 62; i++) if (cw[i]) word |= 1n << BigInt(i);
  word <<= 1n; // make room for the parity LSB
  // Parity: DUIDs {0,3,7,12,15} → 0; {5,10} → 1 (matches decodeNid's check).
  const parity = duid === 5 || duid === 10 ? 1n : 0n;
  word |= parity;
  return word & 0xffffffffffffffffn;
}
