# 06: Filter bank on both boards

**Size:** M. **Needs:** 04, 05. **Stop point:** stop or go xA5-only if this fails.

**Status (2026-11-08):** built and simulated, and the first real-air recordings work (below). The output gain and latency are settled (below). `fpga/rtl/fb_radio.vhd` is the core between its two
clocks, and it matches the model bit for bit through both FIFOs (`fpga/sim/run.sh`). `trunk-pro tool bank-capture`
records a packet file for `replay`. Sizes and timing are in the research note. The xA5's only failing path is the
RX clock's own, as in stock: constrained at 250 MHz, 16 MHz in use.

First recordings (2026-11-08), 60 s from both boards, no lost blocks, FPGA drops or saturation, timestamps right.
Packets and wideband were recorded a minute apart on the xA5, so traffic differs. Control channel messages decoded,
FPGA and wideband: NJICS 2,602 and 2,615, PWAY 1,717 and 1,713, MIDD 1,691 and 1,880. Every clear voice grant in the
packet minute was recorded. 
**Output gain shift: 2.** At 0 the peak is 3,069 of 32,767 on the xA5 and 499 on the xA4, so the xA4's rounding noise
would sit close to its floor. A shift of 2 leaves the xA5 about 8 dB of headroom. It's checked bit for bit in simulation.
If live traffic saturates, the status register will show it: lower it then.

**Latency: nothing to add to the host's constant.** Hosts time blocks by their number, so the core's pipeline doesn't
shift samples. Each packet's header timestamp, written at its end, reads 13,190 (xA5) or 13,185 (xA4) past 12,288 × block,
the same on all 39,000 packets. Simulation gives 492 past the block's last sample; the other 410 samples are the
51 µs the core spends clearing its ring after the reset at RX start.

## Goal

Run the real core in the issue 04 image on both boards, and prove its behaviour with real signals.

## Work

1. Connect `adc_streams(0)` to the core as `bladerf-wlan.vhd:784-786` does, and replace the counter generator.
2. Add timing constraints for the new clock crossings, modelled on `bladerf-wlan.sdc`.
3. Build for A4 and A5, and fix timing. Build A4 first, as it's the tighter one.
4. Test on the air and with a signal generator, if one is available:
   - A tone at a known offset comes out of the expected bin at the expected frequency.
   - Compare levels against the wideband path on the same image, then set the default output gain shift.
   - Measure latency between input and output samples, and set the host's constant offset (issue 03).
   - Compare a channel's output against the issue 01 model run on a wideband capture of similar air.
5. Record a packet file from each radio and run it through issue 03's `replay`.

## Done when

- [ ] Both images meet timing with no failing paths.
- [x] `.fit.summary` figures replace the README's estimates, and both boards fit and meet timing.
- [ ] The first live run (issue 07) shows no dropped blocks or core saturation at normal gain, read from the status
      registers. (Replaces a separate 1-hour run: the USB path passed 10 minutes, so saturation is what's left to see.)
- [ ] Replayed packet files from real air decode control channels and voice as well as a same-time wideband
      capture, within the issue 03 limits.
- [x] The latency offset is measured (nothing to add; see the status). Pre-roll alignment is checked live in issue 07.

## If it doesn't fit

Try these in order: drop to fewer bins, then shrink the TX and loopback FIFOs (about 295 Kb), then reduce the
Nios RAM. Otherwise run the xA5 only and keep the xA4 on the wideband path, which this design already allows.
