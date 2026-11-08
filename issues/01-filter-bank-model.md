# 01: Filter bank model and parameters

**Size:** M. **Needs:** 00. **Stop point:** stop if quality can't be matched.

**Status (2026-11-07):** model, tests, sweep and `tool snr --bank` are done, and CQPSK control and Phase 2 voice
from real captures decode within 0.1 dB of today. 18-bit rounded coefficients reject as well as the
ideal filter, and a weak real channel 25 kHz from a 16 dB stronger one loses nothing. Done. Results are in
[`research/fpga-channelizer.md`](../research/fpga-channelizer.md).

## Goal

Prove in Rust, before writing any HDL, that the filter bank decodes as well as today's channelizer.

## Work

1. Add `crates/trunk-core/src/dsp/filter_bank.rs`: a floating-point model with M = 1024 and hop 256. It takes
   wideband IQ and gives each bin's output at 31.25 kS/s.
2. Keep phase continuous across hops with the per-bin rotation. With hop = M/4 this is `j^(k·t)`, which costs
   no multiplier.
3. Add the host side of a head: bin choice, residual NCO and a short channel FIR, using the existing
   `filters::blackman_sinc`. Heads keep their own cutoffs this way.
4. Add a fixed-point mode with settable widths: 12-bit input, coefficient bits, FFT scaling per stage, 16-bit
   output, with saturation counted.
5. Add a filter bank option to `trunk-pro tool snr` (`snrtool.rs:39` builds its `Channelizer`).
6. Sweep taps, coefficient bits, output bits and scaling on the issue 00 captures. Include channels that land
   midway between bins, the worst case for the filter.

## Done when

- [x] P25 CQPSK control and Phase 2 voice decode within 0.3 dB of today's channelizer at the 50 % point of
      `tool snr`, in fixed point.
- [x] A strong adjacent channel, both real and synthetic, costs no more than it does today.
- [x] Channels at worst-case bin offset meet the same limit.
- [x] A small `assert`-based test fails if a tone comes out at the wrong frequency or with broken phase
      continuity.
- [x] Final M, taps, widths and scaling are recorded in `research/fpga-channelizer.md`, with coefficients
      available from `FixedBank::coefficients`.

## Risk

16-bit output may lose weak channels next to a strong one, because scaling is shared by all bins. Check this
explicitly. A wider sample would raise USB use, and output gain may need to be a register (issue 04).
