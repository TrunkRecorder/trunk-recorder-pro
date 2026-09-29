// P25 1/2-rate trellis code + block de-interleave, ported verbatim from op25's
// `p25p1_fdma.cc::block_deinterleave` (itself from wireshark's packet-p25cai.c,
// © 2008 Michael Ossmann). A TSBK travels as 196 bits over the air; those bits
// are de-interleaved and trellis-decoded down to a 12-byte (96-bit) block.
//
// The decoder is the exact op25 algorithm: a greedy minimum-Hamming-distance walk
// of the 4-state transition table (`NEXT_WORDS`). It is not a full Viterbi — like
// op25 it corrects many isolated errors and leans on the TSBK CRC-16 to reject
// what it can't. `encode` is the exact inverse, used by the sim/tests.
//
// Verified in `web/test/p25.test.ts`: encode → decode round-trips a random block
// byte-for-byte (BER 0).

// De-interleave table: 196 entries, indexes into the 196-bit block, grouped in
// fours (one 4-bit codeword per output dibit; 49 dibits, last one a flush).
const DEINTERLEAVE_TB: number[] = [
  0,  1,  2,  3,  52, 53, 54, 55, 100,101,102,103, 148,149,150,151,
  4,  5,  6,  7,  56, 57, 58, 59, 104,105,106,107, 152,153,154,155,
  8,  9, 10, 11,  60, 61, 62, 63, 108,109,110,111, 156,157,158,159,
  12, 13, 14, 15,  64, 65, 66, 67, 112,113,114,115, 160,161,162,163,
  16, 17, 18, 19,  68, 69, 70, 71, 116,117,118,119, 164,165,166,167,
  20, 21, 22, 23,  72, 73, 74, 75, 120,121,122,123, 168,169,170,171,
  24, 25, 26, 27,  76, 77, 78, 79, 124,125,126,127, 172,173,174,175,
  28, 29, 30, 31,  80, 81, 82, 83, 128,129,130,131, 176,177,178,179,
  32, 33, 34, 35,  84, 85, 86, 87, 132,133,134,135, 180,181,182,183,
  36, 37, 38, 39,  88, 89, 90, 91, 136,137,138,139, 184,185,186,187,
  40, 41, 42, 43,  92, 93, 94, 95, 140,141,142,143, 188,189,190,191,
  44, 45, 46, 47,  96, 97, 98, 99, 144,145,146,147, 192,193,194,195,
  48, 49, 50, 51,
];

// State-transition table: NEXT_WORDS[state][dibit] = the 4-bit codeword emitted.
const NEXT_WORDS: number[][] = [
  [0x2, 0xc, 0x1, 0xf],
  [0xe, 0x0, 0xd, 0x3],
  [0x9, 0x7, 0xa, 0x4],
  [0x5, 0xb, 0x6, 0x8],
];

function countBits(n: number): number {
  let i = 0;
  for (; n !== 0; i++) n &= n - 1;
  return i;
}

/** Index of the unique minimum in a 4-element list, or -1 if not unique. */
function findMin(list: number[]): number {
  let min = list[0];
  let index = 0;
  let unique = true;
  for (let i = 1; i < list.length; i++) {
    if (list[i] < min) {
      min = list[i];
      index = i;
      unique = true;
    } else if (list[i] === min) {
      unique = false;
    }
  }
  return unique ? index : -1;
}

/** De-interleave + trellis-decode 196 bits → 12 bytes. Returns null on an
 *  unresolvable state (op25 returns -1). `bits` is read starting at `start`. */
export function trellisDecode(bits: ArrayLike<number>, start = 0): Uint8Array | null {
  const buf = new Uint8Array(12);
  let state = 0;
  for (let b = 0; b < 98 * 2; b += 4) {
    const codeword =
      (bits[start + DEINTERLEAVE_TB[b + 0]] << 3) +
      (bits[start + DEINTERLEAVE_TB[b + 1]] << 2) +
      (bits[start + DEINTERLEAVE_TB[b + 2]] << 1) +
      bits[start + DEINTERLEAVE_TB[b + 3]];
    const hd = [0, 0, 0, 0];
    for (let j = 0; j < 4; j++) hd[j] = countBits(codeword ^ NEXT_WORDS[state][j]);
    state = findMin(hd);
    if (state === -1) return null;
    const d = b >> 2;
    if (d < 48) buf[d >> 2] |= state << (6 - (d % 4) * 2);
  }
  return buf;
}

/** Inverse of {@link trellisDecode}: 12 bytes → 196 interleaved bits. */
export function trellisEncode(block: Uint8Array): Uint8Array {
  const bits = new Uint8Array(196);
  let state = 0;
  for (let d = 0; d <= 48; d++) {
    const dibit = d < 48 ? (block[d >> 2] >> (6 - (d % 4) * 2)) & 3 : 0; // dibit[48] = flush 0
    const codeword = NEXT_WORDS[state][dibit];
    state = dibit;
    const b = d * 4;
    bits[DEINTERLEAVE_TB[b + 0]] = (codeword >> 3) & 1;
    bits[DEINTERLEAVE_TB[b + 1]] = (codeword >> 2) & 1;
    bits[DEINTERLEAVE_TB[b + 2]] = (codeword >> 1) & 1;
    bits[DEINTERLEAVE_TB[b + 3]] = codeword & 1;
  }
  return bits;
}
