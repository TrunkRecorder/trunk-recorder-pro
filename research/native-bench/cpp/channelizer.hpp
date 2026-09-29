// C++ port of src/engine/channelizer.ts: overlap-save multi-head
// fast-convolution channelizer (after CyberEther's filter_engine). One forward
// N-point FFT per wideband block is shared by every head; each head costs an
// M-bin window × filter multiply, an M-point inverse FFT (decimating by
// D = N/M) and a phase rotation. f32 throughout (the TS original is f64).
#pragma once
#include <cmath>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <vector>

#include "fft.hpp"

class Channelizer {
 public:
  using Sink = std::function<void(const float* iq, int n)>;  // interleaved, n complex samples

  Channelizer(double fs, double minOutputRate, const std::string& fftKind, int fftSize = 16384, int taps = 4097,
              double historyS = 1.0)
      : fs_(fs), n_(fftSize), p_(taps) {
    int d = 1;
    while (d * 2 <= n_ / 64 && fs / (d * 2) >= minOutputRate) d *= 2;
    decim_ = d;
    m_ = n_ / d;
    l_ = n_ - p_ + 1;
    if ((p_ - 1) % d || l_ % d || l_ <= 0) throw std::runtime_error("channelizer: taps/FFT size vs decimation");
    outputRate_ = fs / d;
    fwd_ = make_fft(fftKind, n_);
    inv_ = make_fft(fftKind, m_);
    xRe_ = fft_alloc_split(n_);
    xIm_ = xRe_ + n_;
    histCap_ = std::max(1, int(std::ceil(historyS * fs / l_)));
    spectra_.assign(size_t(histCap_) * 2 * n_, 0.f);
    hist_.assign(2 * (p_ - 1), 0.f);
  }
  ~Channelizer() {
    fft_free(xRe_);
    for (auto& [id, h] : heads_) fft_free(h.re);
  }

  double outputRate() const { return outputRate_; }
  int decim() const { return decim_; }
  int fftSize() const { return n_; }
  int headCount() const { return int(heads_.size()); }

  // `prerollS` replays up to that much stored air first (as the history holds),
  // so a channel opened by a grant starts before the grant arrived. The
  // replay runs inside this call. `startSample` (optional) gets the absolute
  // input sample the head's first output corresponds to.
  int addHead(double offsetHz, double cutoffHz, Sink sink, double prerollS = 0, uint64_t* startSample = nullptr) {
    long bin = std::lround(offsetHz / fs_ * n_);
    double residualHz = offsetHz - bin * fs_ / n_;
    Head h;
    h.bin = int(((bin % n_) + n_) % n_);
    h.residual = -2 * M_PI * residualHz / outputRate_;
    h.filter = &filterFor(cutoffHz);
    h.re = fft_alloc_split(m_);
    h.im = h.re + m_;
    h.out.assign(2 * (l_ / decim_), 0.f);
    h.sink = std::move(sink);
    int id = nextId_++;
    // Inside a block the current spectrum is already stored (and counted); the
    // replay is the blocks before it, and the block loop then runs this one.
    const int stored = historyCount_ - (inBlock_ ? 1 : 0);
    const int replay = std::min<int>(stored, int(std::ceil(prerollS * fs_ / l_)));
    if (startSample) *startSample = (block_ - replay) * uint64_t(l_);
    Head& head = heads_.emplace(id, std::move(h)).first->second;
    for (int k = replay; k >= 1; k--) runHead(head, block_ - k);
    return id;
  }
  // Safe from inside a sink: heads are only erased between blocks.
  void removeHead(int id) {
    auto it = heads_.find(id);
    if (it == heads_.end() || it->second.dead) return;
    it->second.dead = true;
    if (!inBlock_) reap();
  }

  // RTL-SDR native unsigned 8-bit interleaved IQ; `n` complex samples.
  void pushU8(const uint8_t* u8, size_t n) {
    const int base = p_ - 1;
    size_t i = 0;
    while (i < n) {
      int take = int(std::min<size_t>(l_ - fill_, n - i));
      float* re = xRe_ + base + fill_;
      float* im = xIm_ + base + fill_;
      const uint8_t* s = u8 + 2 * i;
      for (int k = 0; k < take; k++) {
        re[k] = (s[2 * k] - 127.5f) * (1.f / 127.5f);
        im[k] = (s[2 * k + 1] - 127.5f) * (1.f / 127.5f);
      }
      fill_ += take;
      i += take;
      if (fill_ == l_) runBlock();
    }
  }

 private:
  struct Filter {
    std::vector<float> re, im;
  };
  struct Head {
    int bin = 0;
    double residual = 0;
    const Filter* filter = nullptr;
    bool dead = false;
    float* re = nullptr;
    float* im = nullptr;
    std::vector<float> out;
    Sink sink;
  };

  const Filter& filterFor(double cutoffHz) {
    long key = std::lround(cutoffHz);
    auto it = filters_.find(key);
    if (it != filters_.end()) return it->second;
    // Blackman windowed sinc → N-point spectrum → keep the M bins around DC,
    // with the 1/N overlap-save IFFT scale folded in. Designed in f64.
    std::vector<double> re(n_, 0.0), im(n_, 0.0);
    double fc = cutoffHz / fs_, sum = 0;
    for (int i = 0; i < p_; i++) {
      double k = i - (p_ - 1) / 2.0;
      double sinc = k == 0 ? 2 * fc : std::sin(2 * M_PI * fc * k) / (M_PI * k);
      double w = 0.42 - 0.5 * std::cos(2 * M_PI * i / (p_ - 1)) + 0.08 * std::cos(4 * M_PI * i / (p_ - 1));
      re[i] = sinc * w;
      sum += re[i];
    }
    for (int i = 0; i < p_; i++) re[i] /= sum;
    // Plain f64 DFT via the f32 backend would lose precision in the stopband;
    // a direct O(N·P) DFT for the M kept bins is cheap and exact enough.
    Filter f;
    f.re.resize(m_);
    f.im.resize(m_);
    for (int k = 0; k < m_; k++) {
      int src = k < m_ / 2 ? k : n_ - m_ + k;
      double ar = 0, ai = 0;
      for (int i = 0; i < p_; i++) {
        double ph = -2 * M_PI * double((long(src) * i) % n_) / n_;
        ar += re[i] * std::cos(ph);
        ai += re[i] * std::sin(ph);
      }
      f.re[k] = float(ar / n_);
      f.im[k] = float(ai / n_);
    }
    return filters_.emplace(key, std::move(f)).first->second;
  }

  void runBlock() {
    const int P1 = p_ - 1;
    // Overlap-save: the first P−1 samples are the previous block's tail.
    // hist_ holds the previous tail: P−1 re, then P−1 im.
    std::copy(hist_.begin(), hist_.begin() + P1, xRe_);
    std::copy(hist_.begin() + P1, hist_.end(), xIm_);
    std::copy(xRe_ + n_ - P1, xRe_ + n_, hist_.begin());
    std::copy(xIm_ + n_ - P1, xIm_ + n_, hist_.begin() + P1);
    fwd_->forward(xRe_, xIm_);
    float* slot = spectra_.data() + size_t(block_ % histCap_) * 2 * n_;
    std::copy(xRe_, xRe_ + n_, slot);
    std::copy(xIm_, xIm_ + n_, slot + n_);
    if (historyCount_ < histCap_) historyCount_++;
    inBlock_ = true;
    for (auto& [id, h] : heads_)
      if (!h.dead) runHead(h, block_);
    inBlock_ = false;
    reap();
    block_++;
    fill_ = 0;
  }

  void reap() {
    for (auto it = heads_.begin(); it != heads_.end();) {
      if (it->second.dead) {
        fft_free(it->second.re);
        it = heads_.erase(it);
      } else {
        ++it;
      }
    }
  }

  void runHead(Head& h, uint64_t b) {
    const float* sRe = spectra_.data() + size_t(b % histCap_) * 2 * n_;
    const float* sIm = sRe + n_;
    const float* fr = h.filter->re.data();
    const float* fi = h.filter->im.data();
    const int half = m_ / 2, mask = n_ - 1;
    for (int k = 0; k < m_; k++) {
      int off = k < half ? k : k - m_;
      int src = (h.bin + off + n_) & mask;
      float a = sRe[src], c = sIm[src];
      h.re[k] = a * fr[k] - c * fi[k];
      h.im[k] = a * fi[k] + c * fr[k];
    }
    inv_->inverse(h.re, h.im);
    // Overlap-save shift e^{-j2π·bin·L·b/N} (integer mod N, exact forever) +
    // residual NCO from the absolute output index.
    double shiftTurns = double(((uint64_t(h.bin) * l_) % n_) * (b % n_) % n_) / n_;
    const int nOut = l_ / decim_;
    const double firstOut = double(b) * nOut;
    const int keepFrom = (p_ - 1) / decim_;
    double ph = -2 * M_PI * shiftTurns + std::fmod(h.residual * firstOut, 2 * M_PI);
    const float rc = float(std::cos(h.residual)), rs = float(std::sin(h.residual));
    float cr = float(std::cos(ph)), ci = float(std::sin(ph));
    float* out = h.out.data();
    for (int j = 0; j < nOut; j++) {
      float a = h.re[keepFrom + j], c = h.im[keepFrom + j];
      out[2 * j] = a * cr - c * ci;
      out[2 * j + 1] = a * ci + c * cr;
      float t = cr * rc - ci * rs;
      ci = cr * rs + ci * rc;
      cr = t;
    }
    h.sink(out, nOut);
  }

  double fs_, outputRate_;
  int n_, p_, m_, l_, decim_;
  std::unique_ptr<Fft> fwd_, inv_;
  float *xRe_, *xIm_;
  std::vector<float> hist_;
  std::vector<float> spectra_;
  int histCap_;
  int historyCount_ = 0;
  bool inBlock_ = false;
  int fill_ = 0;
  uint64_t block_ = 0;
  std::map<int, Head> heads_;
  std::map<long, Filter> filters_;
  int nextId_ = 1;
};
