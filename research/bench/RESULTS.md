# CPU benchmark — can a browser core run a Trunk Recorder workload?

2026-09-28 · Node v22.14.0 (V8, the same JS engine as Chrome) · Apple M4 Max ·
freq-finder's production TypeScript DSP, imported unchanged.

Signals are synthetic (freq-finder's P25 encoders: shaped C4FM + noise + carrier
offset), so these are best-case decode numbers; a real channel spends more time
re-acquiring.

| Stage | Cost, % of one core | Script |
|---|---|---|
| Channelizer, 1 channel: 2.4 MSPS → 48 kSPS, per-sample `Math.cos/sin` NCO + freq-finder's `ComplexFirDecimator`, 101 taps | 4.56 % | `bench.ts` |
| Same, 201 taps | 5.18 % | `bench.ts` |
| Channelizer, 1 channel: phasor-rotator NCO, allocation-free, FIR only at output points, 101 taps | **2.31 %** | `bench_nco.ts` |
| Same, 201 taps | 2.84 % | `bench_nco.ts` |
| P25 Phase 1 voice, 48 kSPS IQ → acquire, C4FM demod, framing, FEC, IMBE → 8 kHz audio (`P25LiveVoice`) | **1.41 %** | `bench.ts` |
| P25 control channel, TSBK decode in 1 s windows (`decodeP25OTA`) | **1.06 %** | `bench.ts` |

Both decode stages were checked for correct output: the voice run produced
1 call with 10.4 s of audio from 10.9 s of air, and the control-channel run
produced 30 distinct messages.

## What it adds up to

- One voice recorder ≈ 2.3 % + 1.4 % ≈ **3.7 % of a core**.
- The control channel ≈ 2.3 % + 1.1 % ≈ **3.4 %**.
- Control channel + 8 simultaneous calls ≈ **33 % of one M4 Max core**.

This is an extrapolation, not a measurement: if a low-end laptop core is 4–5×
slower, the same load is about 1.3–1.7 cores. That is still workable when
spread over Web Workers on a 4-core machine.

Headroom not yet used:

- WASM + SIMD128 FIRs are 2.3–2.4× faster than JS (freq-finder
  `research/wasm-demod/RESULTS.md`).
- An FFT or polyphase channelizer makes the cost per extra channel nearly flat,
  as Trunk Recorder's `freq_xlating_fft_filter` does.

Not measured: converting USB u8 samples to float, and the wideband ring buffer.
Both are one pass over the input, roughly the cost of a single NCO.

## Reproduce

```bash
node --experimental-strip-types bench.ts      [path/to/freq-finder/web/src]
node --experimental-strip-types bench_nco.ts  [path/to/freq-finder/web/src]
```

---

# Update: shared FFT channelizer (after CyberEther's `filter_engine`)

`fastconv.ts` is an overlap-save, multi-head fast-convolution channelizer. One
forward FFT of the wideband block is shared by every channel. Each channel then
costs a 256-bin window × filter multiply, a 256-point inverse FFT (which also
decimates by 64), and a phase correction. All buffers are allocated at
construction. Settings: N = 16384, 4097-tap Blackman filter (7 kHz cutoff),
out 37.5 kSPS.

**Correctness** (`bench_fastconv.ts`):

- A tone at +312,550 Hz, on a head at +312,500, comes out at **+50.0 Hz,
  −0.00 dB**. A head 912 kHz away sees **−175 dB**.
- End to end, 4.4 s of 2.4 MSPS air holding a P25 CC (−600 kHz) and a P25 P1
  call (+312,537 Hz, deliberately off-bin), plus noise, both decode through
  freq-finder's decoders at 37.5 kSPS:
  - CC: 138 TSBKs, NAC 0x4d8, WACN 0xbee00, C4FM.
  - Voice: 1 call, TG 101, 4.0 s of audio from 4.3 s of voice (the decoder's
    acquisition takes the first frames).

**Cost vs channel count:**

| Channels | Fast-conv, % of one core | Per-channel FIR (2.31 % each), for comparison |
|---|---|---|
| 1 | 9.2 % | 2.3 % |
| 8 | 9.2 % | 18.5 % |
| 16 | 10.2 % | 37 % |
| 32 | 12.5 % | 74 % |
| 64 | 16.8 % | 148 % |

The marginal cost is about **0.14 % per channel**. The fixed ~9 % is almost
all the 16k-point forward FFT (`bench_fft.ts`: freq-finder's radix-2 FFT takes
380 µs at 16384 points, which is 7.4 % of a core at this block rate). That is
the one kernel where a SIMD WASM FFT (PFFFT/KissFFT-class, f32) should pay off.
This is a different algorithm from the radix-2-vs-radix-2 comparison freq-finder
measured; the gain is expected, not yet measured. The crossover with per-channel
FIRs is about 4 channels.
