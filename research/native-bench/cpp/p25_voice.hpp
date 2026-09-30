// P25 Phase 1 voice framing — port of src/vendor/ff/p25/voice.ts (op25
// p25p1_fdma.cc process_HDU / process_LLDU / process_LDU2 / process_TDU15 /
// process_LCW and op25_imbe_frame.h imbe_header_decode).
//
// Frame bodies here are op25's: bits counted from the first bit of the sync
// with the status symbols LEFT IN PLACE (Frame::raw), because every bit table
// indexes that layout.
#pragma once
#include <array>
#include <cstdint>
#include <optional>
#include <vector>

#include "p25_fec.hpp"
#include "tables.gen.hpp"

namespace p25 {

constexpr int ALGID_CLEAR = 0x80;
constexpr int ALGID_UNKNOWN = -1;

struct ImbeParams {
  std::array<int, 8> u;  // u0..u6, u7 stored <<1 (op25)
  int errs;              // corrected bits; an uncorrectable word counts 4
  int e0;                // errors in u0 (pitch)
  float cost = 0, cost0 = 0;  // soft: reliability overridden, all words / u0's word
  float meanRel = 0;          // soft: mean bit reliability of the codeword
  int errsHard = 0, e0Hard = 0;  // the hard decoders' counts (the channel-error estimate TIA's thresholds expect)
};

namespace detail {
inline uint32_t extract(const uint8_t* cw, int begin, int end) {
  uint32_t v = 0;
  for (int i = begin; i < end; i++) v = (v << 1) | (cw[i] & 1);
  return v;
}
inline uint32_t pngen(uint32_t& pr, int nbits) {
  uint32_t n = 0;
  for (int i = nbits - 1; i >= 0; --i) {
    pr = (173 * pr + 13849) & 0xffff;
    if (pr & 32768) n += 1u << i;
  }
  return n;
}
}  // namespace detail

// op25 imbe_header_decode: 144-bit codeword → u0..u7. With `soft` (144
// reliabilities, parallel to cw) the Golay words are Chase-decoded and the
// Hamming words decoded by maximum likelihood.
inline ImbeParams imbe_header_decode(const uint8_t cw[144], const float* soft = nullptr);
inline ImbeParams imbe_header_decode_hard(const uint8_t cw[144]) { return imbe_header_decode(cw, nullptr); }

inline ImbeParams imbe_header_decode(const uint8_t cw[144], const float* soft) {
  using detail::extract;
  ImbeParams p{};
  auto c = [](const Fec& r) { return r.errs < 0 ? 4 : r.errs; };
  auto golay = [&](uint32_t v, int at) { return soft ? golay23_decode_soft(v, soft + at) : golay23_decode(v); };
  auto hamming = [&](uint32_t v, int at) { return soft ? hamming15_decode_soft(v, soft + at) : hamming15_decode(v); };
  Fec r0 = golay(extract(cw, 0, 23), 0);
  p.cost0 = p.cost = r0.cost;
  if (soft) {
    for (int i = 0; i < 144; i++) p.meanRel += soft[i];
    p.meanRel /= 144;
  }
  p.u[0] = r0.data;
  p.e0 = r0.errs;
  p.errs = r0.errs;
  uint32_t pr = uint32_t(p.u[0]) << 4;
  for (int k = 1; k <= 3; k++) {
    uint32_t m = detail::pngen(pr, 23);
    Fec r = golay(extract(cw, 23 * k, 23 * (k + 1)) ^ m, 23 * k);
    p.cost += r.cost;
    p.u[k] = r.data;
    p.errs += c(r);
  }
  for (int k = 4; k <= 6; k++) {
    uint32_t m = detail::pngen(pr, 15);
    int s = 92 + (k - 4) * 15;
    Fec r = hamming(extract(cw, s, s + 15) ^ m, s);
    p.cost += r.cost;
    p.u[k] = r.data;
    p.errs += c(r);
  }
  p.u[7] = int(extract(cw, 137, 144) << 1);
  if (soft) {
    const ImbeParams h = imbe_header_decode_hard(cw);
    p.errsHard = h.errs;
    p.e0Hard = h.e0;
  } else {
    p.errsHard = p.errs;
    p.e0Hard = p.e0;
  }
  return p;
}

// u0..u7 → mbelib's imbe_d[88].
inline std::array<uint8_t, 88> imbe_params_to_bits(const std::array<int, 8>& u) {
  static const int W[8] = {12, 12, 12, 12, 11, 11, 11, 7};
  std::array<uint8_t, 88> d{};
  int p = 0;
  for (int k = 0; k < 8; k++) {
    int v = k == 7 ? u[7] >> 1 : u[k];
    for (int b = W[k] - 1; b >= 0; b--) d[p++] = uint8_t((v >> b) & 1);
  }
  return d;
}

// The nine IMBE codewords of an LDU (op25 imbe_deinterleave); `rawSoft` →
// `soft` carries the bit reliabilities along (missing bits: reliability 0).
inline void ldu_codeword(const std::vector<uint8_t>& fb, int f, uint8_t cw[144], const std::vector<float>* rawSoft = nullptr,
                         float* soft = nullptr) {
  for (int j = 0; j < 144; j++) {
    int k = tables::VOICE_CODEWORD_BITS[f * 144 + j];
    cw[j] = k < int(fb.size()) ? (fb[k] & 1) : 0;
    if (soft) soft[j] = rawSoft && k < int(rawSoft->size()) ? (*rawSoft)[size_t(k)] : 0.f;
  }
}

struct LinkControl {
  int lco = 0, mfid = 0;
  bool isProtected = false;
  int svcOpts = -1, tgid = -1, target = -1, source = -1;  // -1: not present
};
struct EncryptionSync {
  int algid = ALGID_CLEAR, keyid = 0;
  std::array<uint8_t, 9> mi{};
};
struct HduInfo {
  EncryptionSync es;
  int mfid = 0, tgid = 0;
};

namespace detail {
inline int bit(const std::vector<uint8_t>& fb, int k) { return k < int(fb.size()) ? (fb[k] & 1) : 0; }
inline std::array<uint8_t, 9> hexbits_to_bytes(const uint8_t* hb, int from) {
  std::array<uint8_t, 9> out{};
  int n = 0, j = from;
  while (n < 9) {
    out[n++] = uint8_t(((hb[j] << 2) | (hb[j + 1] >> 4)) & 0xff);
    if (n < 9) out[n++] = uint8_t((((hb[j + 1] & 0x0f) << 4) | (hb[j + 2] >> 2)) & 0xff);
    if (n < 9) out[n++] = uint8_t((((hb[j + 2] & 0x03) << 6) | hb[j + 3]) & 0xff);
    j += 4;
  }
  return out;
}
inline void ldu_hexbits(const std::vector<uint8_t>& fb, uint8_t HB[63]) {
  std::fill(HB, HB + 63, 0);
  int k = 0;
  for (int i = 0; i < 24; i++) {
    int cw = 0;
    for (int j = 0; j < 10; j++) cw = (cw << 1) | bit(fb, tables::LDU_LS_DATA_BITS[k++]);
    HB[39 + i] = uint8_t(hamming1063_decode(cw >> 4, cw & 0x0f));
  }
}
}  // namespace detail

inline LinkControl parse_lcw(const std::array<uint8_t, 9>& w) {
  LinkControl lc;
  lc.isProtected = (w[0] & 0x80) != 0;
  lc.lco = w[0] & 0x3f;
  lc.mfid = w[1];
  if (lc.isProtected) return lc;
  if (lc.lco == 0x00) {
    lc.svcOpts = w[2];
    lc.tgid = (w[4] << 8) | w[5];
    lc.source = (w[6] << 16) | (w[7] << 8) | w[8];
  } else if (lc.lco == 0x03) {
    lc.svcOpts = w[2];
    lc.target = (w[3] << 16) | (w[4] << 8) | w[5];
    lc.source = (w[6] << 16) | (w[7] << 8) | w[8];
  }
  return lc;
}

// LDU1 link control: RS(24,12,13), ≤ 6 corrections.
inline std::optional<LinkControl> decode_ldu1_lc(const std::vector<uint8_t>& fb) {
  uint8_t HB[63];
  detail::ldu_hexbits(fb, HB);
  int ec = rs_decode(HB, 12, 39);
  if (ec < 0 || ec > 6) return std::nullopt;
  return parse_lcw(detail::hexbits_to_bytes(HB, 39));
}

// LDU2 encryption sync: RS(24,16,9), ≤ 4 corrections.
inline std::optional<EncryptionSync> decode_ldu2_es(const std::vector<uint8_t>& fb) {
  uint8_t HB[63];
  detail::ldu_hexbits(fb, HB);
  int ec = rs_decode(HB, 8, 39);
  if (ec < 0 || ec > 4) return std::nullopt;
  EncryptionSync es;
  es.mi = detail::hexbits_to_bytes(HB, 39);
  const int j = 51;
  es.algid = ((HB[j] << 2) | (HB[j + 1] >> 4)) & 0xff;
  es.keyid = ((HB[j + 1] & 0x0f) << 12) | (HB[j + 2] << 6) | HB[j + 3];
  return es;
}

// HDU: 36 Golay(18,6) hexbits → RS(36,20,17), ≤ 8 corrections.
inline std::optional<HduInfo> decode_hdu(const std::vector<uint8_t>& fb) {
  uint8_t HB[63] = {};
  int k = 0;
  for (int i = 0; i < 36; i++) {
    uint32_t cw = 0;
    for (int j = 0; j < 18; j++) cw = (cw << 1) | uint32_t(detail::bit(fb, tables::HDU_CODEWORD_BITS[k++]));
    HB[27 + i] = uint8_t(golay24_decode(cw).data & 63);
  }
  int ec = rs_decode(HB, 16, 27);
  if (ec < 0 || ec > 8) return std::nullopt;
  HduInfo h;
  h.es.mi = detail::hexbits_to_bytes(HB, 27);
  const int j = 39;
  h.mfid = ((HB[j] << 2) | (HB[j + 1] >> 4)) & 0xff;
  h.es.algid = (((HB[j + 1] & 0x0f) << 4) | (HB[j + 2] >> 2)) & 0xff;
  h.es.keyid = ((HB[j + 2] & 0x03) << 14) | (HB[j + 3] << 8) | (HB[j + 4] << 2) | (HB[j + 5] >> 4);
  h.tgid = ((HB[j + 5] & 0x0f) << 12) | (HB[j + 6] << 6) | HB[j + 7];
  return h;
}

// TDULC: 12 Golay(24,12) words → LC.
inline std::optional<LinkControl> decode_tdulc(const std::vector<uint8_t>& fb) {
  uint8_t HB[63] = {};
  int k = 0;
  for (int i = 0; i <= 22; i += 2) {
    uint32_t cw = 0;
    for (int j = 0; j < 12; j++) {
      cw = (cw << 1) | uint32_t(detail::bit(fb, tables::HDU_CODEWORD_BITS[k++]));
      cw = (cw << 1) | uint32_t(detail::bit(fb, tables::HDU_CODEWORD_BITS[k++]));
    }
    int d = golay24_decode(cw).data;
    HB[39 + i] = uint8_t(d >> 6);
    HB[40 + i] = uint8_t(d & 63);
  }
  int ec = rs_decode(HB, 12, 39);
  if (ec < 0 || ec > 6) return std::nullopt;
  return parse_lcw(detail::hexbits_to_bytes(HB, 39));
}

}  // namespace p25
