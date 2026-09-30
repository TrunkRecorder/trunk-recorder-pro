// Native benchmark for a ground-up C++ trunk recorder engine.
//
//   bench pipeline <cap.cu8> --fs 2400000 --center 773100000 --freqs a,b,... [--fft vdsp] [--dump hz out.cf32]
//       channelizer + streaming CQPSK demod on every listed channel; reports
//       CPU per second of air and P25 frame syncs per channel.
//   bench scale --fs 8000000 --heads 16 [--fft vdsp] [--secs 10]
//       channelizer alone on random u8 IQ: cost vs wideband rate and head count.
//   bench dongles <cap.cu8> --k 4 ... (pipeline options)
//       k independent pipelines (one per simulated dongle) on k threads.
//   bench demod <chan.cf32> --fs 37500
//       the demodulator alone on one channel's IQ.

#include <sys/resource.h>

#include <chrono>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <random>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include "channelizer.hpp"
#include "demod.hpp"

static std::string opt(int argc, char** argv, const char* k, const char* d) {
  for (int i = 0; i + 1 < argc; i++)
    if (std::strcmp(argv[i], k) == 0) return argv[i + 1];
  return d;
}

static double cpuSeconds() {
  rusage u{};
  getrusage(RUSAGE_SELF, &u);
  return u.ru_utime.tv_sec + u.ru_utime.tv_usec * 1e-6 + u.ru_stime.tv_sec + u.ru_stime.tv_usec * 1e-6;
}
static double threadCpuSeconds() {
  timespec t{};
  clock_gettime(CLOCK_THREAD_CPUTIME_ID, &t);
  return t.tv_sec + t.tv_nsec * 1e-9;
}
static double wallSeconds() {
  using namespace std::chrono;
  return duration<double>(steady_clock::now().time_since_epoch()).count();
}

static std::vector<uint8_t> readFile(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  if (!f) throw std::runtime_error("cannot open " + path);
  return std::vector<uint8_t>(std::istreambuf_iterator<char>(f), {});
}

static std::vector<double> parseList(const std::string& s) {
  std::vector<double> v;
  std::stringstream ss(s);
  for (std::string x; std::getline(ss, x, ',');)
    if (!x.empty()) v.push_back(std::stod(x));
  return v;
}

struct PipelineResult {
  double airS, cpuS = 0;  // cpuS: the streaming loop only (not setup/FFT planning)
  std::vector<uint64_t> syncs, symbols;
};

// One dongle's worth of work: channelizer with a head + demod per frequency.
static PipelineResult runPipeline(const std::vector<uint8_t>& cap, double fs, double center, const std::vector<double>& freqs,
                                  const std::string& fft, bool demod, const std::string& dumpHz = "", const std::string& dumpPath = "") {
  Channelizer chz(fs, 24000, fft);
  std::vector<std::unique_ptr<Pi4Demod>> dem;
  std::FILE* dump = nullptr;
  for (double hz : freqs) {
    dem.push_back(std::make_unique<Pi4Demod>(chz.outputRate()));
    Pi4Demod* d = dem.back().get();
    bool dumpThis = !dumpHz.empty() && std::lround(hz) == std::lround(std::stod(dumpHz));
    if (dumpThis) dump = std::fopen(dumpPath.c_str(), "wb");
    std::FILE* df = dumpThis ? dump : nullptr;
    chz.addHead(hz - center, 7000, [d, demod, df](const float* iq, int n) {
      if (demod) d->push(iq, n);
      if (df) std::fwrite(iq, sizeof(float), 2 * n, df);
    });
  }
  const size_t total = cap.size() / 2, chunk = 32768;  // rtl_sdr-sized USB transfers
  double c0 = threadCpuSeconds();
  for (size_t i = 0; i < total; i += chunk) chz.pushU8(cap.data() + 2 * i, std::min(chunk, total - i));
  double cpu = threadCpuSeconds() - c0;
  if (dump) std::fclose(dump);
  PipelineResult r{double(total) / fs, cpu, {}, {}};
  for (auto& d : dem) {
    r.syncs.push_back(d->syncs);
    r.symbols.push_back(d->symbols);
    if (std::getenv("SPACING")) {  // spacing between syncs, in symbols: most common gaps
      std::map<uint64_t, int> gaps;
      for (size_t i = 1; i < d->syncAt.size(); i++) gaps[d->syncAt[i] - d->syncAt[i - 1]]++;
      std::vector<std::pair<int, uint64_t>> top;
      for (auto& [g, c] : gaps) top.push_back({c, g});
      std::sort(top.rbegin(), top.rend());
      std::fprintf(stderr, "head %zu gaps:", r.symbols.size() - 1);
      for (size_t i = 0; i < std::min<size_t>(8, top.size()); i++) std::fprintf(stderr, " %llu×%d", (unsigned long long)top[i].second, top[i].first);
      std::fprintf(stderr, "\n");
    }
  }
  return r;
}

int main(int argc, char** argv) {
  if (argc < 2) {
    std::fprintf(stderr, "usage: bench pipeline|scale|dongles|demod ...\n");
    return 2;
  }
  std::string mode = argv[1];
  std::string fft = opt(argc, argv, "--fft", "vdsp");
  double fs = std::stod(opt(argc, argv, "--fs", "2400000"));

  if (mode == "pipeline" || mode == "dongles") {
    auto cap = readFile(argv[2]);
    double center = std::stod(opt(argc, argv, "--center", "773100000"));
    auto freqs = parseList(opt(argc, argv, "--freqs", ""));
    bool demod = opt(argc, argv, "--demod", "1") == "1";
    int k = mode == "dongles" ? std::stoi(opt(argc, argv, "--k", "2")) : 1;
    double w0 = wallSeconds();
    std::vector<PipelineResult> res(k);
    if (k == 1) {
      std::string dumpHz, dumpPath;  // --dump <hz> <out.cf32>
      for (int i = 0; i + 2 < argc; i++)
        if (std::strcmp(argv[i], "--dump") == 0) dumpHz = argv[i + 1], dumpPath = argv[i + 2];
      res[0] = runPipeline(cap, fs, center, freqs, fft, demod, dumpHz, dumpPath);
    } else {
      std::vector<std::thread> th;
      for (int t = 0; t < k; t++) th.emplace_back([&, t] { res[t] = runPipeline(cap, fs, center, freqs, fft, demod); });
      for (auto& t : th) t.join();
    }
    double wall = wallSeconds() - w0, air = res[0].airS, cpu = 0;
    for (auto& r : res) cpu += r.cpuS;
    std::printf("{\"mode\":\"%s\",\"fft\":\"%s\",\"fs\":%.0f,\"channels\":%zu,\"dongles\":%d,\"demod\":%d,\"airS\":%.2f,\"cpuS\":%.3f,\"wallS\":%.3f,"
                "\"coresPerDongle\":%.4f,\"xRealtimeWall\":%.1f,\"syncs\":[",
                mode.c_str(), fft.c_str(), fs, freqs.size(), k, demod, air, cpu, wall, cpu / air / k, air / wall);
    for (size_t i = 0; i < res[0].syncs.size(); i++) std::printf("%s%llu", i ? "," : "", (unsigned long long)res[0].syncs[i]);
    std::printf("]}\n");
    return 0;
  }

  if (mode == "scale") {
    int heads = std::stoi(opt(argc, argv, "--heads", "8"));
    double secs = std::stod(opt(argc, argv, "--secs", "10"));
    bool demod = opt(argc, argv, "--demod", "0") == "1";
    std::vector<uint8_t> cap(size_t(2 * fs * secs));
    std::mt19937 rng(1);
    for (auto& b : cap) b = uint8_t(rng() & 0xff);
    std::vector<double> freqs;
    for (int h = 0; h < heads; h++) freqs.push_back(-0.4 * fs + (h + 0.5) * 0.8 * fs / std::max(1, heads));
    double cpu = runPipeline(cap, fs, 0, freqs, fft, demod).cpuS;
    std::printf("{\"mode\":\"scale\",\"fft\":\"%s\",\"fs\":%.0f,\"heads\":%d,\"demod\":%d,\"pctCore\":%.2f}\n", fft.c_str(), fs, heads, demod, 100 * cpu / secs);
    return 0;
  }

  if (mode == "demod") {
    auto raw = readFile(argv[2]);
    const float* iq = reinterpret_cast<const float*>(raw.data());
    size_t n = raw.size() / 8;
    Pi4Demod d(fs);
    double c0 = cpuSeconds();
    for (size_t i = 0; i < n; i += 192) d.push(iq + 2 * i, int(std::min<size_t>(192, n - i)));
    double cpu = cpuSeconds() - c0;
    std::printf("{\"mode\":\"demod\",\"airS\":%.2f,\"pctCore\":%.3f,\"symbols\":%llu,\"syncs\":%llu}\n", n / fs, 100 * cpu / (n / fs),
                (unsigned long long)d.symbols, (unsigned long long)d.syncs);
    return 0;
  }
  std::fprintf(stderr, "unknown mode %s\n", mode.c_str());
  return 2;
}
