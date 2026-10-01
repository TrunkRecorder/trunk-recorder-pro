# Weak signals

September 2026, branch `dmr`. How much signal each receiver needs, and what
lowered it. The approaches are meant to be general: one receiver change
should help every system type that uses it.

## How it is measured

`trunk-pro tool snr` works the same way for every system type:

1. It cuts one channel out of a strong capture.
2. It adds noise at each SNR. SNR is signal power over the noise in 12.5 kHz.
   The noise is shaped by the channel filter, as real noise would be.
3. It counts how many of the noiseless run's messages are decoded **again,
   with the same content** (within 0.2 s): DMR CSBKs, LCs and AMBE
   codewords; P25 TSBKs and IMBE codewords; SmartNet OSWs. So a codeword
   decoded wrong doesn't count, however sure its FEC was.
4. Signal power is measured only around the messages, because a channel can
   carry other things the rest of the time.
5. It does this per receiver variant (`--variant base,nomsd,…`).

```
trunk-pro tool snr cap.cu8 --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet \
  [--seconds N] [--snr 10,8,6,4] [--trials 3] [--variant base,nomsd,…] [--cutoff Hz]
```

Reference channels (captures in the session scratchpad; record new ones with
`rtl_sdr -d <91> -f <center> -s 2400000 -g 28`):

| Kind | Capture | Channel |
|---|---|---|
| DMR voice | center 464.2 MHz, first 6 s | 463.550 (CC 1, TG 25) |
| DMR control | center 464.2 MHz, first 45 s | 464.350 (Cap+ rest channel, CC 6) |
| P25 C4FM voice | WMATA, center 496.0 MHz, 70 s | 496.4625 |
| P25 simulcast control | DCFD, center 858.513 MHz | 857.9875 |
| SmartNet control | WMATA, center 496.6375 MHz, 1.024 MS/s | 496.4375 |

## Results

SNR at which half the codewords or messages are decoded:

| Receiver | Before | After | Gain |
|---|---|---|---|
| DMR voice (AMBE) | 12.8 dB | ≈5 dB | ≈8 dB |
| DMR data blocks (voice channel) | 9.5 dB | ≈3 dB | ≈6.5 dB |
| DMR control (Cap+ site status) | 8.5 dB | ≈3.3 dB | ≈5 dB |
| P25 C4FM voice (IMBE) | 10.4 dB | ≈4.4 dB | ≈6 dB |
| P25 simulcast control (CQPSK) | 6.5 dB | 6.5 dB | none (CQPSK unchanged) |
| SmartNet control | 3.5 dB | ≈1.8 dB | ≈1.7 dB |

On a really weak site (Linked Cap+ on 463.375, no noise added):

- Clean AMBE codewords went from 32 % to 92 %.
- Control blocks went from 1088 to 2342 in 90 s.
- The recorded calls are 7.6 s and 4.5 s of audio, where they were 1.4 s
  and 2.0 s.

### What helped, in order of size

1. **Matched filter after the discriminator** (C4FM receiver, P25 and
   DMR). An RRC replaces the 0.9-symbol boxcar: α 0.2 for DMR, matching
   its transmit filter, and α 0.5 for P25, which tested best. About 4 dB
   on DMR, 1 dB on P25.
2. **Levels from cluster means** (P25 and DMR). Before, the levels came
   from the 2 %/98 % quantiles of recent symbols. Noise widens those, so
   the outer threshold sat too high exactly when the signal was weak.
   2 dB on DMR, 3 dB on P25.
3. **Multi-symbol detection, `dsp::msd`** (P25 and DMR). Each symbol is
   re-decided from the IQ by correlating three symbols against every level
   sequence, using the transmitter's own frequency pulse; the neighbours'
   tails come from decisions. This gets past the discriminator's threshold
   (its clicks). 2.8 dB on P25 voice, 1.5–2.2 dB on DMR.
   - Rectangular pulses gained on blocks but left an error floor on voice.
     The shaped pulse fixed that.
   - Decision feedback (16 hypotheses instead of 64) is better on DMR and
     0.8 dB worse on P25, so it is on for DMR only.
   - Cost is about 0.5–1 % of a core per channel while it runs. It switches
     off on a clean signal (level separation over 11), and the scanner
     doesn't use it.
4. **Tone detector for 2FSK** (SmartNet). The decision is the two tones'
   energies over a symbol (noncoherent matched filters) instead of the
   discriminator: 1.5–2 dB, and lost OSWs halved on the clean capture.
   It also does as well with the 5 kHz channel filter as the discriminator
   did with 4 kHz, without needing the carrier centred.
5. **Soft combining of repeated blocks** (DMR): about 1 dB on control blocks.
6. **Soft BPTC** (Chase rows and columns, DMR): about 0.4 dB.
7. **Looser sync on the grid** (a DMR repeater's syncs, up to 11 bits wrong
   where the grid expects one): about 0.3 dB.

### Measured and not kept

- **Clipping the discriminator** (click suppression): no gain, and worse
  combined with the RRC.
- **Longer or shorter timing blocks** (120–960 symbols): ±0.3 dB.
- **Soft Golay(24) for AMBE's c0**: no gain. Low-SNR errors in c0 are too
  many for flipping 5 bits, and a wrong c0 also unscrambles c1 wrong.
- **SmartNet clock gain, level speed, matched-filter width**: none of them
  moves it more than a few tenths of a dB.
- **Narrower DMR channel filter**: under 0.5 dB once the RRC is in.
- **A CQPSK branch for C4FM in noise**: the P25 bank's CQPSK receivers never
  beat C4FM on a C4FM signal in noise (bank = C4FM alone, at every SNR).
  Diversity pays on simulcast, not on noise.

## Next

- **CQPSK** (P25 simulcast and Phase 2). It is still differential
  detection, the one receiver not touched here. Multi-symbol differential
  detection is the same idea as `msd` for π/4-DQPSK and typically gains
  1–2 dB. A non-simulcast CQPSK capture is needed to measure it.
- **MSD on simulcast in `auto` mode.** There the bank's C4FM branch runs MSD
  while CQPSK does the work (about +1 % of a core per channel). Switching it
  off when CQPSK is carrying the channel would need a per-receiver quality
  measure; frame counts don't say which decodes better.
- **AFC.** Recentring the channel filter on the measured carrier would allow
  narrower filters, worth about 1 dB on SmartNet's discriminator. Less now
  that the tone detector is in.
- **Live A/B against Trunk Recorder** on the weak 463.375 site, and against
  DSD-FME on the same IQ.
