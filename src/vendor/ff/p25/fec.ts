// Forward error correction for the P25 voice layers.
//
//   · Golay(24,12,8) and its punctured Golay(23,12,7) — IMBE u0..u3 (Phase 1),
//     AMBE c0/c1 (Phase 2), the HDU/TDULC link-control hexbits.
//   · Hamming(15,11,3) — IMBE u4..u6.
//   · Hamming(10,6,3)  — the LDU link-control / encryption-sync hexbits.
//   · Reed-Solomon over GF(2^6) — LC (24,12,13), ES (24,16,9), HDU (36,20,17),
//     Phase 2 ESS (44,16,29) and the MAC PDUs.
//
// Encoders are op25's, verbatim (op25_golay.h golay_24_encode, op25_hamming.h
// hamming_15_encode / hmg1063EncTbl). op25 decodes Golay and Hamming(15,11) with
// 2048-entry lookup tables; here the same syndrome→error-pattern table is BUILT
// from the encoder (every pattern of weight ≤ 3 for Golay, ≤ 1 for Hamming),
// which is the same mapping without transcribing 4096 numbers. Reed-Solomon
// matches ezpwd's RS<63,k> (op25's decoder): GF(64) primitive polynomial 0x43,
// first consecutive root α^1, systematic with parity last — verified against
// ezpwd's own encoder output in web/test/p25voice.test.ts.

// ── Golay ────────────────────────────────────────────────────────────────────

const GOLAY24_ENC = [
  0o40006165, 0o20003073, 0o10007550, 0o4003664, 0o2001732, 0o1006631,
  0o403315, 0o201547, 0o106706, 0o45227, 0o24476, 0o14353,
];

/** op25 golay_24_encode: 12 data bits → 24-bit codeword, data in the top 12. */
export function golay24Encode(data: number): number {
  let out = 0;
  for (let i = 0; i < 12; i++) if (data & (1 << (11 - i))) out ^= GOLAY24_ENC[i];
  return out >>> 0;
}

/** op25 golay_23_encode: the (24,12) code with its last parity bit dropped. */
export function golay23Encode(data: number): number {
  return golay24Encode(data) >>> 1;
}

function popcount(x: number): number {
  let c = 0;
  while (x) {
    x &= x - 1;
    c++;
  }
  return c;
}

// Syndrome (12 bits for the 24-bit code) → correctable error pattern.
let golay24Syn: Int32Array | null = null;
let golay23Syn: Int32Array | null = null;

function golay24Syndrome(cw: number): number {
  return (golay24Encode(cw >>> 12) ^ cw) & 0xfff;
}
function golay23Syndrome(cw: number): number {
  return (golay23Encode(cw >>> 11) ^ cw) & 0x7ff;
}

function buildSyndromeTable(nBits: number, synBits: number, syndrome: (cw: number) => number, maxWeight: number): Int32Array {
  const t = new Int32Array(1 << synBits).fill(-1);
  t[0] = 0;
  const visit = (pattern: number) => {
    const s = syndrome(pattern);
    if (t[s] === -1 || popcount(t[s]) > popcount(pattern)) t[s] = pattern;
  };
  for (let a = 0; a < nBits; a++) {
    visit(1 << a);
    if (maxWeight < 2) continue;
    for (let b = a + 1; b < nBits; b++) {
      visit((1 << a) | (1 << b));
      if (maxWeight < 3) continue;
      for (let c = b + 1; c < nBits; c++) visit((1 << a) | (1 << b) | (1 << c));
    }
  }
  return t;
}

export interface FecResult {
  /** The corrected data bits. */
  data: number;
  /** Bits corrected; -1 when the word was uncorrectable (data is then the raw data bits). */
  errs: number;
}

/** Golay(24,12) decode: corrects ≤ 3 errors, detects 4. Also decodes a
 *  shortened word (e.g. the HDU's 18-bit (18,6) words, top data bits zero). */
export function golay24Decode(cw: number): FecResult {
  golay24Syn ??= buildSyndromeTable(24, 12, golay24Syndrome, 3);
  const e = golay24Syn[golay24Syndrome(cw & 0xffffff)];
  if (e < 0) return { data: (cw >>> 12) & 0xfff, errs: -1 };
  return { data: ((cw ^ e) >>> 12) & 0xfff, errs: popcount(e) };
}

/** Golay(23,12) decode: a perfect code — every syndrome is a ≤ 3-bit pattern. */
export function golay23Decode(cw: number): FecResult {
  golay23Syn ??= buildSyndromeTable(23, 11, golay23Syndrome, 3);
  const e = golay23Syn[golay23Syndrome(cw & 0x7fffff)];
  return { data: ((cw ^ e) >>> 11) & 0xfff, errs: popcount(e) };
}

// ── Hamming ──────────────────────────────────────────────────────────────────

const HAMMING15_ENC = [0x400f, 0x200e, 0x100d, 0x080c, 0x040b, 0x020a, 0x0109, 0x0087, 0x0046, 0x0025, 0x0013];

/** op25 hamming_15_encode: 11 data bits → 15-bit codeword, data in the top 11. */
export function hamming15Encode(data: number): number {
  let out = 0;
  for (let i = 0; i < 11; i++) if (data & (1 << (10 - i))) out ^= HAMMING15_ENC[i];
  return out;
}

let hamming15Syn: Int32Array | null = null;
function hamming15Syndrome(cw: number): number {
  return (hamming15Encode(cw >>> 4) ^ cw) & 0xf;
}

/** Hamming(15,11) decode: corrects 1 error. */
export function hamming15Decode(cw: number): FecResult {
  hamming15Syn ??= buildSyndromeTable(15, 4, hamming15Syndrome, 1);
  const e = hamming15Syn[hamming15Syndrome(cw & 0x7fff)];
  if (e < 0) return { data: (cw >>> 4) & 0x7ff, errs: -1 };
  return { data: ((cw ^ e) >>> 4) & 0x7ff, errs: popcount(e) };
}

// op25_hamming.h, verbatim.
const HMG1063_ENC = [
  0, 12, 3, 15, 7, 11, 4, 8, 11, 7, 8, 4, 12, 0, 15, 3,
  13, 1, 14, 2, 10, 6, 9, 5, 6, 10, 5, 9, 1, 13, 2, 14,
  14, 2, 13, 1, 9, 5, 10, 6, 5, 9, 6, 10, 2, 14, 1, 13,
  3, 15, 0, 12, 4, 8, 7, 11, 8, 4, 11, 7, 15, 3, 12, 0,
];
const HMG1063_DEC = [0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 8, 1, 16, 32, 0];

/** Hamming(10,6,3): 6 data bits → 4 parity bits (op25 hmg1063EncTbl). */
export function hamming1063Parity(data6: number): number {
  return HMG1063_ENC[data6 & 63];
}
/** op25 hmg1063Dec: the corrected 6-bit hexbit. */
export function hamming1063Decode(data6: number, parity4: number): number {
  return (data6 ^ HMG1063_DEC[HMG1063_ENC[data6 & 63] ^ (parity4 & 15)]) & 63;
}

// ── Reed-Solomon over GF(64) ─────────────────────────────────────────────────

const GF_N = 63;
const GF_EXP = new Uint8Array(2 * GF_N);
const GF_LOG = new Int16Array(64).fill(-1);
{
  let x = 1;
  for (let i = 0; i < GF_N; i++) {
    GF_EXP[i] = x;
    GF_LOG[x] = i;
    x <<= 1;
    if (x & 64) x ^= 0x43; // x^6 + x + 1
  }
  for (let i = GF_N; i < 2 * GF_N; i++) GF_EXP[i] = GF_EXP[i - GF_N];
}
const gmul = (a: number, b: number) => (a === 0 || b === 0 ? 0 : GF_EXP[GF_LOG[a] + GF_LOG[b]]);
const gdiv = (a: number, b: number) => (a === 0 ? 0 : GF_EXP[(GF_LOG[a] - GF_LOG[b] + GF_N) % GF_N]);
const gpow = (e: number) => GF_EXP[((e % GF_N) + GF_N) % GF_N];

const generators = new Map<number, Uint8Array>();
/** g(x) = Π_{i=1..nroots} (x − α^i), coefficients highest-degree first. */
function generator(nroots: number): Uint8Array {
  let g = generators.get(nroots);
  if (g) return g;
  g = new Uint8Array(nroots + 1);
  g[0] = 1;
  let deg = 0;
  for (let i = 1; i <= nroots; i++) {
    const r = gpow(i);
    // g(x) *= (x + r)
    for (let j = deg + 1; j >= 1; j--) g[j] = g[j] ^ gmul(g[j - 1], r);
    deg++;
  }
  generators.set(nroots, g);
  return g;
}

/**
 * Systematic RS(63, 63−nroots) encode, ezpwd's layout: `msg` is the (63 −
 * nroots) data symbols — including any leading zeros of a shortened code —
 * highest degree first; returns the `nroots` parity symbols.
 */
export function rsEncode(msg: ArrayLike<number>, nroots: number): Uint8Array {
  const g = generator(nroots);
  const par = new Uint8Array(nroots);
  for (let i = 0; i < msg.length; i++) {
    const fb = (msg[i] & 63) ^ par[0];
    for (let j = 0; j < nroots - 1; j++) par[j] = par[j + 1] ^ gmul(fb, g[j + 1]);
    par[nroots - 1] = gmul(fb, g[nroots]);
  }
  return par;
}

/**
 * Decode a full 63-symbol RS codeword in place (data first, `nroots` parity
 * last, highest degree first — the vector op25 hands ezpwd). `erasures` are
 * known-bad positions (0..62). `shortenedBelow`: positions below it are the
 * zero padding of a shortened code — a "correction" there means the decoder
 * locked onto a wrong codeword, so it is reported as a failure.
 *
 * Returns the number of symbols corrected (errors + erasures), or -1 when
 * uncorrectable.
 */
export function rsDecode(cw: Uint8Array, nroots: number, erasures: number[] = [], shortenedBelow = 0): number {
  const n = GF_N;
  // Syndromes S_j = c(α^j), j = 1..nroots. Position p has degree n−1−p.
  const S = new Uint8Array(nroots);
  let any = false;
  for (let j = 0; j < nroots; j++) {
    let s = 0;
    const a = gpow(j + 1);
    for (let p = 0; p < n; p++) s = gmul(s, a) ^ cw[p];
    S[j] = s;
    if (s) any = true;
  }
  if (!any) return 0;
  if (erasures.length > nroots) return -1;

  // Erasure locator Γ(x) = Π (1 − X_k x), X_k = α^(n−1−p).
  let lambda = new Uint8Array(nroots + 1);
  lambda[0] = 1;
  for (const p of erasures) {
    const X = gpow(n - 1 - p);
    for (let j = nroots; j >= 1; j--) lambda[j] ^= gmul(lambda[j - 1], X);
  }

  // Berlekamp–Massey, initialised with the erasures (errors-and-erasures form).
  let B = lambda.slice();
  let L = erasures.length;
  let m = 1;
  let b = 1;
  for (let r = erasures.length; r < nroots; r++) {
    let d = S[r];
    for (let i = 1; i <= L; i++) d ^= gmul(lambda[i], S[r - i]);
    if (d === 0) {
      m++;
      continue;
    }
    const T = lambda.slice();
    const coef = gdiv(d, b);
    for (let i = m; i <= nroots; i++) lambda[i] ^= gmul(coef, B[i - m]);
    if (2 * L <= r + erasures.length) {
      L = r + 1 + erasures.length - L;
      B = T;
      b = d;
      m = 1;
    } else {
      m++;
    }
  }
  let degLambda = 0;
  for (let i = nroots; i >= 0; i--)
    if (lambda[i]) {
      degLambda = i;
      break;
    }
  if (degLambda === 0 || degLambda > nroots) return -1;

  // Chien search: roots of Λ are X_k^{-1}.
  const locs: number[] = [];
  for (let p = 0; p < n; p++) {
    const Xinv = gpow(-(n - 1 - p));
    let v = 0;
    let xp = 1;
    for (let i = 0; i <= degLambda; i++) {
      v ^= gmul(lambda[i], xp);
      xp = gmul(xp, Xinv);
    }
    if (v === 0) locs.push(p);
  }
  if (locs.length !== degLambda) return -1;

  // Ω(x) = S(x)·Λ(x) mod x^nroots, S(x) = Σ S_j x^j.
  const omega = new Uint8Array(nroots);
  for (let i = 0; i < nroots; i++) {
    let v = 0;
    for (let j = 0; j <= i && j <= degLambda; j++) v ^= gmul(lambda[j], S[i - j]);
    omega[i] = v;
  }
  // Forney (first root α^1): e_k = Ω(X_k^{-1}) / Λ'(X_k^{-1}).
  for (const p of locs) {
    if (p < shortenedBelow) return -1;
    const X = gpow(n - 1 - p);
    const Xinv = gdiv(1, X);
    let num = 0;
    let xp = 1;
    for (let i = 0; i < nroots; i++) {
      num ^= gmul(omega[i], xp);
      xp = gmul(xp, Xinv);
    }
    // Λ'(x): odd terms only (characteristic 2).
    let den = 0;
    for (let i = 1; i <= degLambda; i += 2) den ^= gmul(lambda[i], gpow(-(n - 1 - p) * (i - 1)));
    if (den === 0) return -1;
    cw[p] ^= gdiv(num, den);
  }
  return locs.length;
}
