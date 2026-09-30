// Streaming C4FM receiver — freq-finder's demodC4fmTracked
// (src/vendor/ff/p25/c4fm.ts) without the windows:
//   FM discriminator (Hz) → boxcar (0.9 symbol) → per-block best sampling phase
//   (max Σ|deviation| over 240 symbols, 2·sps candidate phases), unwrapped
//   across blocks and interpolated between block centres → slice on the rails:
//   centre = midpoint of the 2 %/98 % quantiles of the last ~0.5 s of symbols,
//   inner/outer threshold = 2/3 of the outer rail.
// Latency is two blocks (100 ms): block b's symbols need block b+1's phase.
#pragma once
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <deque>
#include <functional>
#include <vector>

class C4fmDemod {
 public:
  explicit C4fmDemod(double fs) : fs_(fs), sps_(fs / 4800.0) {
    box_ = std::max(1, int(std::lround(sps_ * 0.9)));
    steps_ = std::max(8, int(std::lround(sps_ * 2)));
  }

  // Every decided dibit with its channel-sample instant (boxcar delay removed).
  std::function<void(uint8_t dibit, double sample, float relHi, float relLo)> onDibit;
  uint64_t symbols = 0;

  void push(const float* iq, int n) {
    const float k = float(fs_ / (2 * M_PI));
    for (int i = 0; i < n; i++) {
      float a = iq[2 * i], b = iq[2 * i + 1];
      float f = std::atan2(b * pr_ - a * pi_, a * pr_ + b * pi_) * k;
      pr_ = a;
      pi_ = b;
      // Boxcar of length box_ (sum over the last box_ discriminator outputs).
      hist_.push_back(f);
      acc_ += f;
      if (int(hist_.size()) > box_) {
        acc_ -= hist_.front();
        hist_.pop_front();
      }
      y_.push_back(float(acc_ / hist_.size()));
    }
    // Phase for every block whose samples are all here (+1 for interpolation).
    while (double(yBase_ + y_.size()) > (nextBlock_ + 1) * kBlock * sps_ + sps_ + 2) {
      blockPhase(nextBlock_);
      nextBlock_++;
    }
    emit();
    compact();
  }

 private:
  static constexpr int kBlock = 240;

  float at(double t) const {  // absolute sample index → filtered value
    double p = t - double(yBase_);
    size_t i = size_t(p);
    float f = float(p - double(i));
    return y_[i] + (y_[i + 1] - y_[i]) * f;
  }

  void blockPhase(uint64_t b) {
    double best = -1, ph = 0;
    for (int p = 0; p < steps_; p++) {
      double cand = double(p) / steps_ * sps_;
      double e = 0;
      for (uint64_t s = b * kBlock; s < (b + 1) * kBlock; s++) {
        double t = cand + double(s) * sps_;
        if (t < double(yBase_)) continue;
        e += std::fabs(at(t) - center_);
      }
      if (e > best) best = e, ph = cand;
    }
    if (!phase_.empty()) {
      double prev = phase_.back();
      while (ph - prev > sps_ / 2) ph -= sps_;
      while (ph - prev < -sps_ / 2) ph += sps_;
    }
    phase_.push_back(ph);
  }

  // Phase at symbol s: linear between block centres (b + 0.5)·kBlock.
  double phaseAt(uint64_t s) const {
    double x = double(s) / kBlock - 0.5 - double(phaseBase_);
    if (x <= 0) return phase_.front();
    size_t b = size_t(x);
    if (b + 1 >= phase_.size()) return phase_.back();
    return phase_[b] + (phase_[b + 1] - phase_[b]) * (x - double(b));
  }

  void emit() {
    // Symbols up to the centre of the newest block with a phase are final.
    if (phase_.size() < 2) return;
    const uint64_t upTo = (phaseBase_ + phase_.size() - 1) * kBlock + kBlock / 2;
    while (nextSym_ < upTo) {
      double t = phaseAt(nextSym_) + double(nextSym_) * sps_;
      if (t < double(yBase_)) { nextSym_++; continue; }
      if (t + 1 >= double(yBase_ + y_.size())) break;
      slice(at(t), t - (box_ - 1) / 2.0);
      nextSym_++;
    }
  }

  void slice(float v, double t) {
    // Rails from the last ~0.5 s of symbols, refreshed every 240 symbols.
    soft_.push_back(v);
    if (soft_.size() > 2400) soft_.pop_front();
    if (++sinceRails_ >= kBlock && soft_.size() >= 480) {
      sinceRails_ = 0;
      tmp_.assign(soft_.begin(), soft_.end());
      size_t lo = tmp_.size() * 2 / 100, hi = std::min(tmp_.size() - 1, tmp_.size() * 98 / 100);
      std::nth_element(tmp_.begin(), tmp_.begin() + lo, tmp_.end());
      float qLo = tmp_[lo];
      std::nth_element(tmp_.begin(), tmp_.begin() + hi, tmp_.end());
      float qHi = tmp_[hi];
      center_ = (qHi + qLo) / 2;
      float outer = (qHi - qLo) / 2;
      thr_ = outer > 300 ? outer * 2 / 3 : 1200;
    }
    float x = v - center_;
    uint8_t d = x >= thr_ ? 0b01 : x >= 0 ? 0b00 : x >= -thr_ ? 0b10 : 0b11;
    symbols++;
    // Reliabilities in units of the outer rail: sign bit |x|, outer bit ||x| − thr|.
    const float scale = thr_ > 0 ? 1.f / (thr_ * 1.5f) : 1.f;
    if (onDibit) onDibit(d, t, std::fabs(x) * scale, std::fabs(std::fabs(x) - thr_) * scale);
  }

  void compact() {
    // Keep one block behind the next symbol to be emitted, and two phases.
    double keepFrom = double(nextSym_) * sps_ + (phase_.empty() ? 0 : phase_.front()) - 2 * kBlock * sps_;
    if (keepFrom > double(yBase_) + 8192) {
      size_t drop = size_t(keepFrom - double(yBase_));
      y_.erase(y_.begin(), y_.begin() + drop);
      yBase_ += drop;
    }
    while (phase_.size() > 4 && (phaseBase_ + 2) * kBlock + kBlock / 2 < nextSym_) {
      phase_.pop_front();
      phaseBase_++;
    }
  }

  double fs_, sps_;
  int box_, steps_;
  float pr_ = 0, pi_ = 0;
  std::deque<float> hist_;
  double acc_ = 0;
  std::vector<float> y_;
  uint64_t yBase_ = 0, nextBlock_ = 0, nextSym_ = 0, phaseBase_ = 0;
  std::deque<double> phase_;
  std::deque<float> soft_;
  std::vector<float> tmp_;
  int sinceRails_ = 0;
  float center_ = 0, thr_ = 1200;
};
