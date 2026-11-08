# FPGA channelizer: measurements

Results for the plan in [`issues/`](../issues/README.md). The model is `crates/trunk-core/src/dsp/filter_bank.rs`.

## Design so far (issue 01, 2026-11-07)

| Setting | Value |
|---|---|
| Bins, hop, output rate | 1024, 256, 31.25 kS/s |
| Prototype | 4096 taps, Kaiser β 7, −6 dB at 15.625 kHz |
| Coefficients, twiddles | 18 bits |
| Fold and FFT data | 18 bits |
| FFT scaling | Halve after butterfly stages `0b1110111011` (bit 0 first) |
| Output | Q15 |
| Host head | Residual NCO, then 17-tap Blackman FIR |
| Estimated multipliers | About 20 18×18 (4 for the filter at 80 MHz, 16 for four complex multiplies) |

Accepted (issue 01 done): CQPSK control and Phase 2 voice pass on real captures, including a weak channel next to
a strong one, and rounded coefficients reject as well as the ideal filter.

## Filter and arithmetic

From `cargo test --release -p trunk-core dsp::filter_bank::tests::sweep -- --ignored --nocapture`. Input is a
−53 dB wideband floor (about −80 dB per channel, like the live radios), a −60 dB wanted carrier and a −15 dB
carrier elsewhere in the band.

"Alias" is the worst tone folding into a channel at the worst offset between bins, calculated over the whole
stopband. "Error" is the fixed-point output against the same filter in floating point.

| Taps | β | Worst alias |
|---|---|---|
| 2048 | 4 | −39.7 dB |
| 3072 | 5 | −58.2 dB |
| 4096 | 6 | −68.3 dB |
| **4096** | **7** | **−75.5 dB** |
| 4096 | 8 | −62.8 dB |
| 5120 | 9 | −93.3 dB |
| Today's channelizer (same measure, its filter alone) | | −96.0 dB |

Live radios peak near −20 dBFS over a channel floor near −80 dBFS. A −75.5 dB alias of the strongest signal lands
about 15 dB below the floor.

| Data bits | FFT scaling | Error | Below channel floor | Multipliers |
|---|---|---|---|---|
| 16 | None | −62.6 dBFS | −18.5 dB (above the floor) | 20 |
| 18 | None | −83.4 dBFS | 2.2 dB | 20 |
| 18 | `0b1010101010` | −95.6 dBFS | 14.5 dB | 20 |
| **18** | **`0b1110111011`** | **−97.4 dBFS** | **16.3 dB** | **20** |
| 20 | `0b1110111011` | −101.6 dBFS | 20.5 dB | 36 |
| 24 | None | −102.0 dBFS | 20.9 dB | 36 |

The chosen scaling is 16.3 dB below the floor, which costs about 0.1 dB of SNR. The Q15 output limits error to
about −102 dBFS however wide the data is. Scaling every stage (`0b1111111111`) or nine of them (`0b0111111111`)
saturates in the fold on loud input, so those are out.

Coefficient widths from 12 to 18 bits and twiddle widths from 12 to 18 bits all land within about 2.5 dB of error,
and not in order of width.

Rejection with the ROM's rounded coefficients (the sweep's "rounded" column):

| Coefficient bits | Worst alias, rounded |
|---|---|
| 12 | −69.1 dB |
| 14 | −75.3 dB |
| 16 | −75.5 dB |
| **18** | **−75.5 dB**, the same as the ideal filter |

18 bits costs nothing extra, because the Cyclone V multipliers are 18 bits wide, so it stays. The test
`what_would_alias_into_a_channel_is_rejected` now checks the rounded coefficients against −70 dB.

## Real captures (2026-11-07)

60 s from each radio at 8 MS/s, gain 30 dB, taken with `bladeRF-cli` during a recording outage (22:27 UTC).
Files are in `/home/trunk-recorder/captures/` (`<serial>.cs16`, 1.9 GB each). Levels were steady from the first
sample: wideband RMS −25 dBFS on the xA5 (`37d4f970`) and −39 dBFS on the xA4 (`0da2f521`), peaks under 330 of
2048. All three control channels and nearly all voice are on `37d4f970`, so that capture is the one used.

`tool snr` with `--trials 3`, channelizer against the 18-bit bank (`--bank default`). "50 %" is interpolated
between the SNR steps. "Offset" is the channel's distance from the nearest bin, in bins (0.5 is the worst case).

| Channel | Offset | Clean count: channelizer / bank | 50 %: channelizer | 50 %: bank |
|---|---|---|---|---|
| NJICS control 774.79375 (TSBKs) | 0.2 | 2385 / 2385 | 4.1 dB | 4.0 dB |
| MIDD control 770.59375 (TSBKs) | 0.2 | 1464 / 1464 | 3.0 dB | 3.1 dB |
| PWAY control 771.25625 (TSBKs) | 0.0 | 1589 / 1589 | 2.8 dB | 2.8 dB |
| MIDD Phase 2 voice 770.08125 (frames) | 0.4 | 792 / 788 | 7.4 dB | 7.4 dB |
| NJICS Phase 2 voice 770.68125 (frames) | 0.4 | 910 / 914 | 6.9 dB | 6.8 dB |

| MIDD neighbor site control 773.64375, 16 dB under a signal 25 kHz above (TSBKs) | 0.4 | 1277 / 1277 | 3.2 dB | 3.2 dB |

Every channel is within 0.1 dB, inside the 0.3 dB limit. The closest strong neighbor on real air is the 25 kHz
case in the last row. No real pair 12.5 kHz apart was in the capture, so that case rests on the synthetic test.

Voice frequencies from the baseline's calls (about 3.5 hours): 12 on `37d4f970`, 1 on `0da2f521` (3 calls).
`0da2f521` carries other voice channels too, just not in this window. Both boards stay in scope. Either way
it's well under 64 bins.

Source 0 (`37d4f970`) has run at 6 MS/s since about 19:02 UTC, because it didn't need 8. The design stays at
8 MS/s: a source using the FPGA channelizer is set to 8 MS/s (issue 07).

## Decode through the model (synthetic)

`tool snr` on synthetic C4FM, at 8 MS/s, 12-bit. The wanted channel is midway between two bins, at −60 dB (about
20 dB SNR). Only P25 Phase 1 C4FM has been tested this way, because `p25::synth` has no CQPSK modulator.

| Test | Channelizer | Bank, 18-bit | Bank, floating point |
|---|---|---|---|
| Clean, −30 dB, no neighbors: voice frames | 342 | 342 | 342 |
| Clean, −30 dB, no neighbors: signalling | 39 | 39 | 39 |
| One transmission, neighbors at ±12.5 kHz, −30 dB: voice frames | 342 | 315 | 315 |
| 16 short calls, the same neighbors: voice frames | 270 | 297 | 324 |
| 16 short calls, the same neighbors: calls heard | 12 | 16 | 16 |

The single transmission's missing frames are all at its start, and that doesn't repeat across 16 separate starts,
so it looks like chance. 16 calls scatter too much to call the bank better, and they show no sign of it being
worse at starting calls.

With noise added, on the single-transmission capture, each as a share of that path's own noiseless run:

| Added SNR | Signalling: channelizer | Signalling: bank | Voice: channelizer | Voice: bank |
|---|---|---|---|---|
| 20 dB | 96.5 % | 98.1 % | 51.6 % | 51.4 % |
| 14 dB | 90.4 % | 92.4 % | 26.9 % | 24.6 % |
| 12 dB | 80.7 % | 86.7 % | 15.7 % | 14.4 % |
| 10 dB | 46.5 % | 66.7 % | 3.3 % | 6.9 % |
| 8 dB | 14.9 % | 26.7 % | 0.6 % | 1.0 % |

The two noiseless runs differ (38 and 35 signalling words, 342 and 315 voice frames), and the bank's denominators
are smaller, so counts favor the channelizer at 14 and 12 dB. This doesn't settle the 0.3 dB limit in issue 01.
That needs more trials and real captures.

## To reproduce

```bash
# Real capture, control channel (kind p25) and Phase 2 voice (kind p2; keys in hex)
trunk-pro tool snr /home/trunk-recorder/captures/37d4f970.cs16 --center 772131250 --rate 8000000 \
  --freq 770593750 --kind p25 --snr 20,10,8,6,5,4,3 --trials 3 [--bank default]
trunk-pro tool snr /home/trunk-recorder/captures/37d4f970.cs16 --center 772131250 --rate 8000000 \
  --freq 770081250 --kind p2 --nac 269 --sysid A --wacn 92731 --snr 20,14,12,10,8,6 --trials 3 [--bank default]
```

```bash
# Capture: FB_SYNTH_DB, FB_SYNTH_NEIGHBOUR_DB (below −150: none) and FB_SYNTH_CALLS set the scenario.
FB_SYNTH_OUT=calls.cs16 FB_SYNTH_CALLS=16 \
  cargo test --release -p trunk-core dsp::filter_bank::tests::synth_capture -- --ignored

trunk-pro tool snr calls.cs16 --center 0 --rate 8000000 --freq 316406.25 --kind p25 --snr 14,12,10,8 --trials 3
# The same through the model: add --bank default, or --bank float
```

Don't build into this tree's `target/`. Set `CARGO_TARGET_DIR` elsewhere while the live service is running.

## Quartus (2026-11-08)

Quartus Prime Lite 23.1std.1 build 993, with Cyclone V device support, is installed at
`/home/trunk-recorder/intelFPGA_lite/23.1std`. The `.qdz` device file is a zip of `quartus/common/devinfo/…`. The
installer skipped it as a second run, so it was unzipped into the install after checking it overwrote nothing.

### Stock `hosted` images, bladeRF `35174c3`

Both build (`build_bladerf.sh -b bladeRF-micro -r hosted -s A4|A5`), about 7 minutes each.

| Board | ALMs | RAM blocks (M10K) | Block memory bits | DSP blocks |
|---|---|---|---|---|
| xA4 (5CEBA4F23C8) | 6,770 / 18,480 (37 %) | **274 / 308 (89 %)** | 2,209,280 / 3,153,920 | 8 / 66 |
| xA5 (5CEBA5F23C8) | 6,757 / 29,080 (23 %) | 274 / 446 (61 %) | 2,209,280 / 4,567,040 | 8 / 150 |

The README's "about 1.1 Mb free" on the xA4 was wrong: RAM blocks, not bits, run out first. Stock RAM blocks go to
the Nios program memory (128), the RX sample FIFO (64), the RX meta FIFO (32), the TX sample FIFO (32), the TX meta
FIFO (7) and small things (11). That leaves 34 on the xA4.

### Packet length limit

The GPIF sends a packet as one USB DMA transaction, the 4 header DWORDs and then the payload (`fx3_gpif.vhd`,
`META_READ`). A USB 3 DMA buffer is 2048 DWORDs (`GPIF_BUF_SIZE_SS`), so the payload can be at most 2044 DWORDs. The
issue 02 limit is now **42 bins** (2 + 48 × 42 = 2018), down from 64. USB 2 would allow 21.

### Filter bank core alone (issue 05)

Trial compile of `fpga/rtl` for the xA4 at 80 MHz (`fpga/syn/core_alone.tcl`, every port but the clock virtual):

| ALMs | RAM blocks | DSP blocks | Fmax (slow, 85 °C) |
|---|---|---|---|
| 2,967 | 36 | 15 | 85.1 MHz |

RAM: the sample ring 12, the coefficients 8 (two copies of the half ROM, one per read port), the frame 7, FFT delay
lines and twiddles about 9.

**The xA4 doesn't fit as is:** 274 + 36 is 310 of 308, before the clock-crossing FIFOs. The `p25chan` revision has
to give back RAM. Its TX FIFOs (39 blocks) are the obvious place, since nothing here transmits.

### `p25chan` test image (issue 04)

The `hosted` top level with TX removed, Wishbone registers and a counting packet generator. Both at 80 MHz Nios clock.

| Board | ALMs | RAM blocks | DSP blocks |
|---|---|---|---|
| xA4 | 6,070 / 18,480 (33 %) | 236 / 308 (77 %) | 8 / 66 |
| xA5 | 6,062 / 29,080 (21 %) | 236 / 446 (53 %) | 8 / 150 |

Removing TX frees 38 RAM blocks on net, so the xA4 has 72 free for the core's 36 and its clock-crossing FIFOs. The
xA5's worst setup slack is −0.177 ns, on the RX sample clock at the 0 °C corner. That clock is constrained at
250 MHz and runs at about 16 MHz at 8 MS/s, and the stock xA5 misses the same path by 0.154 ns.

Results on the radios (2026-11-08), both boards at once, 42 bins, 8 MS/s, 10 minutes:

| | xA4 | xA5 |
|---|---|---|
| Packets | 390,619 | 390,619 |
| Bad packets, wrong words, wrong table, wrong timestamps | 0 | 0 |
| Blocks lost, dropped in the FPGA | 0 | 0 |
| Table commit to first packet using it | 1.9 ms | 3.0 ms |
| Wishbone write | 154 µs | 154 µs |
| RX clock | 16.000 MHz | 16.000 MHz |
| Host CPU, test tool included | 1.8 % of a core | 1.8 % of a core |

Each packet is checked word for word, and its timestamp must be 12,288 samples after the last. On the same image,
wideband recording works, and the xA5's three control channels decode as they do on stock.

### `p25chan` with the filter bank (issue 06)

| Board | ALMs | RAM blocks | DSP blocks | Timing |
|---|---|---|---|---|
| xA4 | 9,690 / 18,480 (52 %) | 260 / 308 (84 %) | 23 / 66 | all met |
| xA5 | 9,669 / 29,080 (33 %) | 260 / 446 (58 %) | 23 / 150 | RX clock −0.152 ns at 250 MHz (stock −0.154), else met |

The core's clock reaches 84 MHz against 80. New dual-clock FIFOs aren't covered by bladeRF's constraints:
`fpga/bladerf/p25chan.sdc` declares the RX and Nios clocks asynchronous.

