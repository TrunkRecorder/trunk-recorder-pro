# Native engine: initial performance evaluation

2026-09-29 · Apple M4 Pro (macOS 26.5) · clang 17 `-O3 -ffast-math -mcpu=native`

Question: what would a ground-up native C++ engine cost, with no GNU Radio and
no OP25, built CyberEther-style (one shared FFT channelizer, fixed buffers, no
block scheduler)? It is compared with Trunk Recorder, which is GNU Radio + OP25,
and with this repo's TypeScript engine.

## Headline

Same synthetic P25 air into all three engines. The figure is % of one core
while keeping up with real time.

| Scenario | Trunk Recorder, real time | Trunk Recorder, user time only¹ | TS engine (Node) | **C++ prototype (vDSP)** |
|---|---|---|---|---|
| RTL-SDR: 2.4 MSPS, CC + 8 calls | 200 % (245 threads, 50 MB) | 43 % | 34 % | **1.1 %** |
| USRP: 8 MSPS, CC + 16 calls | 570 % (486 threads, 108 MB) | 130 % | 57 % | **2.3 %** |

¹ Trunk Recorder played unthrottled, counting user-mode CPU only. This is a
lower bound for its DSP work that leaves out the kernel time its scheduler
spends waking threads. On macOS that kernel time is 72 % of its CPU. On Linux,
futexes make it cheaper, so its real-time figure there will fall somewhere
between the two columns.

Every engine decoded every call:
- Trunk Recorder wrote 8/8 and 16/16 calls.
- The TS engine wrote 8/8 and 16/16 with 0 bad frames.
- The C++ prototype found 93–104 frame syncs per 2.4 MSPS call and 48–59 per
  8 MSPS call. Each count matches that call's length.
- The control channel gave 249 syncs in the C++ prototype and 265 TSDUs in TS.

**Compute is not the constraint for a native rewrite.** A native engine without
a flowgraph scheduler costs 40–60× less than Trunk Recorder's user-mode DSP and
about 200× less than its real-time CPU on this machine. Most of that comes from two
things:
- one shared FFT for every channel, replacing a `freq_xlating_fft_filter` chain
  per recorder;
- no thread per block. Trunk Recorder runs about 30 threads per recorder.

## What the C++ prototype does (and doesn't)

`cpp/` contains the following. The prototype has no dependencies beyond an FFT.

- `channelizer.hpp` is a port of `src/engine/channelizer.ts`: overlap-save
  multi-head fast convolution, N = 16384, a 4097-tap Blackman filter with a
  7 kHz cutoff, power-of-two decimation, and pre-roll history. It uses f32; the
  TS original uses f64.
- `fft.hpp` has three backends behind one interface: a port of the TS radix-2
  FFT, FFTW3f (split format), and Apple vDSP.
- `demod.hpp` is a streaming π/4-DQPSK receiver. It uses freq-finder's
  algorithm: derotate, RRC, Gardner, then differential detection. It runs as
  a single pass instead of re-demodulating overlapping 0.9 s windows. It
  re-picks the symbol phase by energy after 1 s without sync, which the TS
  receiver does on every window. The output is dibits, followed by a P25
  frame-sync counter.

It stops at frame sync. **It does not include** NID/FEC, TSBK parsing, IMBE
synthesis, call management or file writing. In the TS profile all of those
together are about 5 % of the engine's time, and there is no reason for them
to cost more than about 0.5 % of a core in C++. That figure is an estimate,
not a measurement.

## Channelizer scaling

Random u8 input, channelizer only. % of one core at real time:

| FFT | Rate | 1 ch | 8 ch | 32 ch | 64 ch | 128 ch |
|---|---|---|---|---|---|---|
| **vDSP** | 2.4 MSPS | 0.37 | 0.52 | 0.95 | 1.53 | 2.71 |
| | 8 MSPS | 1.15 | 1.29 | 1.75 | 2.13 | 2.92 |
| | 20 MSPS | 2.93 | 3.21 | 4.26 | 5.28 | 7.24 |
| FFTW3f (split) | 2.4 MSPS | 1.24 | 1.42 | 1.87 | 2.52 | 3.85 |
| | 8 MSPS | 4.10 | 4.17 | 4.62 | 5.11 | 6.04 |
| | 20 MSPS | 10.19 | 10.89 | 11.71 | 12.77 | 15.21 |
| radix-2 port | 2.4 MSPS | 1.92 | 2.13 | 2.82 | 3.72 | 5.44 |
| | 8 MSPS | 6.42 | 6.36 | 7.27 | 7.95 | 9.38 |
| | 20 MSPS | 15.71 | 17.02 | 18.01 | 19.75 | 23.10 |

- Adding channels is nearly free. At 20 MSPS, going from 1 to 128 channels
  costs +4.3 % of a core with vDSP.
- The wideband forward FFT dominates, and it scales linearly with sample rate.
- FFTW in split format is 3.5× slower than vDSP here. It was the portable
  option tested, but it is the wrong layout for FFTW: interleaved FFTW, PFFFT,
  pocketfft and KFR still need measuring on Linux/Windows.

## Full pipeline by backend

Channelizer plus demod on every channel, % of one core:

| | vDSP | FFTW3f | radix-2 | vDSP, SIMD off² |
|---|---|---|---|---|
| 2.4 MSPS, CC + 8 calls | 1.1 | 1.8 | 2.6 | 2.6 |
| 8 MSPS, CC + 16 calls | 2.3 | 5.2 | 7.5 | — |

² Built with `-fno-vectorize -fno-slp-vectorize`. vDSP itself stays
vectorised. Auto-vectorisation of our own loops is worth about 2.4×.

Demod alone, per channel: **0.05 %** of a core in C++, against **1.3 %** for
TS `demodulate(…, "pi4dqpsk")` on the same channel IQ in a single pass. That
is 27× faster.

## Multiple dongles

This runs k copies of the 2.4 MSPS pipeline (CC + 8 calls each), one thread
per simulated dongle, all at once:

| Dongles | Channels | Total CPU (% of one core) | Per dongle | Wall time for 20 s of air |
|---|---|---|---|---|
| 1 | 9 | 1.0 | 1.0 | 0.21 s |
| 2 | 18 | 2.2 | 1.1 | 0.23 s |
| 4 | 36 | 4.6 | 1.2 | 0.24 s |
| 8 | 72 | 9.6 | 1.2 | 0.26 s |

Scaling is linear with no contention: 8 dongles and 72 channels together take
under a tenth of one core.

**Not measured, because no dongle was attached:** USB. Each RTL-SDR at
2.4 MSPS moves 4.8 MB/s. A USB 2.0 controller manages about 35–40 MB/s in
practice, so the practical limit is roughly 6–8 dongles per controller, and
hubs share the bandwidth. That is the real multi-dongle ceiling, and it is the
next thing to test, using librtlsdr async reads or libusb on each OS.

## A quieter site: 2 dongles × 2 calls

This setup has two RTL-SDRs at 2.4 MSPS on one P25 system:
- **Dongle A** carries the CC and 2 calls.
- **Dongle B** carries 2 calls that are granted on A's CC.
- **Idle** is the same two dongles with the CC only and no calls, which is
  where a quiet system spends most of its time.

Trunk Recorder has 4 recorders per source. % of one core, both dongles
together:

| | Trunk Recorder, real time | Trunk Recorder, user time only | TS engine³ | **C++ vDSP** | C++ FFTW | C++ radix-2 |
|---|---|---|---|---|---|---|
| 2 dongles, 4 calls | 127 % (247 threads, 43 MB) | 23 % | 15 % (A only) | **1.1 %** | 2.9 % | 4.2 % |
| 2 dongles, idle | 26 % (247 threads, 35 MB) | 4.3 % | 8.8 % (A only) | **0.76 %** | 2.5 % | 3.9 % |

³ `tools/replay.ts` takes one capture with a CC, so only dongle A was measured.
The figure includes about 0.3 s of Node startup per run.

- **Idle cost is dominated by per-dongle overhead:**
  - In Trunk Recorder, all 247 threads exist even with no calls, because
    recorders are built up front.
  - In C++, it is the wideband FFT, which runs whether or not there are calls:
    0.33 % per dongle with vDSP.
- **Decoding:** Trunk Recorder wrote all 4 calls (17–19 s each). C++ sync
  counts per call were 99–105.
- **CC sync count in C++ dropped to 209 syncs next to 2 calls, against 266 with
  no calls.** That is a receiver-lock issue to look at, not a cost.

### Measured on real hardware

These two measurements used live dongles. The trunk-recorder figures come from
the two instances running on this machine during these tests:

| | % of one core |
|---|---|
| `rtl_sdr` async reading serial 200 at 2.4 MSPS to /dev/null for 20 s (librtlsdr/libusb USB ingest) | **0.35 %** |
| Live `trunk-recorder`, WMATA SmartNet: 2 × RTL at 1.8 MSPS, 16 recorders (sampled 30 s; average over 2.6 days of uptime) | **130 %** (129 % avg), 493 threads, 86 MB |
| Live `trunk-recorder`, DCFD P25: USRP at 8 MSPS, 8 recorders (sampled 30 s; average over 2.7 days of uptime) | **337 %** (216 % avg), 258 threads, 79 MB |

So a native engine on a 2-dongle RTL site should total about 2 % of one core,
USB included: 2 × 0.35 % USB plus about 1.1 % DSP. The live Trunk Recorder on
the same class of site uses 130 %.

These two live instances were running throughout all the benchmarks in this
file and used about 4.7 of the 12 cores. The figures here are CPU time, which
contention barely affects, but the wall-clock rates are pessimistic.

## Real air: DCFD P25 (simulcast CQPSK)

This is 30 s from RTL-SDR serial 200 at 858.3 MHz and 2.4 MSPS, gain 38.6,
recorded 2026-09-29 12:12 EDT:
- **Control channel:** 857.9875 MHz, at −312.5 kHz from center.
- **Voice channels in band:** 858.5875, 859.0375 and 857.5875 MHz.

| | CPU | Result |
|---|---|---|
| TS engine (`npm run replay`) | 16 % of a core | CC: CQPSK, NAC 443, 436 good / 268 bad TSBKs (62 %). In-band calls: TG 102 got 5.0 s of audio from about 13 s on air. TG 728 got 11.2 s from about 21 s. TG 2147 produced no audio. |
| C++ prototype, vDSP (radix-2) | **0.67 %** (2.2 %) | Frame syncs: CC 380, 858.5875 256, 859.0375 146, 857.5875 0 (idle) |
| Live `trunk-recorder` on the USRP, same 30 s | (whole instance, 8 MSPS: 337 %) | Recorded TG 728 at 858.5875 (12 s, frame error counts up to 167) and TG 729 at 860.9875 (out of the RTL's band) |

The sync spacing shows that the C++ receiver is locking onto real frames:
- **CC:** 359 gaps of exactly 360 symbols (a TSDU of 3 TSBKs) and 20 gaps of
  720 (one frame missed). That is about **95 % of CC frame syncs found** on
  simulcast.
- **Voice:** gaps of 864 (LDUs) and 216 (TDULCs repeated through hang time).

Sync is not decode. The C++ prototype checks no NIDs, CRCs or voice. On this
simulcast system every engine loses frames: the TS engine's TSBK CRC pass
rate is 62 %, and the live Trunk Recorder's per-file error counts run into the
hundreds. The CPU budget leaves room for an LSM equaliser, which is where a
rewrite should spend it.

## Full decode in C++ (2026-09-29, second pass)

The C++ prototype now decodes both the control channel and the voice channels,
and records calls end to end. It was built in stages, and each stage was
checked against the TS engine on identical input. Trunk Recorder is the
end-to-end reference. There is still no GNU Radio and no OP25 runtime. The
algorithms come from the TS code, which descends from op25 and mbelib, via
line-by-line ports.

| Stage | Files | Checked against TS | Result |
|---|---|---|---|
| Receivers: CQPSK (simulcast) and C4FM, run together | `demod.hpp`, `c4fm.hpp` | via the stages below | C4FM is needed: the CQPSK receiver gets 0 valid NIDs on the synthetic C4FM CC (the sync is outer symbols only; the NID's inner symbols come out wrong) |
| Framer: sync, status symbols, NID (BCH 63,16) | `p25_frame.hpp` | via the stages below | streaming, no windows |
| CC: TSDU trellis, CRC-16, TSBK | `p25_tsbk.hpp` | same 12-byte blocks (`compare_tsbk.py`) | synthetic C4FM: **798** vs TS 795 (all 795 identical; the 3 extra are the capture's last TSDU). DCFD simulcast: **435 good / 269 bad** vs TS 436/268, 419 identical blocks |
| Voice FEC: IMBE Golay/Hamming + PN, LC RS(24,12), ES RS(24,16), HDU, TDULC | `p25_fec.hpp`, `p25_voice.hpp` | TS re-decodes the C++ frames' raw bits (`ts_voice_check.ts`) | **0 mismatches**: 1,944 codewords, 109 LCs, 107 ESs, 9 HDUs, 249 TDULCs (synthetic + 2 DCFD voice channels) |
| IMBE vocoder (mbelib and "enhanced" profiles) | `mbe.hpp`, `tables.gen.hpp` (generated from the TS tables) | same parameters, same RNG, sample by sample (`ts_vocoder_check.ts`) | max difference **1.8e-7**, 146–157 dB SNR, both profiles |
| Trunking: TSBK parser, call manager, voice call tracker, WAV + JSON | `trunk.hpp`, `recorder.cpp` | call logs and audio | see below |

**End to end.** CPU is % of one core for the whole recorder: channelizer, both
receivers on every channel, decode, vocoder and files.

| Capture | C++ `recorder` | TS `npm run replay` | Trunk Recorder |
|---|---|---|---|
| Synthetic 2.4 MSPS, CC + 8 calls | 8 calls, 17.5–18.5 s each, **2.3 %** | 8 calls, 16.9–18.7 s, 34 % | 8 calls, 17–19 s, 150–200 % |
| Synthetic 8 MSPS, CC + 16 calls | 16 calls, 8.1–10.3 s, **4.1 %** | 16 calls, 7.6–10.6 s, 57 % | 16 calls, 340–570 % |
| DCFD real air (RTL, 30 s) | TG 102 5.8 s, TG 728 11.0 s, **1.0 %** | TG 102 5.0 s, TG 728 11.2 s, 16 % | same RTL capture: TG 102 8.1 s, TG 728 10.6 s |
| DCFD, C++ with the saved band plan | TG 102 **8.8 s**, TG 728 11.0 s | — | — |

- **The call logs match TS call for call on DCFD.** Grant times, talkgroups,
  and the encrypted, out-of-band (`no_source`) and TDMA decisions all agree.
- **The audio agrees too.** The 20 ms loudness envelopes correlate with TS at
  0.86 (TG 728) and 0.999 (TG 102). With Trunk Recorder the correlation is
  0.72–0.90. Vocoders draw random phases, so waveforms can't match exactly.
- **The band plan explains TG 102's shortfall.** Trunk Recorder resolved it
  from a table-0 IDEN at about 1.4 s. That TSDU failed its CRC in both C++ and
  TS, whose first table-0 IDEN came at 4.08 s. The C++ recorder now saves the
  band plan (`--bandplan file`): it almost never changes, so the next run
  follows the grant heard at 0.10 s. That run recorded 8.8 s (4.4 s voiced vs
  Trunk Recorder's 4.1 s, envelope correlation 0.90).

**Bugs found and fixed on the way:**
- **Heap corruption in the FFTW backend.** A new-array split plan needs the
  same `ii − ri` gap it was planned with.
- **Duplicated voice.** Both receivers delivered the same frames. Frames now
  carry a channel-sample timestamp from the demodulator, since each receiver's
  symbol count drifts on its own. Frames are deduplicated on that clock.

**Not done yet:**
- **Phase 2 TDMA** (H-DQPSK, AMBE+2, scrambling). DCFD's 770 MHz grants are
  TDMA; they are out of this capture's band anyway. *(Since done, in Rust:
  see [Phase 2 TDMA in Rust](#phase-2-tdma-in-rust-2026-09-29).)*
- **Live input.** Needs librtlsdr/libusb async plus several dongles in one
  engine.
- **Other features:**
  - talkgroup CSV;
  - patches;
  - resolving a grant that arrived before its IDEN when the IDEN comes later;
  - CC hunting across several control channels (coded, untested).
- **Simulcast CC quality, the real gap.** 62 % of TSBKs pass CRC. Trunk
  Recorder (OP25's CQPSK receiver) wins some frames ours lose, and ours win
  more overall. Worth trying:
  - keeping CRC-valid TSBKs from several receiver variants per TSDU (cheap:
    the CC costs about 0.4 % of a core);
  - an LSM equaliser.

Tools, all in `cpp/` (`make p25tool recorder`) and the directory above:
- `p25tool cc|frames|voice` (`--iq`, `--audio`, `--demod`, `--profile`) runs
  single stages.
- `recorder` is the end-to-end recorder.
- `ts_cc.ts`, `ts_voice_check.ts` and `ts_vocoder_check.ts` are the TS
  references.
- `compare_tsbk.py` and `compare_audio.py` do the comparisons.
- `gen_tables.ts` regenerates `tables.gen.hpp`.

## Simulcast (LSM) decoding (2026-09-29, third pass)

This pass worked on the control and voice channels of DCFD, which is a CQPSK
simulcast system. Two kinds of test data were used:
- **Real captures:** the 30 s one, plus a new 60 s capture from serial 200
  recorded after this work started, as held-out data.
- **Synthetic ground truth:** `gen_lsm.ts` builds a CQPSK CC and voice through
  a two-transmitter channel, y = x + a·x(t−τ)·e^{j2πΔf·t}, plus AWGN. Every
  frame is known, so `score_lsm.py` reports exact TSBK and IMBE codeword error
  rates. Real air can't give those.

**What was limiting simulcast wasn't mostly the signal.** On DCFD, 250 of the
269 failed TSBKs were op25's greedy trellis decoder giving up on a tie. It then
also dropped the rest of the TSDU, another 328 blocks never tried.

| Change | Where | DCFD 30 s CC (good/bad) | DCFD 60 s CC | Voice |
|---|---|---|---|---|
| Baseline (op25 greedy trellis) | | 435 / 269 (62 %) | 896 / 545 (62 %) | |
| **Viterbi** trellis (hard), `p25_tsbk.hpp` | CC | 937 / 95 (91 %) | 1946 / 178 (92 %) | |
| **Soft bits**, amplitude-weighted (`softAmplitude`) | CC | 1015 / 17 (98.4 %) | 2107 / 17 (99.2 %) | |
| **Flywheel** sync (≤ 12 bit errors where the next frame must start) + **NID recovery** (nearest of the 7 NIDs for the known NAC, ≤ 11 errors), `p25_frame.hpp` | both | 1149 / 21 from 390 TSDUs (344 before, of about 400 on air) | 2354 / 19 from 791 TSDUs (708 before) | LDUs missing inside transmissions: 34 + 42 → **3 + 3**; +15 % LDUs |
| **Soft FEC**: Chase-II Golay(23,12), maximum-likelihood Hamming(15,11), `p25_fec.hpp` | voice | | | synthetic simulcast: wrong codewords **7.6 % → 2.2 %**, audible (wrong but played) 177 → 41 |
| **Receiver diversity** (CQPSK, CQPSK + T/2 CMA equaliser, C4FM; per TSBK the CRC-valid copy, per codeword the most confident), `diversity.hpp` | both | 1156 / 20 | 2356 / 17 | synthetic (40 µs, 0.7 echo): wrong codewords 2.2 % → **0.00 %**, CC 100 % |

The extra TSBKs are real:
- no false TSBKs on synthetic, where every TSBK is known;
- on air, the unusual opcodes are standard ones (0x27, 0x2a, 0x2f with MFID 0);
- one-off blocks are mostly TDMA sync broadcasts, which carry a timestamp.

Recovered LDUs have the same codeword quality as the rest.

**Synthetic ground truth** (`lsm_eval.sh`), CC TSBKs and wrong IMBE codewords:

| Channel | Old receiver | CQPSK + this pass | Diversity |
|---|---|---|---|
| τ 40 µs, echo 0.7, Δf 2 Hz, 20 dB | CC 69 %, 7.4 % wrong | 99.9 %, 2.2 % | **100 %, 0 %** |
| τ 40 µs, echo 0.7, 13 dB (noise-limited) | — | 83.8 %, 20 % | 87.6 %, 18 % |
| τ 80 µs, echo 0.9 (severe) | — | 53.9 %, half the LDUs lost | 55.5 % — still limited upstream (timing / sync) |

**What didn't help:**
- **A fixed CMA equaliser alone.** It is excellent on two-ray multipath but
  hurt on the real DCFD captures: more flagged codewords, and the CC −1 %.
  DCFD at this site behaves as mostly noise-limited. As one receiver in the
  diversity bank it is kept, since it wins where it helps. It needed error
  clipping and a divergence reset: CMA on the noise before a call ran away
  and lost every voice channel.
- **Coherent detection** (decision-directed 8-PSK PLL with decision-feedback
  differential detection). It was worse at every loop gain; the simulcast
  beat defeats a simple PLL. It remains as `--coherent`, off.
- **Vocoder repeat flags from soft costs.** They gave no better trade-off than
  TIA's E0 ≥ 3 / ET ≥ 10 on the soft decoder's own counts, which is kept.
  Flagging on the hard decoder's counts plays more garbage frames.

**End to end** with the `recorder`, which now uses the diversity bank by
default, on DCFD 30 s:

| Call | C++ now | C++ before this pass | TS | Trunk Recorder, same RTL capture | Trunk Recorder live (USRP) |
|---|---|---|---|---|---|
| TG 102 | **9.5 s** (4.5 s voiced) | 5.8 s | 5.0 s | 8.1 s (4.1 s voiced) | — |
| TG 728 | **11.7 s** (6.8 s voiced) | 11.0 s | 11.2 s | 10.6 s (5.6 s voiced) | 11.9 s (6.5 s voiced) |

- **The audio is closer to the best reference.** TG 728's envelope correlation
  with Trunk Recorder's live USRP recording rose from 0.67 to **0.91**. TG 102
  correlates with Trunk Recorder on the same capture at 0.98.
- **TG 102 now starts early without the saved band plan.** The better CC
  decoding catches the early table-0 IDEN.
- **The 60 s capture's calls match the live instance's call for call** (TG 101
  on both voice channels, TG 2501). Envelope correlation is 0.59–0.88 against
  the USRP recordings. The RTL has less voiced audio on the weakest call, which
  is the stronger radio's SNR advantage.

**CPU:** the recorder with three receivers per channel uses 1.5 % of a core on
DCFD (CC + 2 voice channels), 2.3 % on the busier 60 s capture, and 8.6 % on
the synthetic 8 MSPS, 16-call capture.

**Next for simulcast:**
- **Severe delay spread** (τ ≥ 0.3 symbol at near-equal power) breaks timing
  and sync before any of this helps. It needs an equaliser that runs before
  the timing decision, or MLSE.
- **Noise-limited reception** is now the main loss on DCFD here. The decoder
  can only add soft combining across receivers (not just selection); the
  rest is antenna and gain.

Tools: `gen_lsm.ts`, `score_lsm.py` and `lsm_eval.sh` (synthetic ground truth);
`simulcast_eval.sh` (real captures); `p25tool` flags `--trellis`, `--soft`,
`--flywheel`, `--nidrecover`, `--softfec`, `--eq`/`--mu`, `--coherent`,
`--diversity`.

## Phase 2 TDMA in Rust (2026-09-29)

**Capture.** DCFD's control channel grants TDMA voice on 769.9–774.3 MHz. Two
RTL-SDRs recorded 180 s at the same time: SN 200 on the control channel
(858.3 MHz) and SN 91 on three TDMA channels (770.7 MHz centre). Both ran at
2.4 MSPS with 0 samples dropped. Most TDMA talkgroups on this site use AES
(algid 0x84). TGs 2203 and 2207 are clear.

**What was ported** (from the archived TS engine's op25-derived `phase2.ts`
and mbelib's AMBE+2):

- the 6000 sym/s H-DQPSK receiver (the CQPSK receiver with a symbol-rate
  option);
- the slot framer (S-ISCH sync, I-ISCH slot confidence);
- the scrambler (WACN / SysID / NAC from the control channel);
- DUID decoding, AMBE codeword FEC (Golay 24 + PN-masked Golay 23, soft
  Chase-II on c1);
- ESS (RS(44,16)) and MAC PDUs (RS(63,35) with erasures, CRC-12);
- the AMBE+2 vocoder;
- a per-channel TDMA tracker (both slots, PTT / END_PTT, cipher). One channel
  head serves both slots' calls.

**Equivalence with the TS decoder** (`ts_p2_check.ts` re-decodes the raw
slot dibits `trunk-lite tool p2 --soft none` printed):

| | Compared | Mismatches |
|---|---|---|
| Burst types | 6,741 slots | 0 |
| AMBE codewords (bits + FEC error count) | 10,588 | 0 |
| MAC PDUs | 3,741 | 0 |
| AMBE+2 vocoder audio, both slots, two channels | 210 s | max difference 0 |

**Receiver vs the TS H-DQPSK chain** (`ts_p2_rx.ts`, the same channel IQ;
"clean" = AMBE codeword with ≤ 1 bit corrected):

| Channel | TS slots | Rust slots | TS clean | Rust clean | Rust MAC ok / fail |
|---|---|---|---|---|---|
| 769.9062 | 2013 | 2016 | 90.2 % | 90.4 % | 1071 / 142 |
| 770.4688 | 2842 | 2844 | 82.1 % | 82.2 % | 1042 / 314 |
| 770.9688 | 4725 | 4725 | 97.1 % | 97.1 % | 2440 / 88 |

Rust costs 0.85 % of a core per channel, channelizer included. The TS
demodulator alone costs 2 %. The T/2 CMA equaliser is slightly worse here
(5 taps: −0.1 to −1.3 points), so TDMA channels run the plain receiver.

**The engine end to end** (`replay` with both captures):

- **Slot mapping:** MAC_PTT talkgroups sit on the grants' TDMA slots (TG 2207
  and 2203 on logical channel 1, as granted).
- **Encryption:** three calls not flagged encrypted in their grants were
  found encrypted from the voice channel (PTT / ESS algid 0x84).
- **Clear calls:** all three were recorded. Two have exactly the audio on air
  (5.12 s and 3.16 s of voice frames). The third was already in progress at
  capture start (heard via an update at 0.26 s) and is 0.7 s short of its
  11.8 s.
- **Speed:** 180 s of air from both sources in 4.9 s.

**Bug found on the way.** The CQPSK receiver's re-acquisition could move the
sampling point past the filtered samples. It panicked on a TDMA channel
demodulated as Phase 1, where there is no Phase 1 sync, so it re-acquires
every 0.5 s. It now steps back a symbol instead. Phase 1 output is unchanged:
byte-identical audio on the DCFD captures.

**Reproduce:**

```bash
trunk-lite replay --source p2cc.cu8,858300000,2400000 --source p2v.cu8,770700000,2400000 --cc 857987500 --out out/
trunk-lite tool p2 p2v.cu8 --center 770700000 --freq 770968750 --nac 0x443 --sysid 0x445 --wacn 0xbee00 --soft none --audio rs.f32 --slot 0 > p2.jsonl
node --experimental-strip-types ts_p2_check.ts p2.jsonl 0x443 0x445 0xbee00 rs.f32 0
trunk-lite tool voice p2v.cu8 --center 770700000 --freq 770968750 --iq ch.cf32 > /dev/null
node --experimental-strip-types ts_p2_rx.ts ch.cf32 37500 0x443 0x445 0xbee00
```

## Where Trunk Recorder's CPU goes

This comes from `sample` of the 2.4 MSPS real-time run, top of stack:
- **Almost all of it is thread coordination.** `__psynch_cvwait`,
  `__psynch_cvsignal`, mutexes, and `boost::posix_time` timestamps from the
  scheduler dominate.
- **Actual DSP is a small share.** FFTW codelets, VOLK multiplies and OP25's
  fixed-point IMBE (`L_add`, `L_shr`, …) together barely show.
- **It has a lot of threads.** 245 threads at 2.4 MSPS with 8 recorders, and
  486 at 8 MSPS with 16.
- **Unthrottled it burns 1M involuntary context switches for 20 s of air.**
  30.5 CPU-seconds in all, 22 of them in the kernel.

## What this means for the rewrite

1. **Design for I/O and decode quality, not CPU.** Even a Raspberry Pi 5 core,
   which is perhaps 3–5× slower than an M4 Pro core (an estimate, not
   measured), would run several dongles on one core.
2. **Use one thread per dongle.** Its path is the USB callback, then a ring
   buffer, then the channelizer, then each channel's demod and decode inline.
   If one dongle ever overloads its core, a small worker pool can take the
   per-channel stages. Do not use a thread per block.
3. **Acceleration where it pays:**
   - **SIMD: yes.** It was worth 2.4× here. Use compiler auto-vectorisation
     plus a SIMD FFT.
   - **FFT library:** use vDSP on Apple; it is free and part of the OS. Linux
     and Windows need a comparison of PFFFT (BSD, NEON/SSE/AVX), pocketfft,
     KFR and interleaved FFTW.
   - **GPU: not for the channelizer at ≤20 MSPS.** The whole 20 MSPS,
     128-channel channelizer is 7 % of one core, so a GPU round trip would
     cost more than it saves. It makes sense for:
     - 50–100 MSPS SDRs (USRP X/N series, bladeRF at 61.44 MSPS);
     - many dongles on weak CPUs;
     - drawing the waterfall.

     Keep the channelizer batch-shaped (`{heads, samples}`) so a
     Metal/Vulkan (VkFFT) backend can slot in behind it, as CyberEther does.
4. **Change how pre-roll history is stored.** Keeping 1 s of spectra costs
   25 MB at 2.4 MSPS, 85 MB at 8 MSPS and 213 MB at 20 MSPS. Storing the raw
   u8 samples instead (4.8 / 16 / 40 MB) and re-running the FFTs on replay
   would be cheaper.
5. **Receivers need real-air work.** The streaming CQPSK loop needed energy
   re-acquisition to lock reliably on C4FM. A purpose-built C4FM receiver
   (discriminator plus matched filter) alongside the CQPSK one for LSM/simulcast
   is the right split, and both must be validated on real captures. Synthetic
   signals here are clean, with no fading or simulcast.

## Caveats

- **Synthetic, best-case signals.** Real air costs more time in re-acquisition.
- **The C++ prototype stops at frame sync** (see above).
- **Trunk Recorder was measured on macOS, where GNU Radio's thread wake-ups are
  costlier than on Linux.** Its real-time runs use the iqfile source, whose
  throttle differs from an osmosdr or UHD source.
- **Everything ran on one machine** (M4 Pro). Low-end x86 and ARM SBC numbers
  still need measuring.

## Reproduce

```bash
cd research/native-bench
node --experimental-strip-types gen.ts /tmp/rtl8   --fs 2400000 --dur 20 --calls 8    # .cu8 + .cf32
node --max-old-space-size=12000 --experimental-strip-types gen.ts /tmp/usrp16 --fs 8000000 --dur 12 --calls 16
# two dongles, one system: A has the CC + 2 calls, B has 2 calls granted on A's CC
SYSC=772500000,773600000,775000000,776000000
node --experimental-strip-types gen.ts /tmp/d2a --center 773100000 --system $SYSC --render 772500000,773600000 --dur 20
node --experimental-strip-types gen.ts /tmp/d2b --center 775500000 --cc 0 --system $SYSC --render 775000000,776000000 --dur 20

cd cpp && make bench bench-novec
F=773000000,772262500,772500000,772737500,772975000,773225000,773462500,773700000,773937500
./bench pipeline /tmp/rtl8.cu8 --fs 2400000 --center 773100000 --freqs $F --fft vdsp   # or fftw, radix2
./bench dongles  /tmp/rtl8.cu8 --k 8 --fs 2400000 --center 773100000 --freqs $F
./bench scale --fs 20000000 --heads 128 --fft vdsp

# TS engine
npm run replay -- /tmp/rtl8.cu8 --center 773100000 --rate 2400000 --cc 773000000 --out /tmp/calls

# Trunk Recorder: iqfile source on the .cf32 ("iqType": "complex", "center": 773100000,
# "rate": 2400000), P25 system, control channel 773000000, modulation fsk4.
# TR_IQ_FILE_THROTTLE_X=1 plays in real time; set it to 100 for unthrottled.
```
