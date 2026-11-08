# 00: Baseline and prerequisites

**Size:** S. **Needs:** nothing. **Stop point:** decide afterwards whether to continue.

## Goal

Measure what we'd be improving and confirm the tools work.

## Work

1. **Done: `0da2f521` moved to USB3.** Both bladeRFs read 5000 Mb/s on 2026-11-07. Before the move it had
   3,096,139 dropped samples against 64,991 on `37d4f970`. All sources read 0 drops about 2 minutes after the
   service restarted, which is too short to count, so check again in the baseline below.
2. **Done: boards identified** on 2026-11-07 from `bladeRF-cli info`. `37d4f970` (source 0, 772.13 MHz) is
   the xA5: 77 KLE, 64 Mbit flash. `0da2f521` (source 1, 854.37 MHz) is the xA4: 49 KLE, 32 Mbit flash.
3. **Record the baseline** over at least 24 hours, including a busy period:
   - CPU by thread (`dsp-N`, `source-N`), with peak calls recording
   - Dropped samples per source
   - Calls per system, `clean_voice_pct`, control channel good and bad counts
   - Process memory
4. **Count the voice frequencies per source** from the call JSON `freq` fields. This checks the 64-bin limit.
5. **Capture reference IQ** from both radios at 8 MSPS, covering Phase 2 voice and control channels. Use
   `bladeRF-cli`, because `trunk-pro capture` only drives RTL-SDRs. Allow 1.9 GB per minute. This stops
   recording while it runs.
6. **Done: toolchain.** Quartus 23.1std.1 and Cyclone V support are installed, and both stock images build. Numbers
   are in the research note. On 2026-11-08 both images loaded over USB in about 2 s and recorded 10 s at 8 MS/s from
   both boards at once, with no errors (issue 04's counter test is what will prove no samples drop). On the xA5, NJICS and PWAY decoded as in the older capture (381 and
   256 blocks against 384 and 258), and MIDD's control channel (NAC 0x269) had moved to 769.54375 MHz, where it
   decoded 259 messages. The xA4 has no control channel, and 858.1125 MHz gave 238 P25 voice frames.
   **Prove the toolchain.** Install Quartus Prime Lite 23.1.1, then build the stock `hosted` revision of
   bladeRF `35174c3` for A4 and A5. Load each with `bladerf_load_fpga`, and confirm a radio still streams.
   Keep each `.fit.summary`: they replace the free-resource estimates in the README.

## Done when

- [x] Both bladeRFs link at 5 Gb/s.
- [x] Each serial is matched to its board size.
- [ ] Baseline numbers and frequency counts are saved in `research/fpga-channelizer.md`.
- [x] Reference captures exist for both radios.
- [x] Stock images build, load and stream, and free ALMs, M10K and DSP are recorded for each board.

## Decide

Use the 24 hour drop counts from the baseline. If drops stay low and CPU is fine, decide whether the rest of
this plan is still worth doing.
