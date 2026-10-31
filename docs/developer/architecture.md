# Trunk Recorder Pro architecture (text version)

This is the plain-text companion to [architecture.html](architecture.html). It has the same
content as the interactive page, with every diagram written out as lists and tables, so tools
and language models can read it directly. Paths are relative to `crates/` unless they start
with `web/` or `docs/`. Line numbers are left out because they drift; names of types and
functions are given instead.

Contents

1. The big picture: crates and threads
2. How samples flow from the SDR to a saved call
3. The channelizer
4. DSP chains, one per kind of channel
5. How trunking messages are processed
6. How recorders are managed and assigned
7. Buffering and back-pressure, compared with GNU Radio
8. Plugins
9. Data objects
10. The configuration file
11. Time, clocks and identifiers
12. The dashboard's measurements
13. The browser build
14. The interface protocol
15. Where things live

---

## 1. The big picture

### 1.1 Crates

| Layer | Crate | Role | Does I/O? |
|---|---|---|---|
| Core | `trunk-core` | DSP, decoders, vocoders, trunking engine (`trunk::Engine`). No threads, no clock, no files. Builds for native and wasm32. | No |
| App | `trunk-app` | `Config` (and `migrate.rs` for older ones), `Session` (engine events → interface messages, call files, plugin events), the interface protocol (`protocol.rs`: `ToClient`, `Command`), stats, file names, log records, conventional channel CSV, the survey. | No |
| Platform | `trunk-pro` | The desktop program: source and decoding threads, SDR drivers, the engine thread, the finish pool, the web server (axum), the plugin host, the stores, the CLI and analysis tools. | Yes |
| Platform | `trunk-web` | The browser build: `WebSession` + RTL-SDR over WebUSB, exported with wasm-bindgen into a Web Worker; and the config rules (`rules.rs`) the interface runs in both builds. | Yes (WebUSB, OPFS) |
| Protocol | `trunk-recorder-plugin` | The plugin wire protocol (`HostMessage`, `PluginMessage`, `Manifest`, `CallRecord`: the call JSON) and a Rust SDK for plugin authors. Published on crates.io. | SDK only |
| UI | `web/` | The React interface. Embedded in the `trunk-pro` binary and also runs the browser build. Loads trunk-web's WebAssembly for the config rules (`web/src/rules.ts`). | — |

The rule that holds this together: **`trunk-core` and `trunk-app` never do I/O.** They take
samples plus a wall-clock time and return values (events, JSON strings, WAV bytes). The
platform crates own devices, threads, files and sockets. That is why the same recording logic
runs in a desktop process and in a Web Worker and gives bit-identical calls on the same capture.

### 1.2 Threads in the desktop process

| Thread | Count | What it does |
|---|---|---|
| `source-N` | one per source | Runs the radio (`sdr::run_with` over a `Radio`, or `runtime::run_file`) and sends `SourceMsg` blocks into its own queue. Airspy samples arrive on libairspy's callback thread instead. |
| `dsp-N` | one per source | The source's decoding thread (`runtime::feed_source`, a `Feeder`): pushes each block into the engine, which decodes it under the source's own lock and then follows it under the engine's. **The DSP runs here**, each source on a core of its own. |
| `engine` | one | Owns the `Session`. Waits on its one `EngineCmd` channel (20 ms timeout), then `poll` → `deliver`; saves band plans, aliases, AutoTune scores and the radio registry from time to time. |
| `finish-N` | 1–3 | The finish pool: writes every file of a call's plan (.json, .wav, .m4a, frames), sends `concluded`, hands calls to the plugin host and the Archive. |
| `plugin-<id>` + writer + stderr reader | per plugin process | Supervises one plugin child process, feeds its stdin, reads its stdout/stderr. `plugin-hold` keeps what plugin delays hold back. |
| tokio runtime | pool | The axum web server; one task per WebSocket client, one listener per interface. |
| `monitor`, `net-probe`, `spotlight` | one each | Samples the computer and plugins every 2 s; TCP connectivity checks every 15 s; the Spotlight check of the recordings folder. |
| `stats-load`, `stats-store`, `history` | one each | Load/append stats rollups; fill the call list from disk at start. |
| `survey`, `survey-source-N` | while surveying | The first-run survey and its radio (never with recording). |

The browser build has no threads: `engine.worker.ts` pulls blocks from `WebRtl::next()` and
calls `WebSession.push` / `poll` directly, and keeps call files in OPFS.

### 1.3 The recorder's lifecycle

`Ctx.lifecycle` (`runtime.rs`) is one `Mutex<Lifecycle>`: `Idle`, `Recording(Runner)`,
`Surveying(SurveyRunner)` or `Quitting`. Only `Ctx`'s methods change it, each holding the lock
throughout (`begin_recording`, `begin_survey`, `with_radios` for profiling a source,
`stop_recording`, `stop_survey`, `quit`), so two starts never interleave and nothing starts once
quitting. The phase the browsers see (`state`: idle, starting, running, stopping) is the engine
thread's report.

While recording, everything for the engine thread goes through one channel, `Ctx.engine`
(`Ctx::tell_engine`): `EngineCmd::Source` (a source's error or end, by way of its decoding
thread), `Topics` (what the browsers watch), `Talkgroups` (a talkgroup file changed) and `Keys` (a
key container unlocked or locked). The plugin host is cloned once per loop
(`Plugins::host()`), so a plugin reload never waits on the engine.

---

## 2. How samples flow from the SDR to a saved call

Stages in order. Thread in brackets.

1. **SDR driver** [`source-N`]. Each driver is a `Radio` (`sdr.rs`: `BACKOFF`, `retune_to`,
   `stream_once`), run by one loop, `sdr::run_with`, that reopens it after an error (RTL 2 s, the
   others 3 s). `runtime::spawn_source` starts a source this way for recording, the survey and
   profiling. Produces a block of IQ:
   - RTL-SDR (`radio/rtl.rs`, pure-Rust `rtlsdr-nusb`, our fork in `crates/rtlsdr-nusb` with
     settable transfers, `RxBuffers`): by default 16 KiB USB transfers (8,192 IQ samples, about
     3.4 ms at 2.4 MSPS), 128 in flight (2 MiB); `bufLen` / `buffers` per source
     (`RtlConfig::rx_buffers`). The `Sink` gathers them into messages of about 128 KiB. A retune
     or gain change halts the stream, reads the chip's dropped-block count, changes only the
     tuner and restarts, reporting the quiet time (`Sink::tuned`); a USB stall is cleared in place.
     Sent as raw `u8` (`Block::U8`, in a reused `Pooled` buffer).
   - HackRF One (`radio/hackrf.rs`, pure Rust over `nusb`): raw bytes like an RTL-SDR, firmware
     2021.03 or newer.
   - USRP (`radio/uhd.rs`, UHD loaded at run time): about 10 ms per block, float IQ
     (`Block::Iq`). Adds `num_recv_frames=1024` to the device args; picks the wire format from the
     USB link speed (`radio/usb_speed.rs`); doubles a B2xx's master clock when UHD would run it at the rate.
   - Airspy (`radio/airspy.rs`): whatever libairspy hands its callback; by default real samples
     (`Block::Real16`), turned into IQ by the `Inbox` on the thread that takes them.

   What every radio shares lives in `sdr.rs` and `radio/mod.rs`. A radio sends through a `Sink`,
   which never blocks a live radio: if the queue to the decoding thread is full, the block is
   dropped and counted, and the next block carries the count as `dropped`. `radio::open(key,
   deadline, f)` runs every open and driver search on a thread of its own under `opening()`
   and gives up at the deadline (5 s RTL-SDR, HackRF, Airspy; 15 s bladeRF, SoapySDR; 30 s USRP),
   with "isn't answering" and the device refused until the stuck open ends, so one wedged radio
   doesn't hold up the others. `radio/priority.rs` makes the source thread and the threads its
   driver starts real time (Mach time-constraint on macOS, `SCHED_RR` on Linux, needing
   `CAP_SYS_NICE`; a refusal is logged once with the `setcap` fix). `StreamCheck` watches raw-byte
   radios (RTL-SDR, HackRF) for a stuck stream, the same bytes repeating or a rate over 1.5× the
   nominal for 2 s, and reopens. A transfer refused for memory names Linux's `usbfs_memory_mb`.
   - SoapySDR (`radio/soapy.rs`): about 10 ms per block, float IQ.
   - File (`runtime::run_file`): 32,768 samples per chunk, paced to real time or as fast as the
     engine takes them. Captures played as fast as they go keep within 100 ms of air of each other
     (`runtime::Air`).
   Each block is stamped with `Instant::now()` (`at`) and carries the driver's `dropped` count.
2. **Source queue** [between threads]. One `sync_channel::<SourceMsg>(256)` per source, from its
   `source-N` to its `dsp-N`. A live radio's `Sink` never blocks: when the queue is full the block is dropped and counted (see section 7). Errors and ends
   go on to the engine thread as `EngineCmd::Source`.
3. **`Feeder::push` / `push_iq`** [`dsp-N`]. Records drops (`Feeder::gap` feeds them to
   `Engine::push_gap` as faint noise), feeds the wall-clock fit (`ClockFit`) and the sample meter,
   then calls `Engine::push_u8` / `push_iq`. `Feeder::idle` fills in a source quiet for over 1 s.
4. **Channelizer** [`dsp-N`, under the source's lock]. One per source (in its `SourceRx`).
   Collects 12,288 new samples (one block), runs one 16,384-point FFT and stores the spectrum for
   pre-roll.
5. **`SourceRx::decode_block`** [`dsp-N`, under the source's lock]. Every open voice channel on
   this source: head IQ → `VoiceDecoder::push` → a `VoiceResult` (outputs routed to their calls'
   slots, reception, frequency error). The control channel carriers' head IQ is copied out
   (`BlockDsp`).
6. **`BlockDsp::decode_control`** [`dsp-N`, under each control channel's own lock].
   `ControlChannel::push` → `Step`s, plus what the site follows each block (notes, counts,
   `control_hz`, `offset_hz`, voice params) in a `CcResult`, so the site never locks its control
   channel to follow it.
7. **`Core::after_block`** [`dsp-N`, under the engine's lock], in this order:
   1. The voice results are taken (`Radio::take`): reception and frequency error onto each
      recording, outputs into `radio.pending`.
   2. Every site with carriers on this source: `Trunk::on_block` → `Trunk::follow` (site lock) →
      `CallManager::handle` → may open voice channels; then hunting or AutoTune, and
      `CallManager::tick` (timeouts, splits).
   3. `apply_pending()`: decoded audio appended to recordings, `Event::Audio` for live audio,
      call info, ciphers, positions and talker aliases applied.
   4. Conventional channels (`Conventional::on_block`, under the source's lock again): energy
      detection on the shared spectrum, opening heads as needed; what they report is the engine's
      own `Event`s.
   5. `apply_pending()` again, then `emit_call_events()`: `CallStart`, `CallUpdate`, `CallEnd`.
      An ended call goes through `conclude()` → `MultiSite` → `record::save_call` →
      `Event::Concluded` (or `NotSaved` / `Duplicate`).
   Locks are never nested except the engine's with one other; each source decodes on its own
   core while the calls are followed in one place.
8. **`Session::poll`** [`engine`]. Runs about every 20 ms (sooner when a command comes). Steers
   the sources' clocks, drains engine events and turns them into `Output`s: `Message` (a
   `ToClient`: status every 500 ms, stats every 1 s), `Topic` (spectrum, logs, RF and decode
   detail, only while watched), `Audio` (live frames, only while someone could listen), `File`
   (a call to store, its `CallRecord`), `Plugin` (plugin events, only for subscribed topics),
   `Log`, `Rollup`.
9. **`runtime::deliver`** [`engine`]. Routes each output:
   - `Message` → `publish` (the one way onto the hub: a tokio broadcast channel of 4096, sorted by
     who may hear it) → each WebSocket task; `Topic`, `Audio` → the hub too.
   - `Audio`, `Plugin` → `PluginHost` (per-plugin queues).
   - `File` → the finish queue (1024), waiting up to 5 s when it's full.
   - `Log` → logging; `Rollup` → stats store.
10. **Finish** [`finish-N`]. `plugins::plan` decides once which of the call's files are written
   and kept (its system's file rules and what the plugins take). The finisher writes them (to the
   RAM spool what only plugins need), publishes `concluded` to browsers (so the UI never lists a
   call that isn't on disk), adds it to history, encodes the .m4a (to keep, or for a plugin; the
   WAV instead should that fail), registers it with the Archive and hands the plugin host the
   finished files (`call.concluded`).
11. **Plugins** [plugin processes]. Upload or stream the call, answer `call.result`. The Archive
   deletes files that are not to be kept once every taker has answered.

Latency, roughly: driver block time (RTL about 55 ms, others about 10 ms) + up to one
channelizer block (5.12 ms at 2.4 MSPS) + filter delay (under 1 ms) + any backlog in the
source queue + decoder framing (C4FM timing adds about 100 ms).

---

## 3. The channelizer (`trunk-core/src/dsp/channelizer.rs`)

One `Channelizer` per source. It is an overlap-save fast-convolution filter bank modelled on
CyberEther's multi-head `filter_engine`. One forward FFT per block serves every channel.

| Parameter | Value |
|---|---|
| FFT size N | 16,384 |
| Filter taps P | 4,097 (Blackman windowed-sinc, unit DC gain) |
| New samples per block L = N − P + 1 | 12,288 (5.12 ms at 2.4 MSPS) |
| Decimation D | largest power of two with D ≤ N/64 (= 256) and fs/D ≥ 24 kHz |
| Bins per head M = N/D | 256 at 2.4 MSPS |
| Head output rate fs/D | 37.5 kHz at 2.4 MSPS |
| Bin spacing fs/N | 146.5 Hz at 2.4 MSPS |

Output rate by sample rate: 2.4 MSPS → D 64, 37.5 kHz; 3.2 MSPS → D 128, 25 kHz; 6 MSPS →
D 128, 46.875 kHz; 8 MSPS → D 256, 31.25 kHz; 10 MSPS → D 256, 39.0625 kHz.

**A head** is one channel being listened to. `add_head(offset, cutoff, preroll_s)`:
- picks the nearest bin to the offset and a residual NCO for the remainder;
- takes the filter for that cutoff (cached; a direct DFT of the 4,097-tap filter at only the M
  bins around DC);
- per block: multiplies M bins of the shared spectrum by the filter, runs an M-point inverse FFT
  (this is the decimation), keeps the valid outputs, applies the overlap-save phase correction
  (exact integer arithmetic mod N) and the residual NCO.

So each extra channel costs an M-point inverse FFT, not another full-rate filter.

**Head cutoffs (one-sided):**

| Channel | Cutoff |
|---|---|
| P25 control and voice (Phase 1 and 2), conventional P25 | 7,000 Hz |
| DMR, trunked and conventional | 6,250 Hz |
| NXDN48 | 3,400 Hz |
| NXDN96 | 6,250 Hz |
| SmartNet control channel | 5,000 Hz |
| SmartNet analog voice | 7,000 Hz |
| Conventional FM / AM | 7,000 Hz, widened or narrowed with the channel's `bandwidthKhz` |

A trunked voice channel opens at `VoiceKind::cutoff_hz()`; a conventional channel's widths
(detector band, head cutoff, carrier meter) are worked out once per frequency by
`ChannelShape::of`, a digital one's head as a trunked channel of its kind.

**Pre-roll.** Every block's full spectrum is kept in a ring holding
`max(prerollS, conventional pre-roll 0.3 s, 0.1 s)` of history (1 s by default; about 196
blocks × 16,384 × 8 bytes ≈ 26 MB per source at 2.4 MSPS). `add_head` replays up to `prerollS`
of stored blocks (M-point work only) so a voice channel opened when a grant arrives starts with
the air *before* the grant. It also returns the absolute input-sample index of the head's first
output, which ties frame times to the source clock. Live and replayed output are identical.

**Other outputs from the same spectra:** the waterfall (`power_spectrum`), the noise floor per
slice (`noise_profile`: median |X|² / ln 2, in `floor_slices(fs)` slices: 64, fewer where a slice
would be under 30 kHz; `floor_slice` finds the one under a frequency), and the conventional
energy detector (`band_power`), which costs no head while a channel is idle. From the floor,
`noise_in_band` gives the noise a filter passes and `threshold` a carrier level so many dB above
it: the one threshold the trunked analog squelch and conventional's carrier meter both use.

**Input conversion:** `u8` → (b − 127.5)/127.5; `cs16` → i16/32768; `cf32` as is. An odd
trailing byte is carried to the next buffer so I and Q never swap.

**DC and frequency error.** There is no DC removal. Auto-centring places the centre so no
control channel is within 25 kHz of DC. Hardware ppm is applied in the drivers. AutoTune
(section 5.6) only moves head offsets.

**Guard band.** A frequency belongs to a source when `|f − center| ≤ usable_half_width`, where
`usable_half_width = max(rate/2 − guardHz, rate/4)`; `guardHz` defaults to 75 kHz (±1.125 MHz
usable at 2.4 MSPS). `dsp/rolloff.rs` measures a radio's roll-off and suggests a guard; it does
not affect the pipeline.

---

## 4. DSP chains

Each kind of channel is a chain: channelizer head → receiver → framer → decoder → (vocoder).
Receivers output `Symbol { dibit, sample, rel_hi, rel_lo }` with soft reliabilities that feed
the FEC (SmartNet's receiver outputs bits instead).

### 4.1 P25 Phase 1 (control channel and FDMA voice): the receiver bank

`p25::diversity::Bank` runs **several receivers on the same head** and takes the best of each
frame:
- CQPSK (π/4-DQPSK), CQPSK with a CMA equaliser (9 taps, μ 0.02), and C4FM, as the system's
  `modulation` allows (`fsk4` → C4FM only, `qpsk` → the two CQPSK receivers, `auto` → all three).
- Each receiver has its own framer. Frames that start within 36 symbols of each other form a
  group; a group is released once every receiver has passed it by 0.35 s.
- Selection: `best_frame` (fewest NID corrections), `best_tsbks` (per block, the first
  CRC-valid decode), `best_imbe` (per IMBE codeword, the lowest soft cost / mean reliability;
  erased bits count against a copy), `best_lc` / `best_es` (the first that decodes).

**C4FM receiver** (`dsp/c4fm.rs`, 4,800 Bd):
1. Power envelope (one-symbol smoothing); peak/floor over 0.1 s. A 13 dB peak/floor ratio marks
   a bursty (mobile) channel; quiet samples are left out of timing and levels.
2. FM discriminator in Hz.
3. RRC matched filter, α 0.5 for P25 (63 taps at 37.5 kHz), computed 8 outputs at a time (SIMD).
4. Timing per 240-symbol block: try candidate phases within ±½ symbol of the last block's,
   pick the one maximising Σ|y − centre|, interpolate between block centres. About 100 ms
   latency. No PLL.
5. Levels and slicer: 4-cluster means over the last 2,400 symbols (3 rounds); threshold midway
   between inner and outer means; soft reliabilities from distance to thresholds.
6. Multi-symbol detection (`dsp/msd.rs`): switched on when level separation drops below 9 and
   off above 11. Correlates every 3-symbol hypothesis (64 for P25) against the channel IQ.
   Claimed gain 2.8 dB on P25 C4FM.

**CQPSK receiver** (`dsp/cqpsk.rs`, 4,800 Bd for Phase 1):
1. Carrier tracker: phase-difference accumulator per block, NCO derotation.
2. RRC matched filter, α 0.35 (89 taps at 37.5 kHz); shared between the bank's CQPSK receivers.
3. Gardner timing loop (PI, kp 0.02, ki 1e-4, rate clamped ±5 %); reacquire by best-of-16
   phases after about 1 s without frame sync.
4. Optional T/2 CMA equaliser.
5. Differential detection with a 4th-power residual-phase estimate; quadrant slicer.
6. Soft bits from |Im| and |Re|, amplitude weighted.

**Framer and decoders** (`p25/frame.rs`, `tsbk.rs`, `voice.rs`, `fec.rs`):
- Frame sync 0x5575F5FF77FF (≤ 4 bit errors, either polarity), flywheel accepts ≤ 12 errors
  where the next frame must start. Status symbols (every 36th dibit) removed.
- NID: BCH(63,16) with ≤ 4 corrections; recovery by trying the last NAC with each DUID.
- TSDU: 196-bit de-interleave, soft Viterbi of the rate-½ 4-state trellis, CRC-CCITT → 3 TSBKs.
- LDU: nine 144-bit IMBE codewords (Golay(23,12) with Chase-II, Hamming(15,11) ML soft decoding,
  PN whitening). LDU1 link control: Hamming(10,6) + RS(24,12,13). LDU2 encryption sync:
  RS(24,16,9). HDU: Golay(18,6) + RS(36,20,17). TDULC: Golay(24,12) + RS(24,12).
- Voice: `VoiceTracker` (`trunk/tracker.rs`) follows HDU/LDU/TDU, reads talkgroup, source,
  emergency, encryption and talker aliases, and vocodes the best IMBE codewords. Encrypted frames
  are not vocoded.

### 4.2 P25 Phase 2 (TDMA voice)

1. Head (7 kHz cutoff) → one CQPSK receiver at 6,000 Bd (decision feedback β 0.5, no equaliser,
   no bank).
2. `phase2::Framer`: S-ISCH sync 0x575D57F7FF; I-ISCH lookup anchors the 12-slot superframe
   (180-dibit slots, 30 ms each, 360 ms superframe).
3. Descrambling: a 2,160-dibit mask per superframe from an LFSR seeded by WACN, System ID and NAC,
   learned from the control channel (`VoiceParams.tdma_key`).
4. DUID → burst type. 4V bursts carry four AMBE codewords, 2V two. ESS by RS(44,16,29).
   SACCH/FACCH by RS(63,35) + CRC-12 → MAC PDUs (PTT, END_PTT, ACTIVE, aliases).
5. `TdmaTracker` (`trunk/tdma.rs`): both slots on one head, each with its own AMBE decoder.

### 4.3 DMR

1. Head (6.25 kHz cutoff, trunked and conventional) → C4FM at 4,800 Bd, RRC α 0.2, MSD with decision
   feedback (16 hypotheses).
2. `dmr::Framer` → `Burst` (CACH 12 + burst 132 dibits, 30 ms). Nine 48-bit sync patterns.
3. `dmr::Channel`: the CACH TACT (Hamming(7,4)) picks the slot → two `SlotDecoder`s.
4. FEC: Golay(20,8) slot type, QR(16,7) EMB, soft BPTC(196,96) (Chase, up to 4 iterations),
   embedded LC BPTC(128,72) soft-combined over up to 4 superframes, RS(12,9) and CRC-CCITT masks.
5. Voice: three AMBE+2 codewords per voice burst, soft decoded.
6. Both slots share one head; only recorded slots are vocoded (`listen`).

### 4.4 NXDN

1. Head (3,400 Hz cutoff for NXDN48, 6,250 Hz for NXDN96) → C4FM at 2,400 or 4,800 Bd, RRC α 0.2.
2. Frame: 192 symbols = FSW 10 (0xCDF59) + LICH 8 + 174. PN9 descrambling.
3. LICH (7 bits + parity) picks the channel. CAC / SACCH / SCCH / FACCH1 / UDCH: K=5 rate-½
   convolutional code with puncturing and interleave, 16-state soft Viterbi + CRC.
4. Layer 3 messages for Type-C and Type-D trunking. Kenwood talker aliases (PROP_FORM, MFID
   0x68) in the SACCH and FACCH1 are put together in `nxdn::alias` and sent as `TrackerOut::Alias`.
5. Voice: four VCH codewords per frame → AMBE+2 (EFR full-rate voice is not decoded).

### 4.5 SmartNet control channel

1. Head (5 kHz cutoff) → `smartnet::Fsk2` at 3,600 Bd.
2. Discriminator + one-symbol moving average; noncoherent tone matched filters give the decision
   statistic (claimed 1.5–2 dB over the discriminator).
3. Zero-crossing DPLL symbol clock (gain 0.06); level tracker (EMA 0.02, ≥ 1,200 Hz apart).
4. OSW framer: 84 bits = sync 0xAC + 76 interleaved bits (19×4); 2-state soft Viterbi; 10-bit
   CRC. Flywheel at 84 ± 2 bits, up to 3 misses.
5. `smartnet::Parser` (port of Trunk Recorder / OP25): 6-deep OSW queue, band plans 800
   standard / reband / splinter, 900, OBT.

### 4.6 Analog FM (SmartNet analog grants, conventional FM)

`dsp/fm.rs` `Nbfm`:
1. 64-tap Blackman channel filter, 5,500 Hz (a conventional channel's `bandwidthKhz` sets it);
   also the carrier power meter (τ 10 ms).
2. Discriminator (5 kHz deviation = full scale).
3. De-emphasis, 750 µs (a conventional system's `demod.fmDeemphasis` can turn it off).
4. Audio low-pass 128 taps at 3,400 Hz; resample to 8 kHz.
5. 300 Hz 6th-order Butterworth high-pass (removes CTCSS).
6. Squelch gate: opens after 30 ms above threshold, closes after 2 ms below; 10 ms ramps; audio
   delayed 15 ms so the squelch tail is never heard (conventional: `demod.squelchOpenMs` /
   `squelchHangMs`). Trunked threshold: 6 dB above the noise under the channel
   (`Channelizer::threshold`).
7. The gated audio, and (conventional) the pre-high-pass audio, go to the call, whose in-band
   detectors find MDC1200 / FleetSync unit IDs and CTCSS/DCS there (section 4.9).

**Analog AM** (conventional channels, mode `am`: airband), `dsp/am.rs`: the same channel filter
(±5.5 kHz for 25 kHz channels, ±3 kHz for 8.33 kHz ones), the envelope over the tracked carrier
level (50 ms, or 0.5 s with `demod.amLevel` slow), a noise limiter, the 8 kHz low-pass and a low
cut (200 Hz by default, `demod.amLowCutHz`), then the same squelch gate, opened and closed by the
carrier's spectral line rather than the channel's power (`CarrierLine`); spurs' steady lines are
learned and left out (`LineWatch`).

### 4.7 Conventional detection (`trunk/conventional.rs`)

1. Noise floor per source: `noise_profile` in `floor_slices` slices (64 at most) every 0.1 s
   (EMA 0.3), not updated during gap fill.
2. Channel power: `band_power` over the channel's detector band (`ChannelShape`: ±5 kHz,
   ±3.125 kHz NXDN48, an analog channel's following its bandwidth), smoothed with a 20 ms time
   constant. AM also reads its carrier's line, about 9 dB more sensitive.
3. Opens when SNR ≥ the row's `squelchDb` (default 8 dB): `add_head` with 0.3 s pre-roll, then the
   same `voice::build` decoder as a trunked call.
4. A carrier meter with 3 dB hysteresis (its level from `Channelizer::threshold`) confirms the
   carrier; a false open raises the bar 3 dB.
5. The head closes 0.5 s after the carrier is gone and no call is live.
6. Several rows on one frequency are told apart by CTCSS / DCS (FM: held up to 1 s while the
   tone detector decides) or by an `Access` built from the row's `code`, `slot` and
   `matchTalkgroup` by `Access::join`: `Access::P25 { nac, tg }`, colour code / slot / talkgroup
   (DMR) or RAN / talkgroup (NXDN). Traffic no row takes is reported as `ConvSkipped`. The number
   a call is filed under is the row's `talkgroup`, else the talkgroup the air named, else the
   frequency default (kHz, then kHz×10 + k); a row never overwrites it for being one of several.
   The air's talkgroup is kept as `talkgroup_ota`, and a new one within an open call still starts
   a new call.
7. A conventional call is the engine's `Call` (no site; its `voice` from the channel's mode, a
   DMR call on its slot from the start), and conventional reports through the engine's own
   `Event`s (`ConvOut::Event`), link control applied as for a trunked call
   (`CallManager::note_info`). Its frequency error is measured from its decoder (P25), and its P25,
   DMR and NXDN channels name talkgroups by its own system's talkgroup file. It is still its own
   call lifecycle (a call opens on energy or a tone and ends with its carrier), not a grant source
   for `CallManager`.

### 4.8 Vocoders and saving

- `mbe/`: IMBE 7200×4400 (P25 Phase 1) and AMBE+2 3600×2450 (Phase 2, DMR, NXDN), ported from
  mbelib in double precision, with TIA-102.BABA-A concealment. 160 samples (20 ms) at 8 kHz per
  frame.
- Profiles: `fixed` (Pavel Yazev's fixed-point IMBE, the app default for Phase 1), `enhanced`,
  `mbelib`. AMBE always uses `enhanced`.
- AMBE+2 tone frames (`û0(11..6)` all ones, Golay-protected; BABA-A §16.1, §16.2): the tone ID
  (sent four times, bitwise majority) indexes Annex T (`mbe/tone_table.rs`) for the fundamental and
  the one or two harmonics; those are voiced at `16384·10^(0.03555·(level−127))` and the rest
  silent, through the ordinary voiced synthesis. The sender's four zeros at the end of `û3` aren't
  checked: `û3` has no protection. Checked in `Decoder::ambe` before mbelib's parameter decode, so
  the `mbelib` profile reads them as mbelib does.
- AMBE concealment (`Decoder::conceal`, BABA-A §14.6, §14.7): a frame with more than 3 errors is
  invalid and repeats the frame before, the voice or the tone playing, up to 3 in a row; past that
  it fades, and keeps fading until a valid frame. A damaged tone frame whose ID names the tone
  playing isn't invalid: the tone goes on at its level. Erasures fade without counting.
- Scaling: ×7/32768, then a tanh soft limiter above −3 dBFS.
- `trunk/recorded.rs`: `Recorded` is what a call has recorded, trunked or conventional alike (the
  engine's `Recording`, conventional's `Live` and multi-site's `Held` each hold one): its audio,
  the vocoder frames behind it, its transmissions, its in-band signalling, reception and frequency
  error. Voice goes in one way, `Recorded::push(samples, frames, now)`, which returns what the
  voice newly carried (section 4.9); `Recorded::trim` leaves out what the saved call won't have,
  moving its frames, tones and sources with the audio.
- `record::save_call`: drops transmissions shorter than `minTransmissionS` and encrypted ones
  that weren't decrypted,
  refuses too-short or silent calls, normalises loudness (−12 dBFS speech, about −14 LUFS; gain
  clamped −18..+24 dB; look-ahead limiter at −2.5 dBFS), writes the call JSON. `wav.rs`
  writes 16-bit mono 8 kHz.

### 4.9 In-band signalling (`dsp/inband/`, `trunk/inband.rs`)

What a call's voice carries besides speech. Every detector is a `dsp::inband::Detector`: it takes
a `Chunk` of the call's audio (from its `Tap`: the voice, or FM's pre-high-pass audio) and gives
`Found` items:

| Detector | Finds | Calls |
|---|---|---|
| `audio::AudioTones` | DTMF / KNOX keys, call progress and single tones (a 512-point spectrum every 20 ms) | every call |
| `units::Signalling` | MDC1200 and FleetSync unit IDs | analog FM |
| `subaudible::ToneDetector` | the CTCSS tone or DCS code | conventional FM |

A half-rate (AMBE+2) radio sends a tone as tone frames instead. The vocoder plays them (4.8) and
the frame carries the tone it played, repeats included (`VoiceFrame::tone`, from
`Decoder::tone_played`); audio a tone frame named is left to it (`Chunk::named`).

`trunk::inband::InBand`, one per call (in its `Recorded`), owns its detectors and gathers what
they find: the tones (`ToneSeg`s, a run of the vocoder's tone one segment, a clean frame missed in
it bridged), the sub-audible reading (`Call::tone`), and the unit IDs, which become the call's
sources as a voice channel's link control does (`CallManager::note_units`). Its tones are made into
patterns (`pattern::patterns`: a page, a long tone, a warble, digits, Morse, a sequence, a beep)
once nothing can add to them, a second after the last tone, unless a detector is still in the
middle of one (`Detector::busy_since`). Each pattern is told once, as it settles (`News`), and kept:
the call JSON's `tone_patterns` are the same patterns, moved with the audio when it's trimmed.

The engine tells each pattern as `Event::TonePattern(PatternHeard)`: with the radio talking where
it starts (`Recorded::talkers`), the system's signals it is (`signal::Signal::matches`), and
whether its voice was decrypted. A multi-site call's patterns are told from the copy its live audio
comes from (`MultiSite::leads`). The session gives it to plugins (topic `tone_pattern`, only to
those allowed decrypted calls when it was), the log, and the stats: the radio registry's tones log
and, for a signal, the event feed (`MonitorEvent::TonePattern`).

---

## 5. How trunking messages are processed

### 5.1 The path

Each site of a trunked system is a `Trunk` in the engine (`trunk/engine.rs`), built from its
`SiteConfig`; what its system decides (talkgroups, rules, keys) is its `SystemConfig`'s, passed in
where it's needed (`SystemConfig::policy`). How a site listens is one enum, decided once by
`Trunk::start` from its protocol's `CarrierPlan`: `Listening::Off`, `Hunting(Hunt)` (one control
channel, its source, head, clock and AutoTune state) or `Watching(Vec<Carrier>)`.

1. **Control head** on the source that covers the control channel (`Radio::source_for`), with a
   `Tap` on that source so its decoding thread feeds the control channel.
2. **`ControlChannel::push`** (one implementation per protocol in `trunk/control.rs`, built by
   `control::build(&SiteConfig, rate)`), on the source's decoding thread under the control
   channel's own lock (`Cc = Arc<Mutex<Box<dyn ControlChannel>>>`; a watched site's carriers can
   be on several sources, all feeding one decoder): IQ → receiver → framer → parser →
   `Step { good, msgs: Vec<Message>, identity }`, gathered with the rest of the block's news into
   a `CcResult`.
3. **`Trunk::on_block`** (under the engine's lock): the site's clock moves on (hunting: its
   control channel's samples; watching: its source's time), then **`Trunk::follow`**, for each
   step:
   1. Record the site's identity and adjacent sites.
   2. Compare the identity with the configured `expect` (the site lock); on a new mismatch log
      "Control channel X MHz is not this system".
   3. Emit every message as `Event::Message` (the log and the plugin `unit` topic).
   4. If mismatched: skip the step and do not refresh `last_good_s`, so hunting moves on.
   5. If the step decoded cleanly, refresh `last_good_s`.
   6. Hold grants until every locked field has been heard.
   7. Pass the Phase 2 scrambling key and the NAC to voice channels (`VoiceParams`: the site
      keeps its own and copies a change to every source's `SourceRx`).
   8. **`CallManager::handle(msgs, SysHost)`**.

### 5.2 Per protocol

| Protocol | Carriers | Receive and decode | Site lock fields |
|---|---|---|---|
| P25 | one, hunted | `Bank` → `p25::Framer` → TSDU / MBT → `p25::osp::Osp` → `P25Parser` | NAC, WACN, System ID, RFSS, site |
| SmartNet | one, hunted | `Fsk2` → OSW framer → `smartnet::Parser` | System ID, site |
| DMR | every carrier, watched | `C4fm::dmr` per carrier → bursts → CSBK / LC | none (colour code gate) |
| NXDN | every carrier, watched | `C4fm` (NXDN) → frames → CAC / SACCH → layer 3 | Type-C: System ID, site; Type-D: none |

`CarrierPlan::Hunt` (P25, SmartNet) listens to one control channel at a time and moves through
the list. `CarrierPlan::Watch` (DMR, NXDN) opens a head on every listed carrier at once.

### 5.3 `Message` and `MessageType`

`trunk::Message` is one struct for every protocol. Its `kind: MessageType` is one of:
`Grant`, `Status`, `Update`, `ControlChannel`, `Registration`, `Deregistration`, `Affiliation`,
`SysId`, `Acknowledge`, `Location`, `PatchAdd`, `PatchDelete`, `DataGrant`, `UuAnsReq`,
`UuVGrant`, `UuVUpdate`, `PhoneGrant`, `PhoneUpdate`, `CallAlert`, `StatusUpdate`,
`MessageUpdate`, `Adjacent`, `Deny`, `Queued`, `EmergencyAlarm`, `BaseStationId`, `Unknown`. A
grant's or update's `voice: Voice` says what the call's voice is on (see 9.3).

P25: the TSBKs and MBTs are first read into typed outbound signalling packets
(`p25::osp::Osp`, `Msg`), then `P25Parser::parse` makes `Message`s of them. Opcodes:

| Opcode | Standard | Motorola (MFID 0x90) |
|---|---|---|
| 0x00 | Grant (group voice) | PatchAdd |
| 0x01 | — | PatchDelete |
| 0x02 | Update (two if ch2 differs) | Grant (patch grant) |
| 0x03 | Update (explicit) | Update (patch) |
| 0x04 | UuVGrant | extended function |
| 0x05 | UuAnsReq | traffic channel ID (ignored) |
| 0x06 | UuVUpdate | Queued |
| 0x07 | — | Deny |
| 0x08, 0x09 | PhoneGrant, PhoneUpdate | Acknowledge (0x08) |
| 0x0a | telephone answer request | EmergencyAlarm |
| 0x0b | — | BaseStationId (the site's callsign) |
| 0x14 | DataGrant | — |
| 0x18, 0x1c | StatusUpdate, MessageUpdate | — |
| 0x1f | CallAlert | — |
| 0x20 | Acknowledge | — |
| 0x21, 0x27 | Queued, Deny | — |
| 0x28 | Affiliation | — |
| 0x29, 0x39 | ControlChannel (secondary CCs) | — |
| 0x2b | Location | — |
| 0x2c | Registration | — |
| 0x2f | Deregistration | — |
| 0x30 (MFID 0xa4) | L3Harris (M/A-COM) regroup: PatchAdd / PatchDelete | — |
| 0x33, 0x34, 0x3d | IDEN_UP*: update the band plan | — |
| 0x3a | SysId (system, RFSS, site) | — |
| 0x3b | Status (WACN, system, CC frequency) | — |
| 0x3c | Adjacent | — |

Service options byte: emergency 0x80, encrypted 0x40, duplex 0x20, mode 0x10, priority 0x07.

SmartNet emits Grant (OBT two-OSW, analog 0x308, digital 0x321), Update, SysId (on change),
Affiliation, PatchAdd, PatchDelete and Deregistration; talkgroup = address & 0xfff0, low 4 bits
are flags (0x8 = encrypted). DMR emits Grant/Update/UuV* from voice link control and from
Capacity Plus, Capacity Max, Connect Plus and Tier III CSBKs, gated by the site's colour code;
a grant whose LCN can't be resolved has `freq_hz = 0` until the talkgroup is heard on a watched
carrier within 4 s. NXDN emits Grant/Update/UuV* from `VCallAssgn` and from heard traffic,
`Adjacent` from `AdjSiteInfo`, gated by RAN.

### 5.4 What `CallManager::handle` does with each kind

| Kind | Action |
|---|---|
| `Grant` | `grant(m)` |
| `Update` | `grant(m)` (because `new_call_from_update` is always true in the app) |
| `UuVGrant`, `PhoneGrant` | `grant(m, unit-to-unit)` if `recordUnitToUnit` |
| `UuVUpdate`, `PhoneUpdate` | `update(m)` if `recordUnitToUnit` |
| `PatchAdd` | add to `Patches`; calls fold in patch members; a call skipped as unknown talkgroup is re-admitted |
| `PatchDelete` | remove from `Patches` |
| everything else | ignored by the call manager; `Session` turns registrations, affiliations, etc. into plugin `unit` events |

`CallManager::handle` takes the site's system's `Policy` (its talkgroups, and whether it has
keys) with each step, so a talkgroup file edited while recording reaches every site from one
place. A call's identity is (talkgroup, frequency, its place on the channel: Phase 2 or not, the
slot). `grant()` ignores `freq_hz == 0`; if a matching call exists it is refreshed
(`last_update_s`, patches, encrypted/emergency latch, a new source unit); otherwise a new `Call`
gets the next id from the shared `CallIds` counter (one sequence for every system and
conventional channel) and is admitted. A talkgroup of mode `A` turns a Phase 1 grant into
`Voice::Fm` (a two-slot grant keeps its slot); one of an encrypted mode (E, TE, DE) marks the call
encrypted.

### 5.5 Hunting

- `CC_HUNT_S` = 5 s: if no step decoded cleanly for 5 s and the system has more than one control
  channel, `tune(next)`: remove the control head, add one at the next covered frequency, reset
  the protocol's state and forget the site identity.
- Steps from a mismatched site never refresh `last_good_s`, so a foreign control channel is
  hunted away from.
- Watched plans (DMR, NXDN) never hunt; they report when the active control carrier moves
  (Capacity Plus rest channel).

### 5.6 AutoTune

Every 10 s, if the control channel decoded in the last second, the site takes the receiver's
frequency offset (`CcResult::offset_hz`, read with the block's decoding) and records ppm =
applied ppm + offset/f × 1e6 on its source; each call of a second or more whose voice decoder
reported an offset records its average the same way (|ppm| ≤ 50 kept; the median of the last 20,
of both kinds, is the score). `Session::load_tune` / `tune_unsaved` carry each source's score to
the next run (`tune.json`), which starts from it. With `autoTune` on, new heads open at
`f × (1 + ppm·1e-6) − center`. P25 control channels more than 150 Hz off are reopened at the
corrected frequency, at most every 200 s. A watching site (DMR, NXDN, EDACS) measures the carrier
the control channel is on now, against the correction it was opened with; its carriers stay where
they were opened (their receivers follow the offset). The measured error is always reported
(`SourceTune`). `ControlChannel::offset_hz` and `VoiceDecoder::offset_hz` have no default: every
protocol says whether it measures one.

### 5.7 Patches

`Patches` maps supergroup → member → last heard. Members not heard for 10 s (P25) or 4 s
(SmartNet) expire; a patch with one member left is dropped. `members_of(tg)` fills
`Call.patched_talkgroups`.

---

## 6. How recorders are managed and assigned

### 6.1 The pool

`Radio` in `trunk/engine.rs` is shared by every site:
- `rx: Arc<Vec<Mutex<SourceRx>>>`: each source's radio side behind its own lock: its channelizer,
  its open voice channels (`channels: BTreeMap<(site, freq_hz), Channel>`; sites never share
  one), its control channel taps and its copy of each site's `VoiceParams`.
- `cfgs: Arc<[SourceConfig]>`: the engine's one copy of the sources' settings, shared with each
  `Source` (with its index) and the `Engine` handle.
- `recordings: HashMap<CallId, Recording>`: calls being recorded. Their count is capped by
  `maxRecorders` (default 32).
- `free_nums` / `next_num`: recorder numbers (the call JSON's `recorder_num`), reused lowest-free.
- `followed`: calls followed without a recorder now (`Call::followed`).
- `pending`: decoder outputs, as (site, call, output), applied after the block.

A "recorder" is not a pre-built object as in Trunk Recorder. It is a `Recording` (audio buffer,
vocoder frames, transmissions, reception meter) plus a head and decoder that exist only while
the call does.

### 6.2 Admitting a call (`CallManager::admit`)

In order, the first that applies wins:
1. The talkgroup row has `Ignore` (or priority < 0, read as Ignore) → not recorded, reason
   `ignored`.
2. The talkgroup is not in a non-empty talkgroup file (nor a patch member) and `recordUnknown`
   is off → `unknown_tg`.
3. Encrypted and `recordEncrypted` is off, unless the system has keys (`Policy::can_decrypt`)
   and its voice can be decrypted (`Voice::decryptable`: P25 Phase 1 or 2) → `encrypted`; the
   engine may **follow** it (`RecorderHost::follow`: open a channel without a recording, to
   collect units and aliases) while recordings plus followed calls are under `maxRecorders`,
   never on analog.
4. Otherwise `RecorderHost::record(call)` (`SysHost::record`):
   1. `recordings.len() ≥ maxRecorders` → `no_recorder`.
   2. `Radio::source_for(freq)` finds no source covering it → `no_source`.
   3. Take a recorder number, insert a `Recording`, `open_channel`.

There is no priority and no preemption: a call refused for `no_recorder` stays monitored until it
times out and is not retried. The talkgroup CSV `Priority` column only matters when negative
(ignore). `Message.priority` is copied to the call JSON and used nowhere else.

### 6.3 Opening the channel (`SysHost::open_channel`)

1. The voice kind is the call's `voice.kind()` (`Voice::P25` → `Fdma`, `P25Tdma` → `Tdma`,
   `Dmr` → `Dmr`, `Nxdn(rate)` → `Nxdn`, `Fm` → `Analog`, `Am` → `Am`); the cutoff is
   `VoiceKind::cutoff_hz()`.
2. Slot: `voice.slot() & 1` for Phase 2 and DMR, else 0.
3. Under the covering source's lock: if `(site, freq)` already has a channel of **the same
   kind**, put the call in that slot and return. Phase 2 and DMR calls on the two slots of one
   carrier share one head and one decoder. A newer call on an occupied slot takes it over; the
   older call stops getting audio and ends by timeout.
4. If it has a channel of **another kind** (P25 dynamic dual mode, SmartNet analog ↔ digital):
   remove that channel and head; its calls time out.
5. Otherwise: `add_head(offset with AutoTune ppm, cutoff, prerollS)`, the noise under the channel
   from the source's floor (`floor_slice`; the analog squelch 6 dB above it,
   `Channelizer::threshold`), the decoder from `voice::build(VoiceSpec)`, the replayed pre-roll
   decoded at once.

| `VoiceKind` | Decoder chain |
|---|---|
| `Fdma` | `Bank` → `VoiceTracker` → IMBE |
| `Tdma` | `Cqpsk` 6000 Bd → `phase2::Framer` → `TdmaTracker` (both slots) → AMBE+2 |
| `Dmr` | `C4fm::dmr` → `DmrVoice` (both slots) → AMBE+2 |
| `Nxdn(rate)` | `C4fm` (NXDN) → `NxdnVoice` → AMBE+2 |
| `Analog` | `Nbfm` (its unit IDs and tones are found with the call's: section 4.9) |
| `Am` | `Am` (conventional only) |

### 6.4 While recording

Decoder outputs (`VoiceOut { slot, out: TrackerOut }`) are routed to the call on their slot and
queued in `radio.pending`, then applied after the block (`apply_pending`), at the site's
control channel clock, so a voice channel never borrows a `Trunk` mutably. Each output finds its
site, recording and call once, then:
- `Audio` / `AnalogAudio` → appended to the `Recording`, `last_audio_s` moved, live
  `Event::Audio` if this copy leads its multi-site conversation;
- `Info` → the source unit and its flags (merged into the source talking when it names none), the
  call's encrypted / emergency latches → `CallUpdate`;
- `WrongKey` → a warning note, once a key;
- `Cipher` → the call's ALGID / key ID; `Decrypted` → the transmission is kept and the call marked
  `decrypted`;
- `Position` → the talker's GPS position, `Event::UnitPosition`;
- `Alias` → talker alias learned (its system's `UnitAliases`); `AliasLc` → counted.

### 6.5 Ending

`CallManager::tick`, every block, newest call first:
- quiet control channel = no grant/update for `callTimeoutS` (3 s);
- quiet audio = no audio for `callTimeoutS`;
- ends the call when the control channel is quiet and (it isn't recording or the audio is quiet):
  `RecorderHost::release` (clears its slot; the head goes when both slots are empty), then
  `CallEnd`;
- splits a call that has run `maxCallS` (or `STUCK_CALL_S` = 600 s when 0): the old call is saved,
  a new call with a new id carries on with the same channel and recorder
  (`record_continued`, which skips the `maxRecorders` check).

With `conversationMode` off (`CallConfig::conversation`), the same split happens at each new
transmission: `Core::apply_pending`, taking a recorded call's audio after more than
`TRANSMISSION_GAP_S` (0.5 s) of none, or a new talker after the last had talked that long, asks
`CallManager::next_transmission`, and the audio (and
what's queued after it in that pass) goes to the new call. The talkers announced since the last
audio (a grant before its voice) move with it.

The recorder is freed in `Core::conclude`, at the end of the block, when the recording is
removed and its number returned to `free_nums`.

### 6.6 Multi-site

`trunk/multisite.rs`. Sites are twins when they are sites of one system (one `SystemConfig`'s
`sites`); nothing is grouped from what the control channels announce. A call that starts on a twin
site with the same talkgroup (or, unit to unit, the same radio called) within 3 s
(`Span::starts_with`) is linked into one conversation, from the first grant (`Call::twin` marks
the later copies). A conversation has one copy from each site (`MultiSite::link`), so copies never
chain one into the next. **Every copy is recorded.** Live audio comes from one copy until it has
been quiet 0.5 s. When the last copy ends, they're tried best first: the talkgroup's preferred site
if its clean voice is at least 90 % of the best, otherwise the most clean voice, then fewest FEC
errors, then earliest start. Each is saved unless one saved holds it (`Kept::holds`: another site,
its whole `Span`, the same talkers where known, at least as much clean voice give or take 90 %);
those become `Duplicate` events. Every copy saved gets the same `transmission_id`, from the first
copy's start (`SaveContext::first_s`).

---

## 7. Buffering and back-pressure, compared with GNU Radio

### 7.1 How Trunk Recorder (GNU Radio) does it

- Each source is a GNU Radio source block feeding a flowgraph. At start, each source builds a
  fixed number of recorder hierarchical blocks (`digitalRecorders`, `analogRecorders`), all
  connected through a `selector` block. A recorder's port is enabled only while it records.
- Every recorder has its own channelizer (`xlat_channelizer`: frequency translation and
  filtering of the full-rate stream, then decimation), demodulator and decoder. The cost of
  following a channel scales with the full sample rate, once per active recorder.
- The control channel decoder writes messages into a GNU Radio message queue. The main loop
  polls it (`delete_head_nowait`) and starts or stops recorders.
- GNU Radio's thread-per-block scheduler runs each block in its own thread. Blocks are joined by
  fixed-size circular buffers. A block's `work()` runs when its input has enough items and its
  output buffer has room. A writer cannot overwrite data the slowest reader hasn't consumed.
- **Back-pressure** therefore propagates upstream buffer by buffer: a slow decoder stops its
  demodulator, which stops the channelizer, which stops the selector, which stops the source
  block. The source cannot stop the radio, so the driver's buffers overflow and samples are lost
  (UHD prints `O`). Because the selector feeds every recorder, one slow recorder can stall all
  of them.
- Advantages: blocks run on many cores without any code for it; blocks are reusable.

### 7.2 How Trunk Recorder Pro does it

- **A decoding thread per source, push-driven, run to completion.** Each source's `dsp-N` takes one
  block from its queue and calls `push`. Inside, each completed 12,288-sample channelizer block runs
  every stage for that source synchronously, in a fixed order (section 2, steps 4–7), before the
  next block is read: its DSP under the source's lock, then the calls under the engine's. There are
  no buffers between stages: a head's output is overwritten every block, and decoders keep only
  their own state. Fed from one thread (`replay`, the browser), it runs exactly as before, and
  threaded the calls come out the same (`sources_fed_from_threads_decode_as_from_one`).
- **One FFT for all channels.** Opening a channel adds an M-point inverse FFT, not a full-rate
  filter. The recorder count costs little until channels actually decode.
- **One bounded queue in front of each source's decoding, which blocks.**
  `sync_channel::<SourceMsg>(256)` per source. If a source's decoding is slower than real time,
  its queue fills (about 14 s of RTL data at 2.4 MSPS, about 2.5 s for 10 ms blocks), its source
  thread blocks in `send`, the driver stops collecting, and the device's own buffer overflows. The
  driver reports what it lost (`dropped`) where it can tell; the feeder fills it with faint noise
  (`push_gap`, up to 10 s) so sample time stays in step with the air. Loss a driver can't see is
  caught by the wall-clock fit: if samples are more than 0.25 s off, the clock is re-anchored and
  the log says so.
- **Everything after decoding is non-blocking, with an explicit policy per queue.** Nothing
  downstream can stall decoding:

| Queue | Capacity | Producer → consumer | When full |
|---|---|---|---|
| Source → decoding | 256 messages per source | `source-N` → `dsp-N` | a live radio's block is dropped and counted (`dropped`, gap-filled); a capture file's sender waits |
| rtlsdr-nusb USB transfers | 128 × 16 KiB (`bufLen` / `buffers`) | USB → source thread | device FIFO overflows; the chip's own count is read at each retune, the rest caught by `ClockFit` as unreported loss |
| UHD receive frames | 1024 frames | USRP → UHD | overflow `O`; counted from the timestamp gap |
| Engine commands (`EngineCmd`) | unbounded | decoding threads, server → engine | — |
| Hub (tokio broadcast) | 4,096 messages | engine, finish, monitor, logger → each WebSocket | a lagging client loses its oldest messages (silently); others unaffected |
| Finish | 1,024 calls | engine → finish pool | the engine thread waits up to 5 s, then finishes the call itself by the same plan; decoding carries on |
| Plugin events | 1,024 lines per plugin process | engine / finish → plugin stdin | the event is dropped and counted (`dropped`), logged at most every 30 s |
| Stats rollups | unbounded | engine → stats-store | — |
| SDK `CallQueue` (inside a plugin) | 10,000 calls | plugin | `call.result failed` "too many calls waiting" |

- **Topic gating** keeps cost proportional to what's watched: spectra, control-channel logs, RF
  and decode detail are built only while some client subscribes; live audio frames only while a
  browser is connected or a plugin wants audio; plugin events only for topics some plugin
  subscribes to.

### 7.3 What happens when one part is slow

| Slow part | Effect |
|---|---|
| A source's decoding (CPU can't keep up) | Its queue fills → its source blocks → its driver loses samples → `dropped` in status, gap filled, calls carry on with a hole; the busiest source's load is shown on the dashboard. Other sources decode on their own cores. |
| One browser | Its WebSocket task falls behind the 4,096-message ring and skips messages. Recording and other browsers are unaffected. |
| Disk / finish threads | Finish queue fills (1,024 calls) → the engine thread waits up to 5 s for a place, then writes the call itself (its whole plan, .m4a included). Its messages and plugin events wait meanwhile; decoding doesn't. |
| One plugin | Its 1,024-line queue fills → its events are dropped and counted. The engine and other plugins are unaffected. A call it never answered for counts as failed after 1 h. |
| M4A encoder | Slows its finisher (1–3 run at once), as a slow disk does. An encode that fails leaves the call's WAV instead. |
| A radio stalls | No data for 1 s → silence filled in by its feeder (`Feeder::idle`) so calls on that source time out; the driver reopens after 2–3 s. |

### 7.4 Side by side

| | Trunk Recorder (GNU Radio) | Trunk Recorder Pro |
|---|---|---|
| Scheduling | a thread per block | a decoding thread per source; stages called in order per block |
| Inter-stage buffers | circular buffer per block output | none; stage outputs reused every block |
| Unit of work | whatever items are available (`noutput_items`) | one channelizer block (12,288 samples) |
| Channel extraction | a full-rate filter per recorder | a shared FFT; an M-point IFFT per channel |
| Recorders | pre-built at start, enabled by selector | built per call; a count cap only |
| Control → recorder | message queue polled by the main loop | direct call (`CallManager` → `RecorderHost`) in the same block |
| Back-pressure | propagates block by block to the source | one blocking queue per source at the input; downstream queues never block decoding |
| Overload | hardware overflow; any block can stall all | hardware overflow, measured, gap-filled, clock re-anchored |
| Multi-core | automatic | a core per source for DSP (calls followed under one lock); I/O, encoding, plugins and the server on other threads |
| CPU | — | measured 4–6× less than Trunk Recorder on Raspberry Pi / Linux (2026-10-02 benchmark) |

---

## 8. Plugins

### 8.1 Model

A plugin is a **separate executable** that only watches; nothing it does changes what is
recorded. It speaks JSON lines: `HostMessage` on its stdin, `PluginMessage` on its stdout, and
anything on stderr becomes a log line.

- Location: `<data>/plugins/<id>/<id>[.exe]`, or `path` in the plugin's config entry.
- Working folder: `<data>/plugin-data/<id>/` (its state, e.g. `queue.jsonl`).
- Own process group, so Ctrl-C reaches only the recorder, which then stops it.
- **Manifest**: `exe --describe` prints `id`, `name`, `version`, `api`, `subscribe` (topics),
  `audio_formats`, and JSON Schemas for its recorder-wide `config` and per-system
  `system_config`. The Plugins page draws its forms from those schemas. Cached by path and mtime.
  Rejected if `api` is 0 or above the host's `API_VERSION` (1). A plugin with a required setting
  left empty is not started.

### 8.2 Host side (`trunk-pro/src/plugins/`)

| File | Role |
|---|---|
| `mod.rs` | specs, copies, labels, `--describe` |
| `host.rs` | `PluginHost`: processes, queues, dispatch, handing finished calls on, shutdown |
| `manage.rs` | `Plugins`: runtime state for the UI, reloads (`Plugins::host()` hands out the running host), stats, installs |
| `store.rs` | the plugin registry, over the shared `trunk-pro/src/store.rs` |
| `archive.rs` | `plan` (which of a call's files are written and kept); follows each finished call to its plugins: their `call.result`s decide when files can be deleted, and time each delivery |
| `encode.rs` | ffmpeg / afconvert / fdkaac M4A (the finish pool calls it) |
| `delay.rs`, `hold.rs` | plugin delays (`pluginDelays`): what is held back from a plugin, and for how long |
| `timing.rs` | `Totals`: a plugin's delivery timing (age, held, waiting, upload) as `plg/<id>/…` series; the archive keeps one per plugin |
| `cli.rs` | `trunk-pro plugin …` commands |

Per plugin process: a supervisor thread (`plugin-<id>`) reading stdout, a writer thread feeding
stdin from the 1,024-line queue, and a stderr reader. Events are serialised once and shared. A
plugin is handed a call's files as the finish pool wrote them (`CallFiles`); the host writes or
encodes nothing itself. A call decrypted with the recorder's keys goes only to plugins with
`shareDecrypted`.

### 8.3 Conversation

1. Host → `hello` { api, host, config, systems [ {index, short_name, kind, name?, fcc? { "<Hz>": {call_sign, name?} }, fcc_call_sign?, fcc_name?, sites? [ {name} ], config, settings} ],
   capture_dir, data_dir, audio_formats }. `index` is the system's number this run (from 0,
   trunked then conventional); `kind` says which, `conventional` included; `settings` are the
   plugin's own settings as a system's `override` changes them. A plugin asks for recorded
   calls with `calls.query` (answered `calls`, by request number), not by opening the store.
2. Plugin → `ready` (or `status: error` and exit 78 for a config error).
3. Host → events for subscribed topics: `call.start`, `call.end`, `call.concluded` (with
   json / wav / m4a paths), `unit`, `audio`, `status` (every 5 s).
4. Plugin → `log`, `status` { ok | warning | error }, `metrics`, and `call.result` { path,
   ok | skipped | failed, message, url } for each `call.concluded`.
5. Host → `shutdown` { grace_s } behind whatever is queued; stdin closes after it. The plugin
   gets its grace from when it has read it (after up to 30 s to drain), then is killed.

### 8.4 Supervision

- Restart with back-off 1 s doubling to 60 s (reset after a run longer than 60 s).
- Exit code 78 (config error): not restarted, and calls stop waiting on it.
- Changed settings restart the plugins; new processes start once old ones have exited (they share
  data folders); events wait meanwhile.

### 8.5 Archive

For each call its finisher calls `archive.expect(rel, takers, rules, …)`, where takers = live
processes subscribed to `call.concluded`, and says whether a WAV was written. Each `call.result`
counts down. At zero the call's `archiveWav`, `archiveM4a`, `callLog` and `archiveFilesOnFailure`
rules are applied (delete, or move out of the RAM spool): `ok` and `skipped` count the same, and
only a `failed` keeps the files (with `archiveFilesOnFailure`). An unsettled call counts as failed
after 1 h. With no takers, `archive::plan` writes only what the Keep rules keep, and the call
settles at once.

### 8.6 RAM spool

With `recording.ramSpool.enabled`, files that will be deleted once the plugins are done go to a
RAM disk (macOS: one made with `hdiutil`; Linux: `/dev/shm`) to save SSD wear. No room → the
recordings folder. Files older than 2 h are moved out.

### 8.7 Store

- One store for plugins and interfaces (`trunk-pro/src/store.rs`: `Registry`, `Shelf`,
  `download`, `stage`, `put_in_place`, `Release`), with thin specialisations in
  `plugins/store.rs` and `interfaces/store.rs`; the interface asks with `store {kind}` and
  `install {kind, …}` (section 14).
- Registry `index.json` from the TrunkRecorder/plugins repository (cached 15 min in
  `plugin-registry.json`; built-in copy as fallback); assets keyed by target triple.
- Install: download (≤ 200 MiB) → SHA-256 must match → unpack to staging (no links, no path
  escape, ≤ 1 GiB) → `--describe` must match id and version → atomic rename into
  `plugins/<id>/`.
- A GitHub release not in the registry can be installed if it has `SHA256SUMS` (tier
  "unlisted").

### 8.8 Several copies of one plugin

A system's `plugins.<id>` may be an array of settings objects: one process per element
(`openmhz`, `openmhz#2`, …), each with its own queue, back-off, data folder
(`plugin-data/openmhz#2/`) and drop counter. Copy *k* gets the same recorder-wide settings and,
per system, element *k* (or `null` for systems with fewer copies). The Archive waits for a
result from every copy. The UI shows one row per plugin with the worst state and summed metrics.

### 8.9 SDK (`trunk-recorder-plugin`, feature `sdk`)

- `CallRecord`: the call JSON as one typed record. The recorder makes one per call
  (`record::call_record`), writes the `.json` from it (`CallRecord::to_json`, byte for byte as
  the files always were) and hands the same record to the history and the plugins. Every field
  the recorder writes is named; others are kept in `extra`; flags are written 0/1 and read as
  0/1 or booleans.
- `Plugin` trait: associated `Config` and `SystemConfig` types (serde + schemars), `manifest()`,
  `start(Host, Setup)` (`Setup` has each system's settings, by index or `system_named`), optional
  `call_start`, `call_end`, `call_concluded`, `unit`, `audio`, `status`, `shutdown`. `run::<P>()`
  handles `--describe`, `--version`, the handshake and exit codes.
- `CallQueue`: worker threads (default 2, per lane: a queue per server), retries after 10 s, 60 s,
  300 s, 900 s, capacity 10,000, endpoint health (Up / Degraded / Down), metrics every 5–30 s, and
  `queue.jsonl` persistence across restarts.
- `TalkgroupFilter` (allow / deny globs), `schema.rs` (form hints such as `x-secret`,
  `x-system`, `x-required`), `testing.rs` (drive a plugin with messages; HTTP mock server).

---

## 9. Data objects

Lineage of a call: `Symbol` → `Frame` / `Packet` / `Burst` → `Message` → `Call` →
`TrackerOut` / `VoiceOut` → `Recording` → `Concluded` → `Event::Concluded` → `Output::File` →
call JSON + WAV on disk → `HostMessage::CallConcluded` → plugin → `PluginMessage::CallResult`.

### 9.1 Samples and symbols

**`SourceMsg`** (`trunk-pro/src/sdr.rs`): what a source thread sends its decoding thread.

| Variant | Fields |
|---|---|
| `Samples` | `source: usize`, `block: Block`, `dropped: u64`, `at: Instant`. `Block` is `U8` (offset 8-bit IQ), `Iq` (float) or `Real16` (an Airspy's real samples, made IQ by the `Inbox`), each in a reused `Pooled` buffer |
| `Error` | `source`, `error: String` (passed on to the engine thread) |
| `End` | `source` (passed on to the engine thread) |
| `Tuned` | `center_hz` (survey only) |

**`EngineCmd`** (`trunk-pro/src/runtime.rs`): what the engine thread is told, on its one channel:
`Source(SourceMsg)` (an error or end), `Topics(Topics)`, `Talkgroups { short_name, csv }`,
`Keys { short_name, ring }`.

**`Symbol`** (`dsp/mod.rs`): `dibit: u8`, `sample: f64` (channel-sample instant),
`rel_hi: f32`, `rel_lo: f32` (bit reliabilities; negative = hard only).

**`p25::Frame`**: `nid: Nid { nac: u16, duid: u8, errors: u8 }`, `symbol: u64`, `sample: f64`,
`inverted: bool`, `bits: Vec<u8>` (status symbols removed), `raw: Vec<u8>`, `soft: Vec<f32>`,
`raw_soft: Vec<f32>`, `complete: bool`. A TSBK is `[u8; 12]`.

**`phase2::Packet`**: `sf_slot: usize` (0..11), `sample: f64`, `dibits: [u8; 180]`,
`rel: Vec<f32>`.

**`dmr::Burst`**: `cach: [u8; 12]`, `dibits: [u8; 132]`, `rel: [f32; 264]`,
`sync: Option<SyncKind>`, `sync_errs: u32`, `sample: f64`.

**`nxdn::Frame`**: `dibits`, `rel`, `sync_errs: Option<u32>`, `sample: f64`.

### 9.2 Trunking

**`Message`** (`trunk/message.rs`):

| Field | Type | Meaning |
|---|---|---|
| `kind` | `MessageType` | see 5.3 |
| `time_s` | f64 | sample-clock time |
| `freq_hz` | u64 | voice/CC frequency; 0 = unresolved |
| `talkgroup` | u32 | |
| `source` | i64 | unit; −1 = none |
| `encrypted`, `emergency`, `duplex`, `mode` | bool | service options |
| `priority` | u8 | P25 service options bits 0–2; SmartNet 3 |
| `voice` | `Voice` | a grant's or update's: what the call's voice is on (9.3) |
| `sys_id`, `wacn`, `rfss`, `site` | u32 | identity |
| `nac` | u16 | |
| `patch` | `Option<Patch { sg: u32, ga: [u32; 3] }>` | |
| `ran` | `Option<u8>` | NXDN |
| `opcode` | u8 | |
| `callsign` | `Option<String>` | Motorola: the call sign a base station ID names (`freq_hz` its channel) |
| `meta` | String | human summary |

**`Step`**: `good: bool`, `msgs: Vec<Message>`, `identity: Identity`.

**`CcResult`** (engine, internal): a source's control channel decoding of one block, per site:
`site`, `head`, `steps`, `params: Vec<VoiceParams>` (one per step), `samples`, `notes`,
`control_hz` (watching: the carrier control is on), `counts` (good, bad), `offset_hz`.

**`Identity`**: `BTreeMap<IdField, u32>`; `IdField` = `Nac`, `Wacn`, `SysId`, `Rfss`, `Site`.

**`p25::osp::Identifier`** (P25 IDEN, kept in `P25Parser::tables`): `iden: u8`, `base_hz: u64`,
`spacing_hz: u64`, `tx_offset_hz: i64`, `bandwidth_hz: u32`, `slots: u8`,
`system: Option<(wacn, sys_id)>`.

**`Talkgroup`**: `number: u32`, `mode`, `alpha_tag`, `description`, `tag`, `group: String`,
`priority: i32`, `preferred_nac: u32`, `preferred_site: String`, `ignore: bool`.

### 9.3 Calls

**`Voice`** (`trunk/calls.rs`): what a call's voice is on, in one field for every protocol:
`P25`, `P25Tdma { slot }`, `Dmr { slot, color_code: Option<u8> }`, `Nxdn(Rate)`, `Fm`, `Am`.
`kind()` gives the `VoiceKind` its channel needs, `slot()`, `analog()`, `tdma()`,
`decryptable()` (P25 Phase 1 or 2), `color_code()`, `nxdn()`.

**`Call`** (`trunk/calls.rs`):

| Field | Type | Meaning |
|---|---|---|
| `id` | `CallId` (u32) | one sequence for all systems |
| `site` | `Option<u16>` | engine site index; none for a conventional call |
| `system` | u16 | engine system index (trunked, then conventional) |
| `twin` | bool | multi-site: a copy of a call another site of its system had first |
| `talkgroup` | u32 | |
| `freq_hz` | u64 | |
| `voice` | `Voice` | what its voice is on |
| `unit_to_unit` | bool | |
| `recording` | bool | has a recorder |
| `followed` | bool | has a channel without a recorder (encrypted, followed for its link control) |
| `reason` | `Option<Reason>` | why not recorded: `ignored`, `unknown_tg`, `encrypted`, `no_source`, `no_recorder` |
| `encrypted`, `emergency` | bool | latched |
| `cipher` | `Option<CipherId>` | the ALGID / key ID its voice named |
| `decrypted` | bool | some of its voice was decrypted |
| `priority` | u8 | |
| `duplex`, `mode` | bool | |
| `start_s`, `last_update_s`, `last_audio_s` | f64 | sample-clock times |
| `sources` | `Vec<CallSource { src: u32, time_s: f64, flags: Flags, audio_pos, position }>` | units heard; `Flags { emergency, encrypted, duplex, packet, priority }` is what each one's transmission said of itself (the grant's service options, then link control's / Phase 2's voice channel user messages) |
| `talkgroup_info` | `Option<Talkgroup>` | from the talkgroup file |
| `patched_talkgroups` | `Vec<u32>` | |
| `ran`, `nac` | options | NXDN RAN; conventional P25 NAC |
| `talkgroup_ota` | `Option<u32>` | conventional P25 / DMR / NXDN: the talkgroup the air named, whenever it named one |
| `tone`, `tone_set` | options | conventional CTCSS/DCS heard / configured |

**`CallEvent`**: `Start(Call)`, `Update(Call)`, `End(Call)`.

**`RecorderHost`** trait: `record`, `follow`, `release`, `record_continued`. **`Policy`**:
`talkgroups`, `can_decrypt` (a system's, as its call managers see it).

**`SystemConfig`** (`trunk/engine.rs`): `short_name`, `calls: CallConfig`, `save: SaveRules`,
`talkgroups`, `unit_tags`, `keys`, `sites: Vec<SiteConfig>` (none: conventional).
**`SiteConfig`**: `name`, `control_channels`, `bank`, `expect: Identity`, `protocol`.
**`SiteKey { system, site }`**: how a site is named outside the engine: `own_name()`,
`stats_key()` (`sys/<system>` or `sys/<system>/site/<site>`), `file_stem()` (`<system>` or
`<system>.<site>`), and `Display` (`<system>` or `<system>/<site>`, log lines).

**`Channel`** (engine, internal): `site`, `head`, `calls: [Option<OnSlot { id, talkgroup }>; 2]`,
`voice: Box<dyn VoiceDecoder>`, `freq_hz`, `tune_ppm`, `meter`, `noise`.

**`Recording`** (engine, internal): `audio: Vec<f32>`, `frames: CallFrames`,
`recorder_num: u32`, `tx: Transmissions`, `freq_error: (f64, u32)`, `source`,
`reception: Reception`, `decrypted`.

**`TrackerOut`**: `Audio(Vec<f32>, VoiceFrame)` (20 ms at 8 kHz), `Info { source, flags }`,
`Cipher(CipherId)`, `Decrypted` (once the key check passes), `WrongKey(CipherId)`,
`AnalogAudio(Vec<f32>)`, `Subaudible(Vec<f32>)`, `Alias(Alias)` (`crate::alias`, any protocol),
`AliasLc(AliasLc)`, `Position(Position)` (`crate::position`). **`VoiceOut`**: `slot: u8`,
`out: TrackerOut` (applied at the site's control channel clock).

**`VoiceFrame`**: `codec` (Imbe | Ambe), `bits: Vec<u8>`, `e0`, `errs: u32`, `erased: bool`,
`kind`.

**`VoiceSpec`**: `kind`, `rate`, `t0`, `seed`, `vocoder`, `bank`, `squelch`, `subaudible`,
`analog: AnalogTuning`, `ignore_hz`, `keys`. **`VoiceParams`**: `tdma_key: Option<(nac, sysid,
wacn)>`, `nac: Option<u16>`. **`ChannelQuality`**: `sep`, `phase_err_deg` (what
`VoiceDecoder::quality()` and `ControlChannel::quality()` report).

**`SaveRules`**: `keep_silent`, `keep_encrypted`, `min_call_s`, `min_transmission_s`,
`normalize`, `digital_gain_db`, `analog_gain_db`.

**`Concluded`** (`trunk/record.rs`): `call: Call`, `record: CallRecord` (the call JSON),
`short_name`, `site`, `base_name` (`<tg>-<startEpoch>_<freq>[.slot]`), `audio: Vec<f32>`
(8 kHz), `frames: Option<Capture { ext: "sdr" | "frames.jsonl", bytes }>`.

### 9.4 Engine output

**`Event`** (`trunk/engine.rs`):

| Variant | Fields | Meaning |
|---|---|---|
| `ControlChannel` | `site`, `freq_hz` | a site tuned a control channel |
| `Message` | `site`, `msg: Message` | every decoded control message |
| `Note` | `system`, `site: Option<u16>`, `level` (Info / Warning), `text` | log-worthy note |
| `CallStart` / `CallUpdate` / `CallEnd` | `Call` | call lifecycle |
| `Audio` | `call_id`, `system`, `talkgroup`, `samples: Vec<f32>`, `decrypted` | live audio, 8 kHz |
| `UnitAlias` | `system`, `unit`, `alias`, `talkgroup` | talker alias heard |
| `UnitPosition` | `system`, `unit`, `lat`, `lon`, `talkgroup` | a radio on a call gave a new GPS position |
| `Concluded` | `Concluded` | a call to save |
| `NotSaved` | `Call` | too short / silent |
| `Duplicate` | `call`, `kept` | another site's copy kept |
| `ConvSkipped` | `freq_hz`, `code` | conventional traffic no row took |

Conventional channels report with these same events.

**`Status`**: `now_s`, `sites: Vec<SiteStatus>`, `active_calls`, `recording`,
`channels_open`, `conventional_open`, `calls_concluded`, `sources: Vec<SourceTune
{ error_ppm, applied_ppm, seeded }>`.

**`SiteStatus`**: `site`, `system`, `short_name`, `site_name`, `now_s`, `control_channel_hz`,
`identity`, `good`, `bad`, `modulation`, `active_calls`, `recording`, `calls_concluded`,
`mismatch`, `adjacent: Vec<AdjacentSite { sys_id, rfss, site, freq_hz }>`, `patches`,
`protocol: ProtocolStatus`. The session's `status` message groups them by system, with a summary
of each site; `adjacent`, the DMR and NXDN detail and `patches` go out in `decodeDetail`.

**`ChannelSnapshot`**: `system`, `site: Option<u16>`, `freq_hz`, `source`, `kind: ChannelKind`
(`Control` | `Carrier` | `Voice` | `Conventional`), `power_db`, `noise_db`, `offset_hz`,
`quality: ChannelQuality`, `calls`.

### 9.5 Session output (`trunk-app/src/session.rs`)

**`Output`**:

| Variant | Fields | Goes to |
|---|---|---|
| `Message` | `ToClient` (a typed interface message) | the browsers that may hear it (`publish`) |
| `Topic` | `topic`, `msg: ToClient` | browsers subscribed to the topic |
| `Audio` | `system`, `short_name`, `tg`, `frame: Vec<u8>` = `[2][u16 system][u32 call][u32 tg][i16 LE…]`, `decrypted` | listening browsers, plugins with `audio` |
| `File` | `rel`, `system`, `wav: Vec<u8>`, `record: CallRecord`, `frames`, `decrypted` | the finish pool (its history entry is `session::call_entry`) |
| `Plugin` | `HostMessage` | plugin host |
| `Log` | `log::Record` | logging |
| `Rollup` | a minute of dashboard series | stats store |

**`log::Record`**: `level`, `system: Option<String>`, `call: Option<CallTag { num, talkgroup,
tag, encrypted, freq_hz }>`, `body: Body` (`Text`, `Recording`, `NotRecording`, `Concluded`,
`Dropped`, `ControlChannel`, `DecodeRate`, `State`).

### 9.6 Call JSON (`CallRecord`, made by `record::call_record`)

Trunk Recorder's keys plus reception figures, in this order: `call_num`, `freq`,
`freq_error`, `signal`, `noise`, `snr`, `clean_voice_pct`, `source_num` (always 0),
`recorder_num`, `tdma_slot`, `phase2_tdma`, `start_time`, `stop_time`, `start_time_ms`,
`stop_time_ms`, `emergency`, `priority`, `mode`, `duplex`, `encrypted`, `algid`, `key_id` (−1 if
none), `decrypted`, `call_length`, `call_length_ms`, `talkgroup`, `talkgroup_ota`
(conventional P25 / DMR / NXDN calls whose air named a talkgroup; this recorder's addition, absent
otherwise), `talkgroup_tag`,
`talkgroup_description`, `talkgroup_group_tag`, `talkgroup_group`, `color_code` (−1 if none),
`ran` (−1 if none), `tone_mode`, `tone_detected`, `tone_confidence`, `audio_type` (analog |
digital tdma | digital), `short_name`, `site` (the site's name; none for conventional),
`patched_talkgroups` (only when more than one), `freqList` [{freq, time, pos, len, error_count,
spike_count}], `errorList` [{pos, len, frames, error_count, bad_frames, max_frame_errors}],
`srcList` [{src, time, pos, emergency, encrypted, duplex, mode, priority, signal_system, tag,
tag_ota, lat?, lon?}] (each source's own flags: what its transmission said of itself). Flags are
written 0/1; levels to a tenth of a dB (null when not measured). The fields are `CallRecord`'s,
in its order: the finisher writes `<rel>.json` from it once (`CallRecord::to_json`).

### 9.7 Plugin protocol (`trunk-recorder-plugin/src/protocol.rs`)

Constants: `API_VERSION` = 1, `EXIT_CONFIG` = 78.

**`Manifest`**: `id`, `name`, `version`, `description`, `api`, `subscribe`, `audio_formats`,
`config` (JSON Schema), `system_config` (JSON Schema), `homepage`, `repository`, `authors`,
`license`.

**`HostMessage`** (tag `type`):

| `type` | Payload fields |
|---|---|
| `hello` | `api`, `host { name, version }`, `config`, `systems: [SystemInfo { index, short_name, kind, name, fcc (by frequency), fcc_call_sign / fcc_name (the one that names it), sites: [SiteInfo { name }], config, settings }]` (name, FCC fields and sites left out when empty), `capture_dir`, `data_dir`, `store?` (the call store's absolute path, left out when the store didn't open; open it read-only, WAL, read `schema_version` first, never write; remote plugins use the HTTP API), `audio_formats` |
| `call.start`, `call.end` | once per call, from its first copy: `CallInfo { id, system, short_name, site, talkgroup, talkgroup_ota, talkgroup_tag, freq_hz, tdma_slot, analog, encrypted, algid, key_id, decrypted, emergency, duplex, packet, priority, recording, reason, start_time, units, sources: [CallSource { unit, time, emergency, encrypted, duplex, packet, priority }], patched_talkgroups }` |
| `call.concluded` | `path`, `system`, `call: CallRecord` (the call JSON), `files { json, wav, m4a }` |
| `unit` | `system`, `short_name`, `site`, `kind` (registration, deregistration, affiliation, acknowledge, location, data_grant, answer_request, call_alert, emergency_alarm), `unit`, `talkgroup`, `emergency`, `time` |
| `audio` | `call_id`, `system`, `short_name`, `talkgroup`, `sample_rate`, `pcm` (base64 i16 LE) |
| `status` | `time`, `systems [{ index, short_name, site_name, control_channel_hz, decode_rate, active_calls, recording, nac, wacn, sys_id, rfss, site }]` (one entry per site; the identity fields once the control channel has stated them) |
| `shutdown` | `grace_s` |

**`PluginMessage`** (tag `type`): `ready`; `log { level, message }`; `status { state: ok |
warning | error, message }`; `call.result { path, outcome: ok | skipped | failed, message, url }`;
`metrics { queued, retrying, inFlight, retries, bytesSent, latencyMs, lastOk, lastError,
lastErrorText, endpoints [{ name, state, latencyMs, lastOk, lastError }], extra }` (camelCase,
unlike the other messages).

---

## 10. The configuration file

### 10.1 Where it is

- macOS `~/Library/Application Support/trunk-pro/config.json`; Windows `%APPDATA%\trunk-pro\`;
  Linux `$XDG_CONFIG_HOME/trunk-pro/` or `~/.config/trunk-pro/`; Docker
  `/data/config/trunk-pro/config.json`; browser `localStorage["trp.config"]`.
- `--config file.json` picks another file. Its folder becomes the data folder, holding every
  companion file, so two configs in different folders share nothing.
- Versioned: `Ctx.config_version` goes up by one at every change (`Ctx::config_changed`, which
  also tells every browser `config { config, version }`). `setConfig { config, version }` always
  sends the whole config with the version it was edited from; one edited from an older version
  is refused (`configSaved` with an `error`), so another browser's change is never saved over.
  Saved through `write_atomic` (temporary file, `sync_all`, rename).
- A system's talkgroup file (trunked or conventional) is not in it: `<shortName>.talkgroups.csv`
  beside it (`config::talkgroups_path`; `TalkgroupFiles` by short name), written by
  `setTalkgroups` and taken by a recording at once (`EngineCmd::Talkgroups`). An older config's
  `talkgroupsCsv` / `talkgroupsName` are moved out into those files before it is read, by every
  reader (`Config::load` → `migrate::move_talkgroups`). The browser keeps them in
  `localStorage["trp.talkgroups"]`.
- Every upgrade of an older config is in `migrate.rs` (`migrate::config`, run by
  `Config::from_value`: key files named by systems, interfaces as a list and `server.home`, each
  site a system of its own, talkgroups in the config); what it changed is in `Config::notes`.
- Keys are camelCase. Unknown keys are kept and written back at the top level and in each
  system, conventional system, `recording`, `server` and `log`. A value of the wrong type makes
  the file unreadable ("not a config this version reads"), and so does a closed choice it doesn't
  list (a system `type`, `modulation`, `nxdnType`, `nxdnRate`, `defaultMode`, `vocoder`, the M4A
  `encoder`, `unitNames.mode`, the log's `color`, `level` and formats, an interface's `access`, a
  channel `mode`…): each is declared once by the `choice!` macro (`trunk-app/src/choice.rs`) with
  its names, read in any case (`"P25"`, `"QPSK"`), "" meaning the default. A source's `type` is the
  tag of `Source`, put in its own case by `Config::from_value` before it is read.

### 10.2 Top level

| Key | Type | Default | Meaning |
|---|---|---|---|
| `sources` | array | one RTL-SDR at 2.4 MSPS, auto centre | radios and capture files |
| `systems` | array | `[]` | trunked systems, each with its `sites` |
| `conventional` | array | `[]` | conventional systems |
| `recording` | object | see 10.6 | where and how calls are saved |
| `server` | object | see 10.8 | web server |
| `plugins` | map id → `{enabled, settings, path, shareDecrypted}` | `{}` | plugins on/off and recorder-wide settings |
| `pluginDelays` | array of `{plugins, talkgroups, delayS}` | `[]` | how long plugins wait for calls, every system (a system's own come first) |
| `keys` | map name → `{file, passwordFile}` | `{}` | encryption key sources; a system names one in its `keys` |
| `log` | object | see 10.9 | log options |
| `monitor` | `{probeHosts, networkChecks, timeServer, clockWarnMs}` | `["1.1.1.1:443","dns.google:443"]`, `true`, `"pool.ntp.org"`, `250` | internet checks; `networkChecks: false` pauses them and the SNTP check; the clock's SNTP check and its warning threshold |

### 10.3 `sources[]`

`type` is `rtlsdr`, `usrp`, `airspy`, `soapy` or `file`. Every type has `centerHz` (required;
0 = auto-centre), `rateHz` (required), `autoTune` (false) and `guardHz` (75,000).

| Type | Other keys |
|---|---|
| `rtlsdr` | `serial` (required; "" = first free), `gainDb` 25.4, `agc` false, `ppm` (integer, required), `bufLen` 0 (= 16384), `buffers` 0 (= about 2 MiB) |
| `hackrf` | `serial` "" = first, `centerHz`, `rateHz` (1–20 MSPS), `lnaDb` 16, `vgaDb` 20, `amp` false, `biasTee`, `ppm`, `autoTune`, `guardHz` |
| `usrp` | `args` "", `gainDb` 40, `agc` false, `antenna` "", `ppm` 0 |
| `airspy` | `serial` "", `gainMode` linearity / sensitivity / manual, `gainStep` 14, `lnaStep` / `mixerStep` / `vgaStep` 10, `agc`, `biasTee`, `packing` true, `ppm` |
| `soapy` | `args` "", `agc` true, `gainDb` null, `gains` {}, `antenna` "", `settings` "", `ppm` 0 |
| `file` | `path` (required), `realtime` (required), `format` cu8 / cs16 / cf32 (else from the extension) |

### 10.4 `systems[]`

| Key | Default | Meaning |
|---|---|---|
| `shortName` | `sys1` | identity: folder, companion files, plugin key; unique across all systems |
| `name` | "" | display name |
| `type` | `p25` | `p25`, `smartnet`, `dmr`, `nxdn` |
| `enabled` | true | |
| `unitNames` | — | `{csv, name, mode: user / ota / user_only / none}` |
| `recording` | {} | per-system overrides (10.7) |
| `plugins` | {} | per-system plugin settings (with `override` for the plugin's own settings it changes here); an array = several copies |
| `pluginDelays`, `keys` | | plugin delays; the key source it decrypts with |
| SmartNet / NXDN | | `defaultMode` (analog / digital); `nxdnType` (typeC / typeD) |
| `sites` | [] | where it is recorded from, below |

Each of `sites[]`:

| Key | Default | Meaning |
|---|---|---|
| `name` | the system's `shortName` | unique in its system: Preferred Site, call JSON `site`, band plan file, its stats |
| `enabled` | true | |
| `controlChannelsHz` | [] | P25/SmartNet: hunted in order; DMR/NXDN: all watched |
| `modulation` | `auto` | `auto`, `fsk4`, `qpsk` (P25 receiver bank) |
| `expect` | {} | site lock: `nac`, `wacn`, `sysId`, `rfss`, `site` (decimal) |
| `voiceChannelsHz` | [] | from the survey; only for auto-centring |
| SmartNet | | `bandplan` (800_standard, 800_reband, 800_splinter, 900, 400_custom / obt), `bandplanBaseHz`, `bandplanSpacingHz`, `bandplanOffset`, `bandplanHighHz` |
| DMR | | `lcnTableHz` {LCN: Hz}, `dmrChannelsHz`, `colorCode` |
| NXDN | | `nxdnRate` (nxdn48 / nxdn96), `nxdnChannelsHz`, `ran`, `lcnTableHz` |

A site is **active** when it is enabled and has at least one control channel; a system is
active when it is enabled and has an active site. Active systems are numbered 0, 1, … in config
order, then the active conventional systems after them (`Call.system`, `Config::call_systems`),
and the active sites 0, 1, … system by system (`Call.site`).

An older config (each system one site, joined by `siteGroup` or by site locks naming one
system) is rewritten on load by `migrate::config`; the desktop app saves it upgraded
(`migrate::save`), the old file kept as `config.json.before-sites`.

### 10.5 `conventional[]`

| Key | Default | Meaning |
|---|---|---|
| `shortName` | `conv` | unique identity |
| `name`, `enabled` | "", true | |
| `squelchDb` | 8 | open threshold above the noise floor |
| `demod` | {} | analog: `fmDeemphasis`, `amLevel` (fast / slow), `amLowCutHz` (0–600), `squelchOpenMs` (0–500), `squelchHangMs` (0–1000) |
| `channelFile` | "" | CSV path; when set, `channels` is read from it |
| `channels[]` | [] | `{freqHz (required), mode: fm/am/p25/dmr/nxdn48/nxdn96, name, talkgroup, code, slot (DMR, 1/2), matchTalkgroup (P25/DMR/NXDN), description, tag, group, squelchDb, bandwidthKhz (analog, 4–20), enabled}` |
| `recording`, `plugins`, `pluginDelays`, `unitNames`, `keys` | | as for systems |

Its P25, DMR and NXDN channels name the talkgroups they hear by its own talkgroup file
(`<shortName>.talkgroups.csv`); their P25 receivers are the default bank.

`code` examples (bare): CTCSS `151.4`, DCS `D023N`, P25 NAC `293`, DMR colour code `1`, NXDN RAN
`5`. A combined string such as `CC1 TS2 TG201` or `NAC 293 TG 1` is split into `code`, `slot` and
`matchTalkgroup`. An older `tone` key is upgraded on load (P25 rows that shared a frequency with no
talkgroup get their old default written in). A call's number is the row's `talkgroup`, else the
talkgroup the air named, else the channel's kHz (then kHz×10 + k for more rows on the same
frequency).

### 10.6 `recording`

| Key | Default | Per system? | Meaning |
|---|---|---|---|
| `captureDir` | `~/TrunkRecorderPro` | | recordings folder |
| `prerollS` | 1.0 | | pre-roll for trunked calls |
| `maxRecorders` | 32 | | recorder pool size |
| `callTimeoutS` | 3 | yes | silence before a call ends |
| `conversationMode` | true | yes | off: a call per transmission (§6.5) |
| `recordUnknown` | true | yes | record talkgroups not in the file |
| `recordEncrypted` | false | yes | |
| `recordUnitToUnit` | true | yes | |
| `keepSilentCalls` | false | yes | |
| `minCallS` | 0 | yes | |
| `maxCallS` | 0 | yes | split longer calls (0 = at 600 s) |
| `minTransmissionS` | 0 | yes | drop shorter transmissions |
| `captureFrames` | false | | save vocoder frames as `.sdr` |
| `normalizeAudio` | true | yes | |
| `digitalLevelDb`, `analogLevelDb` | 0 | yes | ±40 |
| `archiveWav` | true | yes | keep the WAV once the plugins are done |
| `archiveM4a` | false | yes | keep an M4A of each call |
| `callLog` | true | yes | keep the JSON once the plugins are done |
| `archiveFilesOnFailure` | true | yes | |
| `filenameFormat` | "" | yes | "" = `<short>/<Y>/<M>/<D>/<tg>-<epoch>_<freq>` |
| `dropDuplicateCalls` | true | | multi-site dedupe |
| `vocoder` | `fixed` | | `fixed`, `enhanced`, `mbelib` (IMBE) |
| `m4a` | `{encoder: auto, bitrateKbps: 32}` | | auto / ffmpeg / afconvert / fdkaac / none |
| `ramSpool` | `{enabled: false, sizeMb: 256, dir}` | | see 8.6 |

### 10.7 Per-system `recording`

Any key marked "per system" above can be set in a system's or conventional system's own
`recording`. They are declared once, in the `recording!` macro's `per_system` block, which makes
`Recording`, `RecordingOverride` (each of them optional) and `Recording::with(override)` (the
global settings with what is set replaced) from the same list. Other keys there are dropped.
`Config::recordings()` resolves every system's rules once when recording starts (by the number
calls give it): the session's file name formats and the runtime's file rules.

### 10.8 `server`

`bind` 127.0.0.1, `port` 8080, `autoStart` false, `allowedOrigins` [], `interfaces` {} (by id:
`port`, `enabled` true, `access` "listen", `path` ""; a port of 0 gets the next free one on load;
the old list form `[{name, path}]` is read as this map, and `home` is dropped).

### 10.9 `log`

`level` info (trace, debug, info, warning, error, fatal), `console` true, `file` false, `dir`
"" (`logs` beside the config), `syslogFriendly`, `syslog`, `color`, `frequencyFormat` mhz
(exp, mhz, hz), `talkgroupDisplayFormat` id (id, id_tag, tag_id), `statusAsString` true,
`controlWarnRatePerS` 10.

### 10.10 From config to engine

`Config::engine_config(epoch_ms, &TalkgroupFiles)` builds trunk-core's `EngineConfig`, whose
`systems` are kept once for a whole system: each active system → a `SystemConfig` (short name,
`CallConfig`, `SaveRules`, its talkgroup file, unit tags; keys are set by the app) with its active
sites, each a `SiteConfig` (name, control channels, receiver bank, `expect`, protocol); then each
active conventional system → a `SystemConfig` with no sites and its own talkgroup file; enabled
channels → conventional channel rows naming their system's number; sources with resolved
centres; `prerollS`, `maxRecorders`, `captureFrames`, `dropDuplicateCalls`, `vocoder`. File names,
archive rules, M4A, spool, server, log, monitor and plugins stay in the platform.

`Config::call_systems` numbers every system as the engine does: active trunked systems 0, 1, …,
then active conventional systems (enabled, with an enabled channel). It is the one place that
number is worked out; the engine, plugins, status (`status.systemNames`) and the interface all go
by it. There is no limit on conventional systems.

### 10.11 Validation at Start (`Config::problem`, first failure wins)

1. No sources.
2. A file source without a path.
3. An RTL-SDR's `bufLen` not a multiple of 512 (or over 16 MiB), or more than 256 `buffers`.
4. Nothing to record (no active system, no enabled conventional channel).
5. A missing or duplicate short name, or two sites of a system with one name
   (`Config::name_problem`).
6. A source whose centre can't be resolved (auto-centre fails).
7. An active site's bad SmartNet band plan, DMR/NXDN carriers outside every source, or no
   control channel inside one.
8. One frequency in two conventional systems.
9. A channel frequency ≤ 0, or outside every source.
10. A code that doesn't parse for its mode.
11. An analog channel's `bandwidthKhz` outside 4–20, or a `demod` setting out of range.
12. A file name format with a token nobody knows (the config's, or a system's own).
13. Rows sharing a frequency that can't be told apart.

The interface runs this same code (section 13.3), so Setup says what Start will say.

**Auto-centring** (`resolved_centers`, `auto_center`): sources with `centerHz` 0 are placed, in
order, over the systems not yet covered. Start at the midpoint (rounded to 1 kHz), step to either
side in widening 12.5 kHz steps until no channel is within 25 kHz of DC (`off_dc`), and accept if
every frequency is inside half-width − 10 kHz. The first-run guide places sources for a system
found on the air with `plan_centers`, through the same `off_dc`.

### 10.12 Companion files (in the config's folder)

| File | Contents |
|---|---|
| `<short>.talkgroups.csv` | the system's talkgroup file (trunked or conventional) |
| `<short>.bandplan`, `<short>.<site>.bandplan` | learned P25 IDEN tables / DMR & NXDN channel maps, per site (`SiteKey::file_stem`); saved every 10 s on change and at stop |
| `<short>.units.csv` | talker aliases heard over the air (Trunk Recorder's unitTagsOTA format) |
| `conventional.heard.json` | codes heard per conventional frequency |
| `tune.json` | AutoTune's scores per source, for the next run |
| `radio/<short>.json` | the radio registry (units, talkgroups, affiliations); every 15 min and at stop |

The store keeps the talkgroup files (a line at a time, table `talkgroups`), band plans, talker
aliases, conventional codes, AutoTune's scores, sharing's state, the radio registry (each save's
changes, whole once a day) and the dashboard's history (`trunk-pro/src/kept.rs`,
`statstore.rs`); there are no files for them. Low-write mode keeps no call index. A store of
another schema version is set aside and made afresh (`db/schema.rs`).

| `stats/YYYY-MM-DD.jsonl` | a dashboard rollup per minute; gzipped after the day, kept 8 days (`files`; the store on: `stats_minutes` and `stats_hours`) |
| `plugins/<id>/` | installed plugins |
| `plugin-data/<id>/` | plugin state |
| `plugin-hold/` | finished calls held back by plugin delays |
| `interfaces/<id>/` | installed interfaces |
| `plugin-registry.json`, `interface-registry.json` | cached store indexes |
| `config.json.before-sites` | an older config as it was before the upgrade to sites |
| `logs/` | log files |

Talkgroup CSV (`<short>.talkgroups.csv`, beside the config): headed (`Decimal, Hex, Mode, Alpha Tag,
Description, Tag, Category, Priority, Preferred NAC, Preferred Site, Ignore`, any order) or Trunk
Recorder's headerless form. Channel CSV: `TG Number, Frequency, Code, Slot, Match TG, Mode, Alpha Tag, Description,
Tag, Category, Squelch dB, Bandwidth kHz, Enable` (`trunk-app/src/channels.rs`).

### 10.13 Example

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "00000101", "centerHz": 0, "rateHz": 2400000,
      "gainDb": 25.4, "agc": false, "ppm": 1, "autoTune": true, "guardHz": 75000 }
  ],
  "systems": [
    {
      "shortName": "county-p25", "name": "County Public Safety", "type": "p25", "enabled": true,
      "sites": [
        { "name": "site3", "controlChannelsHz": [851012500, 851262500], "modulation": "auto",
          "expect": { "nac": 659, "wacn": 781824, "sysId": 989, "rfss": 1, "site": 3 } },
        { "name": "site5", "controlChannelsHz": [852137500],
          "expect": { "nac": 659, "wacn": 781824, "sysId": 989, "rfss": 1, "site": 5 } }
      ],
      "recording": { "minCallS": 1, "recordUnknown": false },
      "plugins": { "openmhz": { "apiKey": "REPLACE_ME", "systemName": "countyp25" } }
    },
    {
      "shortName": "city-smartnet", "type": "smartnet", "enabled": true, "defaultMode": "analog",
      "sites": [{ "controlChannelsHz": [851637500], "bandplan": "800_reband", "expect": { "sysId": 18205 } }]
    }
  ],
  "conventional": [
    {
      "shortName": "county-fire-conv", "enabled": true, "squelchDb": 8,
      "channels": [
        { "freqHz": 852087500, "mode": "fm", "name": "FG North", "code": "151.4" },
        { "freqHz": 852087500, "mode": "fm", "name": "FG South", "code": "D023N" },
        { "freqHz": 851812500, "mode": "p25", "name": "PD Tac 2", "code": "293", "squelchDb": 12 }
      ]
    }
  ],
  "recording": { "captureDir": "/home/me/TrunkRecorderPro", "maxRecorders": 32, "archiveM4a": true },
  "server": { "bind": "127.0.0.1", "port": 8080, "autoStart": true },
  "plugins": { "openmhz": { "enabled": true } },
  "log": { "level": "info", "file": true }
}
```

The source auto-centres at 851.55 MHz: the span 851.0125–852.0875 MHz fits in ±1.125 MHz
and no channel is within 25 kHz of the centre.

### 10.14 Command-line flags that override config

`--config file`, `--port n`, `--bind addr`, `--start` (same as `autoStart`), `--log-level l`,
`--ui folder`, `--no-open`. Overrides are for the run only and are not saved.

---

## 11. Time, clocks and identifiers

The engine has no wall clock. Every time inside it (a frame, a message, a call's start, a timeout)
is a source's sample count divided by its rate, plus that source's clock offset. A capture
therefore decodes the same at any speed. The offset is the one link to the wall clock, and
`Session` keeps it for each live source with `ClockFit` (`trunk-app/src/session.rs`), which wraps
sdr-core's `SampleClock`: a phase-and-rate loop, so a source's status also reports `clockPpm`,
the radio's crystal error against the computer's clock.

### 11.1 The clock path

1. **The driver hands over a block**, stamped `at = Instant::now()` in the source thread (by the
   worker in the browser), before it waits in any queue. Time spent queued for a busy decoder
   never counts as clock error.
2. **`Feeder::push`** (`Session::push` in the browser): the source's sample count grows by the
   block's length.
3. **`ClockFit::observe`**: lag = (at − epoch) − samples ÷ rate. Buffers only ever arrive late
   (USB, driver buffering), so the least lag is the clock's own.
4. **1 s windows** (`SampleClock`): the first block sets the target at once (this absorbs the
   radio's open time, such as a USRP's FPGA load); after that, each window's least lag feeds a
   second-order loop that tracks the phase and the rate.
5. **Steer, each poll** (`Session::poll`, on the engine thread): the offset follows the loop. A
   jump of more than 5 ms held for 3 windows means samples were lost unreported (or invented):
   the offset is set at once and the log says "clock re-anchored". A deliberate gap (an RTL-SDR's
   retune halt, `Sink::tuned`) resumes the clock and doesn't count as loss.
6. **`Engine::set_clock_offset`**: the only place wall time enters the engine.
7. **`Source::time()`** = samples ÷ rate + offset: every frame and message is timed this way.
8. **Each `Trunk`'s `now_s`** (a site's clock) comes from its control channel's source
   (timeouts, call starts and ends, and every voice output of a block, whichever source its
   channel is on); the engine's own `now_s` from source 0. Multi-site matching compares these across
   sources, which is why their clocks must agree.
9. **Wall time** = `epoch_ms` (taken at start) + engine time. `Session` applies the local UTC offset
   for folder names.
10. **Gaps** (`Feeder::gap`, `Feeder::idle`; `fill_quiet` in the browser): samples a driver reports
    dropped (up to 10 s, `MAX_GAP_S`), and silence for a running source quiet for over 1 s
    (`QUIET_MS`), are fed as faint noise and counted as samples, so sample time keeps pace with the
    air. Calls on a dead source still time out. Conventional channels don't learn their noise floor
    from it or open on it.

A capture played as fast as it can be is not live: `ClockFit` ignores it and it keeps pure sample
time.

### 11.2 What the steering looks like (the page's simulation)

- **A steady radio.** Lags scatter upward from buffering; the least lag in each window sits on the
  true offset (for example 0.35 s, the radio's open time). The first block anchors the offset;
  it then settles on the first window's minimum.
- **Crystal drift.** Each window's target moves a few milliseconds; the offset slews after it at up
  to 1 ms per second, with no jumps in call times.
- **Samples lost, unreported.** 0.6 s of samples vanish at 30 s; sample time is 0.6 s behind the
  air. At the next window (40 s) the target is more than 0.25 s off, so the offset steps at once
  and the log says "clock re-anchored".

### 11.3 Identifiers

A system's identity is its `shortName`. The numbers below are per-run handles, never saved.

| Name | Indexes | Used by |
|---|---|---|
| Engine system index (`Call::system`) | active systems, trunked then conventional, from 0 | calls, events, live-audio frames, plugin `system`, `{sys_num}` in file names, `status.systems[].index` (`status.systemNames` maps them to short names) |
| Engine site index (`Call::site`) | active sites, system by system | calls, control channel events, `status.systems[].sites[].index` |
| A site's `name` | unique in its system | Preferred Site, call JSON `site`, control channel stats and band plan files (`SiteKey`) |
| A Setup card's key (`cardKey`) | every configured system and site, in this browser | the Setup cards (seeded from the short name, site name or position; never sent) |
| `shortName` | every system, unique | folders, files, plugin settings, what plugins key on |
| `CallId` | one sequence for every system | `call_num` in the JSON |

Frequencies are integer Hz (`u64`) in messages and `f64` Hz in config. TDMA/DMR slots are 0/1
internally and +1 only in display strings. Engine times are seconds of sample time; JSON times
are Unix seconds or milliseconds as the field name says.

---

## 12. The dashboard's measurements

Measured by parts kept out of the decoding code. A decoding part keeps plain counters and current
values in its own fields (no locks, no allocation, nothing on the sample path beyond an
increment) and lists them when asked, about once a second (`trunk_core::metrics`:
`Instrumented::report` into a `Sink`). Rates, history, storage and presentation belong to whoever
asks.

### 12.1 Pipeline

| Producer | Feeds | Output |
|---|---|---|
| `ControlChannel::report` (per protocol; default nothing), `Engine::report` (levels, ppm, calls), `SampleMeter` (headroom and clipping, 1 sample in 16), `Tally` (calls, airtime, reasons, voice errors, counted from events) | `Aggregator` | `stats` message every 1 s; a `Rollup` every minute |
| `Rollup` | `History` (in memory: a week, or a day with the store) and `stats/YYYY-MM-DD.jsonl` or the store | `statsQuery` → `statsResult` |
| Session events → `Stats::observe` | `Registry` | `radioQuery` → `radioResult` (`Shared::radio_query`, names from the `NameBook`); `radio/<short>.json` every 15 min and at stop |
| Session events → `Stats::observe` | `Monitor` | watchers (the hook for alert rules; none yet) and `monitorEvent` |
| monitor thread (desktop, every 2 s): the computer and the plugins | its own aggregator | `host` message every 2 s; rollups into the same history |
| `VoiceDecoder::quality()`, `ControlChannel::quality()` (`ChannelQuality`: eye opening, CQPSK phase error) | `Engine::channels()` | `rfDetail` / `decodeDetail`, only for subscribers |

### 12.2 Parts

- **`Sink`**: `counter(name, total)` is a running total (it may restart at 0 when a part is
  replaced, such as a retuned control channel) that the reader turns into a rate; `gauge(name,
  value)` is a current value. Names are `/`-separated and become series keys, for example
  `sys/<shortName>/cc/good` (a site with a name of its own:
  `sys/<shortName>/site/<site>/cc/good`); renaming one loses its history.
- **`Aggregator`**: rates from counters; each series' value now; per minute its average, lowest
  and highest. A rollup line is `{"t": minute, "s": {key: [avg, min, max]}}`.
- **`History`**: a minute grid per series in memory: a week with no store (the browser build,
  `files`); with one, 26 hours (`statstore::memory_minutes`). `statsQuery` takes names or
  prefixes ending in `*`, a range (10m, 1h, 6h, 24h, 7d, 30d, 90d, 1y) or from/to, and is
  answered downsampled (360 points by default) into a `Span`'s buckets. Within what memory keeps,
  from memory alone. Further back (the store on: `hello.capabilities` has `history`),
  `server::stats_query` adds the store's hours before the last day to memory's minutes since, on
  a grid of whole hours (1, 2, 3, 6, 12 h, a day, two, a week): a few hundred points a series
  whatever the span, so the history lives in the store, not in the recorder or the browser.
- **Kept (`statstore.rs`)**: with the store on, each rollup a row of `stats_minutes` (kept 2 days,
  read back into `History` at start) and, as each hour ends (two minutes late, for stragglers),
  each series' hour a row of `stats_hours` (`History::hour_rows`: avg, lo, hi, minutes), kept
  `storage.statsDays` (365; 0: all). `meta` `stats.hoursDone` is the last hour written; a start
  writes the hours memory has after it. With `files`: UTC days of JSON lines, appended and never rewritten, a
  finished day gzipped, days past 8 deleted. Either is read back in the background at start
  (`statsResult.loading` meanwhile).
- **`Registry`**: per system, bounded maps of talkgroups (5,000), units (20,000), unit pairs,
  frequencies and affiliations, with a week of hourly activity.
- **`Monitor`**: notable events (talkgroup or unit first seen, talkgroup active, call volume,
  control channel lost / regained (`MonitorEvent::ControlLost { system, site, … }`: the system's
  short name and the site's own name), source drops, clipping, disk and spool space). The last 200
  are sent when a connection is greeted (`monitorEvent` for each).
- **`NameBook`**: each system's talkgroups (as `engine_config` makes them) and talker aliases,
  by short name; the session keeps it current, the desktop app fills it at start, on config
  changes and talkgroup edits. Both builds answer `radioQuery` through it.
- **`stats::Shared`** holds the registry, the name book and the monitor so they outlive a
  recording session; the platform answers queries from it.

---

## 13. The browser build

The same `Session` and `Engine`, compiled to WebAssembly (`wasm32-unknown-unknown`, `simd128`,
wasm-bindgen), run in a Web Worker. The RTL-SDR driver is the same `rtlsdr-nusb` on nusb's WebUSB
backend. The worker speaks the same `FromRecorder` / `ToRecorder` messages as the desktop
WebSocket, so the interface code is shared.

### 13.1 Parts

- **Page (main thread)**: the React interface; `WorkerTransport` (starts the worker, forwards
  commands over `postMessage`, plays live audio, keeps the config in
  `localStorage["trp.config"]` and the talkgroup files in `localStorage["trp.talkgroups"]`,
  since workers have no localStorage); a screen wake lock while recording (phones and tablets
  freeze a tab once the screen sleeps).
- **Worker** (`web/src/web/engine.worker.ts`): owns `WebSession` and the `WebRtl` dongles. It
  greets the page as the desktop app greets a connection (`state`, `config`, `talkgroups`,
  `units`, `heard`, `history`, `survey`, `devices`, then `hello` with no `capabilities`), and
  answers a command it can't do with an `error`.
  - `pumpRtl`: `await rtl.next()`, then `session.push(source, bytes, dropped, wallMs())`.
  - Capture files: read in 128 KiB slices, paced to real time or not (the browser forgets the file
    on reload).
  - A 50 ms `setInterval` calls `session.poll`: protocol messages and audio frames go to the page
    (`{msg}` or `{audio, tg}`); call files, talker aliases, heard codes and (once a minute) the
    radio registry go to OPFS.
- **OPFS**: the origin's private file system; **Export to folder…** copies calls out
  (`showDirectoryPicker`).
- wasm exports: `WebSession`, `WebRtl`, `WebSurvey`, `WebProfiler`, and the config rules
  (13.3). Build: `cargo build -p trunk-web --target wasm32-unknown-unknown --profile dist` with
  `-C target-feature=+simd128` (`.cargo/config.toml`), then `wasm-bindgen --target web` into
  `web/src/web/pkg` (`npm run wasm`).

### 13.2 Desktop versus browser

| | Desktop | Browser |
|---|---|---|
| Threads | source and decoding threads, engine, finish pool, plugins, web server | one worker (`Session::push`, the single-threaded path) |
| Back-pressure | a 256-block queue before each source's decoding | none needed: the pump asks for the next block only after pushing the last, so a slow engine leaves USB transfers waiting and the dongle overflows (reported as `dropped`) |
| Transport | WebSocket and binary audio frames | `postMessage` |
| Config | `config.json`, talkgroup files beside it | `localStorage["trp.config"]`, `localStorage["trp.talkgroups"]` |
| Files | recordings and data folders | OPFS |
| Sources | RTL-SDR, USRP, Airspy, SoapySDR, files | RTL-SDR over WebUSB, picked files |
| `hello.capabilities` | all: `service`, `drivers`, `plugins`, `interfaces`, `host`, `keys` | none |
| Not available | | plugins, interfaces, M4A, channel files, `findRadios`, `listDir`, Trunk Recorder import from disk, key containers, `quit` |

### 13.3 The config rules in the interface

The interface doesn't write the config's rules again: it runs the recorder's own, compiled to
WebAssembly, in both builds (the desktop app's page too). `crates/trunk-web/src/rules.rs`
exports, over trunk-app's `config.rs`: `configNormalize` (a config as the recorder reads it,
an older one upgraded, with its `notes`), `configTalkgroups` (the talkgroup files an older
config held), `configProblem` (`Config::problem`), `configNameProblem`, `configCenters`
(`resolved_centers`), `autoCenter`, `planCenters`, `usableHalfWidth`, `channelsParse` /
`channelsWrite` (the channel CSV) and `bandplanProblem`. `web/src/rules.ts` loads them before
the page renders (`loadRules`) and keeps each answer for the store's config until it is replaced
(`configReplaced`). So the dashboard needs a browser with WebAssembly SIMD (Safari 16.4,
Chrome 91, Firefox 89 or later), and says so if it hasn't. Left in TypeScript on purpose: the
file name chip editor, the tone and talkgroup parsers used for inline hints, and the builders for
new cards and the Trunk Recorder import.

---

## 14. The interface protocol

One WebSocket, `/api/ws`: JSON text both ways plus binary frames for live audio.
`web/src/protocol.ts` is the source of truth; `docs/api/protocol.schema.json` is generated from it
(`npm run schema`). The Rust side is two enums in `trunk-app/src/protocol.rs`: `ToClient` (every
message, serialized as `{"type": <variant>, …}`, with `audience()`) and `Command` (every command,
read strictly by `Command::parse`, with `listen_may_send()`). `trunk-pro/src/protocol_tests.rs`
checks their variants against protocol.ts both ways and the messages against the schema. The
full reference is `docs/api/README.md`.

Every message to the browsers goes through `runtime::publish(hub, ToClient)`, which serializes it
once as each kind of connection may hear it (`runtime::Out::of`: for everyone, control access
only, or the config in two versions, the listen one without plugin settings and key sources).
Bodies still put together as JSON ride as a `Body`. A command that doesn't parse, or that the
connection's access doesn't allow, is answered with an `error`. `hello.capabilities` says what a
recorder can do beyond recording (`Capability`: `service`, `drivers`, `plugins`, `interfaces`,
`host`, `keys`), so an interface shows what works rather than checking which build it is.

### 14.1 A typical conversation

1. Client connects to `/api/ws` (the access guard runs first; see 14.5).
2. Recorder → the greeting, as the ordinary messages it sends later: `state`, `config` (with its
   `version`), `talkgroups` (each system's file), `units` (each system's saved aliases), `heard`,
   `history` (the last 300 calls), `radios`, `survey`, `monitorEvent` (each recent event), `host`;
   with control access also `devices`, `keys`, `plugins`, `interfaces`. Then `hello` (version,
   platform, api, config path, survey bands, capabilities): caught up. An interface waits for
   `hello` before treating something missing as missing.
3. Recorder → everyone: `status` (about every 500 ms while recording), `stats` (every second),
   `host` (every 2 s, desktop).
4. Client → `subscribe {topics: ["spectrum:0", "log"]}` (replaces the last; up to 64); recorder →
   `subscribed`; then topic messages to this client only.
5. Client → `listen {on, system, talkgroup}`; recorder → binary audio frames, filtered per client.
6. Recorder → `concluded {entry}` for each call, once its files are on disk.
7. Client → `statsQuery {id, series, range}`; recorder → `statsResult {id, …}` to the asker.
8. Client → `setConfig {config, version}` (always the whole config, edited from that version);
   recorder → `configSaved {version, error?}` to the sender, and `config` to everyone unless the
   version was stale. Client → `setTalkgroups {system, talkgroups}`; recorder → `talkgroups` to everyone.
9. Client → `start` / `stop`; recorder → `state` to everyone.

### 14.2 Who gets what

| Delivery | Messages |
|---|---|
| On connect | `state`, `config`, `talkgroups`, `units`, `heard`, `history`, `radios`, `survey`, `monitorEvent`, `host` (control access also `devices`, `keys`, `plugins`, `interfaces`), then `hello` |
| Every client | `state`, `status`, `stats`, `host`, `concluded`, `callFiles`, `monitorEvent`, `unitAlias`, `heard`, `config`, `talkgroups`, `survey`, `surveySpectrum`, `log` (the recorder's own warnings and errors, with their clock time and level), `quit` |
| Control access only | `plugins`, `pluginRuntime`, `interfaces`, `store`, `installProgress`, `keys` (`ToClient::audience`) |
| Topic subscribers | `spectrum` (`spectrum:<source>`, about 7/s, 512 bins), `log` (`log`: control channel lines and the rest of the recorder's log), `rfDetail` (`rf:<source>`, 1/s), `decodeDetail` (`decode:<shortName>`, 1/s: channels, each site's adjacent sites and DMR / NXDN detail, patches), per-core detail in `host` (`platform`) |
| The sender only | `configSaved`, `devices`, `radios`, `dir`, `trConfig`, `store`, `subscribed`, `statsResult`, `radioResult`, `callsResult`, `sourceProfile`, `error`, `notice` |
| Binary, to listeners | live audio frames |

Nothing is computed for a topic nobody watches. A client more than 4,096 messages behind on the
hub skips the oldest without notice.

### 14.3 Commands (client → recorder)

| Area | Commands |
|---|---|
| Recording | `setConfig`, `setTalkgroups`, `start`, `stop`, `quit`, `channelFile` |
| Watching | `subscribe`, `listen`, `statsQuery`, `radioQuery`, `callsQuery` |
| Setup | `devices`, `findRadios`, `listDir`, `readTrConfig`, `profileSource`, `surveyStart`, `surveyListen`, `surveyRescan`, `surveyStop` |
| Plugins and interfaces | `plugins`, `addPlugin`, `removePlugin`, `interfaces`, `addInterface`, `removeInterface`; their stores: `store {kind, refresh?}`, `install {kind, id}` or `{kind, repository, tag?}` (progress in `installProgress {kind, key, id, stage, message}`), `kind` `plugin` or `interface` |
| Keys | `unlockKeys {source, password}`, `lockKeys {source}` (key containers without a password file) |

### 14.4 Live audio frame

| Bytes | Type | Field |
|---|---|---|
| 0 | u8 | version `2` |
| 1–2 | u16 LE | engine system index this run (map to a short name with `status`) |
| 3–6 | u32 LE | call id |
| 7–10 | u32 LE | talkgroup |
| 11– | i16 LE … | samples, mono, 8 kHz |

### 14.5 HTTP routes and access

| Route | Serves |
|---|---|
| `/api/ws` | the WebSocket |
| `/api/version`, `/api/schema`, `/api/protocol.ts`, `/api/docs`, `/api/*` | version (and interface API), schema, TypeScript types, API docs and examples |
| `/api/v1/calls`, `/api/v1/calls/<path>`, `/api/v1/storage` | the call store as JSON (`docs/api/calls.openapi.yaml`), with listen or control access; 503 when the store is off |
| `/calls/<path>` | recordings (folder, then RAM spool), WAV ↔ M4A substitution, Range requests |
| `/store/screenshots/<sha256>`, `/interfaces/<id>/screenshots/<n>` | store listings' screenshots (built-in port only) |
| everything else | on `server.port` the embedded interface; on an interface's port, its folder (no hidden files) |

These are served on the built-in interface's port and, the same, on each other interface's
(`server.interfaces`, a listener each, kept by `interfaces::serve::reconcile` as the config
changes). The port a request came in on decides what a WebSocket may do (`access.rs`): control
on the built-in port; on an interface's, its `access` — `listen` may only `subscribe`, query,
page through calls and `listen`, gets no plugin, interface or key messages, and sees the config
without plugin settings and key sources.

Every request passes a guard. Programs that aren't browsers send no `Origin` and are let in. A
browser page must be the recorder's own origin or listed in `server.allowedOrigins` (which also
gets CORS headers); anything else gets 403. Bound to localhost, the `Host` must be localhost too,
which defeats DNS rebinding.

---

## 15. Where things live

| Concern | File(s) |
|---|---|
| Channelizer | `trunk-core/src/dsp/channelizer.rs` |
| Receivers | `trunk-core/src/dsp/{c4fm,cqpsk,msd,fm}.rs`, `smartnet/rx.rs` |
| In-band signalling | `trunk-core/src/dsp/inband/` (detectors, patterns, signals, paging tables), `trunk/inband.rs` (a call's), `trunk/recorded.rs` (what a call recorded) |
| P25 framer / FEC / bank | `trunk-core/src/p25/{frame,tsbk,voice,fec,diversity,phase2}.rs` |
| DMR | `trunk-core/src/dmr/` |
| NXDN | `trunk-core/src/nxdn/` |
| SmartNet | `trunk-core/src/smartnet/` |
| Vocoders, loudness | `trunk-core/src/mbe/`, `loudness.rs` |
| Engine, sources, recorder pool | `trunk-core/src/trunk/engine.rs` |
| Control channels per protocol | `trunk-core/src/trunk/control.rs` |
| Messages, P25 parsing (`P25Parser`) · typed P25 OSPs | `trunk-core/src/trunk/message.rs` · `trunk-core/src/p25/osp.rs` |
| Calls, `RecorderHost`, ids | `trunk-core/src/trunk/calls.rs` |
| Voice decoders | `trunk-core/src/trunk/{voice,tracker,tdma}.rs` |
| Site lock | `trunk-core/src/trunk/identity.rs` |
| Saving a call | `trunk-core/src/trunk/{record,frames}.rs` |
| Multi-site | `trunk-core/src/trunk/multisite.rs` |
| Conventional | `trunk-core/src/trunk/conventional.rs` |
| Config, its upgrades, channel CSV | `trunk-app/src/config.rs`, `migrate.rs`, `channels.rs` |
| Session, outputs | `trunk-app/src/session.rs` |
| Interface protocol (Rust) | `trunk-app/src/protocol.rs`; checked by `trunk-pro/src/protocol_tests.rs` |
| Threads, lifecycle, deliver, finish | `trunk-pro/src/runtime.rs` |
| SDR drivers | `trunk-pro/src/sdr.rs` (`Radio`, `run_with`), `trunk-pro/src/radio/` |
| Web server, access | `trunk-pro/src/server.rs`, `access.rs` |
| Logging | `trunk-pro/src/logging.rs` |
| Plugin host | `trunk-pro/src/plugins/` |
| Stores (plugins and interfaces) | `trunk-pro/src/store.rs`, `plugins/store.rs`, `interfaces/` |
| Plugin protocol and SDK | `trunk-recorder-plugin/src/` |
| Config rules for the interface | `trunk-web/src/rules.rs` → `web/src/rules.ts` |
| Interface protocol | `web/src/protocol.ts` → `docs/api/protocol.schema.json` |
