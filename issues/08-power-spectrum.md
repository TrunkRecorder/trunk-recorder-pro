# 08: Noise floor, waterfall and meters

**Size:** M. **Needs:** 07.

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
