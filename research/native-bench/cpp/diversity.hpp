// Receiver diversity: several receivers on one channel (CQPSK, CQPSK with the
// T/2 CMA equaliser, C4FM), each with its own framer. Frames that start at the
// same channel-sample instant (within half a TDU) are grouped; a group is
// released, in time order, once every receiver's output has passed it, and
// the consumer picks the best of the candidates — per TSBK the CRC-valid copy,
// per IMBE codeword the one the soft decoder is surest of. No receiver has to
// be right about the whole channel, only about each frame.
#pragma once
#include <algorithm>
#include <array>
#include <deque>
#include <functional>
#include <memory>
#include <optional>
#include <vector>

#include "c4fm.hpp"
#include "demod.hpp"
#include "p25_frame.hpp"
#include "p25_tsbk.hpp"
#include "p25_voice.hpp"

namespace diversity {

using Group = std::vector<const p25::Frame*>;

class Bank {
 public:
  struct Config {
    bool cqpsk = true, cqpskEq = true, c4fm = true;
    int eqTaps = 9;
    float eqMu = 0.02f;
  };

  // `rate`: channel sample rate. onGroup gets every group, oldest first.
  Bank(double rate, Config cfg, std::function<void(const Group&)> onGroup) : rate_(rate), on_(std::move(onGroup)) {
    guard_ = 36 * rate / 4800;
    // A group can be released once every receiver is this far past its start:
    // the longest frame (LDU, 0.18 s) + the C4FM receiver's latency (~0.1 s) + slack.
    hold_ = 0.35 * rate;
    Pi4Options plain, eq;
    plain.softAmplitude = eq.softAmplitude = true;
    eq.eqTaps = cfg.eqTaps;
    eq.eqMu = cfg.eqMu;
    if (cfg.cqpsk) add(std::make_unique<Pi4Demod>(rate, plain));
    if (cfg.cqpskEq) add(std::make_unique<Pi4Demod>(rate, eq));
    if (cfg.c4fm) add(std::make_unique<C4fmDemod>(rate));
  }

  void push(const float* iq, int n) {
    for (auto& r : rx_) r->push(iq, n);
    release(false);
  }
  void flush() { release(true); }

  uint64_t groups = 0;
  std::vector<uint64_t> framesPerRx() const {
    std::vector<uint64_t> v;
    for (auto& r : rx_) v.push_back(r->frames);
    return v;
  }

 private:
  struct Rx {
    virtual ~Rx() = default;
    virtual void push(const float* iq, int n) = 0;
    p25::Framer framer;
    double progress = 0;  // sample instant of the latest dibit
    uint64_t frames = 0;
  };
  template <typename D>
  struct RxImpl : Rx {
    explicit RxImpl(std::unique_ptr<D> d) : demod(std::move(d)) {}
    void push(const float* iq, int n) override { demod->push(iq, n); }
    std::unique_ptr<D> demod;
  };
  struct Pending {
    double sample;
    std::vector<std::unique_ptr<p25::Frame>> frames;
  };

  template <typename D>
  void add(std::unique_ptr<D> d) {
    auto rx = std::make_unique<RxImpl<D>>(std::move(d));
    Rx* raw = rx.get();
    rx->demod->onDibit = [raw](uint8_t dib, double t, float h, float l) {
      raw->progress = t;
      raw->framer.push(dib, t, h, l);
    };
    rx->framer.onFrame = [this, raw](const p25::Frame& f) {
      raw->frames++;
      accept(f);
    };
    rx_.push_back(std::move(rx));
  }

  void accept(const p25::Frame& f) {
    for (auto& g : pending_)
      if (std::fabs(g.sample - f.sample) < guard_) {
        g.frames.push_back(std::make_unique<p25::Frame>(f));
        return;
      }
    // Keep pending_ sorted by time (receivers deliver with different delays).
    auto it = std::find_if(pending_.begin(), pending_.end(), [&](const Pending& g) { return g.sample > f.sample; });
    Pending g{f.sample, {}};
    g.frames.push_back(std::make_unique<p25::Frame>(f));
    pending_.insert(it, std::move(g));
  }

  void release(bool all) {
    double minProgress = 1e300;
    for (auto& r : rx_) minProgress = std::min(minProgress, r->progress);
    while (!pending_.empty() && (all || pending_.front().sample + hold_ < minProgress)) {
      Pending g = std::move(pending_.front());
      pending_.pop_front();
      // Frames already released later in time can't be emitted out of order.
      if (g.sample < lastReleased_ + guard_ && groups) continue;
      lastReleased_ = g.sample;
      Group view;
      for (auto& f : g.frames) view.push_back(f.get());
      groups++;
      on_(view);
    }
  }

  double rate_, guard_, hold_, lastReleased_ = -1e300;
  std::vector<std::unique_ptr<Rx>> rx_;
  std::deque<Pending> pending_;
  std::function<void(const Group&)> on_;
};

// ── Choosing from a group ────────────────────────────────────────────────────

// Every distinct CRC-valid TSBK in the group's TSDUs, in block order.
inline std::vector<p25::Tsbk> best_tsbks(const Group& g, int* bad = nullptr) {
  std::array<std::optional<p25::Tsbk>, 3> slot;
  int maxBlocks = 0;
  for (const auto* f : g) {
    if (f->nid.duid != p25::TSDU) continue;
    const auto r = p25::decode_tsdu(*f);
    maxBlocks = std::max(maxBlocks, int(r.good.size()) + r.bad);
    // decode_tsdu stops at the last-block flag; the good blocks are in order
    // but we don't know their indices when some failed — recover them by
    // re-decoding block by block.
    const int avail = std::min(3, (int(f->bits.size()) - 112) / 196);
    for (int b = 0; b < avail; b++) {
      p25::Tsbk t;
      p25::trellis_viterbi(f->bits.data() + 112 + b * 196, f->soft.empty() ? nullptr : f->soft.data() + 112 + b * 196, t);
      if (p25::crc16(t.data(), 12) == 0 && !slot[size_t(b)]) slot[size_t(b)] = t;
    }
  }
  std::vector<p25::Tsbk> out;
  int last = 3;
  for (int b = 0; b < 3; b++)
    if (slot[size_t(b)] && p25::tsbk_last(*slot[size_t(b)])) {
      last = b + 1;
      break;
    }
  int missing = 0;
  for (int b = 0; b < last; b++) {
    if (slot[size_t(b)]) out.push_back(*slot[size_t(b)]);
    else if (b < maxBlocks) missing++;
  }
  if (bad) *bad = missing;
  return out;
}

// The group's frame to take header information from: the one whose NID needed
// the fewest corrections.
inline const p25::Frame* best_frame(const Group& g) {
  const p25::Frame* best = g.front();
  for (const auto* f : g)
    if (f->nid.errors < best->nid.errors) best = f;
  return best;
}

// Per codeword, the candidate the soft decoder overrode least (lowest cost
// per unit of reliability), among the group's LDUs.
inline std::array<p25::ImbeParams, 9> best_imbe(const Group& g) {
  std::array<p25::ImbeParams, 9> best{};
  std::array<float, 9> score;
  score.fill(1e30f);
  for (const auto* f : g) {
    if (f->nid.duid != p25::LDU1 && f->nid.duid != p25::LDU2) continue;
    for (int k = 0; k < 9; k++) {
      uint8_t cw[144];
      float sw[144];
      p25::ldu_codeword(f->raw, k, cw, &f->rawSoft, sw);
      const auto p = p25::imbe_header_decode(cw, f->rawSoft.empty() ? nullptr : sw);
      const float s = p.meanRel > 0 ? p.cost / p.meanRel : float(p.errs);
      if (s < score[size_t(k)]) score[size_t(k)] = s, best[size_t(k)] = p;
    }
  }
  return best;
}

inline std::optional<p25::LinkControl> best_lc(const Group& g) {
  for (const auto* f : g)
    if (f->nid.duid == p25::LDU1)
      if (auto lc = p25::decode_ldu1_lc(f->raw)) return lc;
  return std::nullopt;
}
inline std::optional<p25::EncryptionSync> best_es(const Group& g) {
  for (const auto* f : g)
    if (f->nid.duid == p25::LDU2)
      if (auto es = p25::decode_ldu2_es(f->raw)) return es;
  return std::nullopt;
}

}  // namespace diversity
