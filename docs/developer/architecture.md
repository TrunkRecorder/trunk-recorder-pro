# Architecture

How Trunk Recorder Pro is put together: the crates, the threads, and the path a sample takes from
the radio to a saved call, with the types and functions at each step. This is the canonical
text; [architecture.html](architecture.html) is an illustrated version with interactive diagrams,
which may lag it. Paths are relative to `crates/` unless they start with `web/` or `docs/`. Line
numbers are left out because they drift; names of types and functions are given instead.

For building, releasing and the code layout see [Development](README.md); for the config file's
keys, [Configuration](../configuration/README.md); for the interface protocol,
[api/README.md](../api/README.md).

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
| Driver | `rtlsdr-nusb` | Our fork of the RTL-SDR driver (USB transfer sizes), patched in for the whole workspace. | Yes |

`trunk-recorder-plugin` is used by trunk-core, trunk-app and trunk-pro with
`default-features = false`; its SDK is the feature `sdk`.

The rule that holds this together: **`trunk-core` and `trunk-app` never do I/O.** They take
samples plus a wall-clock time and return values (events, JSON strings, WAV bytes). The
platform crates own devices, threads, files and sockets. That is why the same recording logic
runs in a desktop process and in a Web Worker and gives bit-identical calls on the same capture.

| Crate | Main entry points |
|---|---|
| `trunk-core` | `trunk::Engine` (everything), `dsp::Channelizer`, `dsp::c4fm::C4fm`, `dsp::cqpsk::Cqpsk`, `dsp::fm::Nbfm`, `dsp::am::Am`, `dsp::filters` (RRC, windowed-sinc), `p25`, `dmr`, `nxdn`, `smartnet`, `ambe` (AMBE+2 codeword, shared by DMR, NXDN and Phase 2), `bits` (bit fields, CRC-CCITT), `mbe` (vocoders, `mbe::Profile`), `metrics`, `loudness`, `survey::Survey` |
| `trunk-app` | `Config`, `migrate` (older configs upgraded), `Session`, `Feeder`, `Output`, `protocol` (`ToClient`, `Command`), `stats` (dashboard measurements), `survey::SurveySession`, `filename`, `channels` (conventional CSV), `heard` (codes conventional frequencies carried), `keys` (key sources), `samples` (sample formats → IQ), `log::Record` |
| `trunk-pro` | `main.rs` (CLI; `tool.rs` + `toolkit.rs`, `snrtool.rs`, `dmrtool.rs`, `nxdntool.rs` analysis tools), `runtime.rs` (`Ctx`, threads), `server.rs` (HTTP + WebSocket), `access.rs`, `sdr.rs` + `radio/` (drivers), `plugins/` (host, store, manage, archive, encode, delay, hold, cli), `interfaces/` (other interfaces: store, serve, cli), `store.rs` (what both stores share), `db/` (the call store: schema, writer, reads, backfill), `spool.rs`, `unlock.rs` (key containers), `statstore.rs`, `monitor.rs` + `platform.rs` (the computer), `survey.rs`, `paths.rs`, `logging.rs` |
| `trunk-web` | `WebSession`, `WebRtl`, `WebSurvey`, `WebProfiler`; `rules` (the config's rules for the page) |
| `trunk-recorder-plugin` | `protocol` (`HostMessage`, `PluginMessage`, `Manifest`, `CallRecord`, topics), SDK (`Plugin`, `run`, `CallQueue`) |

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
| `stats-load`, `stats-store` | one each | Load/append stats rollups. |
| `db-writer`, `db-backfill` | one each | The store's one writer; bringing the store up to date with the capture folder. |
| `recent` | at start | With the store off: fill the live call list from the newest calls on disk. |
| `clock`, `update-check`, `store-sizes`, `share`, `tune`, `radio-open` | as needed | The clock checks (SNTP and the OS's time service); the daily look for a newer release; the store's sizes; sharing a system with the directory; a gain being tuned; a radio opening under its deadline. |
| `iq-capture`, `iq-rules`, `iq-send`, `iq-watch`, `iq-N` | while capturing | IQ captures and their rules. |
| `survey`, `survey-source-N` | while surveying | The first-run survey and its radio (on a spare radio when recording). |

The browser build has no threads: `engine.worker.ts` pulls blocks from `WebRtl::next()` and
calls `WebSession.push` / `poll` directly, and keeps call files in OPFS.

**Ctx** (`runtime.rs`) is the shared state between the web server and the recorder: the config
(and its version), the lifecycle, the call store, the plugins and interfaces, the hub, the
registry, name book and monitor (`Ctx.shared`, so they outlive recordings), the stats history and
the engine thread's commands. The `hub` is a `tokio::broadcast` channel. Every WebSocket task
subscribes to it, drops topic messages the client didn't `subscribe` to and control-only messages
a listen-only connection may not hear, and filters live audio by what it asked to `listen` to.

**Logging** (`logging.rs`) uses the `log` facade with Trunk Recorder's line format. It writes to
stderr, daily or 100 MB log files, and syslog. What the recorder logs also reaches the browsers as
`log` lines with their clock time and level: warnings and errors to every connection, the rest
only while someone watches the `log` topic. The decoder's own lines (`Output::Log`) reach the
topic from the session instead, so none is sent twice.

### 1.3 The recorder's lifecycle

`Ctx.lifecycle` (`runtime.rs`) is one `Mutex<Lifecycle>`: `recording: Option<Runner>`,
`survey: Option<SurveyRunner>` and `quitting` (a survey can run beside recording, on a spare
radio no source uses). Only `Ctx`'s methods change it, each holding the lock
throughout (`begin_recording`, `begin_survey`, `with_radios` for profiling a source,
`stop_recording`, `stop_survey`, `quit`), so two starts never interleave and nothing starts once
quitting. The phase the browsers see (`state`: idle, starting, running, stopping) is the engine
thread's report.

While recording, everything for the engine thread goes through one channel, `Ctx.engine`
(`Ctx::tell_engine`): `EngineCmd::Source` (a source's error or end, by way of its decoding
thread), `Topics` (what the browsers watch), `Talkgroups` (a talkgroup file changed) and `Keys` (a
key container unlocked or locked). The plugin host is cloned once per loop
(`Plugins::host()`), so a plugin reload never waits on the engine.

**Stop.** `Runner::stop` sets a flag and joins every thread. Each dsp thread first decodes what
its source had already queued; the engine then joins them, polls, runs `session.finish()`,
delivers the last calls, joins the finish threads, gives each plugin 10 s from when it has read
`shutdown` (after up to 30 s to work through its queue), and saves band plans, aliases, AutoTune
scores and the radio registry. Ctrl-C, SIGTERM and the interface's **Quit** all go through this
path.

**Survey** (`trunk-pro/src/survey.rs`, `trunk-core/src/survey.rs`) has the same shape: one source
thread (`runtime::spawn_source`) retuned on request, and one survey thread stepping through the
bands.

---

## 2. How samples flow from the SDR to a saved call

Stages in order. Thread in brackets.

1. **SDR driver** [`source-N`]. Each driver is a `Radio` (`sdr.rs`: `BACKOFF`, `retune_to`,
   `stream_once`), run by one loop, `sdr::run_with`, that reopens it after an error (RTL-SDR and
   HackRF 2 s, the others 3 s). `runtime::spawn_source` starts a source this way for recording, the survey and
   profiling. Produces a block of IQ:
   - RTL-SDR (`radio/rtl.rs`, pure-Rust `rtlsdr-nusb`, our fork in `crates/rtlsdr-nusb` with
     settable transfers, `RxBuffers`): by default 16 KiB USB transfers (8,192 IQ samples, about
     3.4 ms at 2.4 MSPS), 128 in flight (2 MiB); `bufLen` / `buffers` per source
     (`RtlConfig::rx_buffers`). The `Sink` gathers them into messages of about 25 ms (`GATHER`; about 120 KB at 2.4 MSPS). A retune
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
   - HydraSDR (`radio/airspy.rs` too, `Lib::Hydrasdr`; libhydrasdr is libairspy's fork, and what
     only it has, its device-info block and one gain call, is in `radio/airspy/hydrasdr.rs`):
     libhydrasdr 1.1's float IQ (`Block::Iq`), converted and decimated on its own thread, apart
     from the one draining the USB. Its `dropped_samples` is a running total; the driver passes on
     the difference per buffer.

   What every radio shares lives in `sdr.rs` and `radio/mod.rs`. A radio sends through a `Sink`,
   which never blocks a live radio: if the queue to the decoding thread is full, the block is
   dropped and counted, and the next block carries the count as `dropped`. A capture file's sink
   (`live: false`) waits instead. A radio's gain is one `Gain` (`sdr.rs`): `Agc`, `Db`, `Stages`
   (sdr-core's `gain::Stage` names, each in the radio's own count) or a `Preset` step of the
   radio's own table; there is no per-radio gain type. `radio::open(key,
   deadline, f)` runs every open and driver search on a thread of its own under `opening()`
   and gives up at the deadline (5 s RTL-SDR, HackRF, Airspy; 15 s bladeRF, SoapySDR; 30 s USRP),
   with "isn't answering" and the device refused until the stuck open ends, so one wedged radio
   doesn't hold up the others. `radio/priority.rs` makes the source thread and the threads its
   driver starts real time (Mach time-constraint on macOS, `SCHED_RR` on Linux, needing
   `CAP_SYS_NICE`; a refusal is logged once with the `setcap` fix). `StreamCheck` watches raw-byte
   radios (RTL-SDR, HackRF) for a stuck stream, the same bytes repeating or a rate over 1.5× the
   nominal for 2 s, and reopens. A transfer refused for memory names Linux's `usbfs_memory_mb`.
   - bladeRF (`radio/bladerf.rs`, libbladeRF loaded at run time): samples come through the
     synchronous interface with metadata, so overruns show as gaps in the sample timestamps and are
     reported as dropped.
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

Latency, roughly: driver block time (RTL about 25 ms, others about 10 ms) + up to one
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
| Conventional FM / AM / auto | 7,000 Hz, widened or narrowed with the channel's `bandwidthKhz` (`AnalogTuning::head_cutoff_hz`: its passband + 1.5 kHz) |

A trunked voice channel opens at `VoiceKind::cutoff_hz()`; a conventional channel's widths
(detector band, head cutoff, carrier meter) are worked out once per frequency by
`ChannelShape::of`, a digital one's head as a trunked channel of its kind, an auto one's as its
analog receiver's (the digital receivers' matched filters take the wider band).

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

`dmr::gps` reads embedded GPS Info LC (FLCO 8, standard or Hytera FID)
and emits `TrackerOut::Position` for the slot's recently named talker, through
the same call-position handling as P25. See [DMR GPS](protocols/dmr-gps.md)
for wire format, sources and scope.

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


**Modes.** A channel's `ConvMode` is the decoder it opens (`ConvMode::kind`), and what a row of
it can say follows from that decoder's receivers (`VoiceKind::receivers`): analog or not, an FM
receiver (tones), an analog one (bandwidth, `demod`), a talkgroup to match, a slot
(`ConvMode::slotted`), and what tells its rows apart (`code_name`; none for AM and auto). Setup
asks the same methods through trunk-web (`convModes`), and reads a row's code with
`Access::parse` (`accessParse`).

**Auto** (`voice::Auto`, mode `auto`): the five receivers decode the same IQ and their outputs
are held. Each output scores by `TrackerOut::evidence`: a voice frame 1, a checked message (talker,
cipher, position…) 9. A digital receiver reaching 18 is settled on (10 when the channel closes
first); otherwise analog is, 2 s after the channel opened. Then the settled receiver's held
outputs go on, the others are dropped, every `VoiceDecoder` call goes to it alone, and the call's
start is when that receiver's voice began. Until then `carrier()` is the FM receiver's;
afterwards the settled one's, or the carrier meter for a digital one. The channel reports its mode
as `auto` until it settles (`Chan::mode`); an undecided channel that closes is flushed so a
short key-up still settles.

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

**Saving a call.** `record::save_call` turns a finished call (a `Held`) into a `Concluded`:
- it leaves out transmissions shorter than `minTransmissionS`, and encrypted ones that weren't
  decrypted (`Transmissions` marks each), trimming the vocoder frames and error list to match
  (`CallFrames::keep`);
- it refuses a call that is too short or silent (`SaveRules`), except an encrypted one when
  `recordEncrypted` keeps it;
- it normalises loudness (`loudness.rs`, −12 dBFS speech, about −14 LUFS; gain clamped
  −18..+24 dB; look-ahead limiter at −2.5 dBFS; `normalizeAudio`) and applies the digital /
  analog level;
- it makes the call's record (`record::call_record`): a `CallRecord`, the plugin SDK's type, with
  Trunk Recorder's fields plus `signal`, `noise`, `snr` from `Reception`, `clean_voice_pct`,
  `errorList`, `freq_error`. The same typed record goes to the file, the call store and the
  plugins; the finish thread serialises it once (`CallRecord::to_json`), byte for byte as Trunk
  Recorder's files have it (flags as 0 / 1).

The engine only gathers the system's rules and names for it. `wav.rs` writes 16-bit mono 8 kHz.

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

`trunk::Engine` owns everything above the radio. It is a handle (clones share one engine), so
each source can be fed from a thread of its own while another reads events and status. Its config
is an `EngineConfig`: the sources (kept once, as one `Arc<[SourceConfig]>`), the systems
(`EngineConfig.systems`, trunked and conventional), the conventional channels, and the settings
every system shares (pre-roll, `maxRecorders`, the vocoder). Its parts:

- **`Radio`** is shared by all systems: the sources, each behind a lock of its own (`SourceRx`),
  the recordings and the recorder pool (section 6.1).
- **`System`** is one per system, trunked or conventional (a `SystemConfig`): what it decides
  about its calls (talkgroups, call and save rules, unit names, keys), kept once for all its
  sites, its radios' talker aliases, and its saved calls. A trunked system lists its sites
  (`SystemConfig::sites`); a conventional one has none, and its channels name it
  (`ConvChannel::system`). `Engine::set_talkgroups` and `set_keys` change a system's for every
  site at once.
- **`Trunk`** is one per site of a trunked system (a `SiteConfig`, which holds only the site's
  own fields: its name, control channels, receivers, site lock, protocol). Its system's rules are
  handed to it where it needs them, never copied. It knows nothing of any protocol: its
  `ControlChannel` (`control.rs`) decodes that, and the `Trunk` does what every protocol shares:
  listening as the protocol's `CarrierPlan` says, the site lock (`identity.rs`), AutoTune, the
  site's clock, its `CallManager` (`calls.rs`, à la Trunk Recorder's `monitor_systems.cc`;
  standing patches in `patches.rs`), and the adjacent sites (`Neighbours` in `message.rs`, shared
  with the survey).
- **`Conventional`** (`conventional.rs`) covers energy-detected channels and their calls
  (section 4.7).
- **`MultiSite`** (`multisite.rs`) holds the copies of one call heard on several sites of one
  system (section 6.6).

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
| EDACS | every carrier of the site, watched | `edacs::site::Site`: the control channel wherever it is, and each LCN's frequency checked (and learned) by which carrier keys when a call is assigned it | site |

`CarrierPlan::Hunt` (P25, SmartNet) listens to one control channel at a time and moves through
the list. `CarrierPlan::Watch` (DMR, NXDN, EDACS) opens a head on every listed carrier at once. The
watched protocols each implement `WatchedSite` (`control.rs`), and one generic `Watched<S>` is
their `ControlChannel`; `SystemKind::info` (`trunk-app`'s config, generated into
`web/src/systemKinds.gen.ts`) says what each kind of system is.

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

### 5.8 Adding a protocol

A new trunking protocol touches these places (NXDN, the latest, is the
example to follow):

1. **Decoding:** a module of its own (like `nxdn/`): receiver, framer, and a
   parser that produces `trunk::Message`s.
2. **Its control channel:** a `ControlChannel` implementation in
   `control.rs` (`name`, `plan`, `push`, `counts`, `blocks`, `offset_hz`,
   `modulation`, `status`, `quality`; the rest have defaults), or, for a
   protocol that watches every carrier of its site, a `WatchedSite`
   implementation (DMR, NXDN and EDACS do). It sets the `CarrierPlan` and the
   identity fields its site lock can use. If the dashboard shows more of
   it, a `ProtocolStatus` variant, its JSON in the `decodeDetail` sites
   (`session::decode_sites`), and its type in `protocol.ts`.
3. **Wiring it in:** a `Protocol` variant, and its line in `control::build`.
4. **Its voice:** if the voice is new, a `Voice` variant (`calls.rs`, which
   its parser sets on a grant's `Message.voice`), a `VoiceKind` (its arm in
   `Voice::kind`, its width in `VoiceKind::cutoff_hz`), its `VoiceDecoder`,
   and its line in `voice::build`. The compiler then points at each `match`
   on `Voice` that needs an arm (`Voice::slot`, `decryptable`, the frame
   capture in `record.rs`, conventional's `heard_code`).
5. **Its site's identity:** if the site states facts no protocol states, an
   `IdField` for each.
6. **The config:** a `SystemKind` (`type`) in `trunk-app`'s config and Setup
   (its `SystemKind::info` line, which is generated into
   `web/src/systemKinds.gen.ts`), its arm in `Config::engine_config`, and its
   `kind` in `Config::call_systems` (what plugins see).

The call manager, multi-site, recording and plugins don't change.

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
| `Auto` | Each of `VoiceKind::Auto.receivers()` (`Analog`, `Fdma`, `Dmr`, `Nxdn(N48)`, `Nxdn(N96)`) on the same IQ, until one is settled on (section 4.7) |

### 6.4 While recording

Decoder outputs (`VoiceOut { slot, out: TrackerOut }`) are routed to the call on their slot and
queued in `radio.pending`, then applied after the block (`apply_pending`), at the site's
control channel clock, so a voice channel never borrows a `Trunk` mutably. Each output finds its
site, recording and call once, then:
- `Audio` / `AnalogAudio` → appended to the `Recording`, `last_audio_s` moved, live
  `Event::Audio` if this site leads the talkgroup's live audio (`MultiSite::audio_lead`);
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
(`Span::starts_with`) is linked into one conversation, from the first grant, against the copies
`MultiSite` already has. A conversation has one copy from each site (`Conversation::takes`), so copies never
chain one into the next. Every call goes through `MultiSite`, which alone tells it to the world:
`CallStart` when the first copy begins, `CallEnd` when the last ends, `CallUpdate` when what's told changes, all
under the first copy's id and as one view of its copies (`Conversation::view`). `CopyStart` / `CopyEnd`
are each site's own, for the log and voice channel counts. **Every copy is recorded.** Live audio comes from one site per talkgroup until it has
been quiet 0.5 s, whether or not the sites' calls are linked. When the last copy ends, they're tried best first: the talkgroup's preferred site
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
- **One bounded queue in front of each source's decoding; a live radio never waits on it.**
  `sync_channel::<SourceMsg>(256)` per source. If a source's decoding is slower than real time,
  its queue fills (about 6.4 s of RTL data at 2.4 MSPS in ~25 ms messages, about 2.5 s for the
  ~10 ms blocks of USRP, SoapySDR and bladeRF). A live radio's `Sink::put` then drops that block
  and counts its samples as `dropped`, told with the next block that does get through, so the
  driver keeps collecting and the device's buffer isn't what overflows. A capture file's sink
  (`live: false`) waits: nothing is lost, it just runs slower. The feeder fills dropped samples
  with faint noise (`push_gap`, up to 10 s) so sample time stays in step with the air. Loss nobody
  reports is caught by the clock fit: if the offset stays beyond 5 ms for 3 windows, the clock is
  re-anchored and the log says so.
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
| A source's decoding (CPU can't keep up) | Its queue fills → its radio's sink drops blocks and counts them → `dropped` in status, gap filled, calls carry on with a hole; the busiest source's load is shown on the dashboard. Other sources decode on their own cores. |
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
| Back-pressure | propagates block by block to the source | one bounded queue per source at the input (a live radio's block is dropped and counted, a file waits); downstream queues never block decoding |
| Overload | hardware overflow; any block can stall all | hardware overflow, measured, gap-filled, clock re-anchored |
| Multi-core | automatic | a core per source for DSP (calls followed under one lock); I/O, encoding, plugins and the server on other threads |
| CPU | — | measured 4–6× less than Trunk Recorder on Raspberry Pi / Linux (2026-10-02 benchmark) |

---

## 8. Plugins

### 8.1 Model

A plugin is a **separate executable** that only watches; nothing it does changes what is
recorded. It speaks JSON lines: `HostMessage` on its stdin, `PluginMessage` on its stdout, and
anything on stderr becomes a log line. Lines are read lossily (bytes that aren't UTF-8 become
U+FFFD), so a stray byte never stops the reading.

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

**Delays** (`delay.rs`, `hold.rs`; `pluginDelays`). What a plugin is to have late goes to the
hold instead of its queue, and is given to it when due (a finished call is kept on disk until
then, across restarts).

### 8.3 Conversation

1. Host → `hello` { api, host, config, systems [ {index, short_name, kind, name?, fcc? { "<Hz>": {call_sign, name?} }, fcc_call_sign?, fcc_name?, sites? [ {name} ], config, settings} ],
   capture_dir, data_dir, audio_formats }. `index` is the system's number this run (from 0,
   trunked then conventional); `kind` says which, `conventional` included; `settings` are the
   plugin's own settings as a system's `override` changes them. A plugin asks for recorded
   calls with `calls.query` (answered `calls`, by request number), not by opening the store.
2. Plugin → `ready` (or `status: error` and exit 78 for a config error).
3. Host → events for subscribed topics: `call.start`, `call.update`, `call.end`, `call.concluded`
   (with json / wav / m4a paths), `unit`, `audio`, `voice`, `status` (every 5 s), `totals` (every
   10 s), `tone_pattern` and `log`.
4. Plugin → `log`, `status` { ok | warning | error }, `metrics`, and `call.result` { path,
   ok | skipped | failed, message, url } for each `call.concluded`.
5. Host → `shutdown` { grace_s } behind whatever is queued; stdin closes after it. The plugin
   gets its grace from when it has read it (after up to 30 s to drain), then is killed.

### 8.4 Supervision

- Restart with back-off 1 s doubling to 60 s (reset after a run longer than 60 s).
- Exit code 78 (config error): not restarted, and calls stop waiting on it.
- Changed settings restart the plugins; new processes start once old ones have exited (they share
  data folders); events wait meanwhile. The engine thread takes the host once a turn
  (`Plugins::host`, an `Arc` clone), so a reload never waits on it; an old host is shut down once
  nothing holds it.

### 8.5 Archive

For each call its finisher calls `archive.expect(rel, takers, rules, …)`, where takers = live
processes subscribed to `call.concluded`, and says whether a WAV was written. Each `call.result`
counts down. At zero the call's `archiveWav`, `archiveM4a`, `callLog` and `archiveFilesOnFailure`
rules are applied (delete, or move out of the RAM spool): `ok` and `skipped` count the same, and
only a `failed` keeps the files (with `archiveFilesOnFailure`). An unsettled call counts as failed
after 1 h. With no takers, `archive::plan` writes only what the Keep rules keep, and the call
settles at once.

Each `ok` times the delivery (`timing.rs`, #61): the call's age at delivery, split into held,
waiting and the plugin's own upload (`took_ms`, when it says), per plugin as running totals
(`plg/<id>/ageMs`, `heldMs`, `waitMs`, `uploadMs`, `delivered`, `uploads`) and each tick's longest
(`age`, `upload`). It's the same for every plugin, so none measures it.

**WAV only when needed.** A call's `Plan` (`plugins::plan`) is the one place that says which of
its files are written. The WAV goes to the encoder on stdin (afconvert gets a temporary file), so
it's written only when it's kept, a plugin takes WAV, or encoding failed; `call.concluded`'s `wav`
path may not exist for a plugin that asked for M4A.

**Folders** (`folder.rs`). Every file of a call goes into the recordings folder (or the spool)
through its `Folder`: `write`, `prepare` (an encoder's output) and `move_in` (out of the spool).
Each is also checked with a file of its own (`probe`: written, synced, deleted) at startup and when
its path changes (`apply_config`, on a thread of its own, since a network share can hang); after
that the calls' own writes are the check, and the first one that works says it's back. Checks and
writes report to the same state, so the first failure is logged once with why (an io::Error turned
into permission and who owns the folder, macOS privacy, read-only, full, not a folder, a drive not
mounted) and becomes a `folderUnwritable` event; the files not written are counted (logged at most
once a minute); and `folderWritable` is sent when it takes files again. The folder is made only
when it's never been there at that path: a drive that goes isn't replaced by a folder on the disk
beneath. A call the spool can't take goes to the recordings folder. `host.platform.folders`
carries the state (the dashboard's light, Setup's field).

### 8.6 RAM spool

With `recording.ramSpool.enabled` (`spool.rs`), files that will be deleted once the plugins are
done (and the .m4a made only for them) are written to a RAM disk instead, to save SSD wear: on
macOS one the app makes with `hdiutil` (mounted `nobrowse` at `<data>/spool`, kept across restarts
so queued retries still find their files), on Linux a folder in /dev/shm. Kept files go to the
recordings folder directly. `Archive` moves a failed upload's files there; the monitor moves
anything older than 2 h there too, and reports the spool's room (a `spool` disk row, `spoolLow` /
`spoolFull` / `spoolRecovered` events). With no room, calls go to the recordings folder. Gone
while running (Linux: systemd's `RemoveIPC` at logout) it's made again (`Spool::revive`), with a
`spoolRemoved` event and the row's `removed` / `missing`. Turned off, the spool is emptied into
the recordings folder and removed.

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

The Rust SDK (`Plugin`, `run`, `CallQueue`, `TalkgroupFilter`, the form hints in `schema.rs`,
`testing.rs`) is described for plugin authors in [Writing plugins](../plugins/writing-plugins.md),
and every type on [docs.rs](https://docs.rs/trunk-recorder-plugin).

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
`Source(SourceMsg)` (an error or end), `Topics(Topics)`, `Talkgroups { system: SystemId, rows }`,
`Keys { system: SystemId, ring }` (and `Talkgroup`, `UnitNames`, …).

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
| `CopyStart` / `CopyEnd` | `Call` | one site's copy of a call beginning or ending |
| `Audio` | `call_id`, `system`, `talkgroup`, `samples: Vec<f32>`, `decrypted` | live audio, 8 kHz |
| `UnitAlias` | `system`, `unit`, `alias`, `talkgroup` | talker alias heard |
| `UnitPosition` | `system`, `unit`, `lat`, `lon`, `talkgroup` | a radio on a call gave a new GPS position |
| `CallSign` | `system`, `freq_hz`, `call_sign` | a frequency is identified by an FCC call sign |
| `TonePattern` | `PatternHeard` | a call's tones made a pattern |
| `Concluded` | `Concluded` | a call to save |
| `NotSaved` | `record::Unsaved` (`call`, `why`, …) | too short / silent |
| `Duplicate` | `call`, `kept` | another site's copy kept |
| `ConvSkipped` | `freq_hz`, `code` | conventional traffic no row took |

Conventional channels report with these same events.

**`Status`**: `now_s`, `sites: Vec<SiteStatus>`, `active_calls`, `recording`,
`channels_open`, `conventional_open`, `calls_concluded`, `systems: Vec<SystemStatus>`,
`sources: Vec<SourceReport { error_ppm, applied_ppm, seeded, voice_channels }>`.

**`SiteStatus`**: `site`, `system`, `short_name`, `site_name`, `now_s`, `control_channel_hz`,
`identity`, `good`, `bad`, `modulation`, `active_calls`, `recording`, `calls_concluded`,
`mismatch`, `adjacent: Vec<AdjacentSite { sys_id, rfss, site, freq_hz }>`, `patches`,
`protocol: ProtocolStatus`. The session's `status` message groups them by system, with a summary
of each site; `adjacent`, the DMR and NXDN detail and `patches` go out in `decodeDetail`.

**`ChannelSnapshot`**: `system`, `site: Option<u16>`, `freq_hz`, `source`, `kind: ChannelKind`
(`Control` | `Carrier` | `Voice` | `Conventional`), `power_db`, `noise_db`, `offset_hz`,
`quality: ChannelQuality`, `calls`.

### 9.5 Session output (`trunk-app/src/session.rs`)

`Session` wraps the `Engine` and is the only thing the recorder, desktop or browser, talks to (the
`replay` CLI too: a headless `Session` over file sources):

- `feeder(i)` hands out source `i`'s `Feeder`, which the desktop app's `dsp-N` thread pushes
  samples into (`Feeder::push` / `push_iq`; `idle` fills a quiet spell with silence). `push` /
  `push_iq` feed samples from the caller's own thread (the browser build). `source_error` /
  `source_ended` report source trouble.
- `poll(now_ms, &mut Vec<Output>)` drains the engine's events and turns them into `Output`s
  (below); `finish(&mut out)` ends every call (stop / end of input).
- `set_talkgroups` applies a system's talkgroup file edited while recording; `set_keys` its keys.
  `attach` hands it the platform's `stats::Shared` (registry, `NameBook` and monitor). A
  `radioQuery` is answered by `Shared::radio_query`, named from the `NameBook`, which the session
  keeps as its engine has the talkgroups; both builds answer this way.
- Its persistence hooks are in section 10.7.

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

The fields are `CallRecord`'s, in its order: the finisher writes `<rel>.json` from it once
(`CallRecord::to_json`), flags as 0/1. Every field is described in
[The call JSON](../guides/recordings.md#the-call-json).

### 9.7 Plugin protocol (`trunk-recorder-plugin/src/protocol.rs`)

`HostMessage` (host → plugin) and `PluginMessage` (plugin → host), both tagged by `type`, and the
`Manifest` a plugin prints for `--describe`; `API_VERSION` = 1, `EXIT_CONFIG` = 78. Every message
and field is in [Writing plugins](../plugins/writing-plugins.md#how-a-plugin-runs) and on
[docs.rs](https://docs.rs/trunk-recorder-plugin).

---

## 10. The configuration file

Every key, its default and what it does is in [Configuration](../configuration/README.md):
[sources](../configuration/sources.md), [trunked systems](../configuration/systems.md),
[conventional systems](../configuration/conventional.md), [recording](../configuration/recording.md),
[server and log](../configuration/server-and-log.md) and [plugins](../configuration/plugins.md).
This section is how the code reads it, turns it into the engine's config, checks it and keeps what
it learns. The command-line flags that override it for one run are in
[Command line](../command-line.md#running-the-app-serve).

### 10.1 Reading and saving it

- `--config file.json` picks another file. Its folder becomes the data folder, holding every
  companion file, so two configs in different folders share nothing.
- Versioned: `Ctx.config_version` goes up by one at every change (`Ctx::config_changed`, which
  also tells every browser `config { config, version }`). `setConfig { config, version }` always
  sends the whole config with the version it was edited from; one edited from an older version
  is refused (`configSaved` with an `error`), so another browser's change is never saved over.
  Saved through `write_atomic` (temporary file, `sync_all`, rename).
- A system's talkgroups (trunked or conventional) are not in it: they are rows in the store
  (`NameLists`, by `SystemId`), replaced by `setTalkgroups` and taken by a recording at once
  (`EngineCmd::Talkgroups`). A `<shortName>.talkgroups.csv` beside it is only imported once
  (`talkgroups_path`, migration). An older config's
  `talkgroupsCsv` / `talkgroupsName` are moved out into those files before it is read, by every
  reader (`Config::load` → `migrate::move_files_out`, which writes an older config's `unitNames.csv` out beside it too; `migrate::files_into_store` then takes both into the store). The browser keeps them in
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
- In code a `Source` (`trunk-app/src/config.rs`) holds the keys every type has plus a
  `kind: SourceKind` (flattened, tagged by `type`) with the rest; the JSON stays flat.

### 10.2 Active systems and sites

A site is **active** when it is enabled and has at least one control channel; a system is
active when it is enabled and has an active site. Active systems are numbered 0, 1, … in config
order, then the active conventional systems after them (`Call.system`, `Config::call_systems`),
and the active sites 0, 1, … system by system (`Call.site`).

An older config (each system one site, joined by `siteGroup` or by site locks naming one
system) is rewritten on load by `migrate::config`; the desktop app saves it upgraded
(`migrate::save`), the old file kept as `config.json.before-sites`.

### 10.3 Per-system `recording`

Any key marked per system in [Recording](../configuration/recording.md#per-system-rules) can be set in a system's or conventional system's own
`recording`. They are declared once, in the `recording!` macro's `per_system` block, which makes
`Recording`, `RecordingOverride` (each of them optional) and `Recording::with(override)` (the
global settings with what is set replaced) from the same list. Other keys there are dropped.
`Config::recordings()` resolves every system's rules once when recording starts (by the number
calls give it): the session's file name formats and the runtime's file rules.

### 10.4 From config to engine

`Config::engine_config(epoch_ms, &NameLists)` builds trunk-core's `EngineConfig`, whose
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

**Edits while recording.** The recorder keeps the config a run started with (`Ctx.run`; the
browser build's worker keeps its own), and what its radios have taken since. An edit is held
against it, not against the edit before: `Ctx::live_sources` finds each source's radio in the run
by its label (`Config::same_radio`), wherever Setup has moved it, sets a gain the radio takes live
(`live_gain`), and the run then has it. `Config::needs_restart` compares what a run reads only
when it starts — the sources as their radios take them (without a `name`), the systems and
conventional systems without what applies live (plugins and delays, sharing and its link, FCC
call signs, a channel file's status), the recording rules — and the `state` message carries the
verdict as `restartNeeded`, with the phase (`idle`, `starting`, `running`, `stopping`). The
interface shows it; it doesn't work it out.

**Tuning a gain while recording** (`tuneGain`; `trunk-app/src/gain.rs`, `trunk-app/src/tune.rs`,
`trunk-pro/src/tune.rs`). `GainPlan::of` is each radio kind's gain as something to search: its
axes (a table step, the overall gain, or stages), the moves in order (front of the chain first, the
VGA last, a HackRF's amp gated on its LNA) and the ranges config validation and Find my system
read too. A tuning thread walks the plan (`Walk`), comparing each candidate with the best so far by
turns; each candidate goes on the radio through `Ctx::trial_gain` (the radio's `Control`, as a
live edit, but `Ctx.run` and the config keep what they had, so putting it back is setting the run's
gain again). Each second's `stats` values reach it from `deliver` (`Slot::stats`), and the systems
it scores are the engine's sites on that radio (`SiteStatus::source`, asked with
`EngineCmd::Sites`). `apply` keeps the best: the run has it, and `edit_config` saves it. Stopping
recording, or an edit of that radio (`Slot::edited`), stops the tuning.

### 10.5 Validation at Start (`Config::problem`, first failure wins)

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

### 10.6 Auto-centring, pins and advice

**Auto-centring** (`trunk-app/src/place.rs`): one placement for Start's auto centres
(`resolved_centers`), the first-run guide and the survey's suggested centre. Sources with
`centerHz` 0 are placed in order (`place_sources`), each over the window (`busiest_window`) that
takes in the most of what the ones before it don't reach: what a group needs and isn't reached yet
outweighs everything — one of a P25 or SmartNet site's control channels, or (`Group::all`) every
control and watched frequency of a DMR, NXDN or EDACS site and every channel of a conventional
system — then each channel counts once (the guide: by the calls seen on it); of windows taking in as much, the one that leaves the
rest the fewest windows. So a site wider than one radio gets every radio it needs. With
everything reached, a spare radio goes alongside the others, on the side with more of the
channels. Each centre is then centred on what it reaches and stepped to either side in widening
12.5 kHz steps until no channel is within 25 kHz of DC (`off_dc`).

**Which source receives a frequency.** A system or a conventional channel may be kept on
particular sources (`receiveOn`, by `Source::label`: the radio, not its place in the list;
`Config::pins` turns it into the engine's `Pins` by position, none when the radio named is gone,
which `problem` reports). `Pins::first` is the one choice: the first source allowed that reaches
the frequency. The engine opens channels by it (`Radio::source_for`, `Conventional::new`), and
the app asks the same of the centres Start places (`place::receiver`): placement's targets are
groups (a site, a conventional system's channels on the same sources) carrying their pins, so a
source is placed only for what it may receive; `Config::receivers` says the source of each
listed frequency (Setup's coverage chips and to-dos, `configReceivers`); and `place::coverage`
is the one account of what's received: per group whether its need is met and what's missed, per
source how many listed frequencies it's the first to reach. `problem` reports what it misses
(`Config::placed`: the sites' groups come first, in `active_sites` order), and `Config::placement`
reads its counts. A source at 0 that has opened no voice channel this run is `unused` in its status,
and the dashboard says it can probably be removed.

**Advice** (`place::advise`, `Config::advice`; research/source-advice.md): better centres and rates
than the sources have, by re-running `place_sources`. A source's `Choices` say whether its set centre
may move (not a capture file's) and its rates (`rates::offered`: its radio's, else its kind's from
`rates::of_kind`, the one list the web's `SOURCE_KINDS` reads too). Sources are released to auto
(none, each alone, all) and each one's rates tried in turn; ways are compared by `Score` (needs met,
weight reached, radios needed, past the dependable rate, total rate, changes), and only a strict
improvement is advice. `Config::targets_with` weighs it by what's heard (the registry's calls per
frequency, out of band too). Setup's Radios tab and the RF page show it (`web/src/sourceAdvice.tsx`);
`problem` appends the settings that reach what it found missed (`advice_fix`).

### 10.7 What is kept between runs

- `config.json` is written by `setConfig` (the interface always sends the whole config, with the
  version it edited).
- Everything else kept between runs is the store's (`storage.backend` `sqlite` or `postgres`;
  `trunk-pro/src/kept.rs`, `db/`), keyed by each system's id from `config.json`:
  - each system's names (`SystemNames`): talkgroups as rows in `talkgroups` and its own radio
    names in `unit_names`. A file load (`setTalkgroups`, `setUnitNames`) replaces a system's list
    (`runtime::set_names`, one generic over both), an edit (`setTalkgroup`, `setUnitName`)
    changes one row, all committed at once. Each row carries its origin
    (`trunk-core/src/trunk/names.rs`: where its names came from, the last change only), set by
    `replace_rows` and the edits, the same code in the browser build (through wasm);
    `read_rows` / `rows_csv` and `read_unit_names` / `unit_names_csv` are the import and export;
  - talker aliases in `units` (only those that changed are written);
  - band plans, the Heard list, AutoTune's scores and sharing's state in `kept`, written within
    10 s of a change (`Session::due`) and at stop;
  - the radio registry in one shape, `RegistryChanges`: trunk-app hands over each 15 minutes'
    changes (`Saved::Registry`), appended to `registry_changes`; once a day's have piled up
    (`COMPACT_AFTER`) the store makes them over the whole copy in `registry` and lets them go
    (the browser build writes each changed system whole instead);
  - the dashboard's history in `stats_minutes` (2 days) and `stats_hours` (section 12.2).
- `trunk-pro.sqlite` (`trunk-pro/src/db/`) is the call store: one row per finished call, written
  in batches by the `db-writer` thread. The recording and its `.json` are still files; the row is
  how `callsQuery` finds them without reading the folder. The live list (`Ctx::recent`, the last
  500 calls) answers what's just happened and is merged into every query (`runtime::calls`), so
  the store's batching never hides a call and the recorder lists calls with the store off.
  Low-write mode keeps no call index. A store of another schema version is brought up to this
  one when the change only adds to it (`schema::upgrade`), otherwise set aside and made afresh
  (`db/schema.rs`).
- An older install's `<short>.talkgroups.csv`, `<short>.bandplan`, `<short>.units.csv`,
  `conventional.heard.json`, `tune.json` and `radio/<short>.json` are imported once and renamed
  `.imported` or `.unused` (`migrate.rs`).
- The files that are kept, installed plugins and plugin data all live beside the config file
  (`paths::data_dir()`), so recorders with their own `--config` don't share them; the list is in
  [Files kept beside the config](../configuration/README.md#files-kept-beside-the-config). Every
  one of them is written atomically (`config::write_atomic`: a temporary file, then a rename), so
  a crash never leaves half a file.

The session's side of this is its persistence hooks: `due(now_ms, all)` returns what is due to
keep as `Saved` items (units, sharing, band plans, heard codes, AutoTune scores, registry
changes), and `new` (which takes the names, `&NameLists`) / `load_units` / `load_heard` /
`load_tune` take it back next run. The platform decides where these are kept.

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

- **A steady radio.** Lags scatter upward from buffering; the least lag in each 1 s window sits on
  the true offset (for example 0.35 s, the radio's open time). The first block anchors the offset;
  the loop (time constant 8 s, doubling to 256 s after 4τ of windows under 1 ms) then pulls it onto
  the windows' minima and measures the crystal's rate.
- **Crystal drift.** Each window's target moves a fraction of a millisecond (0.6 ms at the
  page's exaggerated 600 ppm); the loop follows the phase and learns the rate, staying within a few
  milliseconds with no jumps in call times.
- **Samples lost, unreported.** 20 ms of samples vanish at 30 s; sample time is 20 ms behind the
  air. The target stays more than 5 ms off for 3 windows, so at 33.75 s the offset steps at once,
  the loop restarts fast and the log says "clock re-anchored".

### 11.3 Identifiers

A system's **short name** is unique: no two systems share one, trunked or conventional, on or off
(`Config::name_problem`). It names the folder and is what users know a system by. Its identity,
though, is its `id` (`SystemId`; a site's is its `SiteId`): given once in the config and never
given again, the same across renames and runs. What's kept of a system outside a run (its calls in
the store, its names, its plugins' delays) and plugin events (`system_id`, `site_id`) go by it;
stats series, band plans and capture rules still go by short name. The interface's filters,
`listen`, "now playing" and system colours go by short name. Every message that carries a
system's number also carries its short name, except the live-audio frame, whose number `status`
maps to a name.

The numbers below that aren't ids are handles for one run, never saved. Keep them apart: each
indexes something different.

| Name | What it indexes | Where it's used |
|---|---|---|
| Position in `config.systems` / `config.conventional` | Every configured system, including disabled ones and ones with no control channel | The config's own order; `channelFile`'s `index` (conventional) |
| Engine system index (`Call::system`) | **Active** systems only, from 0: the trunked ones (`Config::active_systems()`: enabled, with an enabled site that has a control channel), then the conventional ones (`Config::active_conventional()`: enabled, with an enabled channel), in config order. It indexes `EngineConfig.systems` | Calls, events, live-audio frames, `{sys_num}` in file names, plugin `CallInfo.system` / `SystemInfo.index`, `status.systems[].index`, `status.systemNames` |
| Engine site index (`Call::site: Option<u16>`) | Active sites of active trunked systems (`trunk::sites`), system by system; none for a conventional call | Calls, control channel events, `status.systems[].sites[].index` |
| Position in `Status.sites` | Sites that currently have a control channel (can skip some) | Nothing: each `SiteStatus` carries its `site` and `system` |
| A Setup card's key (`cardKey`) | Every configured system and site, in this browser | The Setup cards (seeded from the short name, site name or position; never sent) |
| `SystemId` (`id`) / `SiteId` | Every system / site, given once in the config and never reused | The same across renames and runs: the store, names, plugin delays, plugin events (`system_id`, `site_id`) |
| `shortName` | Every system, unique | Its folder and label: what users and the interface know it by, stats series, band plans |
| A site's `name` | Each site of a system, unique there (absent: the system's short name) | The talkgroup file's Preferred Site, call JSON `site`, band plan files, control channel stats (`SiteKey`) |
| `CallId` | One sequence for every system | `call_num` in the JSON |

`Config::call_systems()` is the one place a config's systems are given their numbers (active
trunked systems, then active conventional systems): the lookup from short name to number, and the
order `Config::engine_config` lists them in for the engine. Plugins' `SystemInfo` and
`status.systemNames` come from it.

Outside the engine a site is named one way, by its `SiteKey { system, site }`
(`SystemConfig::site_key`): `own_name` (its name when it isn't its system's), `stats_key`
(`sys/<system>` or `sys/<system>/site/<site>`), `file_stem` (`<system>` or `<system>.<site>`) and
`Display` (`<system>` or `<system>/<site>`, for log lines). Events about a site, such as
`controlLost`, carry the system's short name and the site's own name apart.

Other conventions:

- Frequencies are integer Hz (`u64`) in messages and `f64` Hz in config.
- TDMA / DMR slots are 0 / 1 internally and +1 only in display strings.
- Times inside the engine are seconds of sample time (`f64`). Times in JSON are Unix seconds or
  milliseconds, as the field name says.

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
| `Rollup` | `History` (in memory: a week with no store, or a day with one) and the store | `statsQuery` → `statsResult` |
| Session events → `Stats::observe` | `Registry` | `radioQuery` → `radioResult` (`Shared::radio_query`, names from the `NameBook`); the registry's changes go to the store every 15 min and at stop |
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
- **`History`**: a minute grid per series in memory: a week with no store (the browser build);
  with one, 26 hours (`statstore::memory_minutes`). `statsQuery` takes names or
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
  writes the hours memory has after it. With no store the history is memory alone, a week. The
  history is read back in the background at start (`statsResult.loading` meanwhile).
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
| `hello.capabilities` | all: `service`, `drivers`, `plugins`, `interfaces`, `host`, `keys`, `tuneGain` (and `calls`, `history` while the store is open) | none |
| Not available | | plugins, interfaces, M4A, channel files, `findRadios`, `listDir`, Trunk Recorder import from disk, key containers, `quit` |

### 13.3 The config rules in the interface

The interface doesn't write the config's rules again: it runs the recorder's own, compiled to
WebAssembly, in both builds (the desktop app's page too). `crates/trunk-web/src/rules.rs`
exports, over trunk-app's `config.rs`: `configNormalize` (a config as the recorder reads it,
an older one upgraded, with its `notes`), `configTalkgroups` (the talkgroup files an older
config held), `configProblem` (`Config::problem`), `configNameProblem`, `configCenters`
(`resolved_centers`), `configReceivers` (`Config::receivers`), `configSources` (each source's label and `Source::title`),
`placeSources` (`place_sources`), `usableHalfWidth`, `channelsParse` /
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
read strictly by `Command::parse`, with `needs()`: the access it takes to send it). `trunk-pro/src/protocol_tests.rs`
checks their variants against protocol.ts both ways and the messages against the schema. The
full reference is `docs/api/README.md`.

Every message to the browsers goes through `runtime::publish(hub, ToClient)`, which serializes it
once as each kind of connection may hear it (`runtime::Out::of`: for everyone, control access
only, or the config in two versions, the listen one only what a View user's pages show: `access::view::listen_config`, where every
config field is named as shown or withheld, so a new one doesn't compile until it's decided).
Bodies still put together as JSON ride as a `Body`. A command that doesn't parse, or that the
connection's access doesn't allow, is answered with an `error`. `hello.capabilities` says what a
recorder can do beyond recording (`Capability`: `service`, `drivers`, `plugins`, `interfaces`,
`host`, `keys`, `calls`, `history`, `tuneGain`), so an interface shows what works rather than checking which build it is.

### 14.1 A typical conversation

1. Client connects to `/api/ws` (the access guard runs first; see 14.5).
2. Recorder → the greeting, as the ordinary messages it sends later: `state`, `config` (with its
   `version`), `talkgroups` and `unitNames` (each system's), `units` (each system's saved aliases), `mdcUnits`,
   `heard`, `history` (the last 300 calls), `radios`, `survey`, `tune`, `monitorEvent` (each recent
   event), `host`; with control access also `devices`, `keys`, `plugins`, `interfaces`. Then
   `hello` (version, platform, api, config path, survey bands, capabilities, access, user): caught up. An interface waits for
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
| Topic subscribers | `spectrum` (`spectrum:<source>`, about 7/s, 512 bins), `log` (`log`: control channel lines and the rest of the recorder's log), `rfDetail` (`rf:<source>`, 1/s), `decodeDetail` (`decode:<shortName>`, 1/s: channels, each site's adjacent sites and DMR / NXDN detail, patches), per-core detail in `host` (`platform`), `unitEvent` (`unit`: each radio's control channel events, the `unit` plugin topic's) |
| The sender only | `configSaved`, `devices`, `radios`, `dir`, `trConfig`, `store`, `subscribed`, `statsResult`, `radioResult`, `callsResult`, `sourceProfile`, `error`, `notice` |
| Binary, to listeners | live audio frames |

Nothing is computed for a topic nobody watches. A client more than 4,096 messages behind on the
hub skips the oldest without notice.

### 14.3 Commands (client → recorder)

| Area | Commands |
|---|---|
| Recording | `setConfig`, `setTalkgroups`, `setUnitNames`, `setUnitName`, `setTalkgroup`, `start`, `stop`, `quit`, `channelFile`, `tuneGain`, `tuneStop` |
| Watching | `subscribe`, `listen`, `statsQuery`, `radioQuery`, `callsQuery` |
| Setup | `devices`, `findRadios`, `listDir`, `readTrConfig`, `profileSource`, `surveyStart`, `surveyListen`, `surveyRescan`, `surveyStop`, `licenseQuery`, `callSignQuery`, `iqCapture*`, `iqRuleCheck`, `dataSummary`, `deleteData`, `compactData`, `createToken` (docs/api/README.md lists them all) |
| Plugins and interfaces | `plugins`, `addPlugin`, `removePlugin`, `pluginAction`, `interfaces`, `addInterface`, `removeInterface`; their stores: `store {kind, refresh?}`, `install {kind, id}` or `{kind, repository, tag?}` (progress in `installProgress {kind, key, id, stage, message}`), `kind` `plugin` or `interface` |
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
| `/api/login`, `/api/logout`, `/api/session`, `/api/passkey/register/{start,finish}`, `/api/passkey/login/{start,finish}`, `/signin` | signing in: set a browser's session cookie (answering who signed in) and clear it; who this request is and what it may do; adding a passkey and signing in with one; the sign-in page |
| `/api/v1/calls`, `/api/v1/calls/<path>`, `/api/v1/storage` | the call store as JSON (`docs/api/calls.openapi.yaml`), with listen or control access; with the store off, `/storage` answers 503 and the calls routes answer from the recent calls |
| `/api/v1/export/<table>` | a table of the store as a CSV file, streamed a page at a time (`data.rs`), control access only |
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
gets CORS headers, and its preflight an answer); anything else gets 403. Bound to localhost, the
`Host` must be localhost too, which defeats DNS rebinding.

With signing in on (`server.auth`), the guard also identifies the request (`access::identify`):
the first valid secret of an `Authorization: Bearer` or a `?token=`, else a session in the
recorder's cookie (cookies only from the recorder's own pages, by `Sec-Fetch-Site`, so another
port's page can't borrow them). The secret is a user's access token (`server.auth.users`,
`Who::User`). A session (`access::Sessions`, issued by `/api/login`) is `user.credential.expires.mac`:
the credential is the token's hash, the MAC an HMAC-SHA256 under a 32-byte key kept in
`session-key` beside the config (0600, never in the config); the cookie is named `trp_` + 8 hex of
the key's SHA-256, so recorders on one host name keep apart, and it lasts 30 days. A request gets the
lower of its port's access and its user's role (`Caller::access`, looked up in the config as it is
now, so a removed user, or a token made again, gives nothing at once); with no token,
`anonymous`'s. With nothing, only what `server::open` lets through answers: `/api/version`,
`/api/login`, `/api/logout`, `/api/session`, `/signin`, and the API's static files (docs, schema,
types, client.js, examples); a browser asking for any other page gets `signin.html` in its place,
anything else 401 `WWW-Authenticate: Bearer`. What signing in changes isn't a setting, so it's kept
beside the config, not in it (`access::state::State`, `access-state.json`, held by `Sessions`): when
each user was last signed out everywhere and each passkey's record as last counted, so a sign-in
changes no config and bumps no version. A WebSocket's access is checked again at each `config` message,
each change of that state and each command; when it has changed, the socket is closed and the client reconnects to a new
greeting. Secrets are 32 random bytes (`trp_` + hex), stored as SHA-256; `createToken` and
`trunk-pro access` (`access/cli.rs`) make a user with one (`access::add_user`).

---

## 15. Where things live

| Concern | File(s) |
|---|---|
| Channelizer | `trunk-core/src/dsp/channelizer.rs` |
| Receivers | `trunk-core/src/dsp/{c4fm,cqpsk,msd,fm,am}.rs`, `smartnet/rx.rs` |
| Shared DSP / bit helpers | `trunk-core/src/dsp/filters.rs`, `bits.rs`, `ambe.rs` |
| In-band signalling | `trunk-core/src/dsp/inband/` (detectors, patterns, signals, paging tables), `trunk/inband.rs` (a call's), `trunk/recorded.rs` (what a call recorded) |
| P25 framer / FEC / bank | `trunk-core/src/p25/{frame,tsbk,voice,fec,diversity,phase2}.rs` |
| DMR | `trunk-core/src/dmr/` |
| NXDN | `trunk-core/src/nxdn/` |
| SmartNet | `trunk-core/src/smartnet/` |
| EDACS | `trunk-core/src/edacs/` |
| Vocoders, loudness | `trunk-core/src/mbe/`, `loudness.rs` |
| Generated tables (IMBE / AMBE) | `trunk-core/src/tables.rs` (op25 / mbelib values, copied mechanically) |
| Engine, sources, recorder pool | `trunk-core/src/trunk/engine.rs` |
| Control channels per protocol | `trunk-core/src/trunk/control.rs` |
| Messages, P25 parsing (`P25Parser`) · typed P25 OSPs | `trunk-core/src/trunk/message.rs` · `trunk-core/src/p25/osp.rs` |
| Calls, the kind of voice (`Voice`), `RecorderHost`, call ids | `trunk-core/src/trunk/calls.rs` |
| Patches | `trunk-core/src/trunk/patches.rs` |
| Voice decoders | `trunk-core/src/trunk/{voice,tracker,tdma}.rs` |
| Site identity and the site lock | `trunk-core/src/trunk/identity.rs` |
| Saving a call (cutting, levels, JSON) | `trunk-core/src/trunk/{record,frames}.rs` |
| Call JSON (Trunk Recorder fields) | `trunk-core/src/trunk/record.rs` (`call_record`); the type, `CallRecord`: `trunk-recorder-plugin/src/protocol.rs` |
| Multi-site | `trunk-core/src/trunk/multisite.rs` |
| Conventional | `trunk-core/src/trunk/conventional.rs` |
| Talkgroup CSV / unit names, talker aliases (`UnitAliases`) | `trunk-core/src/trunk/talkgroups.rs`, `units.rs` |
| Where a row's names came from (`Origin`, `replace_rows`) | `trunk-core/src/trunk/names.rs` |
| Config schema, defaults, validation (`problem()`) | `trunk-app/src/config.rs` |
| Source auto-centring, pins, advice | `trunk-app/src/place.rs` |
| Older configs upgraded when read | `trunk-app/src/migrate.rs` |
| Conventional channel CSV | `trunk-app/src/channels.rs` |
| Codes conventional frequencies carried | `trunk-app/src/heard.rs` |
| File names and folders | `trunk-app/src/filename.rs` |
| Each system's names beside the config (`SystemNames`, `NameLists`) | `trunk-app/src/config.rs`, `trunk-pro/src/runtime.rs` (`set_names`, `set_talkgroup`, `set_unit_name`), `trunk-pro/src/kept.rs` (`load_names`); in the page, `web/src/controller.ts` (one list machinery for both) |
| Session, outputs | `trunk-app/src/session.rs` |
| Dashboard measurements | `trunk-core/src/metrics.rs`, `trunk-app/src/stats/`, `trunk-pro/src/statstore.rs`, `monitor.rs`, `platform.rs` |
| Interface protocol | `web/src/protocol.ts` → `docs/api/protocol.schema.json`; in Rust `trunk-app/src/protocol.rs` (`ToClient`, `Command`), checked against it by `trunk-pro/src/protocol_tests.rs` |
| Who may hear and send what (listen / control access, signing in) | `trunk-app/src/protocol.rs` (`audience`, `needs`), `trunk-pro/src/access.rs`, `access/cli.rs`, `server.rs` (`guard`) |
| Threads, lifecycle, deliver, finish | `trunk-pro/src/runtime.rs` |
| SDR drivers | `trunk-pro/src/sdr.rs` (`Radio`, `run_with`), `trunk-pro/src/radio/` |
| Web server | `trunk-pro/src/server.rs` |
| What's kept between runs, the call store | `trunk-pro/src/kept.rs`, `db/`, `statstore.rs` |
| Data folder | `trunk-pro/src/paths.rs` |
| Log format | `trunk-app/src/log.rs`, `trunk-pro/src/logging.rs` |
| Analysis tools (`trunk-pro tool` cc / voice / frames / p2 / snr / dmr / nxdn …) | `trunk-pro/src/tool.rs`, `toolkit.rs` (what they share), `snrtool.rs`, `dmrtool.rs`, `nxdntool.rs` |
| Plugin host | `trunk-pro/src/plugins/` |
| Stores (plugins and interfaces) | `trunk-pro/src/store.rs`, `plugins/store.rs`, `interfaces/` |
| Plugin protocol and SDK | `trunk-recorder-plugin/src/` |
| Config in the UI, Trunk Recorder import | `web/src/config.ts`, `web/src/Setup.tsx`; the rules it applies are the recorder's, as WebAssembly: `trunk-web/src/rules.rs` → `web/src/rules.ts` |
