// Trunking above the radio — ports of the TS engine's
//   src/protocols/p25/tsbkParser.ts  (Trunk Recorder P25Parser::decode_tsbk)
//   src/trunking/callManager.ts      (monitor_systems.cc grant/update/timeout)
//   src/vendor/ff/p25Voice.ts        (P25VoiceDecoder, Phase 1 path: call
//                                     tracking over HDU/LDU/TDU → vocoder)
// Phase 2 (TDMA) is not ported yet: grants for TDMA channels are monitored.
#pragma once
#include <cstdint>
#include <cstdio>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "mbe.hpp"
#include "p25_frame.hpp"
#include "p25_tsbk.hpp"
#include "p25_voice.hpp"
#include "diversity.hpp"

namespace trunk {

// ── TSBK → messages (the opcodes the call manager and identity need) ─────────

enum class Msg { Grant, Update, UuGrant, UuUpdate, Status, SysId, PatchAdd, Other };

struct Message {
  Msg type = Msg::Other;
  double timeS = 0;
  int opcode = 0;
  long freqHz = 0;
  int talkgroup = 0;
  long source = -1;
  bool encrypted = false, emergency = false, duplex = false, mode = false;
  int priority = 0;
  bool phase2Tdma = false;
  int tdmaSlot = 0;
  int wacn = 0, sysId = 0, rfss = 0, site = 0;
  int patchSg = 0, patchGa[3] = {0, 0, 0};
};

struct FreqTable {
  long offsetHz = 0, stepHz = 0, baseHz = 0;
  bool phase2Tdma = false;
  int slotsPerCarrier = 1;
};

class TsbkParser {
 public:
  std::map<int, FreqTable> tables;

  // The band plan (IDEN tables) persists: it almost never changes, and without
  // it a grant heard before the next IDEN broadcast cannot be followed.
  // Lines: id offsetHz stepHz baseHz phase2Tdma slotsPerCarrier.
  void load(const std::string& path) {
    std::FILE* f = std::fopen(path.c_str(), "r");
    if (!f) return;
    int id, p2, slots;
    long off, step, base;
    while (std::fscanf(f, "%d %ld %ld %ld %d %d", &id, &off, &step, &base, &p2, &slots) == 6) tables[id] = {off, step, base, p2 != 0, slots};
    std::fclose(f);
  }
  void save(const std::string& path) const {
    std::FILE* f = std::fopen(path.c_str(), "w");
    if (!f) return;
    for (auto& [id, t] : tables) std::fprintf(f, "%d %ld %ld %ld %d %d\n", id, t.offsetHz, t.stepHz, t.baseHz, int(t.phase2Tdma), t.slotsPerCarrier);
    std::fclose(f);
  }

  long channelToHz(int ch) const {
    auto it = tables.find((ch >> 12) & 0xf);
    if (it == tables.end()) return 0;
    const FreqTable& t = it->second;
    const int channel = ch & 0xfff;
    return t.phase2Tdma ? t.baseHz + t.stepHz * (channel / t.slotsPerCarrier) : t.baseHz + t.stepHz * channel;
  }
  int tdmaSlot(int ch) const {
    auto it = tables.find((ch >> 12) & 0xf);
    return it != tables.end() && it->second.phase2Tdma ? (ch & 0xfff) & 1 : -1;
  }

  std::vector<Message> parse(const p25::Tsbk& blk, double timeS) {
    // The 12-byte block as a 96-bit integer; b(shift, mask) as the TS/C++.
    auto b = [&](int shift, uint64_t mask) -> uint64_t {
      uint64_t v = 0;
      for (int i = 0; i < 64 && shift + i < 96; i++) {
        const int bit = shift + i;  // bit index from the LSB of the 96-bit value
        const int byte = 11 - bit / 8;
        if ((blk[byte] >> (bit % 8)) & 1) v |= uint64_t(1) << i;
      }
      return v & mask;
    };
    std::vector<Message> out;
    Message m;
    m.timeS = timeS;
    m.opcode = int(b(88, 0x3f));
    auto opts = [&] {
      m.emergency = b(72, 0x80);
      m.encrypted = b(72, 0x40);
      m.duplex = b(72, 0x20);
      m.mode = b(72, 0x10);
      m.priority = int(b(72, 0x07));
    };
    auto setSlot = [&](int ch) {
      int s = tdmaSlot(ch);
      m.phase2Tdma = s >= 0;
      m.tdmaSlot = s >= 0 ? s : 0;
    };
    const int mfid = int(b(80, 0xff));
    switch (m.opcode) {
      case 0x00:
        if (mfid == 0x90) {
          m.type = Msg::PatchAdd;
          m.patchSg = int(b(64, 0xffff));
          m.patchGa[0] = int(b(48, 0xffff));
          m.patchGa[1] = int(b(32, 0xffff));
          m.patchGa[2] = int(b(16, 0xffff));
        } else {
          opts();
          const int ch = int(b(56, 0xffff));
          m.type = Msg::Grant;
          m.freqHz = channelToHz(ch);
          m.talkgroup = int(b(40, 0xffff));
          m.source = long(b(16, 0xffffff));
          setSlot(ch);
        }
        break;
      case 0x02:
        if (mfid == 0x90) {
          opts();
          const int ch = int(b(56, 0xffff));
          m.type = Msg::Grant;
          m.freqHz = channelToHz(ch);
          m.talkgroup = int(b(40, 0xffff));
          m.source = long(b(16, 0xffffff));
          setSlot(ch);
        } else {
          const int ch1 = int(b(64, 0xffff)), ga1 = int(b(48, 0xffff)), ch2 = int(b(32, 0xffff)), ga2 = int(b(16, 0xffff));
          const long f1 = channelToHz(ch1), f2 = channelToHz(ch2);
          m.type = Msg::Update;
          m.freqHz = f1;
          m.talkgroup = ga1;
          setSlot(ch1);
          if (f1 != f2 && ch2 != 0xffff) {
            out.push_back(m);
            m.freqHz = f2;
            m.talkgroup = ga2;
            setSlot(ch2);
          }
        }
        break;
      case 0x03:
        if (mfid == 0x90) {
          const int ch1 = int(b(64, 0xffff)), sg1 = int(b(48, 0xffff)), ch2 = int(b(32, 0xffff)), sg2 = int(b(16, 0xffff));
          const long f1 = channelToHz(ch1), f2 = channelToHz(ch2);
          m.type = Msg::Update;
          m.freqHz = f1;
          m.talkgroup = sg1;
          setSlot(ch1);
          if (f1 != f2) {
            out.push_back(m);
            m.freqHz = f2;
            m.talkgroup = sg2;
            setSlot(ch2);
          }
        } else {
          m.emergency = b(72, 0x80);
          m.encrypted = b(72, 0x40);
          const int ch1 = int(b(48, 0xffff));
          m.type = Msg::Update;
          m.freqHz = channelToHz(ch1);
          m.talkgroup = int(b(16, 0xffff));
          setSlot(ch1);
        }
        break;
      case 0x04: {
        opts();
        const int ch = int(b(64, 0xffff));
        m.type = Msg::UuGrant;
        m.freqHz = channelToHz(ch);
        m.talkgroup = int(b(40, 0xffffff));
        m.source = long(b(16, 0xffffff));
        setSlot(ch);
        break;
      }
      case 0x06: {
        const int ch = int(b(64, 0xffff));
        m.type = Msg::UuUpdate;
        m.freqHz = channelToHz(ch);
        m.talkgroup = int(b(40, 0xffffff));
        m.source = long(b(16, 0xffffff));
        setSlot(ch);
        break;
      }
      case 0x33:  // IDEN_UP_TDMA
        if (mfid == 0) {
          static const int SLOTS[6] = {1, 1, 1, 2, 4, 2};
          const int ct = int(b(72, 0xf)), toff0 = int(b(58, 0x3fff)), spac = int(b(48, 0x3ff));
          int toff = toff0 & 0x1fff;
          if (((toff0 >> 13) & 1) == 0) toff = -toff;
          const int slots = ct < 6 ? SLOTS[ct] : 1;
          tables[int(b(76, 0xf))] = {long(toff) * spac * 125, long(spac) * 125, long(b(16, 0xffffffff)) * 5, slots > 1, slots};
        }
        break;
      case 0x34: {  // IDEN_UP_VU
        const int toff0 = int(b(58, 0x3fff)), spac = int(b(48, 0x3ff));
        int toff = toff0 & 0x1fff;
        if (((toff0 >> 13) & 1) == 0) toff = -toff;
        tables[int(b(76, 0xf))] = {long(toff) * spac * 125, long(spac) * 125, long(b(16, 0xffffffff)) * 5, false, 0};
        break;
      }
      case 0x3a:
        m.type = Msg::SysId;
        m.sysId = int(b(56, 0xfff));
        m.rfss = int(b(48, 0xff));
        m.site = int(b(40, 0xff));
        break;
      case 0x3b:
        if (channelToHz(int(b(24, 0xffff)))) {
          m.type = Msg::Status;
          m.wacn = int(b(52, 0xfffff));
          m.sysId = int(b(40, 0xfff));
          m.freqHz = channelToHz(int(b(24, 0xffff)));
        }
        break;
      case 0x3d: {  // IDEN_UP
        const int toff0 = int(b(58, 0x1ff)), spac = int(b(48, 0x3ff));
        int toff = toff0 & 0xff;
        if (((toff0 >> 8) & 1) == 0) toff = -toff;
        tables[int(b(76, 0xf))] = {long(toff) * 250000, long(spac) * 125, long(b(16, 0xffffffff)) * 5, false, 1};
        break;
      }
      default:
        break;
    }
    out.push_back(m);
    return out;
  }
};

// ── Calls (callManager.ts) ────────────────────────────────────────────────────

enum class Reason { None, Encrypted, NoSource, NoRecorder, Phase2 };
inline const char* reason_name(Reason r) {
  switch (r) {
    case Reason::Encrypted: return "encrypted";
    case Reason::NoSource: return "no_source";
    case Reason::NoRecorder: return "no_recorder";
    case Reason::Phase2: return "phase2_unsupported";
    default: return "";
  }
}

struct Source {
  long src;
  double timeS;
  bool emergency;
};

struct Call {
  int id = 0, talkgroup = 0;
  long freqHz = 0;
  bool phase2Tdma = false;
  int tdmaSlot = 0;
  bool unitToUnit = false, recording = false;
  Reason reason = Reason::None;
  bool encrypted = false, emergency = false, duplex = false, mode = false;
  int priority = 0;
  double startS = 0, lastUpdateS = 0, lastAudioS = 0;
  std::vector<Source> sources;
};

struct CallConfig {
  double callTimeoutS = 3;
  bool recordEncrypted = false, recordUnitToUnit = true, newCallFromUpdate = true;
};

class CallManager {
 public:
  std::vector<std::shared_ptr<Call>> calls;
  std::function<Reason(const std::shared_ptr<Call>&)> startRecording;  // Reason::None = ok
  std::function<void(Call&)> stopRecording;
  std::function<void(Call&)> onCallStart, onCallEnd;
  CallConfig cfg;

  void handle(const std::vector<Message>& msgs) {
    for (const auto& m : msgs) {
      switch (m.type) {
        case Msg::Grant: grant(m, false); break;
        case Msg::Update:
          if (cfg.newCallFromUpdate) grant(m, false);
          else update(m);
          break;
        case Msg::UuGrant:
          if (cfg.recordUnitToUnit) grant(m, true);
          break;
        case Msg::UuUpdate:
          if (cfg.recordUnitToUnit) update(m);
          break;
        default: break;
      }
    }
  }
  void noteAudio(Call& c, double t) { c.lastAudioS = std::max(c.lastAudioS, t); }
  void noteSource(Call& c, long src, double t, bool emergency) {
    if (src <= 0) return;
    if (!c.sources.empty() && c.sources.back().src == src) return;
    c.sources.push_back({src, t, emergency});
  }
  void tick(double now) {
    for (size_t i = calls.size(); i-- > 0;) {
      auto c = calls[i];
      const bool quietCc = now - c->lastUpdateS > cfg.callTimeoutS, quietAudio = now - c->lastAudioS > cfg.callTimeoutS;
      if (quietCc && (!c->recording || quietAudio)) {
        calls.erase(calls.begin() + long(i));
        if (c->recording) stopRecording(*c);
        onCallEnd(*c);
      }
    }
  }
  void endAll() {
    auto all = std::move(calls);
    calls.clear();
    for (auto& c : all) {
      if (c->recording) stopRecording(*c);
      onCallEnd(*c);
    }
  }

 private:
  static bool matches(const Call& c, const Message& m) {
    return c.talkgroup == m.talkgroup && c.freqHz == m.freqHz && c.tdmaSlot == m.tdmaSlot && c.phase2Tdma == m.phase2Tdma;
  }
  void refresh(Call& c, const Message& m) {
    c.lastUpdateS = m.timeS;
    if (m.encrypted) c.encrypted = true;
    if (m.emergency) c.emergency = true;
    if (m.source > 0) noteSource(c, m.source, m.timeS, m.emergency);
  }
  void update(const Message& m) {
    for (auto& c : calls)
      if (matches(*c, m)) refresh(*c, m);
  }
  void grant(const Message& m, bool uu) {
    if (!m.freqHz) return;
    for (auto& c : calls)
      if (matches(*c, m)) return refresh(*c, m);
    auto c = std::make_shared<Call>();
    c->id = nextId_++;
    c->talkgroup = m.talkgroup;
    c->freqHz = m.freqHz;
    c->phase2Tdma = m.phase2Tdma;
    c->tdmaSlot = m.tdmaSlot;
    c->unitToUnit = uu;
    c->encrypted = m.encrypted;
    c->emergency = m.emergency;
    c->priority = m.priority;
    c->duplex = m.duplex;
    c->mode = m.mode;
    c->startS = c->lastUpdateS = c->lastAudioS = m.timeS;
    if (m.source > 0) c->sources.push_back({m.source, m.timeS, m.emergency});
    if (c->encrypted && !cfg.recordEncrypted) c->reason = Reason::Encrypted;
    else {
      Reason r = startRecording(c);
      if (r == Reason::None) c->recording = true;
      else c->reason = r;
    }
    calls.push_back(c);
    onCallStart(*c);
  }
  int nextId_ = 1;
};

// ── Voice channel call tracker (P25VoiceDecoder, Phase 1) ─────────────────────

struct VoiceEvents {
  std::function<void(const float* samples, int n)> onAudio;
  // A call's metadata changed: talkgroup/source/emergency/encrypted.
  std::function<void(long source, bool emergency, bool encrypted)> onInfo;
};

class VoiceTracker {
 public:
  int frames = 0, badFrames = 0;  // this channel's vocoder frames / repeated-or-muted

  VoiceTracker(VoiceEvents ev, mbe::Rng rng) : ev_(std::move(ev)), dec_(std::move(rng)) {}

  // A diversity group (one frame slot, several receivers) at time t: header
  // fields from the best NID, link control / encryption sync from whichever
  // copy decodes, each codeword from the receiver the soft decoder trusts most.
  void group(const diversity::Group& g, double t) {
    const p25::Frame* best = diversity::best_frame(g);
    if (best->nid.duid != p25::LDU1 && best->nid.duid != p25::LDU2) return frame(*best, t);
    const auto chosen = diversity::best_imbe(g);
    const auto lc = diversity::best_lc(g);
    const auto es = diversity::best_es(g);
    bool anyComplete = false;
    for (const auto* f : g) anyComplete |= f->complete;
    frame(*best, t, &chosen, &lc, &es, anyComplete);
  }

  // A frame from the framer at time t (s, sample clock).
  void frame(const p25::Frame& f, double t, const std::array<p25::ImbeParams, 9>* chosen = nullptr,
             const std::optional<p25::LinkControl>* lcIn = nullptr, const std::optional<p25::EncryptionSync>* esIn = nullptr,
             bool complete = false) {
    complete = complete || f.complete;
    switch (f.nid.duid) {
      case p25::HDU: {
        auto h = p25::decode_hdu(f.raw);
        algid_ = h ? h->es.algid : p25::ALGID_CLEAR;
        start(t, h ? h->tgid : -1);
        return;
      }
      case p25::LDU1:
      case p25::LDU2: {
        touch(t);
        if (f.nid.duid == p25::LDU1) {
          auto lc = lcIn ? *lcIn : p25::decode_ldu1_lc(f.raw);
          if (lc && lc->isProtected) setEncryption(algid_ == p25::ALGID_CLEAR ? p25::ALGID_UNKNOWN : algid_);
          else if (lc && (lc->lco == 0 || lc->lco == 3)) {
            if (tgid_ >= 0 && lc->tgid >= 0 && tgid_ != lc->tgid && callFrames_ > 0) start(t, -1);
            if (lc->tgid >= 0) tgid_ = lc->tgid;
            const bool emergency = lc->svcOpts >= 0 && (lc->svcOpts & 0x80);
            if (lc->svcOpts >= 0 && (lc->svcOpts & 0x40) && algid_ == p25::ALGID_CLEAR) setEncryption(p25::ALGID_UNKNOWN);
            if (ev_.onInfo) ev_.onInfo(lc->source, emergency, encrypted_);
          }
        } else {
          auto es = esIn ? *esIn : p25::decode_ldu2_es(f.raw);
          if (es) setEncryption(es->algid);
        }
        // A cut frame (the next sync came early): from the first codeword that
        // fails its FEC or runs past the cut, nothing is trusted — erasures.
        const int cutAt = complete ? -1 : int(f.raw.size());
        bool erased = false;
        for (int k = 0; k < 9; k++) {
          uint8_t cw[144];
          float sw[144];
          p25::ldu_codeword(f.raw, k, cw, &f.rawSoft, sw);
          // Soft FEC (Chase Golay, ML Hamming): 7.6 % → 2.2 % wrong codewords on synthetic simulcast.
          auto p = chosen ? (*chosen)[size_t(k)] : p25::imbe_header_decode(cw, f.rawSoft.empty() ? nullptr : sw);
          if (cutAt >= 0 && (p.errs > 2 || endBit(k) >= cutAt)) erased = true;
          voiceFrame(p, erased);
        }
        return;
      }
      case p25::TDU:
      case p25::TDULC:
        if (f.nid.duid == p25::TDULC && active_ && tgid_ < 0) {
          auto lc = p25::decode_tdulc(f.raw);
          if (lc && !lc->isProtected && lc->tgid >= 0) tgid_ = lc->tgid;
        }
        if (active_) endS_ = t;
        end();
        algid_ = p25::ALGID_CLEAR;
        return;
      default:
        return;
    }
  }

 private:
  static int endBit(int f) {
    int m = 0;
    for (int j = 0; j < 144; j++) m = std::max(m, int(tables::VOICE_CODEWORD_BITS[f * 144 + j]));
    return m;
  }
  void start(double t, int tgid) {
    end();
    dec_.reset();
    active_ = true;
    tgid_ = tgid;
    encrypted_ = algid_ != p25::ALGID_CLEAR;
    startS_ = endS_ = t;
    callFrames_ = 0;
  }
  void end() { active_ = false; }
  void touch(double t) {
    if (active_ && t - endS_ > 1.0) {
      end();
      algid_ = p25::ALGID_CLEAR;
    }
    if (!active_) start(t, -1);
    endS_ = t;
  }
  void setEncryption(int algid) {
    algid_ = algid;
    if (active_ && algid != p25::ALGID_CLEAR && !encrypted_) {
      encrypted_ = true;
      if (ev_.onInfo) ev_.onInfo(0, false, true);
    }
  }
  void voiceFrame(const p25::ImbeParams& p, bool erased) {
    frames++;
    callFrames_++;
    if (encrypted_) return;
    float out[mbe::FRAME_SAMPLES];
    auto bits = p25::imbe_params_to_bits(p.u);
    auto k = dec_.imbe(bits.data(), p.e0, erased ? 0 : p.errs, erased, out);
    if (k != mbe::Kind::Voice) badFrames++;
    mbe::to_unit(out, mbe::FRAME_SAMPLES);
    if (ev_.onAudio) ev_.onAudio(out, mbe::FRAME_SAMPLES);
  }

  VoiceEvents ev_;
  mbe::Decoder dec_;
  bool active_ = false, encrypted_ = false;
  int algid_ = p25::ALGID_CLEAR, tgid_ = -1, callFrames_ = 0;
  double startS_ = 0, endS_ = 0;
};

}  // namespace trunk
