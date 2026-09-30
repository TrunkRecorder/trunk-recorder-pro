// P25 TSDU → TSBKs: block de-interleave + 1/2-rate trellis decode (op25
// p25p1_fdma.cc::block_deinterleave, after wireshark packet-p25cai.c) and the
// TSBK CRC-16 (op25 p25p1_fdma.cc::crc16), by way of src/vendor/ff/p25/
// trellis.ts and crc.ts.
#pragma once
#include <array>
#include <cstdint>
#include <vector>

#include "p25_frame.hpp"

namespace p25 {

using Tsbk = std::array<uint8_t, 12>;

// De-interleave + trellis-decode 196 bits at bits[start] → 12 bytes. Greedy
// minimum-distance walk of the 4-state table, as op25 (not a full Viterbi).
inline bool trellis_decode(const uint8_t* bits, Tsbk& out) {
  // op25's 196-entry table: 12 rows of four 4-bit groups at 4g, 52+4g, 100+4g,
  // 148+4g, then the flush group 48..51.
  static const std::array<uint8_t, 196> TB = [] {
    std::array<uint8_t, 196> t{};
    int k = 0;
    for (int g = 0; g < 12; g++)
      for (int base : {0, 52, 100, 148})
        for (int e = 0; e < 4; e++) t[k++] = uint8_t(base + 4 * g + e);
    for (int e = 0; e < 4; e++) t[k++] = uint8_t(48 + e);
    return t;
  }();
  static const uint8_t NEXT[4][4] = {{0x2, 0xc, 0x1, 0xf}, {0xe, 0x0, 0xd, 0x3}, {0x9, 0x7, 0xa, 0x4}, {0x5, 0xb, 0x6, 0x8}};
  out.fill(0);
  int state = 0;
  for (int b = 0; b < 196; b += 4) {
    int cw = (bits[TB[b]] << 3) | (bits[TB[b + 1]] << 2) | (bits[TB[b + 2]] << 1) | bits[TB[b + 3]];
    int best = -1, min = 99;
    bool unique = true;
    for (int j = 0; j < 4; j++) {
      int hd = __builtin_popcount(cw ^ NEXT[state][j]);
      if (hd < min) min = hd, best = j, unique = true;
      else if (hd == min) unique = false;
    }
    if (!unique) return false;
    state = best;
    int d = b >> 2;
    if (d < 48) out[d >> 2] |= uint8_t(state << (6 - (d % 4) * 2));
  }
  return true;
}

// Viterbi decode of the same 1/2-rate trellis (4 states = the last input
// dibit; 49 steps, the last a flush to state 0). Unlike op25's greedy walk it
// never gives up on a tie: it returns the best whole path, and the CRC judges.
// `soft` (optional, same indexing as `bits`) is each bit's reliability ≥ 0;
// without it every bit counts 1 (hard-decision Hamming metric).
inline void trellis_viterbi(const uint8_t* bits, const float* soft, Tsbk& out) {
  static const auto TB = [] {
    std::array<uint8_t, 196> t{};
    int k = 0;
    for (int g = 0; g < 12; g++)
      for (int base : {0, 52, 100, 148})
        for (int e = 0; e < 4; e++) t[k++] = uint8_t(base + 4 * g + e);
    for (int e = 0; e < 4; e++) t[k++] = uint8_t(48 + e);
    return t;
  }();
  static const uint8_t NEXT[4][4] = {{0x2, 0xc, 0x1, 0xf}, {0xe, 0x0, 0xd, 0x3}, {0x9, 0x7, 0xa, 0x4}, {0x5, 0xb, 0x6, 0x8}};
  constexpr float INF = 1e30f;
  float pm[4] = {0, INF, INF, INF};
  uint8_t from[49][4];
  for (int d = 0; d < 49; d++) {
    // Cost of each received bit disagreeing with 0 or 1.
    float c0[4], c1[4];
    for (int k = 0; k < 4; k++) {
      const int idx = TB[d * 4 + k];
      const float w = soft ? soft[idx] : 1.f;
      c0[k] = bits[idx] ? w : 0.f;  // cost if the code bit is 0
      c1[k] = bits[idx] ? 0.f : w;  // cost if the code bit is 1
    }
    float nm[4] = {INF, INF, INF, INF};
    for (int s = 0; s < 4; s++) {
      if (pm[s] >= INF) continue;
      for (int j = 0; j < 4; j++) {
        if (d == 48 && j != 0) continue;  // flush dibit is 0
        const int cw = NEXT[s][j];
        float m = pm[s];
        for (int k = 0; k < 4; k++) m += ((cw >> (3 - k)) & 1) ? c1[k] : c0[k];
        if (m < nm[j]) nm[j] = m, from[d][j] = uint8_t(s);
      }
    }
    std::copy(nm, nm + 4, pm);
  }
  out.fill(0);
  int state = 0;
  for (int d = 48; d >= 0; d--) {
    if (d < 48) out[d >> 2] |= uint8_t(state << (6 - (d % 4) * 2));
    state = from[d][state];
  }
}

inline uint16_t crc16(const uint8_t* buf, int len) {
  uint32_t crc = 0;
  for (int i = 0; i < len; i++)
    for (int j = 0; j < 8; j++) {
      crc = ((crc << 1) | ((buf[i] >> (7 - j)) & 1)) & 0x1ffff;
      if (crc & 0x10000) crc = (crc & 0xffff) ^ 0x1021;
    }
  return uint16_t((crc ^ 0xffff) & 0xffff);
}

inline bool tsbk_last(const Tsbk& t) { return t[0] >> 7; }
inline int tsbk_opcode(const Tsbk& t) { return t[0] & 0x3f; }

struct TsduResult {
  std::vector<Tsbk> good;
  int bad = 0;
  int trellisFail = 0, crcFail = 0, blocksSkipped = 0;  // why `bad`; blocks never tried after a trellis failure
};

// Every block of a TSDU frame. Greedy (op25, the TS ControlDecoder): a trellis
// failure ends the frame. Viterbi: every block is decoded; the CRC alone
// judges. Either way the last-block flag stops.
enum class Trellis { Greedy, Viterbi };
inline TsduResult decode_tsdu(const Frame& f, Trellis mode = Trellis::Viterbi) {
  TsduResult r;
  const int avail = int(f.bits.size()) - 112;
  const int blocks = std::min(3, avail / 196);
  for (int b = 0; b < blocks; b++) {
    Tsbk t;
    if (mode == Trellis::Viterbi) {
      trellis_viterbi(f.bits.data() + 112 + b * 196, f.soft.empty() ? nullptr : f.soft.data() + 112 + b * 196, t);
    } else if (!trellis_decode(f.bits.data() + 112 + b * 196, t)) {
      r.bad++;
      r.trellisFail++;
      r.blocksSkipped = blocks - b - 1;
      break;
    }
    if (crc16(t.data(), 12) != 0) {
      r.bad++;
      r.crcFail++;
      continue;
    }
    r.good.push_back(t);
    if (tsbk_last(t)) break;
  }
  return r;
}

}  // namespace p25
