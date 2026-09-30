// IMBE 7200x4400 (P25 Phase 1) vocoder — port of src/vendor/ff/mbe/mbe.ts,
// itself a line-by-line port of mbelib 1.3.0 (ISC) with Trunk Recorder's
// "enhanced" synthesis and TIA-102.BABA-A §7.7/§7.8 concealment. Double
// arithmetic like the TS, so the two can be compared sample by sample with the
// same RNG. AMBE+2 (Phase 2) is not ported yet.
#pragma once
#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <functional>

#include "tables.gen.hpp"

namespace mbe {

constexpr int FRAME_SAMPLES = 160;
constexpr int NL = 58;  // one past mbelib's [57]: the prediction can read log2Ml[57]

using Rng = std::function<double()>;  // uniform [0, 1]

struct Parms {
  double w0 = 0;
  int L = 0, K = 0;
  std::array<int8_t, NL> Vl{};
  std::array<double, NL> Ml{}, log2Ml{}, PHIl{}, PSIl{};
  double gamma = 0;
  int repeat = 0;
};

inline void move_parms(const Parms& cur, Parms& prev) {
  prev.w0 = cur.w0;
  prev.L = cur.L;
  prev.K = cur.K;
  prev.Ml[0] = 0;
  prev.gamma = cur.gamma;
  prev.repeat = cur.repeat;
  for (int l = 0; l <= 56; l++) {
    prev.Ml[l] = cur.Ml[l];
    prev.Vl[l] = cur.Vl[l];
    prev.log2Ml[l] = cur.log2Ml[l];
    prev.PHIl[l] = cur.PHIl[l];
    prev.PSIl[l] = cur.PSIl[l];
  }
}
inline void use_last_parms(Parms& cur, const Parms& prev) { move_parms(prev, cur); }

inline void init_parms(Parms& cur, Parms& prev, Parms& prevEnh) {
  prev.w0 = 0.09378;
  prev.L = 30;
  prev.K = 10;
  prev.gamma = 0;
  for (int l = 0; l <= 56; l++) {
    prev.Ml[l] = 0;
    prev.Vl[l] = 0;
    prev.log2Ml[l] = 0;
    prev.PHIl[l] = 0;
    prev.PSIl[l] = M_PI / 2;
  }
  prev.repeat = 0;
  move_parms(prev, cur);
  move_parms(prev, prevEnh);
}

inline void spectral_amp_enhance(Parms& cur) {
  std::array<double, NL> Wl{};
  double Rm0 = 0, Rm1 = 0;
  for (int l = 1; l <= cur.L; l++) {
    Rm0 += cur.Ml[l] * cur.Ml[l];
    Rm1 += cur.Ml[l] * cur.Ml[l] * std::cos(cur.w0 * l);
  }
  const double R2m0 = Rm0 * Rm0, R2m1 = Rm1 * Rm1;
  for (int l = 1; l <= cur.L; l++) {
    if (cur.Ml[l] != 0) {
      Wl[l] = std::sqrt(cur.Ml[l]) *
              std::pow((0.96 * M_PI * (R2m0 + R2m1 - 2 * Rm0 * Rm1 * std::cos(cur.w0 * l))) / (cur.w0 * Rm0 * (R2m0 - R2m1)), 0.25);
      if (8 * l <= cur.L) {
      } else if (Wl[l] > 1.2) {
        cur.Ml[l] = 1.2 * cur.Ml[l];
      } else if (Wl[l] < 0.5) {
        cur.Ml[l] = 0.5 * cur.Ml[l];
      } else {
        cur.Ml[l] = Wl[l] * cur.Ml[l];
      }
    }
  }
  double sum = 0;
  for (int l = 1; l <= cur.L; l++) sum += cur.Ml[l] * cur.Ml[l];
  const double gamma = sum == 0 ? 1 : std::sqrt(Rm0 / sum);
  for (int l = 1; l <= cur.L; l++) cur.Ml[l] = gamma * cur.Ml[l];
}

struct SynthOpts {
  double cEnv = 0.7, lowBlend = 0.4, wRand = 0.25;
  int kernelD = 19;
  double kernelGamma = 0.6, interpTol = 0.2, hfLiftDb = 3, hfLiftF1 = 2200;
};

inline double hf_gain(double w, const SynthOpts& o) {
  if (o.hfLiftDb == 0) return 1;
  const double f = (w * 8000) / (2 * M_PI);
  if (f <= o.hfLiftF1) return 1;
  const double t = std::min(1.0, (f - o.hfLiftF1) / (3700 - o.hfLiftF1));
  return std::pow(10.0, (o.hfLiftDb * t) / 20);
}

inline std::array<double, 58> envelope_phase(const Parms& mp, int maxl, const SynthOpts& o) {
  const int D = std::min(19, o.kernelD), OFF = D;
  std::array<double, 57 + 2 * 19 + 2> B{};
  const int Blen = 57 + 2 * D + 2;
  const int L = mp.L;
  double mean = 0;
  for (int l = 1; l <= L; l++) {
    const double m = mp.Ml[l];
    const double b = m > 1e-6 ? std::log2(m) : -20;
    B[l + OFF] = b;
    mean += b;
  }
  if (L > 0) {
    mean /= L;
    for (int l = 1; l <= L; l++) B[l + OFF] -= mean;
  }
  double decay = 1;
  const double BL = B[L + OFF];
  for (int l = L + 1; l <= L + D && l + OFF < Blen; l++) {
    decay *= o.kernelGamma;
    B[l + OFF] = BL * decay;
  }
  for (int l = 1; l <= D; l++) B[-l + OFF] = B[l + OFF];
  std::array<double, 58> env{};
  for (int ell = 1; ell <= maxl; ell++) {
    double e = 0;
    for (int m = 1; m <= D; m += 2) {
      const int hi = ell + m + OFF;
      e += ((2 / M_PI) * ((hi < Blen ? B[hi] : 0) - B[ell - m + OFF])) / m;
    }
    env[ell] = e;
  }
  return env;
}

// mbe_synthesizeSpeechf. `opts` null = mbelib exactly.
inline void synthesize_speech(float* out, Parms& cur, Parms& prev, int uvquality, const Rng& rand, const SynthOpts* opts) {
  using tables::Ws;
  const int N = 160;
  auto randPhase = [&] { return rand() * (M_PI * 2) - M_PI; };
  auto uvNoise = [&] { return opts ? rand() - 0.5 : rand(); };
  const double uvthreshold = (2700.0 * M_PI) / 4000;
  const double uvsine = 1.3591409 * M_E, uvrand = 2.0;
  if (uvquality < 1 || uvquality > 64) uvquality = 3;
  const double loguvquality = uvquality == 1 ? 1 / M_E : std::log(double(uvquality)) / uvquality;
  const double uvstep = 1.0 / uvquality, qfactor = loguvquality, uvoffset = (uvstep * (uvquality - 1)) / 2;

  int numUv = 0;
  for (int l = 1; l <= cur.L; l++)
    if (cur.Vl[l] == 0) numUv++;
  const double cw0 = cur.w0, pw0 = prev.w0;
  std::fill(out, out + N, 0.f);

  int maxl;
  if (cur.L > prev.L) {
    maxl = cur.L;
    for (int l = prev.L + 1; l <= maxl; l++) {
      prev.Ml[l] = 0;
      prev.Vl[l] = 1;
    }
  } else {
    maxl = prev.L;
    for (int l = cur.L + 1; l <= maxl; l++) {
      cur.Ml[l] = 0;
      cur.Vl[l] = 1;
    }
  }

  std::array<double, 58> env{};
  if (opts) env = envelope_phase(cur, maxl, *opts);
  const double rho = cur.L > 0 ? double(numUv) / cur.L : 0;
  for (int l = 1; l <= 56; l++) {
    cur.PSIl[l] = prev.PSIl[l] + (pw0 + cw0) * ((l * N) / 2.0);
    if (opts) {
      if (l <= cur.L / 4) cur.PHIl[l] = cur.PSIl[l] + opts->lowBlend * opts->cEnv * env[l];
      else cur.PHIl[l] = cur.PSIl[l] + opts->cEnv * env[l] + opts->wRand * rho * randPhase();
    } else if (l <= cur.L / 4) {
      cur.PHIl[l] = cur.PSIl[l];
    } else {
      cur.PHIl[l] = cur.PSIl[l] + (numUv * randPhase()) / cur.L;
    }
  }

  std::array<double, NL> mNew{}, mOld{};
  for (int l = 1; l <= maxl; l++) {
    mNew[l] = cur.Ml[l] * (opts ? hf_gain(cw0 * l, *opts) : 1);
    mOld[l] = prev.Ml[l] * (opts ? hf_gain(pw0 * l, *opts) : 1);
  }

  double rphase[64], rphase2[64];
  for (int l = 1; l <= maxl; l++) {
    const double cw0l = cw0 * l, pw0l = pw0 * l;
    if (cur.Vl[l] == 0 && prev.Vl[l] == 1) {
      for (int i = 0; i < uvquality; i++) rphase[i] = randPhase();
      for (int n = 0; n < N; n++) {
        const double C1 = Ws[n + N] * mOld[l] * std::cos(pw0l * n + prev.PHIl[l]);
        double C3 = 0;
        for (int i = 0; i < uvquality; i++) {
          C3 += std::cos(cw0 * n * (l + i * uvstep - uvoffset) + rphase[i]);
          if (cw0l > uvthreshold) C3 += (cw0l - uvthreshold) * uvrand * uvNoise();
        }
        C3 = C3 * uvsine * Ws[n] * mNew[l] * qfactor;
        out[n] = float(out[n] + (C1 + C3));
      }
    } else if (cur.Vl[l] == 1 && prev.Vl[l] == 0) {
      for (int i = 0; i < uvquality; i++) rphase[i] = randPhase();
      for (int n = 0; n < N; n++) {
        const double C1 = Ws[n] * mNew[l] * std::cos(cw0l * (n - N) + cur.PHIl[l]);
        double C3 = 0;
        for (int i = 0; i < uvquality; i++) {
          C3 += std::cos(pw0 * n * (l + i * uvstep - uvoffset) + rphase[i]);
          if (pw0l > uvthreshold) C3 += (pw0l - uvthreshold) * uvrand * uvNoise();
        }
        C3 = C3 * uvsine * Ws[n + N] * mOld[l] * qfactor;
        out[n] = float(out[n] + (C1 + C3));
      }
    } else if (cur.Vl[l] == 1 || prev.Vl[l] == 1) {
      if (opts && std::fabs(cw0 - pw0) < opts->interpTol * cw0) {
        double Dpl = cur.PHIl[l] - prev.PHIl[l] - (pw0 + cw0) * l * (N / 2.0);
        Dpl -= 2 * M_PI * std::floor((Dpl + M_PI) / (2 * M_PI));
        const double THa = pw0l + Dpl / N, THb = ((cw0 - pw0) * l) / (2 * N), Mb = (mNew[l] - mOld[l]) / N;
        for (int n = 0; n < N; n++) out[n] = float(out[n] + (mOld[l] + n * Mb) * std::cos(prev.PHIl[l] + (THa + THb * n) * n));
      } else {
        for (int n = 0; n < N; n++) {
          const double C1 = Ws[n + N] * mOld[l] * std::cos(pw0l * n + prev.PHIl[l]);
          const double C2 = Ws[n] * mNew[l] * std::cos(cw0l * (n - N) + cur.PHIl[l]);
          out[n] = float(out[n] + (C1 + C2));
        }
      }
    } else {
      for (int i = 0; i < uvquality; i++) rphase[i] = randPhase();
      for (int i = 0; i < uvquality; i++) rphase2[i] = randPhase();
      for (int n = 0; n < N; n++) {
        double C3 = 0;
        for (int i = 0; i < uvquality; i++) {
          C3 += std::cos(pw0 * n * (l + i * uvstep - uvoffset) + rphase[i]);
          if (pw0l > uvthreshold) C3 += (pw0l - uvthreshold) * uvrand * uvNoise();
        }
        C3 = C3 * uvsine * Ws[n + N] * mOld[l] * qfactor;
        double C4 = 0;
        for (int i = 0; i < uvquality; i++) {
          C4 += std::cos(cw0 * n * (l + i * uvstep - uvoffset) + rphase2[i]);
          if (cw0l > uvthreshold) C4 += (cw0l - uvthreshold) * uvrand * uvNoise();
        }
        C4 = C4 * uvsine * Ws[n] * mNew[l] * qfactor;
        out[n] = float(out[n] + (C3 + C4));
      }
    }
  }
}

// mbelib's float → short gain (×7, clipped at ±32760), scaled to [-1, 1].
inline void to_unit(float* buf, int n) {
  for (int i = 0; i < n; i++) {
    double a = 7.0 * buf[i];
    a = std::clamp(a, -32760.0, 32760.0);
    buf[i] = float(a / 32768);
  }
}

// mbe_decodeImbe4400Parms: 0 valid, 1 bad pitch / L.
inline int decode_imbe4400_parms(const uint8_t* d, Parms& cur, Parms& prev) {
  using namespace tables;
  uint8_t bb[58][12] = {};
  double Cik[7][11] = {}, Gm[7] = {}, Ri[7] = {};
  std::array<double, NL> Tl{}, flokl{}, deltal{};
  std::array<int, NL> intkl{};

  cur.repeat = prev.repeat;
  const int b0 = (d[0] << 7) | (d[1] << 6) | (d[2] << 5) | (d[3] << 4) | (d[4] << 3) | (d[5] << 2) | (d[85] << 1) | d[86];
  if (b0 > 207) return 1;
  cur.w0 = (4 * M_PI) / (b0 + 39.5);
  const int L = int(0.9254 * int(M_PI / cur.w0 + 0.25));
  if (L > 56 || L < 9) return 1;
  cur.L = L;
  const int L9 = L - 9;
  const int K = L < 37 ? (L + 2) / 3 : 12;
  cur.K = K;

  const int boBase = L9 * 79 * 2;
  for (int i = 6, p = 0; i < 85; i++, p++) bb[bo[boBase + 2 * p]][bo[boBase + 2 * p + 1]] = d[i];

  int j = 1, k = K - 1;
  for (int i = 1; i <= L; i++) {
    cur.Vl[i] = int8_t(bb[1][k]);
    if (j == 3) {
      j = 1;
      k = k > 0 ? k - 1 : 0;
    } else {
      j++;
    }
  }

  const int b2 = (bb[2][5] << 5) | (bb[2][4] << 4) | (bb[2][3] << 3) | (bb[2][2] << 2) | (bb[2][1] << 1) | bb[2][0];
  Gm[1] = B2[b2];
  const int baBase = L9 * 5 * 2;
  for (int i = 2; i < 7; i++) {
    const int ba1 = int(ba[baBase + (i - 2) * 2]);
    const double ba2 = ba[baBase + (i - 2) * 2 + 1];
    int bm = 0;
    for (int jj = ba1 - 1; jj >= 0; jj--) bm = (bm << 1) | bb[i + 1][jj];
    Gm[i] = ba2 * (bm - std::pow(2.0, ba1 - 1) + 0.5);
  }
  for (int i = 1; i <= 6; i++) {
    double sum = 0;
    for (int m = 1; m <= 6; m++) sum += (m == 1 ? 1 : 2) * Gm[m] * std::cos((M_PI * (m - 1) * (i - 0.5)) / 6);
    Ri[i] = sum;
  }
  int m = 8;
  for (int i = 1; i <= 6; i++) {
    Cik[i][1] = Ri[i];
    for (k = 2; k <= ImbeJi[L9 * 6 + i - 1]; k++) {
      const int Bm = hoba[L9 * 50 + m - 8];
      if (Bm == 0) {
        Cik[i][k] = 0;
      } else {
        int bm = 0;
        for (int b = 0; b < Bm; b++) bm = (bm << 1) | bb[m][Bm - b - 1];
        Cik[i][k] = quantstep[Bm - 1] * standdev[k - 2] * (bm - std::pow(2.0, Bm - 1) + 0.5);
      }
      m++;
    }
  }
  int l = 1;
  for (int i = 1; i <= 6; i++) {
    const int ji = ImbeJi[L9 * 6 + i - 1];
    for (j = 1; j <= ji; j++) {
      double sum = 0;
      for (k = 1; k <= ji; k++) sum += (k == 1 ? 1 : 2) * Cik[i][k] * std::cos((M_PI * (k - 1) * (j - 0.5)) / ji);
      Tl[l] = sum;
      l++;
    }
  }
  const double rho = cur.L <= 15 ? 0.4 : cur.L <= 24 ? 0.03 * cur.L - 0.05 : 0.7;
  if (cur.L > prev.L) {
    for (l = prev.L + 1; l <= cur.L; l++) {
      prev.Ml[l] = prev.Ml[prev.L];
      prev.log2Ml[l] = prev.log2Ml[prev.L];
    }
  }
  double Sum77 = 0;
  for (l = 1; l <= cur.L; l++) {
    flokl[l] = (double(prev.L) / cur.L) * l;
    intkl[l] = int(flokl[l]);
    deltal[l] = flokl[l] - intkl[l];
    Sum77 += (1 - deltal[l]) * prev.log2Ml[intkl[l]] + deltal[l] * prev.log2Ml[intkl[l] + 1];
  }
  Sum77 = (rho / cur.L) * Sum77;
  for (l = 1; l <= cur.L; l++) {
    const double c1 = rho * (1 - deltal[l]) * prev.log2Ml[intkl[l]];
    const double c2 = rho * deltal[l] * prev.log2Ml[intkl[l] + 1];
    cur.log2Ml[l] = Tl[l] + c1 + c2 - Sum77;
    cur.Ml[l] = std::pow(2.0, cur.log2Ml[l]);
  }
  return 0;
}

enum class Kind { Voice, Repeat, Muted };

// One IMBE stream (a P25 Phase 1 call). MbeDecoder.imbe of the TS.
class Decoder {
 public:
  enum class Profile { Mbelib, Enhanced };
  explicit Decoder(Rng rand, Profile profile = Profile::Enhanced, int uvquality = 3)
      : rand_(std::move(rand)), uvq_(uvquality), profile_(profile) {
    reset();
  }
  void reset() {
    init_parms(cur_, prev_, enh_);
    er_ = 0;
  }

  // 88 information bits → 160 samples at mbelib's float scale (to_unit() after).
  Kind imbe(const uint8_t* d, int e0, int et, bool erased, float* out) {
    const SynthOpts* so = profile_ == Profile::Enhanced ? &opts_ : nullptr;
    if (profile_ == Profile::Mbelib) {
      const int errs2 = erased ? 99 : et;
      const int bad = decode_imbe4400_parms(d, cur_, prev_);
      Kind kind = Kind::Voice;
      if (bad == 1 || errs2 > 5) {
        use_last_parms(cur_, prev_);
        cur_.repeat++;
        kind = Kind::Repeat;
      } else {
        cur_.repeat = 0;
      }
      if (cur_.repeat <= 3) {
        move_parms(cur_, prev_);
        spectral_amp_enhance(cur_);
        synthesize_speech(out, cur_, enh_, uvq_, rand_, nullptr);
        move_parms(cur_, enh_);
      } else {
        std::fill(out, out + FRAME_SAMPLES, 0.f);
        init_parms(cur_, prev_, enh_);
        kind = Kind::Muted;
      }
      return kind;
    }
    if (!erased) er_ = 0.95 * er_ + 0.000365 * et;
    if (er_ > 0.0875) {
      fade_out(out, so);
      return Kind::Muted;
    }
    const int bad = erased ? 1 : decode_imbe4400_parms(d, cur_, prev_);
    if (bad == 1 || erased || e0 >= 3 || et >= 10 + 40 * er_) {
      if (prev_.repeat >= 4) {
        fade_out(out, so);
        return Kind::Muted;
      }
      use_last_parms(cur_, prev_);
      cur_.repeat = prev_.repeat + 1;
      speak(out, so);
      return Kind::Repeat;
    }
    cur_.repeat = 0;
    speak(out, so);
    return Kind::Voice;
  }

 private:
  void speak(float* out, const SynthOpts* so) {
    move_parms(cur_, prev_);
    spectral_amp_enhance(cur_);
    synthesize_speech(out, cur_, enh_, uvq_, rand_, so);
    move_parms(cur_, enh_);
  }
  void fade_out(float* out, const SynthOpts* so) {
    use_last_parms(cur_, prev_);
    for (int l = 0; l <= 56; l++) {
      cur_.Ml[l] = 0;
      cur_.Vl[l] = 0;
    }
    synthesize_speech(out, cur_, enh_, uvq_, rand_, so);
    move_parms(cur_, enh_);
    use_last_parms(cur_, prev_);
  }

  Rng rand_;
  int uvq_;
  Profile profile_;
  SynthOpts opts_;
  Parms cur_, prev_, enh_;
  double er_ = 0;
};

}  // namespace mbe
