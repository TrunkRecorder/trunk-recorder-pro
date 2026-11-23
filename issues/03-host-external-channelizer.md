# 03: Engine runs from channel blocks

**Size:** M. **Needs:** 02. **Can run alongside:** 05.

**Status (2026-11-08):** done.
- **Backend:** `Channelizer::external` in `dsp/channelizer.rs`. A source opts in with `SourceConfig::filter_bank`,
  which only issue 07's config will set.
- **Engine:** `Engine::push_bank`, `push_bank_gap` and `bank_bins` (wanted bins plus those evicted since the last
  call, for the driver to log). The app side is `Feeder::push_bank` and `Session::push_bank`, and the radio side
  is `Block::Bank`.
- **Tools:** `trunk-pro tool bank-pack` makes a packet file from a capture, and `trunk-pro replay file.pkt` plays
  it back.
- **Config checks:** a filter bank source at a rate other than 8 MS/s, or with a conventional channel on it, is
  refused when the engine starts.

## Goal

Run the whole recording engine from channel packets, with no FPGA, by replaying packet files made by the model.

## Approach

Keep the `Channelizer` type and add an internal backend, instead of introducing a trait across the engine. It's
used directly at `engine.rs:573` and `conventional.rs:830`, by the survey, and by the tools. Only the engine
ever constructs the new backend.

| `Channelizer` method | External backend |
|---|---|
| `add_head` | Pick the nearest bin, set the residual NCO and FIR, replay that bin's ring as pre-roll, record the bin as wanted |
| `remove_head` | Remove the head and keep the bin enabled (eviction is separate) |
| `output` | Output of the head's NCO and FIR for this block |
| `output_rate`, `fs`, `decim`, `sample_position`, `block_seconds` | Same values as today |
| `power_spectrum`, `noise_profile`, `band_power`, `noise_in_band`, `threshold` | Issue 08. Use a placeholder floor until then, and don't save its numbers as real |
| `cell_powers`, `band_bins` | Not supported. Only conventional channels and the survey call them, and neither runs on this backend |

## Work

1. Add `Channelizer::external` and a block feed, with a 1 s ring per enabled bin.
2. Keep sample indexing absolute, using a constant latency offset. Issue 06 measures the real value.
3. Add `Block::Bank` in `sdr.rs:35`, with `len()` reporting 12288 per block so rate measurement still reads
   8 MSPS. Handle it at every `match` on `Block`: `runtime.rs:817`, `survey.rs:71`, `profile.rs:45` and
   `main.rs:786`.
4. Add `Engine::push_bank`, `Session::push_bank` and `Feeder::push_bank`, next to the existing `push_iq`.
5. Handle gaps. Missing blocks feed quiet at head rate and set `in_gap`, as `push_gap` does today.
6. Handle a head on a bin that isn't active yet by feeding quiet samples, so receivers always see 48 per block.
7. Add `Engine::wanted_bins(source)`:
   - Enable every control channel in the source's range at start.
   - Add a bin on each `add_head`.
   - At the limit (42 bins, issue 02), evict the least recently used bin that isn't a control channel, and log it.
   - Two frequencies can share a bin. Enable it once and keep separate heads.
8. Handle AutoTune. Offset changes go through `remove_head` and `add_head` (`engine.rs:1444`), and a head near a
   bin edge must not flap between bins.
9. Teach `trunk-pro replay` to read a packet file.
10. Refuse conventional channels on this kind of source when the config is checked.

## Done when

- [x] Replaying a model packet file gives the same calls as replaying the original capture: same talkgroups and
      durations within one block, `clean_voice_pct` within 1 point.
- [x] Pre-roll covers the same air as today for a bin that was already enabled.
- [x] Dropping blocks from the file keeps call times correct.
- [x] A test covers bin sharing, eviction and a head on a not-yet-active bin.
- [x] Existing sources' output is unchanged, and `cargo test` passes for native and wasm.

## Results

From the 60 s xA5 capture, with 15 bins (3 control channels and 12 voice frequencies), using
`--system NJICS:774793750:nac=398,sysid=39a,wacn=bee00 --system MIDD:770593750:nac=269,sysid=a,wacn=92731
--system PWAY:771256250:nac=a,sysid=a,wacn=92a53`:

- **Wideband replay against packet replay:** both give the same 12 Phase 2 calls, with the same talkgroups,
  frequencies and start times. 10 match exactly in length and `clean_voice_pct`.
- **The two that differ** were already on the air at the first sample. In one, the packet path kept 80 ms more
  audio, and those 80 ms are exactly its lower clean figure (94.3 against 100). The other is cleaner through the
  packet path (100 against 98.8).
- **One block in 50 dropped** (`--drop-every 50`): 10 calls keep their start and stop times to the millisecond,
  and the two from the first second start about 1 s later, since control-channel messages were lost. Clean voice
  falls 6 to 9 points, as it should with 2% of the air missing.

`protocol_tests::the_servers_messages` fails on this machine whatever the change: it lists the real bladeRFs,
which this user can't open, so their serials read `ANY`.

