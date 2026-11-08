# 08: Noise floor, waterfall and meters

**Size:** M. **Needs:** 07.

**Status (2026-11-08):** the power data path is built and simulated bit for bit, and hasn't run on a radio. The plan's
separate power packet can't work: a channel packet streams across all 48 hops of its block, leaving no gap for another.
Instead every packet ends with a slice of the band's power (`bank_wire.rs`), 64 bins a block, 24 at 42 bins, wire format
version 2. An image and a host must be the same version, and a mismatch is refused with a clear error. The host's
waterfall, noise profile and band power come from those slices, in wideband units (`BIN_POWER_SCALE`, `HEAD_SCALE`,
checked by `the_banks_readings_match_the_wideband_path`).

Against a wideband replay of the same capture (12 calls): signal within 0.1 dB on every call. Noise within 1 dB on 4
of 12, typically 7 dB off, and as much as 17 dB lower on some, so SNR isn't yet comparable. Percentiles from 25 % to 56 %
of a slice didn't help; 25 % was the best. A 7.8 kHz bin is about a channel wide, so a busy slice has few plain-noise
bins.

Not done: clip and peak meters, AutoTune's frequency error on 7.8 kHz bins, checking the waterfall on a live radio.

## Goal

Replace the placeholder measurements from issue 03, so SNR figures and the dashboard are
trustworthy again.

Today all of these come from the wideband spectra, which no longer reach the host in channel mode:

| Use | Code |
|---|---|
| Waterfall | `engine.rs:2366`, `session.rs:607` |
| Noise profile | `engine.rs:2375`, `stats/mod.rs:522` |
| Noise and squelch when opening a channel | `engine.rs:976-987` |
| Control channel signal and noise | `engine.rs:2394`, `engine.rs:2456` |
| Peak and clip meters | `stats/meter.rs:64-72` |

## Work

1. **RTL.** Add accumulators of `|X|²` per bin, and send a kind 1 packet every 16 blocks (about 40 per second,
   about 164 KB/s).
2. **Clip and peak.** Read them from the registers about once a second. Don't add them to the sample path.
3. **Host.** Keep the latest power frame in the external backend, and serve `power_spectrum`, `noise_profile`,
   `band_power`, `noise_in_band` and `threshold` from it.
4. **Calibration.** Bins are now 7.8 kHz wide instead of 488 Hz, and `metrics::bin_dbfs` takes an FFT size. Match
   the numbers with a scale factor measured against the wideband path, kept as a named constant.
5. **Control channel signal.** A 7.8 kHz bin may be a poor measure of a 12.5 kHz channel. Measure signal from the
   head's own IQ if that matches better.

## Done when

- [ ] Waterfall looks right at the existing 512-bin display, with center and edges aligned to known carriers.
- [ ] Noise, signal and SNR read within 1 dB of wideband mode on the same air.
- [ ] Clip and peak meters respond to raised gain.
- [ ] `noise` and `snr` in new call JSON are real numbers, comparable to wideband recordings.
- [ ] The placeholder from issue 03 is gone.
- [ ] Measure AutoTune's frequency error with this data: it should settle where wideband mode does.
