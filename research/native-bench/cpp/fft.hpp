// Split-complex, in-place, unscaled FFTs behind one interface, so the
// channelizer can be timed with each backend:
//   radix2 — straight port of src/engine/fft.ts (f32), no dependencies
//   fftw   — FFTW3f guru split plans (portable, GPL)
//   vdsp   — Apple Accelerate vDSP_fft_zip (macOS/iOS only)
#pragma once
#include <cmath>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

#ifdef HAVE_FFTW
#include <fftw3.h>
#endif
#ifdef __APPLE__
#include <Accelerate/Accelerate.h>
#endif

struct Fft {
  virtual ~Fft() = default;
  virtual void forward(float* re, float* im) = 0;
  virtual void inverse(float* re, float* im) = 0;
};

// Buffers every backend can use (FFTW wants its own alignment for SIMD).
inline float* fft_alloc(size_t n) {
#ifdef HAVE_FFTW
  auto* p = static_cast<float*>(fftwf_malloc(n * sizeof(float)));
#else
  auto* p = static_cast<float*>(aligned_alloc(64, ((n * sizeof(float) + 63) / 64) * 64));
#endif
  for (size_t i = 0; i < n; i++) p[i] = 0;
  return p;
}
inline void fft_free(float* p) {
#ifdef HAVE_FFTW
  fftwf_free(p);
#else
  free(p);
#endif
}

class Radix2 : public Fft {
 public:
  explicit Radix2(int n) : n_(n), cos_(n / 2), sin_(n / 2), rev_(n) {
    if (n < 2 || (n & (n - 1))) throw std::runtime_error("radix2: n not a power of two");
    for (int i = 0; i < n / 2; i++) {
      cos_[i] = float(std::cos(-2 * M_PI * i / n));
      sin_[i] = float(std::sin(-2 * M_PI * i / n));
    }
    int bits = 0;
    while ((1 << bits) < n) bits++;
    for (int i = 0; i < n; i++) {
      uint32_t r = 0;
      for (int b = 0, x = i; b < bits; b++, x >>= 1) r = (r << 1) | (x & 1);
      rev_[i] = r;
    }
  }
  void forward(float* re, float* im) override { run(re, im, 1.f); }
  void inverse(float* re, float* im) override { run(re, im, -1.f); }

 private:
  void run(float* re, float* im, float sg) {
    const int n = n_;
    for (int i = 0; i < n; i++) {
      int j = rev_[i];
      if (j > i) {
        std::swap(re[i], re[j]);
        std::swap(im[i], im[j]);
      }
    }
    for (int size = 2; size <= n; size <<= 1) {
      int half = size >> 1, step = n / size;
      for (int i = 0; i < n; i += size) {
        for (int j = i, k = 0; j < i + half; j++, k += step) {
          float c = cos_[k], s = sg * sin_[k];
          int a = j + half;
          float tr = re[a] * c - im[a] * s;
          float ti = re[a] * s + im[a] * c;
          re[a] = re[j] - tr;
          im[a] = im[j] - ti;
          re[j] += tr;
          im[j] += ti;
        }
      }
    }
  }
  int n_;
  std::vector<float> cos_, sin_;
  std::vector<uint32_t> rev_;
};

#ifdef HAVE_FFTW
// FFTW's new-array execute requires the same ii − ri separation the plan was
// made with, so every split buffer is one allocation: im = re + n (see
// fft_alloc_split). The inverse is the forward DFT with re/im swapped, which
// flips the separation's sign — hence its own plan.
class Fftw : public Fft {
 public:
  explicit Fftw(int n) : n_(n) {
    float* b = fft_alloc(2 * size_t(n));
    fftwf_iodim dim{n, 1, 1};
    fwd_ = fftwf_plan_guru_split_dft(1, &dim, 0, nullptr, b, b + n, b, b + n, FFTW_MEASURE);
    inv_ = fftwf_plan_guru_split_dft(1, &dim, 0, nullptr, b + n, b, b + n, b, FFTW_MEASURE);
    fft_free(b);
  }
  ~Fftw() override {
    fftwf_destroy_plan(fwd_);
    fftwf_destroy_plan(inv_);
  }
  void forward(float* re, float* im) override {
    check(re, im);
    fftwf_execute_split_dft(fwd_, re, im, re, im);
  }
  void inverse(float* re, float* im) override {
    check(re, im);
    fftwf_execute_split_dft(inv_, im, re, im, re);
  }

 private:
  void check(float* re, float* im) const {
    if (im != re + n_) throw std::runtime_error("fftw: split buffers must come from fft_alloc_split");
  }
  int n_;
  fftwf_plan fwd_, inv_;
};
#endif

// A split complex buffer as one allocation: re = [0, n), im = [n, 2n).
inline float* fft_alloc_split(int n) { return fft_alloc(2 * size_t(n)); }

#ifdef __APPLE__
class Vdsp : public Fft {
 public:
  explicit Vdsp(int n) {
    while ((1 << log2n_) < n) log2n_++;
    setup_ = vDSP_create_fftsetup(log2n_, kFFTRadix2);
  }
  ~Vdsp() override { vDSP_destroy_fftsetup(setup_); }
  void forward(float* re, float* im) override {
    DSPSplitComplex s{re, im};
    vDSP_fft_zip(setup_, &s, 1, log2n_, kFFTDirection_Forward);
  }
  void inverse(float* re, float* im) override {
    DSPSplitComplex s{re, im};
    vDSP_fft_zip(setup_, &s, 1, log2n_, kFFTDirection_Inverse);
  }

 private:
  vDSP_Length log2n_ = 0;
  FFTSetup setup_;
};
#endif

inline std::unique_ptr<Fft> make_fft(const std::string& kind, int n) {
  if (kind == "radix2") return std::make_unique<Radix2>(n);
#ifdef HAVE_FFTW
  if (kind == "fftw") return std::make_unique<Fftw>(n);
#endif
#ifdef __APPLE__
  if (kind == "vdsp") return std::make_unique<Vdsp>(n);
#endif
  throw std::runtime_error("unknown or unavailable FFT backend: " + kind);
}
