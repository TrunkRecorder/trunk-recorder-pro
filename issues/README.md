# FPGA channelizer for bladeRF

Move channel splitting off the host and onto the bladeRF FPGA. Everything after channel splitting
(receivers, FEC, vocoders, trunking) stays in `trunk-core`, unchanged.

Status: 01 to 07 are done or in production use on both bladeRFs, with a few checks left in their files. 08 is built and
simulated, and its waterfall and meters haven't run on a radio. 09 is next after that. 00's baseline has gaps from the
outages.

## Why

The radios each send 32 MB/s over USB today, and the host keeps about 85 MB of spectra per radio for
pre-roll. With this work, a radio sends at most 5.4 MB/s (42 channels), and pre-roll needs about 250 KB
per enabled channel.

Expect a modest CPU saving. `trunk-pro` used about 0.2 core of 8 when measured on 2026-11-07, with up
to 3 calls recording in the sample, and part of that stays on the host. The main gains are USB traffic
and memory. Issue 00 measures the baseline and issue 09 decides whether the result is worth keeping.

## Issues

Do them in order. Sizes are rough part-time guesses: S is a few days, M is 1–2 weeks, L is several
weeks.

| # | Issue | Size | Needs | Runs on |
|---|---|---|---|---|
| [00](00-baseline-and-prerequisites.md) | Baseline and prerequisites | S | | Hardware |
| [01](01-filter-bank-model.md) | Filter bank model and parameters | M | 00 | Host, captures |
| [02](02-wire-format.md) | Wire format | S | 01 | Host |
| [03](03-host-external-channelizer.md) | Engine runs from channel blocks | M | 02 | Host, captures |
| [04](04-fpga-packet-skeleton.md) | FPGA skeleton: test packets and registers | M | 00, 02 | Hardware |
| [05](05-filter-bank-rtl.md) | Filter bank RTL in simulation | L | 01, 02 | Simulator |
| [06](06-fpga-integration.md) | Filter bank on both boards | M | 04, 05 | Hardware |
| [07](07-host-driver.md) | bladeRF driver and live recording | M | 03, 06 | Hardware |
| [08](08-power-spectrum.md) | Noise floor, waterfall and meters | M | 07 | Hardware |
| [09](09-soak-and-rollout.md) | Comparison, soak and decision | S | 08 | Hardware |

Issues 03 and 05 can run in parallel. Nothing touches the live service until issue 07, and issues 04
and 06 need a spare radio or a planned recording outage.

**Stop points:**

- After 00: fixing USB may remove the urgency. Decide whether to continue.
- After 01: stop if the filter bank can't match today's decode quality.
- After 06: stop if the xA4 won't fit or close timing, or run the xA5 only.
- After 09: keep it only if it clears the targets.

## Design

```text
AD9361 → adc_streams(0) ─┬─ packet mode off: stock wideband path (survey, capture, profile)
                         │
                         └─ async FIFO → 80 MHz domain
                              polyphase filter → 1024-point FFT → per-bin rotation
                              bin select (up to 42) ─ every 48 hops ─► one packet
                              power accumulators ─── every 16 blocks ─► one packet
                              → async FIFO → fifo_writer → FX3 → USB

host: packet → Block::Bank → Channelizer (external) → residual NCO + channel FIR → head output
      → decode_block (unchanged)
```

| Parameter | Start value | Why |
|---|---|---|
| Input rate | 8 MSPS only | Both radios use it. Filter widths scale with the rate, so other rates are refused |
| Filter bank size M | 1024 | Bins every 7.8125 kHz |
| Hop | 256 | Output is 31.25 kS/s, the rate `output_rate` gives at 8 MSPS today |
| Prototype filter | 3244 taps at 60 dB (Kaiser estimate), about 4096 in practice | Passband 11.16 kHz, stopband 20.09 kHz. Issue 01 settles it |
| Packet | 48 hops | Exactly 12288 input samples, today's block, so `decode_block` still sees 48 samples per head |
| Enabled bins | Up to 42 | 5.4 MB/s at the limit. A packet is one USB 3 DMA transaction of 2048 DWORDs, 4 of them bladeRF's header, so 2 + 48 × 42 = 2018 DWORDs is the most |

M = 512 doesn't work. Its bins are 15.625 kHz apart, which leaves a 1.1 kHz transition band and needs
about 25,800 taps. M = 1024 needs about an eighth of that.

**Passband sum:** half a bin (3.9 kHz) + half a P25 channel (6.25 kHz) + 1 kHz for tuning error =
11.16 kHz. Signals above 31.25 − 11.16 = 20.09 kHz alias into that passband.

### Estimated FPGA cost

| Block | Multipliers (18×18) | RAM |
|---|---|---|
| Polyphase filter | 3–4 | Delay 131 Kb, coefficients 37 Kb |
| 1024-point FFT, 18-bit (issue 01) | About 16 | Delay 37 Kb, twiddles 9 Kb |
| Power accumulators | 2 | 33 Kb |
| **Total** | **About 22 of 132 (xA4) / 300 (xA5)** | **About 250 Kb of about 1.1 Mb free (xA4, est.)** |

Logic is estimated at 3–5k ALMs. These figures aren't from synthesis. Measurements so far are in
[`research/fpga-channelizer.md`](../research/fpga-channelizer.md).

### Decisions

- **The HDL core lives in this repo, in `fpga/`.** Nuand's WLAN build does the same thing from a sibling
  repo: `bladerf-wlan.qip` includes `../../../../../bladeRF-wiphy/fpga/quartus/wlan.qip`. A small patch
  against bladeRF commit `35174c3` adds the revision.
- **`bladerf-wlan.vhd` is the template.** It already taps `adc_streams(0)` and drives `rx_packet_control`.
- **The image is loaded at run time** with `bladerf_load_fpga`. Never flash it. A power cycle returns the
  board to its normal image.
- **The stream format selects the path.** `SC16_Q11_META` gives wideband and `PACKET_META` gives channels,
  so survey and capture keep working on the same image. Issue 04 proves this.
- **Bins are enabled from what the engine has used.** Your config lists no voice frequencies, so the host
  learns them from grants. The first grant on a new frequency has no pre-roll.

## Out of scope

- Demodulation, FEC or anything else after channel splitting on the FPGA.
- Rates other than 8 MSPS, and sources other than bladeRF.
- Conventional channels on an FPGA-channelized source. Their detector needs today's 488 Hz resolution, so
  the config refuses the combination. Your config has none.
- Reducing the Nios or removing TX logic, unless the xA4 doesn't fit.
- Publishing bitstreams or packaging. This is a local, opt-in setup for now.
