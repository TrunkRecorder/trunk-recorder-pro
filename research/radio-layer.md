# The radio layer: one runtime for every radio (branch rtl-wedge)

Started 2026-10-31 from a field report (four RTL-SDRs, waterfall frozen
solid at the full rate, fixed only by a cold reboot) and a review of the
MimoCAD `sdr-*` crates (sdr-core, sdr-rtl, sdr-hackrf, sdr-uhd,
sdr-driver). Their bench derivation (`derivation/findings.md`) is cited
below as *findings N*.

## What was wrong with the old shape

Every backend repeated the same things, each a little differently:
opening under `radio::opening()` (with no deadline: a wedged device hung
its thread *and* every other radio's reopen behind the lock); a blocking
`tx.send` per block (a slow consumer stalled the driver thread, the USB
queue starved and an RTL lost samples nobody counted); its own `dropped`
bookkeeping; its own allocation per block (UHD, Soapy, bladeRF); priority
only for UHD on macOS. Three sample messages (`Data`, `Iq`, `Real16`)
meant every consumer matched three arms, and the survey silently ignored
`Real16` (Airspy) samples.

## The new shape

`sdr.rs` keeps the runtime; `radio/` keeps the backends.

- **`SourceMsg::Samples { source, block: Block, dropped, at }`** with
  `Block::{U8, Iq, Real16}` (all pooled). One `Inbox` turns `Real16` into
  `Iq` for every consumer (recording, survey, profile, `trunk-pro rx`).
- **`Sink`**: what a backend sends through. Live radios never block on
  it: a full queue drops the block, counts its samples, and the next block
  carries them as `dropped` (exact, and gap-filled downstream). Small
  driver blocks (an RTL's 16 KiB transfers) are gathered into ~128 KiB
  messages, so the USB transfer size and the engine's block size are
  independent. `Sink::tuned(center, gap)` reports a change the radio made
  on purpose (`gap`: samples not produced, e.g. an RTL retune's halt).
- **`radio::open(key, deadline, f)`**: every open and every driver-level
  search runs on a thread of its own under `opening()`, given up at its
  deadline (sdr-core's `open_with_deadline`), the device (not the whole
  backend, as sdr-core does) refused at once while its abandoned open is
  still stuck.
- **`priority`** (from branch `sdr-buffers`, 580c9ed): the source thread
  and the threads its driver starts run real time — Mach time-constraint
  on macOS (QoS isn't inherited by library threads), SCHED_RR on Linux
  (sdr-core's choice; refusal logged once with the `setcap cap_sys_nice`
  fix: a Pi 5 shed ~46 s over 12 h without it). Done by the runtime, not
  each backend.
- **`StreamCheck`** (raw-byte radios: RTL, HackRF): a block that repeats
  (the last block again, or one run of bytes over and over — any period,
  incl. the chip's 3,760-byte FIFO replay, findings 9) or a rate above
  1.5× the nominal for 2 s is a wedged stream: error and reopen.
- **USB memory**: a transfer refused for memory names Linux's
  `usbfs_memory_mb` (sdr-core).

## Sample clock

`ClockFit`'s steering is replaced by sdr-core's `SampleClock` (used as a
dependency, not copied): 1 s windows, a second-order loop, step detection
at 5 ms held 3 windows, a measured rate. A retune's deliberate gap
`resume`s it instead of reading as loss.

## RTL-SDR (crates/rtlsdr-nusb)

- Control layer: STALL retries + wake read (findings 16, 21), proactive
  wake read when no read in 100 ms, counters; refuses USB 0x1C (test mode)
  and tuner R15 with CLK_OUT_ENB (both wedge the chip until power is
  removed).
- Tuner writes only registers that change (the shadow already held them).
- Gain change: gain registers only. Retune: the tuner (and the V4's input)
  only. The page 1 0x01 bit 2 pulse ("soft reset") only at first init: it
  resets the demodulator's signal state (findings 19).
- Stream: stop/start by page 0 0x06 bit 7; a retune halts, keeps what the
  transfers already hold, resets the FIFO, reads the chip's dropped-block
  counter (0x163, 188-byte blocks, findings 16), changes, restarts, and
  reports the quiet time; a bulk STALL is cleared in place; shutdown
  leaves EPA_CTL released and the start bit clear.
- PLL: another divider is tried when one doesn't lock.
- Default transfers 16 KiB × 128 (findings 16.7/16.8).

## HackRF (new native source, sdr-hackrf)

Pure Rust over nusb, no driver to install; firmware ≥ 2021.03 checked at
open. Soapy stays for rad1o and SoapyRemote.

## USRP (keeps run-time-loaded libuhd)

USB link speed (Linux sysfs / macOS IOKit, from sdr-uhd's `link_speed`),
master clock ≥ 2× the rate (spurs at decimation 1), wire format option
(sc16/sc12/sc8), model + serial remembered, `num_recv_frames=1024`
default (also Soapy `driver=uhd`).

## Status (2026-10-31)

All of the above built, in one PR. Tests: workspace 700+ Rust tests, 137
web; regress.sh against main identical but for timings. Hardware: spare
RTL 200 here — 10 s capture 0 dropped, 417 good OSWs in 10 s on WMATA's
CC; a survey (4 retunes, 7 gain steps through the halt path) decoded the
system. Not on hardware: HackRF (none here), the USRP changes (the only
B200 is production's), Linux priority/usbfs (the Pi testbed next).

Found on the way: the RTL queue's `dropped_samples` was a running total
the app read per block (silence re-fed after every retune); now per block.

Follow-ups, done in the same PR (2026-10-31): `Source` is a struct of the
fields every source has plus a flattened `SourceKind` (the JSON unchanged;
production's sources round-trip); one gain model, `Gain::{Agc, Db,
Stages, Preset}` on sdr-core's `gain::Stage`; architecture.md/.html
brought up to date (the clock simulation models SampleClock); CI's macOS
build on macos-15 (macos-14 retires 2026-11-02).

Still open: HackRF on Windows (sdr-hackrf uses device-level control
transfers nusb lacks there — upstream); R828D / Blog V4 support in
sdr-rtl (upstream, if we ever want to migrate).
