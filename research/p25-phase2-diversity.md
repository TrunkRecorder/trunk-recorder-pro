# Phase 2 receiver diversity

Phase 1 voice and every P25 control channel go through a receiver bank (`p25::diversity::Bank`):
several receivers on the same channel, the best copy of each frame or codeword kept. On simulcast it
took wrong IMBE codewords from 2.2 % to 0.00 % (native-bench/RESULTS.md). Phase 2 voice had one
CQPSK receiver, and [weak-signal.md](weak-signal.md) found that on simulcast "it is the channel, not
the noise, that limits Phase 2", with per-burst diversity first on its list of what to try.

## What it does

`trunk::voice::P25Tdma` runs a second receiver when the bank's `cqpsk_eq` is on (the default): the
same CQPSK receiver with the T/2 CMA equaliser (9 taps, μ 0.02) and **without** decision-feedback
differential detection. Each receiver has its own slot framer.

- **Matching copies.** Each framed packet joins the group of packets from the other receiver that
  started within half a slot of it (the receivers' timing loops differ by a few symbols). A group
  goes to the tracker, in time order, once both receivers have framed it or 0.25 s after it began.
  A copy that turns up after its group went on is dropped, so nothing is decoded twice. The
  harness prototype weak-signal.md mentions failed at this step; the slot framers' timing makes it
  straightforward.
- **Choosing.** `TdmaTracker::packets` takes the copies: those whose superframe slot disagrees with
  the first receiver's are left out, the burst type is the majority's, each AMBE codeword is the
  copy's that decodes with the fewest errors (then the highest mean reliability), and a MAC PDU is the
  first copy's that passes its CRC. One copy goes through as before (`packet` is the one-copy case).

## Measured

21 voice-channel captures of MetroC (Metrocrest), a P25 Phase 2 CQPSK simulcast system on Harris
infrastructure (12 control channels, 852–860 MHz), from an Airspy R2 at 10 MS/s via the RF page's **Next call** capture, 154 s of air on
six voice frequencies. Each was replayed with `tool channel` as captured (diversity) and with the
bank's `cqpsk_eq` set off (one receiver, as before). The live control channel reads 17–19° RMS
phase error on both an Airspy and a HackRF at the same site, so the distortion is the air's.

| Receivers after the first | AMBE frames | Bad | Bit errors corrected |
|---|---:|---:|---:|
| none (before) | 2989 | 530 (17.7 %) | 4692 |
| CMA equaliser, with decision feedback | 3025 | 425 (14.0 %) | 3888 |
| **CMA equaliser, no decision feedback** (kept) | 3025 | **373 (12.3 %)** | 3495 |
| no decision feedback, no equaliser | 3025 | 411 (13.6 %) | 3873 |
| coherent detection | 3007 | 446 (14.8 %) | 4065 |
| CMA, 15 taps | 3023 | 421 (13.9 %) | 3833 |
| CMA, μ 0.005 / μ 0.05 | 3025 / 3017 | 430 / 416 | 3963 / 3746 |
| CMA with and without decision feedback (3 receivers) | 3015 | 355 (11.8 %) | 3248 |
| … and no decision feedback without CMA (4 receivers) | 3049 | 356 (11.7 %) | 3218 |

Good frames rose from 2459 to 2652 (+7.8 %); bad frames fell by 30 %. Extra frames (2989 → 3025)
are bursts the first receiver didn't frame at all. The third and fourth receivers gain about half a
point more for 50–100 % more decoding, so the second alone is kept.

Decision feedback is the difference between the second receiver's two forms: it helped Phase 2 in
added noise (about 1 dB, weak-signal.md) and is kept on the first receiver, but the equalised copy
does better without it on simulcast, as Phase 1's control channels did.

**Held out.** The variants above were chosen on those 21 calls. A further 18 MetroC calls,
captured afterwards and not looked at before the choice, through the kept pair:

| | AMBE frames | Bad | Good | Bit errors corrected |
|---|---:|---:|---:|---:|
| One receiver | 2963 | 459 (15.5 %) | 2504 | 4325 |
| Diversity | 2967 | **343 (11.6 %)** | **2624** | 3344 |

Every one of the 18 calls has more good frames; none has fewer.

**Other systems, other infrastructure.** Two busier Phase 2 TDMA simulcast systems on Motorola
infrastructure (DallasLayer1 and DallasLayer2, 800 MHz), 20 calls each, captured the same way, one on
the Airspy R2 and one on a HackRF One at 16 MS/s, through the kept pair as chosen on MetroC:

| System | Radio | Calls (air) | Bad AMBE frames, one receiver → diversity | Good frames | Calls better / worse / same |
|---|---|---:|---|---:|---|
| MetroC (Harris), held out | Airspy R2 | 18 | 459 (15.5 %) → **343 (11.6 %)**, −25 % | +4.8 % | 18 / 0 / 0 |
| DallasLayer1 (Motorola) | Airspy R2 | 20 (162 s) | 170 (4.5 %) → **134 (3.6 %)**, −21 % | +1.0 % | 8 / 0 / 11 |
| DallasLayer2 (Motorola) | HackRF One | 20 (201 s) | 341 (8.7 %) → **263 (6.7 %)**, −23 % | +2.2 % | 12 / 0 / 6 |

The cleaner Motorola layers start from fewer errors, but lose about the same share of them; no call
on any of the three is worse. (Calls with no voice, encrypted, are left out of the counts per call.)

**Live.** With MetroC recorded by two recorders at once, one on the Airspy (this build) and one on a
HackRF (the build before, as a control for traffic), the Airspy's voice frames lost against the
HackRF's went from 18.2 % vs 24.8 % (30 min, both before) to 9.4 % vs 17.2 % (20 min, after): 27 %
less than the control before, 45 % less after, about the quarter the captures predict.

**Other Phase 2 recordings.** The corpus's `phase2` (17 calls, 700 MHz) and `voice-p25-phase2`
decode the same good frames with fewer bit errors corrected (103 → 93, 21 → 20, 21 → 19): `changed`,
not `regressed`. Their expected output for `next` is updated.

## Cost

Decoding the 154 s of captures took 0.53 s of CPU with one receiver and 0.83 s with two (Apple silicon,
release build, process start included): about 0.2 % of a core more per Phase 2 voice channel while
it is open. The perf gate has no Phase 2 stage.

## Next

- Choose the ESS (encryption sync) per copy too: it is taken from the first copy's burst now.
- Phase 2 calls' voice-lost figure on the dashboard counts the chosen codewords, so it falls with
  this; nothing else reports per-receiver decoding yet.
- A data-aided equaliser trained on each slot's known ISCH, and MLSE (weak-signal.md's other two).
