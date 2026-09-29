// P25 Phase 1 frame layer: streaming framer (sync → status-symbol strip → NID)
// and the NID's BCH(63,16) decoder.
//
// bch_decode is a port of op25's bch.cc::bchDec (© 2010 KA1RBI) by way of
// src/vendor/ff/p25/nid.ts; decode_nid mirrors op25 p25_framer::nid_codeword.
// The framer is new: it runs on a dibit stream, so there are no windows.
#pragma once
#include <cstdint>
#include <cstring>
#include <functional>
#include <vector>

namespace p25 {

enum Duid : int { HDU = 0x0, TDU = 0x3, LDU1 = 0x5, TSDU = 0x7, LDU2 = 0xA, PDU = 0xC, TDULC = 0xF };

inline const char* duid_name(int d) {
  switch (d) {
    case HDU: return "HDU";
    case TDU: return "TDU";
    case LDU1: return "LDU1";
    case TSDU: return "TSDU";
    case LDU2: return "LDU2";
    case PDU: return "PDU";
    case TDULC: return "TDULC";
    default: return "?";
  }
}

// ---------------------------------------------------------------------------
// BCH(63,16) — op25 bchDec. cw[0..62] is the codeword (cw[i] = coeff of x^i),
// corrected in place. Returns errors corrected, or < 0 if undecodable.
// ---------------------------------------------------------------------------
inline int bch_decode(uint8_t cw[64]) {
  static const int GF_EXP[64] = {1,  2,  4,  8,  16, 32, 3,  6,  12, 24, 48, 35, 5,  10, 20, 40, 19, 38, 15, 30, 60, 59,
                                 53, 41, 17, 34, 7,  14, 28, 56, 51, 37, 9,  18, 36, 11, 22, 44, 27, 54, 47, 29, 58, 55,
                                 45, 25, 50, 39, 13, 26, 52, 43, 21, 42, 23, 46, 31, 62, 63, 61, 57, 49, 33, 0};
  static const int GF_LOG[64] = {-1, 0,  1,  6,  2,  12, 7,  26, 3,  32, 13, 35, 8,  48, 27, 18, 4,  24, 33, 16, 14, 52,
                                 36, 54, 9,  45, 49, 38, 28, 41, 19, 56, 5,  62, 25, 11, 34, 31, 17, 47, 15, 23, 53, 51,
                                 37, 44, 55, 40, 10, 61, 46, 30, 50, 22, 39, 43, 29, 60, 42, 21, 20, 59, 57, 58};
  int elp[24][22] = {};
  int S[23] = {}, D[23] = {}, L[24] = {}, uLu[24] = {}, locn[11] = {}, reg[12] = {};
  int i, j, U, q, count;
  bool synError = false;
  int cantDecode = 0;

  for (i = 1; i <= 22; i++) {
    S[i] = 0;
    for (j = 0; j <= 62; j++)
      if (cw[j]) S[i] ^= GF_EXP[(i * j) % 63];
    if (S[i]) synError = true;
    S[i] = GF_LOG[S[i]];
  }
  if (!synError) return 0;

  L[0] = 0; uLu[0] = -1; D[0] = 0; elp[0][0] = 0;
  L[1] = 0; uLu[1] = 0; D[1] = S[1]; elp[1][0] = 1;
  for (i = 1; i <= 21; i++) { elp[0][i] = -1; elp[1][i] = 0; }
  U = 0;
  do {
    U = U + 1;
    if (D[U] == -1) {
      L[U + 1] = L[U];
      for (i = 0; i <= L[U]; i++) { elp[U + 1][i] = elp[U][i]; elp[U][i] = GF_LOG[elp[U][i]]; }
    } else {
      q = U - 1;
      while (D[q] == -1 && q > 0) q = q - 1;
      if (q > 0) {
        j = q;
        do { j = j - 1; if (D[j] != -1 && uLu[q] < uLu[j]) q = j; } while (j > 0);
      }
      if (L[U] > L[q] + U - q) L[U + 1] = L[U]; else L[U + 1] = L[q] + U - q;
      for (i = 0; i <= 21; i++) elp[U + 1][i] = 0;
      for (i = 0; i <= L[q]; i++)
        if (elp[q][i] != -1) elp[U + 1][i + U - q] = GF_EXP[(D[U] + 63 - D[q] + elp[q][i]) % 63];
      for (i = 0; i <= L[U]; i++) { elp[U + 1][i] ^= elp[U][i]; elp[U][i] = GF_LOG[elp[U][i]]; }
    }
    uLu[U + 1] = U - L[U + 1];
    if (U < 22) {
      if (S[U + 1] != -1) D[U + 1] = GF_EXP[S[U + 1]]; else D[U + 1] = 0;
      for (i = 1; i <= L[U + 1]; i++)
        if (S[U + 1 - i] != -1 && elp[U + 1][i] != 0) D[U + 1] ^= GF_EXP[(S[U + 1 - i] + GF_LOG[elp[U + 1][i]]) % 63];
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
      for (j = 1; j <= L[U]; j++)
        if (reg[j] != -1) { reg[j] = (reg[j] + j) % 63; q ^= GF_EXP[reg[j]]; }
      if (q == 0) { if (count < 11) locn[count] = 63 - i; count = count + 1; }
    }
    if (count == L[U]) { for (i = 0; i <= L[U] - 1; i++) cw[locn[i]] ^= 1; cantDecode = count; }
    else cantDecode = -1;
  } else {
    cantDecode = -2;
  }
  return cantDecode;
}

struct Nid {
  int nac, duid, errors;
};

// Systematic BCH(63,16) encode (nid.ts bchEncode) and the 64-bit NID word for
// {nac, duid} with its parity bit — the inverse of decode_nid.
inline uint64_t encode_nid(int nac, int duid) {
  static const uint8_t G[48] = {1, 1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 1, 1, 1, 0, 1, 1,
                                1, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 0, 1, 1};
  const int v = ((nac & 0xfff) << 4) | (duid & 0xf);
  uint8_t c[63] = {};
  for (int k = 0; k < 16; k++) c[62 - k] = uint8_t((v >> (15 - k)) & 1);
  for (int pos = 62; pos >= 47; pos--)
    if (c[pos])
      for (int k = 0; k <= 47; k++) c[k + pos - 47] ^= G[k];
  for (int k = 0; k < 16; k++) c[62 - k] = uint8_t((v >> (15 - k)) & 1);
  uint64_t word = 0;
  for (int i = 0; i <= 62; i++)
    if (c[i]) word |= uint64_t(1) << i;
  word <<= 1;
  return word | uint64_t(duid == 5 || duid == 10 ? 1 : 0);
}

// 64-bit NID word (MSB = first bit on air). False if BCH fails (>4 errors, as
// op25) or the DUID/parity check fails.
inline bool decode_nid(uint64_t acc, Nid& out) {
  const int parity = int(acc & 1);
  uint64_t a = acc >> 1;
  uint8_t cw[64];
  for (int i = 0; i < 64; i++) { cw[i] = uint8_t(a & 1); a >>= 1; }
  int ec = bch_decode(cw);
  if (ec < 0 || ec > 4) return false;
  uint64_t word = 0;
  for (int i = 62; i >= 0; i--) word = (word << 1) | cw[i];
  word = (word << 1) | uint64_t(parity);
  if ((word >> 1) == 0) return false;
  int nac = int((word >> 52) & 0xfff), duid = int((word >> 48) & 0xf);
  if ((duid == 0 || duid == 3 || duid == 7 || duid == 12 || duid == 15) && parity) return false;
  if ((duid == 5 || duid == 10) && !parity) return false;
  out = {nac, duid, ec};
  return true;
}

// ---------------------------------------------------------------------------
// Streaming framer.
// ---------------------------------------------------------------------------
struct Frame {
  Nid nid;
  uint64_t symbol;           // dibit index of the sync's first symbol (this receiver's count)
  double sample;             // channel-sample instant of that symbol (common to all receivers)
  bool inverted;             // polarity the sync was found in
  std::vector<uint8_t> bits;  // status symbols removed; [0,48) sync, [48,112) NID, then the body
  std::vector<uint8_t> raw;   // op25's frame body: status symbols left in place (voice tables index this)
  std::vector<float> soft;    // per-bit reliability, parallel to `bits` (empty if the receiver gives none)
  std::vector<float> rawSoft; // per-bit reliability, parallel to `raw`
  bool complete;             // reached its full length (false: cut short by the next sync)
};

struct FramerOptions {
  // Flywheel: right where the next frame must start after a fixed-length one
  // (back-to-back LDUs, 3-block TSDUs), accept a sync with ≤ flywheelErrs bit
  // errors in the same polarity, not just the usual ≤ 4.
  bool flywheel = true;
  int flywheelErrs = 12;
  // NID recovery: when BCH fails, the NID can only be the site's NAC with one
  // of 7 DUIDs — take the nearest (soft distance) if within nidMaxErrs bits
  // (BCH(63,16) has distance 23, so ≤ 11 is unambiguous).
  bool nidRecover = true;
  int nidMaxErrs = 11;
};

class Framer {
 public:
  explicit Framer(FramerOptions o = {}) : opt_(o) {}
  std::function<void(const Frame&)> onFrame;
  uint64_t syncs = 0, nidFails = 0, flywheels = 0, nidRecovered = 0;

  void push(uint8_t dibit, double sample = 0, float relHi = -1, float relLo = -1) {
    sr_ = ((sr_ << 2) | dibit) & 0xFFFFFFFFFFFFull;
    const uint64_t n = count_++;
    times_[n % 24] = sample;
    // A new sync (≤ 4 bit errors, either polarity) ends whatever was running.
    // Ignore matches inside the sync we are already in.
    if (!(active_ && pos_ < 48)) {
      int e = __builtin_popcountll(sr_ ^ kSync), ei = __builtin_popcountll(sr_ ^ kSync ^ kInv);
      const bool strict = e <= 4 || ei <= 4;
      const bool fly = !strict && opt_.flywheel && expecting_ && n == expectAt_ && (expectInv_ ? ei : e) <= opt_.flywheelErrs;
      if (n >= expectAt_) expecting_ = false;
      if (strict || fly) {
        finish(false);
        syncs++;
        if (fly) flywheels++;
        expecting_ = false;
        active_ = true;
        inv_ = fly ? expectInv_ : ei < e;
        pos_ = 24;  // dibits since sync start, counting this one
        start_ = n - 23;
        f_.sample = times_[(n + 1) % 24];  // the oldest of the last 24: the sync's first symbol
        f_.bits.assign(kSyncBits, kSyncBits + 48);
        f_.raw.assign(kSyncBits, kSyncBits + 48);
        f_.soft.assign(relHi >= 0 ? 48 : 0, 1.f);
        f_.rawSoft.assign(relHi >= 0 ? 48 : 0, 1.f);
        target_ = 0;
        return;
      }
    }
    if (!active_) return;
    const int p = pos_++;
    const uint8_t d = inv_ ? uint8_t(dibit ^ 0b10) : dibit;
    f_.raw.push_back(d >> 1);
    f_.raw.push_back(d & 1);
    if (relHi >= 0) {
      f_.rawSoft.push_back(relHi);
      f_.rawSoft.push_back(relLo);
    }
    if ((p + 1) % 36 == 0) {  // status symbol
      if (target_ && p + 1 >= target_) finish(true);
      return;
    }
    f_.bits.push_back(d >> 1);
    f_.bits.push_back(d & 1);
    if (relHi >= 0) {
      f_.soft.push_back(relHi);
      f_.soft.push_back(relLo);
    }
    if (f_.bits.size() == 112) {
      uint64_t acc = 0;
      for (int i = 48; i < 112; i++) acc = (acc << 1) | f_.bits[i];
      if (!decode_nid(acc, f_.nid) && !recover_nid(acc)) {
        nidFails++;
        active_ = false;
        return;
      }
      lastNac_ = f_.nid.nac;
      target_ = frame_dibits(f_.nid.duid);
      if (!target_) {  // PDU or unknown: report the header, don't try to follow
        finish(false);
        return;
      }
    }
    if (target_ && p + 1 >= target_) finish(true);
  }

 private:
  // Frame length in dibits including status symbols; TSDU is its 3-TSBK maximum.
  static int frame_dibits(int duid) {
    switch (duid) {
      case HDU: return 396;
      case TDU: return 72;
      case LDU1: case LDU2: return 864;
      case TSDU: return 360;
      case TDULC: return 216;
      default: return 0;
    }
  }
  bool recover_nid(uint64_t acc) {
    if (!opt_.nidRecover || lastNac_ < 0) return false;
    static const int DUIDS[7] = {HDU, TDU, LDU1, TSDU, LDU2, PDU, TDULC};
    int best = -1, bestHard = 99;
    float bestCost = 1e30f;
    const bool soft = f_.soft.size() >= 112;
    for (int d : DUIDS) {
      const uint64_t diff = acc ^ encode_nid(lastNac_, d);
      const int hard = __builtin_popcountll(diff);
      float cost = 0;
      if (soft) {
        for (int i = 0; i < 64; i++)
          if ((diff >> (63 - i)) & 1) cost += f_.soft[48 + size_t(i)];
      } else {
        cost = float(hard);
      }
      if (cost < bestCost) bestCost = cost, best = d, bestHard = hard;
    }
    if (best < 0 || bestHard > opt_.nidMaxErrs) return false;
    f_.nid = {lastNac_, best, bestHard};
    nidRecovered++;
    return true;
  }

  void finish(bool complete) {
    if (!active_) return;
    active_ = false;
    if (f_.bits.size() < 112) return;  // NID never decoded
    if (complete) {
      // The next frame's sync must end exactly target_ + 23 dibits after this start.
      expecting_ = true;
      expectAt_ = start_ + uint64_t(target_) + 23;
      expectInv_ = inv_;
    }
    f_.symbol = start_;
    f_.inverted = inv_;
    f_.complete = complete;
    if (onFrame) onFrame(f_);
  }

  static constexpr uint64_t kSync = 0x5575F5FF77FFull, kInv = 0xAAAAAAAAAAAAull;
  static constexpr uint8_t kSyncBits[48] = {0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 1, 1, 0, 1, 0, 1,
                                            1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1};
  FramerOptions opt_;
  uint64_t sr_ = 0, count_ = 0, start_ = 0, expectAt_ = 0;
  bool expecting_ = false, expectInv_ = false;
  int lastNac_ = -1;
  double times_[24] = {};
  bool active_ = false, inv_ = false;
  int pos_ = 0, target_ = 0;
  Frame f_;
};

}  // namespace p25
