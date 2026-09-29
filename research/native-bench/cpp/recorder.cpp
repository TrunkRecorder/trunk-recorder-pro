// End-to-end native trunk recorder prototype — the TS engine's
// src/trunking/trunkEngine.ts over the C++ stages:
//
//   u8 IQ → Channelizer ─ CC head → CQPSK+C4FM receivers (voted) → framer →
//                        │          TSDU → TSBK → TsbkParser → CallManager
//                        └ voice head per granted frequency (1 s pre-roll) →
//                          receivers → framer → VoiceTracker → IMBE → audio
//   call end → <out>/<tg>-<epoch>_<freq>.wav + .json (Trunk Recorder fields)
//
//   recorder <cap.cu8> --fs 2400000 --center Hz --cc Hz[,Hz] [--out dir]
//            [--recorders 8] [--preroll 1] [--epoch unix-seconds] [--fft vdsp]
//            [--bandplan file]   IDEN tables: loaded at start, saved at exit

#include <sys/stat.h>

#include <cmath>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <map>
#include <memory>
#include <sstream>
#include <string>
#include <vector>

#include "c4fm.hpp"
#include "channelizer.hpp"
#include "demod.hpp"
#include "p25_frame.hpp"
#include "p25_tsbk.hpp"
#include "trunk.hpp"

static std::string opt(int argc, char** argv, const char* k, const char* d) {
  for (int i = 0; i + 1 < argc; i++)
    if (std::strcmp(argv[i], k) == 0) return argv[i + 1];
  return d;
}
static double threadCpu() {
  timespec t{};
  clock_gettime(CLOCK_THREAD_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}

// Each channel's receivers: a diversity bank (CQPSK, CQPSK + T/2 CMA
// equaliser, C4FM — see diversity.hpp); groups are released in time order.
using Receiver = diversity::Bank;

static void writeWav(const std::string& path, const std::vector<float>& s) {
  std::FILE* f = std::fopen(path.c_str(), "wb");
  auto u32 = [&](uint32_t v) { std::fwrite(&v, 4, 1, f); };
  auto u16 = [&](uint16_t v) { std::fwrite(&v, 2, 1, f); };
  std::fwrite("RIFF", 1, 4, f);
  u32(uint32_t(36 + s.size() * 2));
  std::fwrite("WAVEfmt ", 1, 8, f);
  u32(16), u16(1), u16(1), u32(8000), u32(16000), u16(2), u16(16);
  std::fwrite("data", 1, 4, f);
  u32(uint32_t(s.size() * 2));
  for (float x : s) {
    auto v = int16_t(std::lround(std::max(-1.f, std::min(1.f, x)) * 32767));
    std::fwrite(&v, 2, 1, f);
  }
  std::fclose(f);
}

class Engine {
 public:
  struct Config {
    std::string shortName = "replay", outDir = "calls", bandplan;
    double centerHz = 0, rateHz = 0, prerollS = 1, epochS = 0;
    std::vector<double> controlChannels;
    int maxRecorders = 8;
  };

  Engine(Config cfg, Channelizer& chz) : cfg_(std::move(cfg)), chz_(chz), rate_(chz.outputRate()) {
    calls_.startRecording = [this](const std::shared_ptr<trunk::Call>& c) { return startRecording(c); };
    calls_.stopRecording = [this](trunk::Call& c) { stopRecording(c); };
    calls_.onCallStart = [this](trunk::Call& c) {
      std::printf("%7.2fs  CALL %d start TG %d %.4f MHz%s → %s\n", c.startS, c.id, c.talkgroup, c.freqHz / 1e6,
                  c.phase2Tdma ? " (TDMA)" : "", c.recording ? "recording" : (std::string("monitoring (") + trunk::reason_name(c.reason) + ")").c_str());
    };
    calls_.onCallEnd = [this](trunk::Call& c) { concluded(c); };
    if (!cfg_.bandplan.empty()) {
      parser_.load(cfg_.bandplan);
      if (!parser_.tables.empty()) std::printf("band plan: %zu IDEN table(s) from %s\n", parser_.tables.size(), cfg_.bandplan.c_str());
    }
    tuneControl(0);
  }
  // End every open call (end of capture), as the TS replay does.
  void finish() {
    // Release frames the receiver banks are still holding, then end the calls.
    if (cc_) cc_->flush();
    for (auto& [f, ch] : channels_)
      if (ch->rx) ch->rx->flush();
    calls_.endAll();
    if (!cfg_.bandplan.empty()) parser_.save(cfg_.bandplan);
  }

  bool inBand(double hz) const { return std::fabs(hz - cfg_.centerHz) <= cfg_.rateHz / 2 * 0.9; }
  int good = 0, bad = 0, written = 0;
  const char* ccModulation() const {
    if (!cc_) return "?";
    const auto v = cc_->framesPerRx();  // CQPSK, CQPSK+EQ, C4FM
    return v.size() == 3 && v[2] > std::max(v[0], v[1]) ? "C4FM" : "CQPSK";
  }

 private:
  struct Recording {
    std::vector<float> audio;
    int recorderNum;
    long freqHz;
  };
  // Heap-allocated so the sinks can hold a plain pointer to it.
  struct Channel {
    int head = -1;
    uint64_t startSample = 0;
    std::shared_ptr<trunk::Call> call;
    std::unique_ptr<trunk::VoiceTracker> tracker;
    std::unique_ptr<Receiver> rx;
  };

  void tuneControl(int index) {
    std::vector<double> list;
    for (double f : cfg_.controlChannels)
      if (inBand(f)) list.push_back(f);
    if (list.empty()) throw std::runtime_error("no control channel inside the source bandwidth");
    ccIndex_ = ((index % int(list.size())) + int(list.size())) % int(list.size());
    if (ccHead_ >= 0) chz_.removeHead(ccHead_);
    ccHz_ = list[ccIndex_];
    ccStartS_ = nowS_;
    ccSamples_ = 0;
    lastGoodS_ = nowS_;
    cc_ = std::make_unique<Receiver>(rate_, diversity::Bank::Config{}, [this](const diversity::Group& g) { onControlGroup(g); });
    ccHead_ = chz_.addHead(ccHz_ - cfg_.centerHz, 7000, [this](const float* iq, int n) { onControlIq(iq, n); });
    std::printf("control channel %.4f MHz\n", ccHz_ / 1e6);
  }

  void onControlIq(const float* iq, int n) {
    retired_.clear();  // channels stopped last block: their heads are gone now
    cc_->push(iq, n);
    ccSamples_ += uint64_t(n);
    nowS_ = ccStartS_ + double(ccSamples_) / rate_;
    if (nowS_ - lastGoodS_ > 5 && cfg_.controlChannels.size() > 1) return tuneControl(ccIndex_ + 1);
    calls_.tick(nowS_);
  }

  void onControlGroup(const diversity::Group& g) {
    const p25::Frame* f = diversity::best_frame(g);
    if (f->nid.duid != p25::TSDU) return;
    int missing = 0;
    const auto blocks = diversity::best_tsbks(g, &missing);
    bad += missing;
    const double t = ccStartS_ + f->sample / rate_;
    for (auto& blk : blocks) {
      good++;
      lastGoodS_ = nowS_;
      calls_.handle(parser_.parse(blk, t));
    }
  }

  trunk::Reason startRecording(const std::shared_ptr<trunk::Call>& c) {
    if (!inBand(double(c->freqHz))) return trunk::Reason::NoSource;
    if (c->phase2Tdma) return trunk::Reason::Phase2;
    if (int(recordings_.size()) >= cfg_.maxRecorders) return trunk::Reason::NoRecorder;
    int num;
    if (!freeNums_.empty()) {
      num = freeNums_.back();
      freeNums_.pop_back();
    } else {
      num = nextNum_++;
    }
    recordings_[c->id] = {{}, num, c->freqHz};
    // A newer call on the same frequency takes the channel over, as in TR.
    auto& slot = channels_[c->freqHz];
    if (!slot) {
      slot = std::make_unique<Channel>();
      slot->call = c;
      openChannel(c->freqHz, *slot);
    } else {
      slot->call = c;
    }
    return trunk::Reason::None;
  }

  void openChannel(long freqHz, Channel& ch) {
    Channel* chp = &ch;
    trunk::VoiceEvents ev;
    ev.onAudio = [this, chp](const float* s, int n) {
      if (!chp->call) return;
      auto it = recordings_.find(chp->call->id);
      if (it == recordings_.end()) return;
      it->second.audio.insert(it->second.audio.end(), s, s + n);
      calls_.noteAudio(*chp->call, nowS_);
    };
    ev.onInfo = [this, chp](long src, bool emergency, bool encrypted) {
      if (!chp->call) return;
      if (encrypted) chp->call->encrypted = true;
      if (emergency) chp->call->emergency = true;
      if (src > 0) calls_.noteSource(*chp->call, src, nowS_, emergency);
    };
    uint32_t seed = uint32_t(freqHz);
    ch.tracker = std::make_unique<trunk::VoiceTracker>(std::move(ev), [seed]() mutable {
      seed = seed * 1664525u + 1013904223u;
      return seed / 4294967296.0;
    });
    trunk::VoiceTracker* tracker = ch.tracker.get();
    ch.rx = std::make_unique<Receiver>(rate_, diversity::Bank::Config{}, [this, tracker, chp](const diversity::Group& g) {
      tracker->group(g, double(chp->startSample) / cfg_.rateHz + diversity::best_frame(g)->sample / rate_);
    });
    Receiver* rx = ch.rx.get();
    ch.head = chz_.addHead(double(freqHz) - cfg_.centerHz, 7000, [rx](const float* iq, int n) { rx->push(iq, n); }, cfg_.prerollS,
                           &ch.startSample);
  }

  void stopRecording(trunk::Call& c) {
    auto it = channels_.find(c.freqHz);
    if (it == channels_.end() || it->second->call.get() != &c) return;
    chz_.removeHead(it->second->head);
    // Its head is only reaped at the end of this block; free it next block.
    retired_.push_back(std::move(it->second));
    channels_.erase(it);
  }

  void concluded(trunk::Call& c) {
    auto it = recordings_.find(c.id);
    if (it == recordings_.end()) {
      std::printf("%7.2fs  CALL %d end   TG %d (%s)\n", nowS_, c.id, c.talkgroup, c.recording ? "no audio" : trunk::reason_name(c.reason));
      return;
    }
    Recording rec = std::move(it->second);
    recordings_.erase(it);
    freeNums_.push_back(rec.recorderNum);
    if (c.encrypted) rec.audio.clear();
    std::string srcs;
    for (auto& s : c.sources) srcs += (srcs.empty() ? "" : ",") + std::to_string(s.src);
    std::printf("%7.2fs  CALL %d end   TG %d srcs [%s]%s  %.1f s audio\n", nowS_, c.id, c.talkgroup, srcs.c_str(), c.encrypted ? " ENC" : "",
                rec.audio.size() / 8000.0);
    if (rec.audio.empty()) return;
    const long startMs = std::lround((cfg_.epochS + c.startS) * 1000), stopMs = std::lround((cfg_.epochS + c.lastAudioS) * 1000);
    const std::string base = cfg_.outDir + "/" + std::to_string(c.talkgroup) + "-" + std::to_string(startMs / 1000) + "_" + std::to_string(c.freqHz);
    writeWav(base + ".wav", rec.audio);
    const double secs = rec.audio.size() / 8000.0;
    std::ofstream j(base + ".json");
    j << "{\"call_num\":" << c.id << ",\"freq\":" << c.freqHz << ",\"recorder_num\":" << rec.recorderNum << ",\"tdma_slot\":" << c.tdmaSlot
      << ",\"phase2_tdma\":" << c.phase2Tdma << ",\"start_time\":" << startMs / 1000 << ",\"stop_time\":" << stopMs / 1000
      << ",\"start_time_ms\":" << startMs << ",\"stop_time_ms\":" << stopMs << ",\"emergency\":" << c.emergency << ",\"priority\":" << c.priority
      << ",\"mode\":" << c.mode << ",\"duplex\":" << c.duplex << ",\"encrypted\":" << c.encrypted << ",\"call_length\":" << std::lround(secs)
      << ",\"call_length_ms\":" << std::lround(secs * 1000) << ",\"talkgroup\":" << c.talkgroup << ",\"audio_type\":\"digital\",\"short_name\":\""
      << cfg_.shortName << "\",\"srcList\":[";
    for (size_t i = 0; i < c.sources.size(); i++)
      j << (i ? "," : "") << "{\"src\":" << c.sources[i].src << ",\"time\":" << std::lround(cfg_.epochS + c.sources[i].timeS)
        << ",\"pos\":" << std::max(0.0, std::round((c.sources[i].timeS - c.startS) * 100) / 100) << ",\"emergency\":" << c.sources[i].emergency << "}";
    j << "]}\n";
    written++;
  }

  Config cfg_;
  Channelizer& chz_;
  double rate_;
  trunk::TsbkParser parser_;
  trunk::CallManager calls_;
  std::unique_ptr<Receiver> cc_;
  int ccHead_ = -1, ccIndex_ = 0;
  double ccHz_ = 0, ccStartS_ = 0, nowS_ = 0, lastGoodS_ = 0;
  uint64_t ccSamples_ = 0;
  std::map<long, std::unique_ptr<Channel>> channels_;
  std::vector<std::unique_ptr<Channel>> retired_;
  std::map<int, Recording> recordings_;
  std::vector<int> freeNums_;
  int nextNum_ = 0;
};

int main(int argc, char** argv) {
  if (argc < 2) {
    std::fprintf(stderr, "usage: recorder <cap.cu8> --fs Hz --center Hz --cc Hz[,Hz] [--out dir] [--recorders 8] [--preroll 1] [--epoch s]\n");
    return 2;
  }
  std::ifstream in(argv[1], std::ios::binary);
  std::vector<uint8_t> cap((std::istreambuf_iterator<char>(in)), {});
  Engine::Config cfg;
  cfg.rateHz = std::stod(opt(argc, argv, "--fs", "2400000"));
  cfg.centerHz = std::stod(opt(argc, argv, "--center", "0"));
  std::stringstream ss(opt(argc, argv, "--cc", ""));
  for (std::string x; std::getline(ss, x, ',');) cfg.controlChannels.push_back(std::stod(x));
  cfg.outDir = opt(argc, argv, "--out", "calls");
  cfg.maxRecorders = std::stoi(opt(argc, argv, "--recorders", "8"));
  cfg.prerollS = std::stod(opt(argc, argv, "--preroll", "1"));
  cfg.epochS = std::stod(opt(argc, argv, "--epoch", "0"));
  cfg.bandplan = opt(argc, argv, "--bandplan", "");
  mkdir(cfg.outDir.c_str(), 0755);

  Channelizer chz(cfg.rateHz, 24000, opt(argc, argv, "--fft", "vdsp"), 16384, 4097, std::max(cfg.prerollS, 0.1));
  Engine eng(cfg, chz);
  const size_t total = cap.size() / 2, chunk = 32768;
  const double c0 = threadCpu();
  for (size_t i = 0; i < total; i += chunk) chz.pushU8(cap.data() + 2 * i, std::min(chunk, total - i));
  eng.finish();
  const double cpu = threadCpu() - c0, air = double(total) / cfg.rateHz;
  std::printf("\n%.1f s of air, %.3f CPU-s (%.2f %% of a core). CC: %d good / %d bad TSBKs, %s. %d call(s) written to %s/\n", air, cpu,
              100 * cpu / air, eng.good, eng.bad, eng.ccModulation(), eng.written, cfg.outDir.c_str());
  return 0;
}
