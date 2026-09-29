# Trunk Recorder Lite — feasibility study

2026-09-28, revised the same day after reviewing CyberEther and confirming that
freq-finder decodes real signals from real dongles. The question: can Trunk
Recorder be rebuilt to run entirely in a web page, so that someone plugs an
RTL-SDR into Chrome and starts recording a trunked system, with nothing to
install?

## Verdict

**Yes. Build our own DSP engine in TypeScript (plus WASM where it pays), not a
GNU Radio port.** Take the decoders from freq-finder and the engine design from
CyberEther.

- **The decode chain is proven on real hardware.** freq-finder's WebUSB
  RTL-SDR source and its P25 control-channel and voice decoders have been
  tested against real dongles and real signals (confirmed by you, 2026-09-28).
  They are op25 and mbelib ports, checked against the reference C.
- **CPU is not the problem.** A prototype of CyberEther's shared-FFT
  channelizer, fed from one dongle, runs **32 channels in 12.5 % of one core**
  (M4 Max). A control channel plus 8 simultaneous calls, decode included, comes
  to about **21 %**. Both were measured, and the channelizer's output was
  verified to decode P25.
- **A GPU isn't needed at RTL-SDR rates.** CyberEther's own browser build runs
  its DSP on the CPU in WebAssembly and uses WebGPU only for drawing. Keep the
  engine batch-shaped so a WebGPU compute path can be added if wideband SDRs
  make it worthwhile (§5).
- **What's left are platform limits.** It needs Chromium (WebUSB). One dongle
  covers ~2.4 MHz. The page must stay open with the machine awake. Windows and
  Linux need USB driver setup. Uploads may need a relay if servers lack CORS
  support (§7).

---

## 1. What "Trunk Recorder" has to mean in a browser

| Trunk Recorder | Browser equivalent |
|---|---|
| `osmosdr` source, 1..N SDRs | WebUSB RTL-SDR (`@jtarrio/webrtlsdr`, proven in freq-finder), one worker per dongle. Later: libusb-WebUSB + vendor drivers in WASM, as CyberEther does (§4). |
| GNU Radio flowgraph (`tb`) | Our own fixed-buffer engine: a source-paced pipeline with preallocated blocks (§5) |
| Control channel: `p25_trunking` + `p25_parser`, `smartnet_*` | freq-finder `dsp/p25/*` (P25). SmartNet needs a port. |
| `monitor_messages()`: grant → free recorder → tune | A grant handler that adds a channel to the shared channelizer and assigns a decoder |
| `freq_xlating_fft_filter` / `xlat_channelizer` per recorder | **One** shared fast-convolution channelizer; a channel is a cheap "head" on it (§3) |
| `p25_recorder` / `analog_recorder` / `dmr_recorder` | `P25LiveVoice` / NFM demod / (DMR later) |
| `transmission_sink` → WAV | A per-call audio buffer, then WAV or Opus |
| `call_concluder`: ffmpeg → m4a, JSON, retries | WebCodecs `AudioEncoder` + muxer, JSON sidecar, a retry queue in IndexedDB |
| Files on disk | OPFS (default) or a user-picked folder (File System Access API) |
| Plugins (rdio-scanner, OpenMHz, Broadcastify) | `fetch()` multipart uploads (CORS permitting, §7) |
| `config.json` + talkgroup CSV | Import the same files; a wizard fills in the rest |

## 2. What freq-finder provides

Paths are under `freq-finder/web/src/`.

| Capability | Where | Status |
|---|---|---|
| RTL-SDR over WebUSB, read in a worker, 4 transfers in flight | `sdr/WebRtlSource.ts`, `sdr/delivery.ts` | **Works with real dongles** |
| Sample-loss / stall monitor | `sdr/delivery.ts` `DeliveryMonitor` | Built, tested |
| P25 P1 control channel: sync, NID/BCH, trellis, CRC, TSBK, system roll-up | `dsp/p25/*`, `dsp/p25.ts` | **Works on real signals** |
| C4FM and CQPSK/LSM receivers, automatic modulation detection | `dsp/p25/c4fm.ts`, `cqpsk.ts`, `modulation.ts` | Verified; no simulcast equalizer yet |
| P25 P1 voice (HDU/LDU/TDU, FEC, IMBE) | `dsp/p25/voice.ts`, `dsp/p25Voice.ts` | **Works on real signals** |
| P25 P2 TDMA voice (scrambler, ISCH, MAC PDUs, AMBE+2) | `dsp/p25/phase2.ts` | Verified against op25 |
| MBE vocoder (mbelib port, plus Trunk Recorder's "enhanced" changes) | `dsp/mbe/*` | Verified against compiled mbelib |
| Encrypted-call detection, live decode worker, audio playback | `dsp/p25Voice.ts`, `worker/p25voice.worker.ts`, `sdr/AudioSink.ts` | Built |
| Trunk Recorder config generation from a CC harvest | `dsp/p25/trunkRecorder.ts` | Built (the onboarding wizard's core) |
| NFM/AM demod | `dsp/Demodulator.ts` | Built |
| Simulated P25 system | `sdr/SimulatedSource.ts`, `sdr/simVoice.ts` | Built; a no-hardware dev loop |

**Gaps to fill for trunking.**

- **Trunk following.** Grant → channel → recorder, updates, and call end.
- **Grant opcodes.** freq-finder parses only `GRP_V_CH_GRANT` and
  `GRP_V_CH_GRANT_UPDT`. Port the rest from Trunk Recorder's `p25_parser.cc`:
  explicit updates, unit-to-unit, Motorola extended grants, IDEN_UP_TDMA slot
  mapping, patches, affiliations, denials.
- **The multi-channel engine** (§5).
- **Call finalization**: encode, metadata, storage, upload.
- **SmartNet**, then **DMR** later.

## 3. Evidence: CPU budget

The benchmarks are in `research/bench/`: see `RESULTS.md`, `bench.ts`,
`bench_nco.ts`, `fastconv.ts`, `bench_fastconv.ts` and `bench_fft.ts`. They
run in V8 (Chrome's engine) on an M4 Max, using freq-finder's production
decoders unchanged.

**Channelizing: one shared FFT, not one filter per channel.** `fastconv.ts` is
a TypeScript prototype of CyberEther's multi-head fast-convolution filter
engine (overlap-save, decimation by 64 in the frequency domain, 2.4 MSPS in,
37.5 kSPS out per channel).

| Channels | Shared-FFT channelizer | One FIR per channel |
|---|---|---|
| 1 | 9.2 % | 2.3 % |
| 8 | 9.2 % | 18.5 % |
| 32 | **12.5 %** | 74 % |
| 64 | 16.8 % | 148 % |

Verified correct, not just fast:

- A test tone comes out at exactly the expected frequency and level.
- A channel 912 kHz away sees −175 dB of it.
- A synthetic 2.4 MSPS capture holding a P25 control channel and an off-bin
  P25 voice call decodes through it: 138 TSBKs with the right NAC and WACN,
  and TG 101 with 4.0 s of audio.

Its fixed ~9 % is almost entirely the 16k-point forward FFT, which is
freq-finder's radix-2 JS FFT. A SIMD WASM FFT is the obvious next
optimization.

**Decoding**, per active channel:

| Stage | % of one core |
|---|---|
| P25 P1 voice: IQ → IMBE → 8 kHz audio | 1.4 % |
| P25 control channel | 1.1 % |

**Total, CC + 8 simultaneous calls ≈ 21 % of one core.** With 32 channels
(where a wideband SDR could take us) it is ≈ 57 %. A machine 4–5× slower
handles the RTL-SDR case on one or two cores.

## 4. What to take from CyberEther

CyberEther (luigifcruz/CyberEther, read at commit e80ddb3) is a C++ GPU
signal-processing framework, "Jetstream", with native (Metal/Vulkan/CUDA) and
browser (WASM + WebGPU) builds. Notes from its `docs/architecture.md`,
`docs/tensors.md` and source:

**Adopt these design ideas.**

1. **Fixed buffers, allocated once.** "Allocate in `create()`, reuse across
   cycles." Shapes are fixed when a block is created, and a shape change is a
   graph rebuild, not something the running pipeline absorbs. Output buffers
   are rewritten in place every cycle. In JS this also removes GC hitches;
   freq-finder measured ~50 MB/s of garbage from its per-block allocations.
2. **Samples move by reference.** Tensors are handles over shared storage, and
   links pass references, never copies. Every real copy is an explicit
   `copyFrom` in someone's code, so copies are visible.
3. **A circular buffer at the hardware edge, with a declared overflow policy
   and counters.**
   - The SoapySDR receive thread pushes into a `CircularBuffer`
     (`OverwriteOldest` or `Reject`, with `overflows()` and `throughput()`).
   - The source module's `hasPendingCompute()` waits until one full
     fixed-size output batch is available. That is what paces the whole graph.
   - `computeSubmit()` pops exactly one batch.
   - Buffer "health" (fill fraction) and throughput are reported.
   - This pairs well with freq-finder's `DeliveryMonitor`.
4. **A source-paced compute cycle, separate from rendering.** The compute loop
   never blocks except at the source, and rendering never waits on compute.
5. **Batch dimensions.** Tensors carry `{batch, samples}` shapes, so one
   kernel processes many channels at once. This is what makes a GPU path
   possible later.
6. **Metadata travels with the data.** `sampleRate`, `center` and `bandwidth`
   are tensor attributes, so the filter engine derives its own resampling
   plan. For us that means every block carries its sample index, centre
   frequency and rate. Recordings can then be timestamped exactly, and
   pre-roll can work (§5).
7. **The multi-head `filter_engine`**: a pipeline of FFT → per-head multiply →
   fold → IFFT → phase correction → overlap-add. This is exactly Trunk
   Recorder's "many recorders on one source" problem, prototyped above.

**Don't adopt.**

- **The framework itself, or an Emscripten build of it.** It is C++ with
  meson, dozens of subprojects, Qt-free but ImGui/GLFW-based UI and a YAML
  flowgraph editor. The DSP we need is small and already in TS or easily
  ported. We want a fixed pipeline, not a general flowgraph editor.
- **WebGPU compute, for now.** CyberEther's docs say the browser build
  "runs the blocks' CPU implementations compiled to WebAssembly, since no
  modules target WebGPU compute yet". WebGPU is graphics-only there and hosts
  no tensor storage. At 2.4 MSPS the whole channelizer is ~10 % of a core, and
  a GPU round trip (`mapAsync` readback per block) adds latency and complexity
  for no gain.
  - The GPU becomes worth it with a 10–20 MSPS SDR covering a whole system,
    running 30+ channels, and drawing the waterfall.
  - Keep the channelizer interface batch-shaped (`{heads, samples}` in, fixed
    buffers out) so a WebGPU backend could slot in behind it.

**Hardware, as a later option.** CyberEther compiles libusb, using libusb's
own WebUSB backend (`subprojects/libusb-browser.wrap`, with patches for
transfer cancellation), and SoapySDR plus `librtlsdr`, `libairspy`,
`libhackrf` and `libbladerf`, to WASM. That is a known-working route to
**Airspy/HackRF/bladeRF in the browser**. It is the natural way to get past
the RTL-SDR's 2.4 MHz without rewriting vendor drivers in TS. Keep the RTL-SDR
on the proven JS path for v1.

## 5. Proposed architecture

```
main thread (React UI) ── call list, live audio, waterfall, config/wizard
     ▲ small messages (call events, audio chunks, stats)
     │
source worker (1 per dongle)
  WebUSB transfers (4 in flight) ─▶ u8→f32 into a fixed block pool
  ─▶ wideband ring (~1 s, OverwriteOldest, overflow + health counters)
  ─▶ shared fast-conv channelizer: 1 forward FFT per block,
     heads = CC + active recorders (+ pre-roll heads)
     │  channel IQ (37.5 kSPS) via SharedArrayBuffer SPSC rings,
     │  one per head, fixed size
     ├─▶ control worker ── P25/SmartNet decode → grants → add/remove heads
     ├─▶ recorder worker(s) ── P25LiveVoice / NFM per call → audio ring
     └─▶ concluder worker ── WAV/Opus → OPFS/folder → JSON → upload queue
```

**Moving samples between workers.** There are two options.

- **SharedArrayBuffer ring buffers.** Single-producer single-consumer, with
  `Atomics` indices and `Atomics.wait` in consumers. This is the closest
  analogue of CyberEther's circular buffer, with zero copies and zero
  allocation. It needs cross-origin isolation (COOP/COEP headers). Any
  static host that lets you set headers works, e.g. Cloudflare Pages
  `_headers`.
- **Transferable buffer pool.** Fixed `ArrayBuffer`s ping-ponged between
  workers with `postMessage(…, [buf])`. No special headers, one message per
  block.

**Recommended: SharedArrayBuffer rings**, with the transferable pool kept as
a fallback behind the same interface.

What this design adds beyond native Trunk Recorder:

- **Pre-roll for free.** The wideband ring holds the last second of air, so a
  head created on a grant can start from a few hundred ms *before* the grant.
  The first syllable of a call is not lost.
- **Adding a recorder costs ~0.14 % of a core**, so there's no recorder-count
  tuning. Record every granted call in the band.
- **Zero-config onboarding.** freq-finder's harvest produces WACN, SysID, NAC,
  the CCs, modulation and the ppm correction: scan, confirm, record.

## 6. Approaches compared (updated)

| | **A. Own engine in TS (+WASM kernels), freq-finder decoders, CyberEther design** — recommended | B. Emscripten Trunk Recorder + GNU Radio + OP25 | C. Emscripten CyberEther + trunking blocks |
|---|---|---|---|
| Decoders | Exist, real-world proven | op25 as-is | Would have to be written or ported as Jetstream modules |
| Channelizer | Prototyped, verified, measured | GNU Radio's | `filter_engine` exists |
| SDR access | WebUSB RTL-SDR works today | Needs a WebUSB source written from scratch | libusb-WebUSB + Soapy works; wider device support |
| Build / size | Vite; seconds; hundreds of KB | ~1 h build, tens of MB | Large meson/emsdk build, tens of MB |
| Hosting | Static + COOP/COEP headers | Static + COOP/COEP | Static + COOP/COEP |
| UI | React, fits the product | None | ImGui canvas: a tool, not a product UI |

## 7. Constraints and risks

| Area | Issue | Mitigation |
|---|---|---|
| **Browser** | WebUSB is Chromium-only (desktop, and Android over USB-OTG). No Firefox/Safari/iOS. | Hard limit; state it up front. |
| **OS drivers** | Windows needs WinUSB (Zadig). Linux needs the `dvb_usb_rtl28xxu` module blacklisted and a udev rule. macOS works as-is. | Per-OS guidance; detect the claimed-device error and link to the fix. |
| **Spectrum coverage** | RTL-SDR ≈ 2.4 MHz. Wider systems need more dongles or a wideband SDR. | Multiple dongles (one source worker each); Airspy/HackRF via the libusb-WASM route later. Report out-of-band grants as missed calls. |
| **Tab lifetime** | The tab must stay open and the machine awake. Hidden-tab throttling, Memory Saver and sleep all interfere. | All real-time work in workers; Wake Lock; a "keep this tab open" state. **Needs a 24 h hidden-tab soak test** — the one open go/no-go item. |
| **Cross-origin isolation** | SharedArrayBuffer needs COOP/COEP, which also restricts loading third-party resources (maps, fonts) without CORP. | Self-host assets; the transferable-pool fallback. |
| **Storage** | OPFS has quota and eviction; the folder picker needs permission re-granted after reload. | `navigator.storage.persist()`, retention limits, optional folder mode. |
| **Encoding** | Opus via WebCodecs is universal in Chromium. AAC (Trunk Recorder's m4a) may be missing on Linux. | Opus by default; AAC when `isConfigSupported`. |
| **Uploads / CORS** | Unverified for rdio-scanner, OpenMHz and Broadcastify. | Test each; fall back to a small relay (e.g. a Cloudflare Worker). |
| **Can't exist** | simplestream (UDP), unit_script (shell), stat_socket, TCP MQTT | Drop them; offer MQTT-over-WebSocket or a status WebSocket. |
| **Simulcast** | LSM without an equalizer loses some TSBKs. | Same class of problem as native; compare side by side with Trunk Recorder. |
| **Licensing** | The op25 ports are GPLv3, and freq-finder's `package.json` says ISC. | Ship Lite as GPLv3; reconcile before copying code. |

## 8. Plan

**M0 — Engine spike (≈1–2 weeks).**

1. Build the source worker: WebUSB → fixed block pool → wideband ring with
   overflow and health stats.
2. Harden `fastconv.ts` for production and add a SIMD WASM forward FFT; get
   below 5 % of a core at 32 heads.
3. Add SharedArrayBuffer SPSC rings to decoder workers.
4. On a real dongle, run CC + all granted channels live in Chrome, with
   pre-roll.
5. Run the 24 h hidden-tab soak on macOS, Windows and Linux.

**M1 — P25 trunk follower MVP.** CC → grants (opcodes ported from
`p25_parser.cc`) → heads → P1/P2 voice → WAV in OPFS → call list with playback
and talkgroup CSV names. Validate side by side against native Trunk Recorder.

**M2 — Onboarding.** A wizard (find CC, harvest, choose center/rate/ppm),
`config.json` + talkgroup CSV import, per-OS driver help.

**M3 — Sharing.** Opus/AAC encoding, rdio-scanner and OpenMHz uploads (relay if
CORS blocks), a retry queue, and call JSON matching Trunk Recorder's.

**M4 — Breadth.** SmartNet, conventional analog/P25, multiple dongles.

**Later.**

- DMR.
- Airspy/HackRF through libusb-WebUSB + WASM drivers.
- A WebGPU channelizer backend and waterfall, if wideband sources arrive.

## 9. Decisions (answered 2026-09-28)

1. **Code sharing:** copy freq-finder's decoders into this repo, no shared
   package — done, `src/vendor/ff/` (`VENDORED.md` records the source commit
   and every change).
2. **Stack:** whatever fits — React 18 + Vite + TypeScript, no state library.
3. **Scope:** P25 only for now; DMR, SmartNet and analog conventional later, so
   the design keeps a protocol seam (`src/protocols/types.ts`).
4. **Uploads:** later, and never through a server of ours.
5. **Hosting:** COOP/COEP headers are fine (`public/_headers`).

The first build (M0 + most of M1) is in place; see README.md → Verified.
