// Streaming π/4-DQPSK (CQPSK) receiver — the algorithm of freq-finder's
// demodPi4Dqpsk (src/vendor/ff/demod.ts), restructured to see each sample
// once instead of re-demodulating overlapping 0.9 s windows:
//   carrier-offset tracker (smoothed arg Σ x[n]·x*[n−1]) → NCO derotate
//   → RRC matched filter (α 0.35, 11 symbols) → Gardner loop (PI, lerp)
//   → [optional T/2 CMA equaliser — simulcast/LSM, see Pi4Options]
//   → differential detection with a 4th-power residual-turn estimate
//   → dibits → P25 frame-sync counter (both polarities, ≤4 bit errors).
// It also decodes C4FM: π/4 phase steps are C4FM's levels integrated over
// a symbol, which is why the TS receiver picks it for these channels too.
#pragma once
#include <algorithm>
#include <cmath>
#include <complex>
#include <cstdint>
#include <functional>
#include <vector>

struct Pi4Options {
  // Fractionally spaced (T/2) CMA equaliser after the timing loop: `eqTaps`
  // taps (odd; 0 = off) over alternating mid-symbol / symbol samples. π/4-DQPSK
  // symbols have constant modulus, so CMA needs no decisions and is blind to
  // carrier phase — the differential detector after it doesn't care either.
  int eqTaps = 0;
  float eqMu = 0.002f;
  // Soft bits: false = phase only (|ri|, |rr| ÷ |d|), true = also weighted by
  // the symbol's amplitude (a faded symbol counts less).
  bool softAmplitude = false;
  // Coherent: decision-directed PLL + decision-feedback differential detection
  // (about 2–3 dB better than plain differential in noise).
  bool coherent = false;
  float pllKp = 0.04f, pllKi = 0.0004f;
};

class Pi4Demod {
 public:
  explicit Pi4Demod(double fs, Pi4Options opt = {}, double baud = 4800, double alpha = 0.35) : sps_(fs / baud), opt_(opt) {
    if (opt_.eqTaps > 0) {
      if (opt_.eqTaps % 2 == 0) opt_.eqTaps++;
      eqX_.assign(size_t(opt_.eqTaps), {0.f, 0.f});
      eqW_.assign(size_t(opt_.eqTaps), {0.f, 0.f});
      eqW_[size_t(opt_.eqTaps / 2)] = {1.f, 0.f};
    }
    const int maxTaps = 512;
    int persym = std::max(1, int(std::lround(sps_)));
    int span = std::max(4, std::min(11, maxTaps / persym));
    int len = persym * span;
    if (len % 2 == 0) len++;
    taps_.resize(len);
    double mid = (len - 1) / 2.0, sum = 0;
    for (int i = 0; i < len; i++) {
      double t = (i - mid) / sps_, v;
      if (std::fabs(t) < 1e-8) {
        v = 1 - alpha + 4 * alpha / M_PI;
      } else if (std::fabs(std::fabs(t) - 1 / (4 * alpha)) < 1e-8) {
        double a = M_PI / (4 * alpha);
        v = alpha / M_SQRT2 * ((1 + 2 / M_PI) * std::sin(a) + (1 - 2 / M_PI) * std::cos(a));
      } else {
        double pt = M_PI * t;
        v = (std::sin(pt * (1 - alpha)) + 4 * alpha * t * std::cos(pt * (1 + alpha))) / (pt * (1 - (4 * alpha * t) * (4 * alpha * t)));
      }
      taps_[i] = float(v);
      sum += v;
    }
    for (auto& t : taps_) t = float(t / sum);
    xi_.assign(len - 1, 0.f);
    xq_.assign(len - 1, 0.f);
  }

  uint64_t symbols = 0, syncs = 0, eqResets = 0;
  std::vector<uint64_t> syncAt;  // symbol index of each sync (for spacing checks)
  // Every decided dibit, in order, with the channel-sample instant it was
  // sampled at (input sample index; the matched filter's delay removed) and
  // each bit's reliability (distance from its decision boundary, ≥ 0).
  std::function<void(uint8_t dibit, double sample, float relHi, float relLo)> onDibit;

  void push(const float* iq, int n) {
    // 1. Carrier offset: smoothed power-weighted mean phase increment.
    double br = 0, bi = 0;
    for (int k = 0; k < n; k++) {
      float a = iq[2 * k], b = iq[2 * k + 1];
      br += a * lastR_ + b * lastI_;
      bi += b * lastR_ - a * lastI_;
      lastR_ = a;
      lastI_ = b;
    }
    accR_ = 0.97 * accR_ + br;
    accI_ = 0.97 * accI_ + bi;
    const double w = (accR_ == 0 && accI_ == 0) ? 0 : std::atan2(accI_, accR_);
    // 2. Derotate (phasor recurrence, renormalised per block) into the FIR input.
    const int T = int(taps_.size());
    size_t base = xi_.size();
    xi_.resize(base + n);
    xq_.resize(base + n);
    float cr = float(std::cos(ncoPh_)), ci = float(std::sin(ncoPh_));
    const float wr = float(std::cos(-w)), wi = float(std::sin(-w));
    for (int k = 0; k < n; k++) {
      float a = iq[2 * k], b = iq[2 * k + 1];
      xi_[base + k] = a * cr - b * ci;
      xq_[base + k] = a * ci + b * cr;
      float t = cr * wr - ci * wi;
      ci = cr * wi + ci * wr;
      cr = t;
    }
    ncoPh_ = std::remainder(ncoPh_ - w * n, 2 * M_PI);
    // 3. RRC matched filter.
    size_t yb = yi_.size();
    yi_.resize(yb + n);
    yq_.resize(yb + n);
    const float* tp = taps_.data();
    for (int j = 0; j < n; j++) {
      const float* xa = xi_.data() + j;
      const float* xb = xq_.data() + j;
      float si = 0, sq = 0;
      for (int k = 0; k < T; k++) {
        si += xa[k] * tp[k];
        sq += xb[k] * tp[k];
      }
      yi_[yb + j] = si;
      yq_[yb + j] = sq;
    }
    xi_.erase(xi_.begin(), xi_.begin() + n);
    xq_.erase(xq_.begin(), xq_.begin() + n);
    // 4. Gardner timing loop over the filtered stream.
    gardner();
  }

 private:
  float at(const std::vector<float>& y, double posAbs) const {
    double p = posAbs - yBase_;
    size_t i = size_t(p);
    float f = float(p - double(i));
    return y[i] + (y[i + 1] - y[i]) * f;
  }

  void gardner() {
    const double end = double(yBase_ + yi_.size());
    if (!started_) {
      if (end - yBase_ < taps_.size() + 4) return;
      pos_ = yBase_ + double(taps_.size());  // past the filter's warm-up
      pi_ = at(yi_, pos_);
      pq_ = at(yq_, pos_);
      started_ = true;
    }
    const double kp = 0.02, ki = kp * kp * 0.25;
    while (pos_ + sps_ + rate_ + 2 < end) {
      double next = pos_ + sps_ + rate_;
      double mid = pos_ + (next - pos_) / 2;
      float ci = at(yi_, next), cq = at(yq_, next);
      float mi = at(yi_, mid), mq = at(yq_, mid);
      float e = (ci - pi_) * mi + (cq - pq_) * mq;
      float p = ci * ci + cq * cq;
      power_ = power_ == 0 ? p : power_ + 0.01f * (p - power_);
      float en = power_ > 0 ? std::clamp(e / power_, -1.f, 1.f) : 0.f;
      rate_ = std::clamp(rate_ - ki * en * sps_, -sps_ * 0.05, sps_ * 0.05);
      pos_ = next - kp * en * sps_;
      float si = at(yi_, pos_), sq = at(yq_, pos_);
      if (opt_.eqTaps > 0) equalise(mi, mq, si, sq);
      else symbol(si, sq, pos_);
      pi_ = si;
      pq_ = sq;
      // No sync for ~1 s: the loop may sit on a wrong equilibrium (C4FM through
      // a CQPSK receiver is only half matched). Re-pick the phase by energy, as
      // the TS receiver does on every window.
      if (symbols - lastSync_ > 4800 && symbols - lastReacq_ > 2400) reacquire();
    }
    // Drop what the loop can no longer reach (keeping enough for reacquire()).
    size_t keep = size_t(kReacqSymbols * sps_) + 8;
    size_t drop = size_t(std::max(0.0, std::floor(pos_ - keep) - double(yBase_)));
    if (drop > 4096) {
      yi_.erase(yi_.begin(), yi_.begin() + drop);
      yq_.erase(yq_.begin(), yq_.begin() + drop);
      yBase_ += drop;
    }
  }

  static constexpr int kReacqSymbols = 1000;

  // Push one mid-symbol and one symbol sample (power-normalised), output the
  // equalised symbol at the centre tap (eqTaps/4 symbols late) and adapt.
  void equalise(float mi, float mq, float si, float sq) {
    const float g = power_ > 0 ? 1.f / std::sqrt(power_) : 1.f;
    const size_t N = eqX_.size();
    for (size_t i = N - 1; i >= 2; i--) eqX_[i] = eqX_[i - 2];
    eqX_[1] = {mi * g, mq * g};
    eqX_[0] = {si * g, sq * g};
    std::complex<float> y{0.f, 0.f};
    for (size_t i = 0; i < N; i++) y += eqW_[i] * eqX_[i];
    // Clipped CMA error (noise before a call must not throw the taps about),
    // and a reset to a plain centre tap if they diverge anyway.
    const float e = std::clamp(std::norm(y) - 1.f, -1.f, 1.f);
    const std::complex<float> k = opt_.eqMu * e * y;
    float wn = 0;
    for (size_t i = 0; i < N; i++) {
      eqW_[i] -= k * std::conj(eqX_[i]);
      wn += std::norm(eqW_[i]);
    }
    if (!(wn < 4.f) || !(wn > 0.05f)) {
      std::fill(eqW_.begin(), eqW_.end(), std::complex<float>{0.f, 0.f});
      eqW_[N / 2] = {1.f, 0.f};
      eqResets++;
    }
    // The centre tap is a symbol sample (N odd, centre even): N/4 symbols back.
    posHist_[posN_++ % 16] = pos_;
    const size_t lag = N / 4;
    const double at = posN_ > lag ? posHist_[(posN_ - 1 - lag) % 16] : pos_;
    symbol(y.real(), y.imag(), at);
  }

  void reacquire() {
    lastReacq_ = symbols;
    const double from = pos_ - kReacqSymbols * sps_;
    if (from < double(yBase_) + 1) return;
    int best = 0;
    double bestE = -1;
    for (int ph = 0; ph < 16; ph++) {
      double e = 0;
      for (int k = 0; k < kReacqSymbols; k++) {
        double p = from + (ph / 16.0 + k) * sps_;
        float i = at(yi_, p), q = at(yq_, p);
        e += i * i + q * q;
      }
      if (e > bestE) bestE = e, best = ph;
    }
    // Move the strobe to the best phase (a shift of less than one symbol).
    // (`from` is a whole number of symbols behind pos_, so phase 0 ≡ pos_.)
    double shift = std::remainder((best / 16.0) * sps_, sps_);
    pos_ += shift;
    rate_ = 0;
    pi_ = at(yi_, pos_);
    pq_ = at(yq_, pos_);
  }

  void symbol(float si, float sq, double at) {
    float dr, di;
    if (opt_.coherent) {
      // Decision-directed PLL on the 8 π/4-DQPSK phases, then decision-feedback
      // differential detection: d = z[k]·conj(p̂[k−1]) — the previous symbol
      // is its clean decided point, so only this symbol's noise counts.
      std::complex<float> z(si, sq);
      if (opt_.eqTaps == 0 && power_ > 0) z /= std::sqrt(power_);
      z *= std::polar(1.f, -cth_);
      const int m8 = int(std::lround(std::arg(z) / float(M_PI / 4))) & 7;
      const std::complex<float> p = std::polar(1.f, float(m8 * M_PI / 4));
      const float err = std::arg(z * std::conj(p));
      cfr_ = std::clamp(cfr_ + opt_.pllKi * err, -0.2f, 0.2f);
      cth_ = std::remainder(cth_ + opt_.pllKp * err + cfr_, float(2 * M_PI));
      const std::complex<float> d = z * std::conj(prevP_);
      prevP_ = p;
      dr = d.real();
      di = d.imag();
    } else {
      // Differential product d = s[k]·conj(s[k−1]).
      dr = si * dpr_ + sq * dpi_;
      di = sq * dpr_ - si * dpi_;
      dpr_ = si;
      dpi_ = sq;
    }
    float m = std::sqrt(dr * dr + di * di);
    if (m > 0 && !opt_.coherent) {
      // Residual turn from the 4th power (every ideal step^4 = e^{jπ}).
      float a = dr / m, b = di / m;
      float a2 = a * a - b * b, b2 = 2 * a * b;
      q4r_ = 0.995f * q4r_ + (a2 * a2 - b2 * b2) * m;
      q4i_ = 0.995f * q4i_ + 2 * a2 * b2 * m;
    }
    float theta = opt_.coherent ? 0.f : std::atan2(-q4i_, -q4r_) / 4;
    float c = std::cos(theta), s = std::sin(theta);
    float rr = dr * c + di * s, ri = di * c - dr * s;  // d · e^{−jθ}
    // +45° = 00, +135° = 01, −45° = 10, −135° = 11 (= P25 dibits +1 +3 −1 −3).
    uint32_t dib = ri >= 0 ? (rr > 0 ? 0b00 : 0b01) : (rr > 0 ? 0b10 : 0b11);
    symbols++;
    // Bits: hi = (ri < 0), lo = (rr ≤ 0); their boundaries are the axes, so the
    // reliabilities are |ri|, |rr| — as angles (÷|d|) or scaled by amplitude.
    if (onDibit) {
      // (Coherent d is already on the unit scale: z was normalised, p̂ is unit.)
      const float ampNorm = opt_.coherent ? 1.f : (power_ > 0 ? 1.f / power_ : 1.f);
      const float norm = opt_.softAmplitude ? ampNorm : (m > 0 ? 1.f / m : 0.f);
      onDibit(uint8_t(dib), at - (taps_.size() - 1) / 2.0, std::fabs(ri) * norm, std::fabs(rr) * norm);
    }
    sr_ = ((sr_ << 2) | dib) & 0xFFFFFFFFFFFFull;
    constexpr uint64_t FS = 0x5575F5FF77FFull, INV = 0xAAAAAAAAAAAAull;
    if (symbols - lastSync_ >= 24 && (__builtin_popcountll(sr_ ^ FS) <= 4 || __builtin_popcountll(sr_ ^ FS ^ INV) <= 4)) {
      syncs++;
      lastSync_ = symbols;
      syncAt.push_back(symbols);
    }
  }

  double sps_;
  Pi4Options opt_;
  std::vector<std::complex<float>> eqX_, eqW_;
  double posHist_[16] = {};
  size_t posN_ = 0;
  float dpr_ = 0, dpi_ = 0;
  float cth_ = 0, cfr_ = 0;
  std::complex<float> prevP_{1.f, 0.f};
  std::vector<float> taps_;
  float lastR_ = 0, lastI_ = 0;
  double accR_ = 0, accI_ = 0, ncoPh_ = 0;
  std::vector<float> xi_, xq_, yi_, yq_;
  uint64_t yBase_ = 0;
  bool started_ = false;
  double pos_ = 0, rate_ = 0;
  float power_ = 0, pi_ = 0, pq_ = 0;
  float q4r_ = 0, q4i_ = 0;
  uint64_t sr_ = 0, lastSync_ = 0, lastReacq_ = 0;
};
