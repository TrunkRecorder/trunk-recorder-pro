// Staged P25 decoder for comparing against the TS engine and Trunk Recorder.
//
//   p25tool cc <cap.cu8> --fs 2400000 --center 858300000 --cc 857987500 [--fft vdsp] [--iq out.cf32]
//       control channel: channelizer → CQPSK/C4FM receiver (--demod) → framer → NID → TSDU →
//       trellis → CRC. Prints one JSON line per CRC-valid TSBK on stdout and a
//       summary on stderr.
//   p25tool frames <cap.cu8> --fs … --center … --freq hz [--iq out.cf32]
//       any channel: frame counts by DUID (NID-valid frames only).
//   p25tool voice <cap.cu8> --fs … --center … --freq hz
//       voice channel: one JSON line per frame with its raw bits (status
//       symbols in place) and the C++ decode — IMBE u0..u7/errors per codeword,
//       LC / ES / HDU / TDULC — for ts_voice_check.ts to re-decode and compare.
//       --audio out.f32 [--profile enhanced|mbelib]: every LDU codeword through
//       one IMBE decoder (8 kHz f32), RNG = 32-bit LCG seeded 1, for
//       ts_vocoder_check.ts to reproduce sample by sample.
//
// --iq writes the channel's IQ (the channelizer output) so the TS decoders can
// be run on exactly the same samples.

#include <cstdio>
#include <cstring>
#include <fstream>
#include <map>
#include <string>
#include <vector>

#include "channelizer.hpp"
#include "c4fm.hpp"
#include "demod.hpp"
#include "p25_frame.hpp"
#include "p25_tsbk.hpp"
#include "p25_voice.hpp"
#include "mbe.hpp"
#include "diversity.hpp"

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
static std::vector<uint8_t> readFile(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  if (!f) throw std::runtime_error("cannot open " + path);
  return std::vector<uint8_t>(std::istreambuf_iterator<char>(f), {});
}

static void printLc(const char* key, const std::optional<p25::LinkControl>& lc) {
  if (!lc) return void(std::printf(",\"%s\":null", key));
  std::printf(",\"%s\":{\"lco\":%d,\"prot\":%d,\"svc\":%d,\"tgid\":%d,\"target\":%d,\"src\":%d}", key, lc->lco, lc->isProtected,
              lc->svcOpts, lc->tgid, lc->target, lc->source);
}

static int gArgc;
static char** gArgv;
static std::FILE* gAudio = nullptr;
static long gCodewords = 0, gFecErrs = 0, gRepeatWorthy = 0;
static mbe::Decoder* gVocoder = nullptr;
static int gVoiceKinds[3] = {};

// `chosen` (diversity): the codewords picked across receivers, used instead of
// decoding this frame's own; `lcOverride`/`esOverride` likewise.
static void printVoiceFrame(const p25::Frame& f, const std::array<p25::ImbeParams, 9>* chosen = nullptr,
                            const std::optional<p25::LinkControl>* lcOverride = nullptr,
                            const std::optional<p25::EncryptionSync>* esOverride = nullptr) {
  std::printf("{\"t\":%.3f,\"duid\":%d,\"nac\":%d,\"complete\":%d,\"nbits\":%zu,\"raw\":\"", f.symbol / 4800.0, f.nid.duid, f.nid.nac,
              f.complete, f.raw.size());
  for (size_t i = 0; i < f.raw.size(); i += 4) {
    int v = 0;
    for (size_t j = 0; j < 4; j++) v = (v << 1) | (i + j < f.raw.size() ? f.raw[i + j] : 0);
    std::printf("%x", v);
  }
  std::printf("\"");
  const int d = f.nid.duid;
  if (d == p25::LDU1 || d == p25::LDU2) {
    std::printf(",\"imbe\":[");
    for (int k = 0; k < 9; k++) {
      uint8_t cw[144];
      float sw[144];
      const bool softFec = !f.rawSoft.empty() && opt(gArgc, gArgv, "--softfec", "1") == "1";
      p25::ldu_codeword(f.raw, k, cw, &f.rawSoft, sw);
      auto p = chosen ? (*chosen)[size_t(k)] : p25::imbe_header_decode(cw, softFec ? sw : nullptr);
      gCodewords++;
      gFecErrs += p.errs;
      if (p.e0 >= 3 || p.errs >= 10) gRepeatWorthy++;
      if (gVocoder) {
        float out[mbe::FRAME_SAMPLES];
        auto bits = p25::imbe_params_to_bits(p.u);
        gVoiceKinds[int(gVocoder->imbe(bits.data(), p.e0, p.errs, false, out))]++;
        mbe::to_unit(out, mbe::FRAME_SAMPLES);
        if (gAudio) std::fwrite(out, sizeof(float), mbe::FRAME_SAMPLES, gAudio);
      }
      std::printf("%s{\"u\":[%d,%d,%d,%d,%d,%d,%d,%d],\"errs\":%d,\"e0\":%d,\"c\":%.3f,\"c0\":%.3f,\"mr\":%.3f,\"eh\":%d,\"e0h\":%d}", k ? "," : "", p.u[0], p.u[1], p.u[2],
                  p.u[3], p.u[4], p.u[5], p.u[6], p.u[7], p.errs, p.e0, p.cost, p.cost0, p.meanRel, p.errsHard, p.e0Hard);
    }
    std::printf("]");
    if (d == p25::LDU1) printLc("lc", lcOverride ? *lcOverride : p25::decode_ldu1_lc(f.raw));
    else {
      auto es = esOverride ? *esOverride : p25::decode_ldu2_es(f.raw);
      if (es) std::printf(",\"es\":{\"algid\":%d,\"keyid\":%d}", es->algid, es->keyid);
      else std::printf(",\"es\":null");
    }
  } else if (d == p25::HDU) {
    auto h = p25::decode_hdu(f.raw);
    if (h) std::printf(",\"hdu\":{\"algid\":%d,\"keyid\":%d,\"mfid\":%d,\"tgid\":%d}", h->es.algid, h->es.keyid, h->mfid, h->tgid);
    else std::printf(",\"hdu\":null");
  } else if (d == p25::TDULC) {
    printLc("tdulc", p25::decode_tdulc(f.raw));
  }
  std::printf("}\n");
}

int main(int argc, char** argv) {
  if (argc < 3) {
    std::fprintf(stderr, "usage: p25tool cc|frames <cap.cu8> --fs Hz --center Hz (--cc Hz | --freq Hz) [--fft vdsp] [--iq out.cf32]\n");
    return 2;
  }
  gArgc = argc;
  gArgv = argv;
  const std::string mode = argv[1];
  const auto cap = readFile(argv[2]);
  const double fs = std::stod(opt(argc, argv, "--fs", "2400000"));
  const double center = std::stod(opt(argc, argv, "--center", "0"));
  const double freq = std::stod(opt(argc, argv, mode == "cc" ? "--cc" : "--freq", "0"));
  if (mode != "cc" && mode != "frames" && mode != "voice") {
    std::fprintf(stderr, "unknown mode %s\n", mode.c_str());
    return 2;
  }
  const std::string iqPath = opt(argc, argv, "--iq", "");

  Channelizer chz(fs, 24000, opt(argc, argv, "--fft", "vdsp"));
  // Receivers: CQPSK (simulcast/LSM, also passable on C4FM) and C4FM, each with
  // its own framer. "auto" runs both and takes frames from whichever has
  // decoded more valid NIDs so far (the TS engine's modulation vote, streaming).
  const std::string which = opt(argc, argv, "--demod", "auto");
  Pi4Options po;
  po.eqTaps = std::stoi(opt(argc, argv, "--eq", "0"));
  po.eqMu = std::stof(opt(argc, argv, "--mu", "0.002"));
  // --soft none|phase|amp: bit reliabilities for the Viterbi decoder.
  const std::string softMode = opt(argc, argv, "--soft", "amp");
  const bool soft = softMode != "none";
  po.softAmplitude = softMode == "amp";
  po.coherent = opt(argc, argv, "--coherent", "0") == "1";
  po.pllKp = std::stof(opt(argc, argv, "--pllkp", "0.04"));
  po.pllKi = po.pllKp * po.pllKp / 4;
  Pi4Demod cqpsk(chz.outputRate(), po);
  C4fmDemod c4fm(chz.outputRate());
  p25::FramerOptions fo;
  fo.flywheel = opt(argc, argv, "--flywheel", "1") == "1";
  fo.nidRecover = opt(argc, argv, "--nidrecover", "1") == "1";
  p25::Framer fq(fo), fc(fo);
  const bool useQ = which != "c4fm", useC = which != "cqpsk";
  cqpsk.onDibit = [&](uint8_t d, double t, float h, float l) { fq.push(d, t, soft ? h : -1, soft ? l : -1); };
  c4fm.onDibit = [&](uint8_t d, double t, float h, float l) { fc.push(d, t, soft ? h : -1, soft ? l : -1); };
  uint64_t validQ = 0, validC = 0;

  std::map<std::string, int> frames;
  int good = 0, bad = 0, trellisFail = 0, crcFail = 0, skipped = 0;
  auto onFrame = [&](const p25::Frame& f) {
    frames[p25::duid_name(f.nid.duid)]++;
    if (mode == "voice") {
      printVoiceFrame(f);
      return;
    }
    if (mode != "cc" || f.nid.duid != p25::TSDU) return;
    auto r = p25::decode_tsdu(f, opt(gArgc, gArgv, "--trellis", "viterbi") == "greedy" ? p25::Trellis::Greedy : p25::Trellis::Viterbi);
    bad += r.bad;
    trellisFail += r.trellisFail;
    crcFail += r.crcFail;
    skipped += r.blocksSkipped;
    for (auto& t : r.good) {
      good++;
      std::printf("{\"t\":%.3f,\"nac\":%d,\"op\":%d,\"hex\":\"", f.symbol / 4800.0, f.nid.nac, p25::tsbk_opcode(t));
      for (uint8_t b : t) std::printf("%02x", b);
      std::printf("\"}\n");
    }
  };
  fq.onFrame = [&](const p25::Frame& f) {
    validQ++;
    if (!useC || validQ >= validC) onFrame(f);
  };
  fc.onFrame = [&](const p25::Frame& f) {
    validC++;
    if (!useQ || validC > validQ) onFrame(f);
  };

  // --diversity 1: a bank of receivers (CQPSK, CQPSK + T/2 CMA equaliser,
  // C4FM) with the best of each frame group taken (see diversity.hpp).
  const bool div = opt(argc, argv, "--diversity", "0") == "1";
  diversity::Bank::Config dc;
  dc.c4fm = which != "cqpsk";
  dc.cqpsk = dc.cqpskEq = which != "c4fm";
  dc.eqTaps = std::stoi(opt(argc, argv, "--eq", "9"));
  dc.eqMu = std::stof(opt(argc, argv, "--mu", "0.02"));
  std::unique_ptr<diversity::Bank> bank;
  if (div)
    bank = std::make_unique<diversity::Bank>(chz.outputRate(), dc, [&](const diversity::Group& g) {
      const p25::Frame* best = diversity::best_frame(g);
      frames[p25::duid_name(best->nid.duid)]++;
      validQ++;
      if (mode == "voice") {
        if (best->nid.duid == p25::LDU1 || best->nid.duid == p25::LDU2) {
          auto chosen = diversity::best_imbe(g);
          auto lc = diversity::best_lc(g);
          auto es = diversity::best_es(g);
          printVoiceFrame(*best, &chosen, &lc, &es);
        } else {
          printVoiceFrame(*best);
        }
        return;
      }
      if (mode != "cc" || best->nid.duid != p25::TSDU) return;
      int missing = 0;
      for (auto& t : diversity::best_tsbks(g, &missing)) {
        good++;
        std::printf("{\"t\":%.3f,\"nac\":%d,\"op\":%d,\"hex\":\"", best->sample / chz.outputRate(), best->nid.nac, p25::tsbk_opcode(t));
        for (uint8_t b : t) std::printf("%02x", b);
        std::printf("\"}\n");
      }
      bad += missing;
    });

  uint32_t lcg = 1;
  mbe::Decoder vocoder([&lcg] { lcg = lcg * 1664525u + 1013904223u; return lcg / 4294967296.0; },
                       opt(argc, argv, "--profile", "enhanced") == "mbelib" ? mbe::Decoder::Profile::Mbelib : mbe::Decoder::Profile::Enhanced);
  const std::string audioPath = opt(argc, argv, "--audio", "");
  if (!audioPath.empty()) {
    gAudio = std::fopen(audioPath.c_str(), "wb");
    gVocoder = &vocoder;
  }
  std::FILE* iq = iqPath.empty() ? nullptr : std::fopen(iqPath.c_str(), "wb");
  chz.addHead(freq - center, 7000, [&](const float* x, int n) {
    if (bank) {
      bank->push(x, n);
    } else {
      if (useQ) cqpsk.push(x, n);
      if (useC) c4fm.push(x, n);
    }
    if (iq) std::fwrite(x, sizeof(float), 2 * n, iq);
  });
  const size_t total = cap.size() / 2, chunk = 32768;
  const double c0 = threadCpu();
  for (size_t i = 0; i < total; i += chunk) chz.pushU8(cap.data() + 2 * i, std::min(chunk, total - i));
  if (bank) bank->flush();
  const double cpu = threadCpu() - c0;
  if (iq) std::fclose(iq);
  if (gAudio) {
    std::fclose(gAudio);
    std::fprintf(stderr, "{\"vocoder\":{\"voice\":%d,\"repeat\":%d,\"muted\":%d}}\n", gVoiceKinds[0], gVoiceKinds[1], gVoiceKinds[2]);
  }

  const double air = double(total) / fs;
  std::fprintf(stderr, "{\"mode\":\"%s\",\"airS\":%.2f,\"pctCore\":%.3f,\"demod\":\"%s\",\"nidValid\":{\"cqpsk\":%llu,\"c4fm\":%llu},\"good\":%d,\"bad\":%d,\"frames\":{",
               mode.c_str(), air, 100 * cpu / air, which.c_str(), (unsigned long long)validQ, (unsigned long long)validC, good, bad);
  bool first = true;
  for (auto& [k, v] : frames) std::fprintf(stderr, "%s\"%s\":%d", first ? "" : ",", k.c_str(), v), first = false;
  std::fprintf(stderr, "}");
  std::fprintf(stderr, ",\"syncs\":%llu,\"nidFails\":%llu,\"flywheels\":%llu,\"nidRecovered\":%llu", (unsigned long long)(fq.syncs + fc.syncs),
               (unsigned long long)(fq.nidFails + fc.nidFails), (unsigned long long)(fq.flywheels + fc.flywheels),
               (unsigned long long)(fq.nidRecovered + fc.nidRecovered));
  if (mode == "cc") std::fprintf(stderr, ",\"trellisFail\":%d,\"crcFail\":%d,\"blocksSkipped\":%d", trellisFail, crcFail, skipped);
  if (mode == "voice")
    std::fprintf(stderr, ",\"codewords\":%ld,\"meanFecErrs\":%.3f,\"repeatWorthy\":%ld", gCodewords, gCodewords ? double(gFecErrs) / gCodewords : 0.0,
                 gRepeatWorthy);
  std::fprintf(stderr, "}\n");
  return 0;
}
