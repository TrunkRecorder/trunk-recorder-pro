// FEC for the P25 voice layers — port of src/vendor/ff/p25/fec.ts:
//   Golay(24,12) / (23,12)  IMBE u0..u3, HDU/TDULC hexbits
//   Hamming(15,11)          IMBE u4..u6
//   Hamming(10,6)           LDU link-control / encryption-sync hexbits
//   Reed-Solomon GF(2^6)    LC (24,12), ES (24,16), HDU (36,20) — ezpwd RS<63,k>
// Encoders are op25's; the syndrome → error-pattern tables are built from them
// (every pattern of weight ≤ 3 for Golay, ≤ 1 for Hamming), as in the TS.
#pragma once
#include <algorithm>
#include <array>
#include <cstdint>
#include <vector>

namespace p25 {

struct Fec {
  int data;
  int errs;          // bits corrected, -1 uncorrectable (data = raw data bits)
  float cost = 0;    // soft decoders: summed reliability of the bits overridden
};

inline uint32_t golay24_encode(uint32_t data) {
  static const uint32_t E[12] = {040006165, 020003073, 010007550, 04003664, 02001732, 01006631,
                                 0403315,   0201547,   0106706,   045227,   024476,   014353};
  uint32_t out = 0;
  for (int i = 0; i < 12; i++)
    if (data & (1u << (11 - i))) out ^= E[i];
  return out;
}
inline uint32_t golay23_encode(uint32_t data) { return golay24_encode(data) >> 1; }

inline uint32_t hamming15_encode(uint32_t data) {
  static const uint32_t E[11] = {0x400f, 0x200e, 0x100d, 0x080c, 0x040b, 0x020a, 0x0109, 0x0087, 0x0046, 0x0025, 0x0013};
  uint32_t out = 0;
  for (int i = 0; i < 11; i++)
    if (data & (1u << (10 - i))) out ^= E[i];
  return out;
}

namespace detail {
template <int N, int SYN, int MAXW, typename F>
std::array<int32_t, 1 << SYN> syndrome_table(F syndrome) {
  std::array<int32_t, 1 << SYN> t;
  t.fill(-1);
  t[0] = 0;
  auto visit = [&](uint32_t pat) {
    uint32_t s = syndrome(pat);
    if (t[s] == -1 || __builtin_popcount(uint32_t(t[s])) > __builtin_popcount(pat)) t[s] = int32_t(pat);
  };
  for (int a = 0; a < N; a++) {
    visit(1u << a);
    if (MAXW < 2) continue;
    for (int b = a + 1; b < N; b++) {
      visit((1u << a) | (1u << b));
      if (MAXW < 3) continue;
      for (int c = b + 1; c < N; c++) visit((1u << a) | (1u << b) | (1u << c));
    }
  }
  return t;
}
inline uint32_t golay24_syn(uint32_t cw) { return (golay24_encode(cw >> 12) ^ cw) & 0xfff; }
inline uint32_t golay23_syn(uint32_t cw) { return (golay23_encode(cw >> 11) ^ cw) & 0x7ff; }
inline uint32_t hamming15_syn(uint32_t cw) { return (hamming15_encode(cw >> 4) ^ cw) & 0xf; }
}  // namespace detail

inline Fec golay24_decode(uint32_t cw) {
  static const auto T = detail::syndrome_table<24, 12, 3>(detail::golay24_syn);
  cw &= 0xffffff;
  int32_t e = T[detail::golay24_syn(cw)];
  if (e < 0) return {int((cw >> 12) & 0xfff), -1};
  return {int(((cw ^ uint32_t(e)) >> 12) & 0xfff), __builtin_popcount(uint32_t(e))};
}
inline Fec golay23_decode(uint32_t cw) {
  static const auto T = detail::syndrome_table<23, 11, 3>(detail::golay23_syn);
  cw &= 0x7fffff;
  int32_t e = T[detail::golay23_syn(cw)];
  return {int(((cw ^ uint32_t(e)) >> 11) & 0xfff), __builtin_popcount(uint32_t(e))};
}
inline Fec hamming15_decode(uint32_t cw) {
  static const auto T = detail::syndrome_table<15, 4, 1>(detail::hamming15_syn);
  cw &= 0x7fff;
  int32_t e = T[detail::hamming15_syn(cw)];
  if (e < 0) return {int((cw >> 4) & 0x7ff), -1};
  return {int(((cw ^ uint32_t(e)) >> 4) & 0x7ff), __builtin_popcount(uint32_t(e))};
}

// ── Soft-decision decoders ────────────────────────────────────────────────────
// `r` is the received word (MSB = first bit), `w[i]` the reliability of bit i
// counted from the MSB (i = 0 is bit N−1). Both return the codeword whose
// disagreement with `r`, weighted by `w`, is smallest; `errs` is its plain bit
// distance from `r` (what the vocoder's error thresholds expect).

// Hamming(15,11): maximum likelihood over all 2048 codewords.
inline Fec hamming15_decode_soft(uint32_t r, const float* w) {
  static const auto CW = [] {
    std::array<uint16_t, 2048> t{};
    for (uint32_t d = 0; d < 2048; d++) t[d] = uint16_t(hamming15_encode(d));
    return t;
  }();
  r &= 0x7fff;
  float best = 1e30f;
  uint32_t bestD = 0;
  for (uint32_t d = 0; d < 2048; d++) {
    uint32_t diff = CW[d] ^ r;
    float cost = 0;
    while (diff) {
      const int b = __builtin_ctz(diff);
      cost += w[14 - b];
      diff &= diff - 1;
    }
    if (cost < best) best = cost, bestD = d;
  }
  return {int(bestD), __builtin_popcount(CW[bestD] ^ r), best};
}

// Golay(23,12): Chase-II — flip every subset of the 5 least reliable bits,
// hard-decode each, keep the lowest-cost codeword.
inline Fec golay23_decode_soft(uint32_t r, const float* w) {
  r &= 0x7fffff;
  int idx[23];
  for (int i = 0; i < 23; i++) idx[i] = i;
  std::partial_sort(idx, idx + 5, idx + 23, [&](int a, int b) { return w[a] < w[b]; });
  float best = 1e30f;
  uint32_t bestCw = r;
  for (int m = 0; m < 32; m++) {
    uint32_t t = r;
    for (int k = 0; k < 5; k++)
      if (m & (1 << k)) t ^= 1u << (22 - idx[k]);
    const Fec h = golay23_decode(t);
    const uint32_t c = golay23_encode(uint32_t(h.data)) ;
    uint32_t diff = (c ^ r) & 0x7fffff;
    float cost = 0;
    while (diff) {
      const int b = __builtin_ctz(diff);
      cost += w[22 - b];
      diff &= diff - 1;
    }
    if (cost < best) best = cost, bestCw = c;
  }
  return {int(bestCw >> 11), __builtin_popcount((bestCw ^ r) & 0x7fffff), best};
}

// op25_hamming.h hmg1063EncTbl / hmg1063Dec.
inline int hamming1063_decode(int data6, int parity4) {
  static const uint8_t ENC[64] = {0,  12, 3,  15, 7,  11, 4,  8,  11, 7,  8,  4, 12, 0, 15, 3,  13, 1,  14, 2,  10, 6,
                                  9,  5,  6,  10, 5,  9,  1,  13, 2,  14, 14, 2, 13, 1, 9,  5,  10, 6,  5,  9,  6,  10,
                                  2,  14, 1,  13, 3,  15, 0,  12, 4,  8,  7,  11, 8, 4, 11, 7,  15, 3,  12, 0};
  static const uint8_t DEC[16] = {0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 8, 1, 16, 32, 0};
  return (data6 ^ DEC[ENC[data6 & 63] ^ (parity4 & 15)]) & 63;
}

// ── Reed-Solomon over GF(64), primitive 0x43, first root α^1 (ezpwd) ─────────
namespace rs {
struct Gf {
  uint8_t exp[126];
  int16_t log[64];
  Gf() {
    for (auto& l : log) l = -1;
    int x = 1;
    for (int i = 0; i < 63; i++) {
      exp[i] = uint8_t(x);
      log[x] = int16_t(i);
      x <<= 1;
      if (x & 64) x ^= 0x43;
    }
    for (int i = 63; i < 126; i++) exp[i] = exp[i - 63];
  }
};
inline const Gf& gf() {
  static const Gf g;
  return g;
}
inline int mul(int a, int b) { return a == 0 || b == 0 ? 0 : gf().exp[gf().log[a] + gf().log[b]]; }
inline int div(int a, int b) { return a == 0 ? 0 : gf().exp[(gf().log[a] - gf().log[b] + 63) % 63]; }
inline int pw(int e) { return gf().exp[((e % 63) + 63) % 63]; }
}  // namespace rs

// Decode a 63-symbol codeword in place (data first, nroots parity last). A
// correction below `shortenedBelow` (the zero padding of a shortened code)
// means a wrong codeword: reported as failure. Returns symbols corrected, -1
// if uncorrectable. (No erasures: the voice paths never pass any.)
inline int rs_decode(uint8_t cw[63], int nroots, int shortenedBelow = 0) {
  using namespace rs;
  const int n = 63;
  std::vector<int> S(nroots);
  bool any = false;
  for (int j = 0; j < nroots; j++) {
    int s = 0, a = pw(j + 1);
    for (int p = 0; p < n; p++) s = mul(s, a) ^ cw[p];
    S[j] = s;
    any |= s != 0;
  }
  if (!any) return 0;
  std::vector<int> lambda(nroots + 1, 0), B, T;
  lambda[0] = 1;
  B = lambda;
  int L = 0, m = 1, b = 1;
  for (int r = 0; r < nroots; r++) {
    int d = S[r];
    for (int i = 1; i <= L; i++) d ^= mul(lambda[i], S[r - i]);
    if (d == 0) {
      m++;
      continue;
    }
    T = lambda;
    int coef = div(d, b);
    for (int i = m; i <= nroots; i++) lambda[i] ^= mul(coef, B[i - m]);
    if (2 * L <= r) {
      L = r + 1 - L;
      B = T;
      b = d;
      m = 1;
    } else {
      m++;
    }
  }
  int deg = 0;
  for (int i = nroots; i >= 0; i--)
    if (lambda[i]) {
      deg = i;
      break;
    }
  if (deg == 0 || deg > nroots) return -1;
  std::vector<int> locs;
  for (int p = 0; p < n; p++) {
    int Xinv = pw(-(n - 1 - p)), v = 0, xp = 1;
    for (int i = 0; i <= deg; i++) {
      v ^= mul(lambda[i], xp);
      xp = mul(xp, Xinv);
    }
    if (v == 0) locs.push_back(p);
  }
  if (int(locs.size()) != deg) return -1;
  std::vector<int> omega(nroots, 0);
  for (int i = 0; i < nroots; i++) {
    int v = 0;
    for (int j = 0; j <= i && j <= deg; j++) v ^= mul(lambda[j], S[i - j]);
    omega[i] = v;
  }
  for (int p : locs) {
    if (p < shortenedBelow) return -1;
    int X = pw(n - 1 - p), Xinv = div(1, X), num = 0, xp = 1;
    for (int i = 0; i < nroots; i++) {
      num ^= mul(omega[i], xp);
      xp = mul(xp, Xinv);
    }
    int den = 0;
    for (int i = 1; i <= deg; i += 2) den ^= mul(lambda[i], pw(-(n - 1 - p) * (i - 1)));
    if (den == 0) return -1;
    cw[p] ^= uint8_t(div(num, den));
  }
  return int(locs.size());
}

}  // namespace p25
