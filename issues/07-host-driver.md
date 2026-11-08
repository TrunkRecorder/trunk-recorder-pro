# 07: bladeRF driver and live recording

**Size:** M. **Needs:** 03, 06.

**Status (2026-11-08):** written, and a first 3-minute live run on the xA5 worked (below). A bladeRF source with `fpgaChannelizer`
set loads the image into RAM and streams `Block::Bank`s (`stream_bank` in `radio/bladerf.rs`). The dsp thread tells the
driver which bins the engine wants, through `Control::bins`. A survey or profile opens the radio wideband. A
source's rate and conventional channels are refused as issue 03 set up. Board size is checked by libbladeRF as it loads.
A scratch config for the first test is in `/home/trunk-recorder/captures/p25chan/live/`. The status registers are logged
as warnings when they change, which is the saturation check issue 06 hands over.

First live run, xA5, 3 minutes, a scratch config: all three control channels decoded, 30 calls recorded on 10 voice
frequencies (15 NJICS, 15 MIDD), clean voice frames mean 98.3 %, lowest 85.3 %, and no saturation or dropped-block
warnings at gain shift 2. To look at:
- **Noise reads −371 dB** on every call, so the SNR in a call's JSON means nothing on this path.
- **The two lowest calls (85 % and 88 %)** may be new frequencies opening late, losing the call's start. Not checked.
- **Start-up:** loading the image takes about 5 s, long enough for a control channel timer to give up and try the
  site's next channel. MIDD lost about 10 s to that.

## Goal

Record real calls live, from one radio first, with a config option that defaults to today's behaviour.

## Work

1. **Config.** Add an optional `fpgaChannelizer` image path to the bladeRF source. Unset means no change. Follow
   an existing source field through `crates/trunk-app/src/config.rs`, `web/src/config.ts`,
   `docs/configuration/sources.md` and the API schema.
2. **Validation.** Refuse a rate other than 8 MSPS, and refuse conventional channels on the source, with a clear
   message.
3. **Bind the libbladeRF calls** in `radio/bladerf.rs`: `bladerf_load_fpga`, `bladerf_get_fpga_size`,
   `bladerf_wishbone_master_read` and `bladerf_wishbone_master_write`.
4. **Image load.** Check the board size against the image, load it, and read the core's version register. Any
   failure is a visible source error, with no silent fall back to wideband.
5. **Stream.** Use `PACKET_META` at `bladerf.rs:399`, parse with `bank_wire`, and send `Block::Bank`. Count lost
   blocks from the counter and timestamp as `dropped`, in input samples, as the wideband path does.
6. **Bin control.** Add a pending bin list to `sdr::Control`, next to `freq_hz`. The driver loop applies it
   through `c.apply` and commits a new generation. Keep the loop's USB reads ahead of the Wishbone writes, using
   issue 04's latency measurement.
7. **Retune and reopen.** Reapply gain, center frequency and the current table when a device reopens. Reset the
   rings after a retune, as `Applied::Tuned` does for wideband.
8. **Other modes.** Survey, capture and profile keep requesting wideband on the same image.
9. **Logging.** Log board size, image version and bin count at start.

## Rollout on this machine

1. Use `37d4f970` first, or whichever radio carries fewer important calls, and leave the other radio as it is.
2. Run for a day and check call counts against the baseline.
3. Then add the second radio.

**Rollback:** unset the option and restart. If the wideband stream misbehaves on the loaded image, power-cycle
the board.

## Done when

- [ ] Live calls record from one radio, with the other unchanged.
- [ ] A new voice frequency is enabled by its first grant, and later calls on it have pre-roll.
- [ ] Unplugging the radio and restarting the service both recover without manual steps.
- [ ] Survey and capture work on a channelized radio.
- [ ] With the option unset, behaviour is identical to today.
- [ ] `docs/guides/radios.md` covers the option, its limits and rollback.
