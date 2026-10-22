# Architecture

How Trunk Recorder Pro is put together: the crates, the threads, and the
path a sample takes from the radio to a `.wav` / `.json` pair on disk. For
the config file see [configuration/](configuration/README.md); for the interface
protocol see [api/README.md](api/README.md).

## The crates

```text
          trunk-core      DSP, decoders, trunking engine. No I/O, no threads, no clock.
              ▲           Builds for native and wasm32.
              │
          trunk-app       Config (and its upgrades), Session (engine → interface
              ▲           messages, call files, plugin events), the interface
              │           protocol (ToClient, Command), survey session, filenames,
       ┌──────┴──────┐    log records. Platform-independent too.
   trunk-pro      trunk-web
   desktop app:   browser build: Session + RTL-SDR over WebUSB, exported with
   threads, USB,  wasm-bindgen to web/src/web/engine.worker.ts; and the config's
   web server,    rules for the page (rules.rs → web/src/rules.ts), in both builds
   plugins, CLI

   trunk-recorder-plugin   The plugin wire protocol and the call record (`CallRecord`),
                           used by trunk-core, trunk-app and trunk-pro with
                           default-features = false, and a Rust SDK for plugin
                           authors (feature "sdk"). Published on crates.io.
   rtlsdr-nusb             Our fork of the RTL-SDR driver (USB transfer sizes),
                           patched in for the whole workspace.

   web/            The React interface. It is embedded in the trunk-pro binary
                   and also runs the browser build.
```

The rule that holds this together is that **`trunk-core` and `trunk-app` never
do I/O**. They take samples and a wall-clock time and return values: events,
messages, call records, WAV bytes. The platform crates (`trunk-pro`,
`trunk-web`) own devices, threads, files and sockets. This is why the same
recording logic runs in a desktop process and in a Web Worker, and gives
bit-identical calls on the same capture.

| Crate | Main entry points |
|---|---|
| `trunk-core` | `trunk::Engine` (everything), `dsp::Channelizer`, `dsp::c4fm::C4fm`, `dsp::cqpsk::Cqpsk`, `dsp::fm::Nbfm`, `dsp::am::Am`, `dsp::filters` (RRC, windowed-sinc), `p25`, `dmr`, `nxdn`, `smartnet`, `ambe` (AMBE+2 codeword, shared by DMR, NXDN and Phase 2), `bits` (bit fields, CRC-CCITT), `mbe` (vocoders, `mbe::Profile`), `metrics`, `loudness`, `survey::Survey` |
| `trunk-app` | `Config`, `migrate` (older configs upgraded), `Session`, `Feeder`, `Output`, `protocol` (`ToClient`, `Command`), `stats` (dashboard measurements), `survey::SurveySession`, `filename`, `channels` (conventional CSV), `heard` (codes conventional frequencies carried), `keys` (key sources), `samples` (sample formats → IQ), `log::Record` |
| `trunk-pro` | `main.rs` (CLI; `tool.rs` + `toolkit.rs`, `snrtool.rs`, `dmrtool.rs`, `nxdntool.rs` analysis tools), `runtime.rs` (`Ctx`, threads), `server.rs` (HTTP + WebSocket), `access.rs`, `sdr.rs` + `radio/` (drivers), `plugins/` (host, store, manage, archive, encode, delay, hold, cli), `interfaces/` (other interfaces: store, serve, cli), `store.rs` (what both stores share), `spool.rs`, `unlock.rs` (key containers), `statstore.rs`, `monitor.rs` + `platform.rs` (the computer), `survey.rs`, `paths.rs`, `logging.rs` |
| `trunk-web` | `WebSession`, `WebRtl`, `WebSurvey`, `WebProfiler`; `rules` (the config's rules for the page) |
| `trunk-recorder-plugin` | `protocol` (`HostMessage`, `PluginMessage`, `Manifest`, `CallRecord`, topics), SDK (`Plugin`, `run`, `CallQueue`) |

## From the antenna to a file

```text
 USB / driver thread           dsp-N thread (one per source)                         finish-N threads
 ───────────────────           ─────────────────────────────────────────────────     ────────────────
 RTL-SDR (nusb)   ┐            Feeder::push ─► Engine::push_u8 / push_iq
 USRP (UHD)       │ SourceMsg      │
 Airspy           ├──────────►     ▼   (under the source's lock)
 SoapySDR         │ sync_channel  Channelizer (one per source: shared FFT, many heads, pre-roll)
 capture file     ┘ (256 each)     │
                                   ├─ control-channel head ─► receivers ─► framer ─► TSBK / OSW / CSBK / CAC
                                   │                                  ─► Message ─► site lock ─► CallManager
                                   │                                                     │ grant
                                   │                     ┌───────────────────────────────┘
                                   │                     ▼
                                   ├─ voice heads (opened per grant, with pre-roll)
                                   │    ─► receivers ─► framer ─► voice tracker ─► vocoder ─► 8 kHz audio
                                   │
                                   └─ conventional: energy in the shared spectrum ─► open head with
                                        pre-roll ─► FM / AM / P25 / DMR / NXDN voice
                                   │
                  each block: its voice channels and its control channels' carriers are
                  decoded under the source's own lock (and each control channel's); what
                  they decoded is followed (site lock, CallManager, grants) under the
                  engine's, and conventional channels are decoded and followed there too
                  — one source's logic at a time, every source's trunked DSP at once. Fed
                  from one thread (replay, the browser), it runs as it always did;
                  `replay --threads` feeds each source from its own.
                                   │
                         call ends ▼
                         MultiSite (hold copies; keep the best one)
                                   │
                                   ▼
                         Event::Concluded {record, audio}
                                   │
                         Session::poll ─► Output::File {wav, record}
                                   │
                         runtime::deliver ─► Finish queue (1024) ─────────────►  plan the call's files
                         (engine thread)                                         write .wav / .json / .m4a
                                                                                 `concluded` to browsers
                                                                                 plugins: call.concluded
                                                                                 Archive: delete when uploaded
```

### 1. Sources

Each source runs on a thread of its own (`source-N`), started by
`runtime::spawn_source` (recording, the survey and profiling all start their
sources this way). The four drivers implement `sdr::Radio` (`BACKOFF`,
`retune_to`, `stream_once`, `prepare_thread`) and run in one `sdr::run_with`
loop, which reopens a radio after an error once its back-off is up (RTL-SDR
2 s, the others 3 s) and answers the survey's retunes with `SourceMsg::Tuned`.

| Kind | Code | Notes |
|---|---|---|
| `rtlsdr` | `trunk-pro/src/sdr.rs` | Pure Rust over `rtlsdr-nusb`. R820T / R828D tuners only. Sends raw `u8` IQ (`SourceMsg::Data`). |
| `usrp` | `radio/uhd.rs` | UHD's C API, loaded with `libloading` at run time. Float IQ (`SourceMsg::Iq`). |
| `airspy` | `radio/airspy.rs` | libairspy, loaded at run time. Its samples arrive on libairspy's own callback thread as real 16-bit samples (`SourceMsg::Real16`), made IQ on the source's dsp thread (`sdr::IqFrom`). |
| `soapy` | `radio/soapy.rs` | SoapySDR 0.7 / 0.8, loaded at run time. Float IQ. |
| `file` | `runtime::run_file` | `cu8`, `cs16`, `cf32` captures, paced to real time or as fast as possible. |

Each source sends into **its own** `sync_channel::<SourceMsg>(256)`, which its
`dsp-N` thread reads (`runtime::feed_source`). When that thread falls behind,
sends block. The driver then loses samples at its own level and reports them
in `dropped`; the channel itself never drops anything silently.

In the browser build, `engine.worker.ts` pulls blocks from `WebRtl::next()`
(WebUSB) and feeds `WebSession::push` directly. There is no channel there; the
worker is single-threaded.

### 2. Channelizer (`dsp/channelizer.rs`)

There is one `Channelizer` per source. It is an overlap-save FFT filter bank
(N = 16384, hop 12288) after CyberEther's `filter_engine`. Every channel the
engine wants is a **head** (`HeadId`): an M-bin slice of the shared FFT, an
inverse FFT and a phase rotation. A head outputs `Complex32` at
`output_rate = fs / D`, where D is the largest power of two that keeps the rate
≥ 24 kHz (37.5 kHz at 2.4 MSPS).

The channelizer keeps as much past spectra as the longest pre-roll (1 s by
default). `add_head` replays them, so a
voice channel opened when a grant arrives starts with the air *before* the
grant (**pre-roll**). It also returns the absolute input-sample index of the
head's first output, which is how frame times are tied back to a source clock.

The same spectra give the waterfall (`spectrum`), the noise floor
(`noise_profile`) and, for conventional channels, the energy detector, so
watching a conventional frequency costs almost nothing. `threshold` (so many
dB above the noise in a band) sets both a trunked analog channel's squelch and
a conventional channel's carrier meter.

### 3. Receivers (`dsp/`)

A receiver implements `dsp::Receiver`: channel IQ goes in, `Symbol { dibit,
sample, rel_hi, rel_lo }` comes out. Soft reliabilities feed the FEC.

| Receiver | Used for |
|---|---|
| `C4fm` (4800 Bd, `c4fm::SYMBOL_RATE`; RRC matched filter; one sampling phase per 240-symbol block, 100 ms latency; cluster-mean levels; multi-symbol detection (`msd`) switched on while the levels are poorly separated; on a bursty channel (a mobile) quiet samples are left out of timing and levels) | P25 Phase 1 C4FM, DMR, NXDN (2400 / 4800 Bd: `nxdn::Rate::receiver`) |
| `Cqpsk` (π/4-DQPSK, Gardner timing, optional CMA equaliser) | P25 Phase 1 CQPSK / simulcast (4800 Bd), Phase 2 H-DQPSK (6000 Bd) |
| `smartnet::Fsk2` (3600 Bd 2FSK; emits `Bit`, not `Symbol`) | SmartNet control channel |
| `fm::Nbfm` (discriminator, de-emphasis, 300 Hz high-pass, squelch gate) | Analog voice: conventional FM and SmartNet analog grants |
| `am::Am` (envelope over the carrier level, noise limiter, squelch opened by the carrier's spectral line) | Analog AM: conventional airband channels |

For P25 Phase 1, `p25::diversity::Bank` runs **several receivers on the same
channel**: CQPSK, CQPSK with equaliser, and C4FM, as `modulation` allows. It
takes the best of each frame (`best_frame`, `best_tsbks`), and per IMBE
codeword the copy the soft decoder trusted most (`best_imbe`; erased bits
count against a copy). Frames from the receivers are matched by their
sample instant. With both kinds (`auto`), both run until one has clearly needed fewer NID
corrections over 40 frame slots or more (a missed frame counting as 11);
then only that kind runs, on that channel, until it is reopened or goes
10 s without a frame (`diversity::Detect`).

Shared pieces: `dsp::filters` (RRC and windowed-sinc taps), `bits` (MSB-first
fields, CRC-CCITT), `ambe` (the 72-bit AMBE+2 codeword: `decode_vcw`).

### 4. Framers and decoders

| Protocol | Framer → unit | Decoding |
|---|---|---|
| P25 Phase 1 | `p25::Framer` → `Frame` (NID, bits, soft bits) | `p25::tsbk` (soft Viterbi + CRC) for TSBKs; `p25::fec` / `voice` for LDU, HDU, LC, ES, TDULC; IMBE codewords |
| P25 Phase 2 | `p25::phase2::Framer` → `Packet` (one `SLOT_DIBITS` = 180-dibit slot; `sf_slot` is its superframe slot 0..11) | Descrambling (seed = WACN, System ID, NAC from the control channel), ISCH / DUID, MAC PDUs, AMBE+2 codewords |
| DMR | `dmr::Framer` → `Burst` → `dmr::Channel` (CACH picks the slot) → `SlotDecoder` ×2 → `SlotEvent` | BPTC, RS, link control, CSBKs, AMBE+2 (shares `ambe::decode_vcw` with Phase 2) |
| SmartNet | `smartnet::osw::Framer` → `Word` (`Osw`, or `Bad` when a due frame fails) → `smartnet::Parser` | OSW deinterleave, soft Viterbi, CRC, flywheel; channel numbers mapped through a band plan |
| NXDN | `nxdn::frame::Framer` → `Frame` (FSW, 192-symbol frames, LICH) | Descrambling, Viterbi + CRC per channel (SACCH, FACCH1, UDCH, CAC), `nxdn::layer3` messages, AMBE+2 codewords |

The control-channel decoders all end in the same type, **`trunk::Message`**
(grants, updates, IDEN, identity, patches, affiliations…). This is what lets
one `CallManager` serve P25, SmartNet, trunked DMR and NXDN.

### 5. Trunking (`trunk/`)

`trunk::Engine` owns everything above the radio. It is a handle (clones share
one engine), so each source can be fed from a thread of its own while another
reads events and status. Its config is an `EngineConfig`: the sources (kept
once, as one `Arc<[SourceConfig]>`), the systems (`EngineConfig.systems`,
trunked and conventional), the conventional channels, and the settings every
system shares (pre-roll, `maxRecorders`, the vocoder).

- **`Radio`** is shared by all systems. It holds the sources, each behind a
  lock of its own (`SourceRx`: its channelizer and AutoTune ppm, the voice
  channels open on it as `BTreeMap<(site, freq), Channel>`, its control
  channel carriers (`Tap`s), and its copy of what each site's control channel
  tells voice channels, `VoiceParams`: the Phase 2 scrambler key), the
  recordings and the recorder pool (capped by `maxRecorders`), and how many
  calls are followed without a recorder.
- **`System`** is one per system, trunked or conventional (a
  `SystemConfig`): what it decides about its calls (talkgroups, call and
  save rules, unit names, keys), kept once for all its sites, its radios'
  talker aliases, and its saved calls. A trunked system lists its sites
  (`SystemConfig::sites`); a conventional one has none, and its channels
  name it (`ConvChannel::system`). `Engine::set_talkgroups` and `set_keys`
  change a system's for every site at once.
- **`Trunk`** is one per site of a trunked system (a `SiteConfig`, which holds
  only the site's own fields: its name, control channels, receivers, site
  lock, protocol). Its system's rules are handed to it where it needs them,
  never copied. It knows nothing of any protocol: its `ControlChannel`
  (`control.rs`) decodes that, and the `Trunk` does what every protocol
  shares:
  - listening as the protocol's `CarrierPlan` says, decided once in
    `Trunk::start`: `Listening::Hunting` (one control channel at a time,
    through the list) or `Listening::Watching` (every carrier at once);
  - the site lock (`identity.rs`), on the fields the protocol states;
  - AutoTune;
  - the site's clock (`now_s`, its control channel's);
  - its `CallManager` (`calls.rs`: turns `Message`s into `Call`s, à la Trunk
    Recorder's `monitor_systems.cc`; standing patches in `patches.rs`); call
    ids come from one `CallIds` sequence shared by every system;
  - the adjacent sites.
- **`Conventional`** (`conventional.rs`) covers energy-detected channels,
  tone / NAC / colour-code / RAN matching, and their calls. A conventional
  call opens on energy and ends when its carrier drops, so it doesn't go
  through a `CallManager`, but it speaks the engine's language: its outputs
  (`ConvOut`) carry the engine's `Event`s, link control is applied the one
  way (`CallManager::note_info`), and an ended call is saved as a trunked
  one is. Each frequency's widths (`ChannelShape`) are worked out once.
- **`MultiSite`** (`multisite.rs`) holds the copies of one call heard on
  several sites of one system (only the config says which sites those are;
  nothing is joined from the air). When the last copy ends it keeps the most cleanly decoded
  copy; the talkgroup's preferred site wins while its copy has 90 % of that.

**Control channels** (`control.rs`): one `ControlChannel` implementation per
protocol.

| Protocol | Carriers | Decoding | Site lock fields |
|---|---|---|---|
| `P25` | one, hunted | receiver bank → framer → TSBKs → `P25Parser` | NAC, WACN, System ID, RFSS, site |
| `SmartNet` | one, hunted | `Fsk2` → OSW framer → `smartnet::Parser` | System ID, site |
| `Dmr` | every carrier of the site, watched | `dmr::Site`: bursts → CSBKs / link control | none (colour codes) |
| `Nxdn` | every carrier of the site, watched | `nxdn::trunking::Site`: CAC on the control channel, VCALL / SCCH on traffic channels | System ID, site (Type-C); none (Type-D) |

Each one turns IQ into `Step`s (messages, plus what the site has said about
itself so far). The source's dsp thread decodes a block's carriers under the
control channel's own lock and hands the site a `CcResult`: the steps with
their `VoiceParams`, notes, the carrier control is on, counts and the
frequency offset. `Trunk::on_block` follows it without taking that lock:
`Trunk::follow` checks every step against the site lock before the call
manager sees it. A site's protocol is `SiteConfig::protocol`
(`Protocol::{P25, SmartNet, Dmr, Nxdn}`). What the dashboard shows beyond the
common fields is `ProtocolStatus` (e.g. a DMR site's channel table).

**Voice channels** (`voice.rs`): a `CallManager` reaches the radio through the
`RecorderHost` trait (`calls.rs`), which `SysHost` (`engine.rs`) implements:
`record`, `follow` (an encrypted call: link control only, when a recorder's
room is spare), `release` (the channel closes with its last call), and
`record_continued` (a call longer than `maxCallS`, or `STUCK_CALL_S` = 600 s,
is saved and carries on as a new call on the same channel and recorder).
1. A grant calls `RecorderHost::record`, which takes a recorder from the pool and
   calls `open_channel` on the source `Radio::source_for` picks.
2. `open_channel` adds a head there and replays the pre-roll. A channel
   already open of the same kind just takes the slot; one of another kind
   is replaced.
3. It then builds the call's `VoiceDecoder` (`voice::build`), on a head as
   wide as `VoiceKind::cutoff_hz` says. Which decoder depends on the call,
   not the control channel: a grant's `Message.voice`, and so `Call.voice`,
   is a `Voice` (`P25`, `P25Tdma { slot }`, `Dmr { slot, color_code }`,
   `Nxdn(rate)`, `Fm`, `Am`), and `Voice::kind` gives its `VoiceKind`. A P25
   system grants Phase 1 and Phase 2 calls, a SmartNet system analog and P25
   ones.

| `VoiceKind` | Path |
|---|---|
| `Fdma` | `Bank` → `VoiceTracker` (`tracker.rs`) → IMBE |
| `Tdma` | `Cqpsk` (6000 Bd) → `phase2::Framer` → `TdmaTracker` (`tdma.rs`), both slots on one head |
| `Analog` | `Nbfm`, squelched; MDC1200 / FleetSync IDs via `dsp::signalling` |
| `Am` | `Am` (conventional only) |
| `Dmr` | `C4fm` → `DmrVoice` (both slots) |
| `Nxdn(rate)` | `C4fm` at its rate → `NxdnVoice` |

Conventional channels use the same decoders, with their own squelch and
carrier meter around them. A decoder's outputs (`VoiceOut`: slot,
`TrackerOut`) are queued (`Radio::pending`) and applied after the block, at
the site's control channel clock, so a voice channel never borrows a `Trunk`
mutably; `apply_pending` finds each output's trunk, recording and call once.

For each source block the engine works in this order:

1. Under the source's lock (its dsp thread): decode its voice channels, and
   take its control channels' carriers' samples (`SourceRx::decode_block`).
2. Under each control channel's lock: decode those carriers into
   `CcResult`s (`BlockDsp::decode_control`).
3. Under the engine's lock (`Core::after_block`): take what the voice
   channels heard, let the sites on that source follow their control
   channels (`Trunk::on_block`), apply pending tracker output, run that
   source's conventional channels, apply again, and emit call events.

**Time.** The engine has no wall clock. Every time comes from a source's
sample count, plus that source's clock offset (`Engine::set_clock_offset`):
- each `Trunk` keeps `now_s` from its control channel's source;
- the engine's own `now_s` comes from source 0.

Wall time is `epoch_ms` (taken by the platform at start) plus engine time.
`Session` applies the local UTC offset for folder names.

`Session` sets each live source's offset by measuring its samples against
the wall clock (`ClockFit`). Each buffer carries the time its driver handed
it over (`push`'s `at_ms`; the desktop app stamps it in the source thread),
so time a buffer spends queued for a busy engine doesn't count. Samples
only ever arrive late, so the least lag in a window is the clock's own. The
offset is anchored:
- **at first data, at once.** This covers the time a radio took to open (a
  USRP's FPGA load);
- **afresh every 10 s window**, to that window's least lag:
  - within 0.25 s of the offset set, it is drift (crystal error), slewed at
    most 1 ms a second;
  - further, samples were lost unreported or invented (a stalled radio's
    backlog after its quiet was filled in): the offset is set at once, and
    the log says so.

A capture played as fast as it can be keeps its sample time. `now_ms`, which
the platform passes to `poll`, is Unix ms on a monotonic clock: the epoch
plus `Instant` / `performance.now()`.

Sample counts stay in step with the air only if no samples go missing, so
missing samples are fed as silence (`Engine::push_gap`):

- samples a driver reports it dropped;
- the time a running source sends nothing for over a second (unplugged,
  wedged, reopening), filled in by its `Feeder::idle` (on its dsp thread;
  `Session::poll` does it for a source fed from the caller's thread).

Calls on a dead source therefore still time out, and sources' clocks stay
comparable for multi-site matching. Conventional channels don't learn their
noise floor from the silence or open on it.

**Output.** The engine does no I/O. Everything comes out of
`Engine::drain_events()` as an `Event`:

| Event | Meaning |
|---|---|
| `ControlChannel`, `Message`, `Note` | Control-channel activity (by site) and log-worthy notes (by system, and site) |
| `CallStart`, `CallUpdate`, `CallEnd` | Call lifecycle (`Call`, `calls.rs`) |
| `Audio` | Live audio for a call, 8 kHz f32 (one copy per multi-site conversation) |
| `Concluded` | A saved call: `Call`, its record (`CallRecord`), short name, site, base name, 8 kHz audio, optional frame capture |
| `NotSaved`, `Duplicate` | A call that wasn't saved, and why (too short / silent, or another site's copy kept) |
| `UnitAlias`, `UnitPosition` | A talker alias learned over the air; a radio's GPS position |
| `ConvSkipped` | A conventional transmission that no channel row took (another tone / NAC) |

`Engine::status()` returns a snapshot of every site and source for the
dashboard; `Engine::channels()` lists every channel listened to
(`ChannelSnapshot`: kind, power, floor, offset, `ChannelQuality`).
`Engine::set_talkgroups` swaps a system's talkgroup table while recording.

#### Saving a call

`record::save_call` turns a finished call (a `Held`) into a `Concluded`:
- it leaves out transmissions shorter than `minTransmissionS`, and
  encrypted ones (`Transmissions` marks each), trimming the vocoder frames
  and error list to match (`CallFrames::keep`);
- it refuses a call that is too short or silent (`SaveRules`), except an
  encrypted one when `recordEncrypted` keeps it;
- it normalises loudness (`loudness.rs`, −12 dBFS speech, about −14 LUFS;
  `normalizeAudio`) and applies the digital / analog level;
- it makes the call's record (`record::call_record`): a `CallRecord`, the
  plugin SDK's type, with Trunk Recorder's fields plus `signal`, `noise`,
  `snr` from `Reception`, `clean_voice_pct`, `errorList`, `freq_error`.
  The same typed record goes to the file, the history and the plugins; the
  finish thread serialises it once (`CallRecord::to_json`), byte for byte as
  Trunk Recorder's files have it (flags as 0 / 1).

The engine only gathers the system's rules and names for it. `wav.rs` writes
16-bit mono 8 kHz.

#### Adding a protocol

A new trunking protocol touches these places (NXDN, the latest, is the
example to follow):

1. **Decoding:** a module of its own (like `nxdn/`): receiver, framer, and a
   parser that produces `trunk::Message`s.
2. **Its control channel:** a `ControlChannel` implementation in
   `control.rs` (`name`, `plan`, `push`, `counts`, `modulation`,
   `status`; the rest have defaults). It sets the `CarrierPlan` and the
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
6. **The config:** a `SystemKind` (`type`) in `trunk-app`'s config and Setup,
   its arm in `Config::engine_config`, and its `kind` in `Config::call_systems`
   (what plugins see).

The call manager, multi-site, recording and plugins don't change.

### 6. Session (`trunk-app/src/session.rs`)

`Session` wraps the `Engine` and is the only thing the recorder, desktop or
browser, talks to (the `replay` CLI drives the `Engine` directly):

- `feeder(i)` hands out source `i`'s `Feeder`, which the desktop app's
  `dsp-N` thread pushes samples into (`Feeder::push` / `push_iq`; `idle`
  fills a quiet spell with silence). `push` / `push_iq` feed samples from
  the caller's own thread (the browser build). `source_error` /
  `source_ended` report source trouble.
- `poll(now_ms, &mut Vec<Output>)` drains the engine's events and turns them
  into `Output`s:
  - `Output::Message` carries a `ToClient` for everyone: `status` every
    500 ms, `stats` every second, `unitAlias`, `heard`, `monitorEvent`.
  - `Output::Topic` carries the costly ones, made only while someone watches
    their topic (`set_topics`): `spectrum` every 150 ms (`spectrum:<i>`),
    control channel `log` lines (`log`), `rfDetail` (`rf:<i>`) and
    `decodeDetail` (`decode:<shortName>`).
  - `Output::Rollup` carries a minute of the dashboard's series for the
    history (see [Measurements](#measurements)).
  - `Output::Audio` carries live audio frames `[2][u16 system][u32 call
    id][u32 talkgroup][i16…]`, built only when `want_audio` is set.
  - `Output::File` carries a call to store: relative path from
    `filenameFormat`, WAV bytes, its `CallRecord`, optional frame capture,
    and whether some of it was decrypted. The platform makes the history
    entry from it (`session::call_entry`) and sends it as a `concluded`
    message once the files are stored, so the interface never lists a call
    that isn't on disk.
  - `Output::Plugin` carries `HostMessage`s, built only for the topics in
    `plugin_topics`.
  - `Output::Log` carries `log::Record`s, which the platform formats in Trunk
    Recorder's log style.
- `finish(&mut out)` ends every call (stop / end of input).
- `set_talkgroups` applies a system's talkgroup file edited while recording;
  `set_keys` its keys. `attach` hands it the platform's `stats::Shared`
  (registry, `NameBook` and monitor). A `radioQuery` is answered by
  `Shared::radio_query`, named from the `NameBook`, which the session keeps
  as its engine has the talkgroups; both builds answer this way.
- Persistence hooks: `bandplans()`, `units_changed()`, `heard_unsaved()`,
  `tune_unsaved()` hand back text to save, and `new` (band plans, and the
  talkgroup files) / `load_units` / `load_heard` / `load_tune` take it back
  next run. The platform decides where these files live.

### Measurements

What the dashboard shows is measured by dedicated parts, kept out of the
decoding code:

```text
ControlChannel::report, Engine::report ──────────────────────────────────┐
stats::SampleMeter (headroom, clipping, on the sample path, 1 in 16) ────┤
stats Tally (calls, airtime, reasons, voice errors, from events) ────────┴─► stats::Aggregator
                                                                               ├─► `stats` (1/s)
                                                                               └─► Rollup (1/min) ─► History (a week in memory) + stats/*.jsonl
session events ─► Stats::observe ─► stats::Registry (talkgroups, radios, affiliations, pairs, frequencies) ─► radioQuery
                               └─► stats::Monitor ─► Watcher (the hook for alert rules; none registered) + the event feed
VoiceDecoder::quality, ControlChannel::quality (ChannelQuality) ─► Engine::channels() ─► rfDetail / decodeDetail
```

- **`trunk-core/src/metrics.rs`**: the `Sink` / `Instrumented` traits. A
  part keeps plain counters and current values in its own fields and lists
  them when asked, about once a second (`cc/good`, `cc/sep`, …). Running
  totals become rates in the aggregator; nothing on the sample path changes.
  Each protocol says what it measures through `ControlChannel::report`
  (default: nothing), so a new protocol brings its own figures;
  `Engine::report` adds what every system and source shares (levels, ppm,
  calls). `VoiceDecoder::quality` and `ControlChannel::quality`
  (`ChannelQuality { sep, phase_err_deg }`: the eye opening, CQPSK's phase
  error) go to `Engine::channels()` only.
- **`trunk-app/src/stats/`**: `Aggregator` (rates, per-minute avg/min/max),
  `History` (a minute grid per series, queries downsampled), `Registry`
  (bounded maps per system, saved as JSON), `NameBook` (the names a
  `radioQuery` answers with), `Monitor` (events), and `Stats`, the facade
  `Session` holds: `observe(event)`, `meter(source)`, `tick()`.
- **`trunk-pro`**: `statstore.rs` appends rollups to daily files (gzipped
  after the day, deleted after 8) and reads them back at startup;
  `monitor.rs` samples the computer (`platform.rs`: sysinfo, cgroup limits,
  container detection, TCP connectivity checks) and the plugins into their
  own aggregator every 2 s; `Ctx.shared` holds the registry, the name book
  and the monitor so they outlive recordings.

### 7. The desktop process (`trunk-pro`)

```text
main ─► serve()
         ├─ Ctx (Arc): config (Mutex) + its version, talkgroup files, lifecycle (Lifecycle), phase,
         │             history, plugins, interfaces, hub, shared (registry + monitor + names),
         │             series + store (stats history), topics, engine (the engine thread's commands)
         ├─ logging (log facade): stderr, files, syslog, and the browsers (warnings and errors
         │             to every connection, the rest only while someone watches the `log` topic)
         ├─ tokio runtime ─► axum server on server.port: the built-in UI + the API (/api/ws, /api/*, /calls/)
         │                    one listener per other interface (interfaces::serve): its folder + the API
         │                    one task per WebSocket; blocking commands via spawn_blocking
         ├─ monitor thread: the computer and plugins every 2 s (net-probe thread beside it)
         ├─ stats-load / stats-store threads: history files in, rollups out
         ├─ history thread: fills the call list from disk in the background
         └─ Ctx::begin_recording on "start" (or --start / autoStart) ─► runtime::start(cfg)
               ├─ source-0 … source-N threads (runtime::spawn_source) ─► one SourceMsg queue each (256)
               ├─ dsp-0 … dsp-N threads: each source's samples into the engine (Feeder);
               │     errors and ends on to the engine thread; quiet spells filled with
               │     silence; a capture played as fast as it goes kept within 100 ms of
               │     air of the others (runtime::Air)
               ├─ engine thread: owns Session; loop { recv 20 ms on its one EngineCmd channel
               │     → poll → deliver }. Ctx::tell_engine is the one way in: Source (an error
               │     or end, from a dsp thread), Topics, Talkgroups, Keys. The plugin host is
               │     cloned once a turn (Plugins::host), held by no lock.
               │     deliver(): Message → runtime::publish → hub (tokio broadcast, 4096)
               │                Topic / Audio → hub   Plugin, Audio → PluginHost
               │                Log → logging         Rollup → statstore
               │                File → Finish queue (1024; when full, waits up to 5 s, then
               │                       the call is finished on this thread by the same plan)
               ├─ finish-0 … finish-N threads (1–3: half the cores): each call's plan
               │     (plugins::plan: the system's file rules, what the plugins take, whether
               │     there's an encoder) says which files to write; they write .json + .wav +
               │     .m4a (WAV piped to the encoder; the WAV instead should that fail), send
               │     `concluded` + history, hand PluginHost::concluded the files as they are,
               │     and tell the Archive; files only the plugins need go to the RAM spool
               │     (spool.rs) if on
               └─ PluginHost: per enabled plugin a supervisor thread (plugin-<id>) and its
                              child process
```

- **Ctx** is the shared state between the web server and the recorder. The
  `hub` is a `tokio::broadcast` channel. Every WebSocket task subscribes to it,
  drops topic messages the client didn't `subscribe` to and control-only
  messages a listen-only connection may not hear, and filters live audio by
  what it asked to `listen` to.
- **Lifecycle.** What the radios are doing is one `Lifecycle` (`Idle`,
  `Recording(Runner)`, `Surveying`, `Quitting`) behind one lock, changed only
  by `Ctx`'s methods (`begin_recording`, `begin_survey`, `with_radios` for
  profiling a source, `stop_recording`, `stop_survey`, `quit`), each holding
  the lock from beginning to end, so two never interleave. Once quitting,
  nothing starts. The phase the browsers see (`state`) is the engine
  thread's report.
- **Stop.** `Runner::stop` sets a flag and joins every thread. Each dsp
  thread first decodes what its source had already queued; the engine then
  joins them, polls, runs `session.finish()`, delivers the last calls, joins
  the finish threads, gives each plugin 10 s from when it has read
  `shutdown` (after up to 30 s to work through its queue), and saves band
  plans, aliases, AutoTune scores and the radio registry. Ctrl-C, SIGTERM
  and the interface's **Quit** all go through this path.
- **Persistence.**
  - `config.json` is written by `setConfig` (the interface always sends the
    whole config, with the version it edited).
  - `<short>.talkgroups.csv` (a system's talkgroup file, trunked or
    conventional) is written by `setTalkgroups`. An older config's
    talkgroups are moved out into these when it's read (`migrate.rs`).
  - `<short>.bandplan` (`<short>.<site>.bandplan` for a site with a name of
    its own: `SiteKey::file_stem`) is written every 10 s when it changes,
    and at stop.
  - `<short>.units.csv` and `conventional.heard.json` are written when they
    change; `tune.json` (AutoTune's scores) every minute and at stop.
  - `radio/<short>.json` (the radio registry) is written every 15 min and
    at stop.
  - `stats/YYYY-MM-DD.jsonl` gets each minute's rollup appended (see
    [Measurements](#measurements)).
  - These companion files, installed plugins and plugin data all live
    beside the config file (`paths::data_dir()`), so recorders with their
    own `--config` don't share them.
  - Every one of these but the stats files is written atomically
    (`config::write_atomic`: a temporary file, then a rename), so a crash
    never leaves half a file.
- **Survey** (`trunk-pro/src/survey.rs`, `trunk-core/src/survey.rs`) has the
  same shape: one source thread (`runtime::spawn_source`) retuned on request,
  and one survey thread stepping through the bands. It never runs alongside
  recording.
- **Logging** (`logging.rs`) uses the `log` facade with Trunk Recorder's line
  format. It writes to stderr, daily or 100 MB log files, and syslog. What
  the recorder logs also reaches the browsers as `log` lines with their clock
  time and level: warnings and errors to every connection, the rest only
  while someone watches the `log` topic. The decoder's own lines
  (`Output::Log`) reach the topic from the session instead, so none is sent
  twice.

### 8. The browser build (`trunk-web` + `web/src/web/`)

```text
page (React UI) ─► WorkerTransport ─postMessage─► engine.worker.ts
     │                                             ├─ WebRtl (WebUSB)  ─► WebSession.push
     │                                             ├─ setInterval poll ─► WebSession.poll ─► messages / audio back to the page
     │                                             └─ call files       ─► OPFS (opfs.ts); "Export to folder…" copies them out
     └─ rules.ts: trunk-web's config rules (rules.rs), loaded before the page renders (in both builds)
```

The worker speaks the same `FromRecorder` / `ToRecorder` messages as the
desktop WebSocket (`web/src/protocol.ts`), and greets the page the same way
(`hello` last), so the interface code is the same for both. The page keeps
the config and each system's talkgroup file in `localStorage`. Its `hello`
lists no `capabilities`, so the interface hides what needs the desktop app,
and the worker answers such a command with an `error`:

- USRP / Airspy / SoapySDR sources, `findRadios` (`drivers`)
- plugins, other interfaces, key containers, the computer's figures
  (`plugins`, `interfaces`, `keys`, `host`)
- channel files, `listDir`, Trunk Recorder config import from disk, `quit`
  (`service`)

### 9. The interface protocol

`web/src/protocol.ts` is the source of truth. `docs/api/protocol.schema.json`
is generated from it (`npm run schema`). In Rust every message is a
`ToClient` and every command a `Command` (`trunk-app/src/protocol.rs`), one
variant per `type`; `trunk-pro/src/protocol_tests.rs` checks both enums'
variants against protocol.ts's, both ways, every message sent against the
schema, and every documented command against `Command::parse`.

- **Recorder → client.**
  - On connect (`server::greeting`), the ordinary messages describing how
    the recorder is now (`state`, `config`, `talkgroups` for each system
    with a file, `units` for each with saved talker aliases, `heard`,
    `history`, `radios`, `survey`, `monitorEvent` for each recent event,
    `host`; with control access also `devices`, `keys`, `plugins`,
    `interfaces`), through the same audience rules as every other message,
    then `hello` last: caught up. `hello` is `{ version, platform, api,
    configPath? (control access), surveyBands, capabilities }`; the
    capabilities (`Capability`: `service`, `drivers`, `plugins`,
    `interfaces`, `host`, `keys`) say what this recorder can do beyond
    recording, so an interface shows only what works.
  - Broadcast to every client: `state`, `status`, `stats`, `host`,
    `monitorEvent`, `concluded`, `callFiles`, `unitAlias`, `heard`, `config`
    (with its `version`), `talkgroups`, `survey`, `surveySpectrum`, `log`
    (warnings and errors the recorder logs), `quit`. `status` (twice a
    second) is kept slim: calls, sources, and a summary of each site, with
    `systemNames` (each system's short name by its number).
  - Broadcast to clients with control access: `plugins`, `pluginRuntime`,
    `interfaces`, `installProgress` (with `kind`), `keys`.
  - Only to clients subscribed to the topic: `spectrum` (`spectrum:<i>`),
    `log` (`log`), `rfDetail` (`rf:<i>`), `decodeDetail`
    (`decode:<shortName>`: also each site's adjacent sites and DMR / NXDN
    detail, and the patches).
  - Replies to the sender only: `configSaved` (to every `setConfig`: the
    version now, and `error` when the edit wasn't taken or saved), `devices`,
    `radios`, `dir`, `trConfig`, `sourceProfile`, `store` (with `kind`; its
    list is `listings`), `subscribed`, `statsResult`, `radioResult`,
    `olderCalls`, `error`, `notice`.
  - A site's identity, in `status`, the survey and the config's `expect`,
    is one shape, `SiteIdentity` (trunk-app's config, made from the engine's
    `Identity`): a field not heard yet is left out.
  - By access: `ToClient::audience` says which messages are control-only
    (plugin, interface, store and key management), and `runtime::publish` —
    the one way onto the hub — serializes a message once per audience: `config`
    whole and without plugin settings and key sources (`access.rs`). A
    session passes on what its access may have. A command a listen-only
    connection may not send (`Command::listen_may_send`), and one that
    doesn't parse, is answered with an `error`.
- **Client → recorder.** `setConfig { config, version }` (the whole config,
  and the version it edits: an edit of an older one is refused),
  `setTalkgroups { system, csv }`, `start`, `stop`, `listen`, `unlockKeys`,
  `lockKeys`, `channelFile`, `devices`, `findRadios`, `listDir`,
  `readTrConfig`, `survey*`, `profileSource`, `plugins`, `addPlugin`,
  `removePlugin`, `interfaces`, `addInterface`, `removeInterface`, `store`
  and `install` (each with `kind`: `plugin` or `interface`), `subscribe`,
  `statsQuery`, `olderCalls`, `radioQuery`, `quit`. A listen-only connection
  may send only `subscribe`, `statsQuery`, `radioQuery`, `olderCalls` and
  `listen`.
- **HTTP routes**, on every interface's port (`server::api`):
  - `/api/ws`, `/api/version`, `/api/docs`, `/api/schema`, `/api/protocol.ts`,
    `/api/*` (the docs/api folder).
  - `/calls/<path>` (the recordings folder).
  - Everything else: the embedded interface on `server.port`; the
    interface's folder on its own port. A request's port is its `Door`, and
    a WebSocket's access follows from it (`access.rs`): control on the
    built-in port, the config's `server.interfaces.<id>.access` on an
    interface's.

### 10. Plugins (`trunk-pro/src/plugins/`, `trunk-recorder-plugin`)

A plugin is a separate executable that only *watches*: nothing it does
changes what is recorded.

- **Manifest.** `exe --describe` prints a `Manifest`: id, version, plugin
  `api`, the subscribed topics, extra audio formats, and JSON Schemas for its
  global and per-system settings. The Plugins page draws its forms from those
  schemas.
- **Process.** While recording, `PluginHost` keeps one supervised process per
  enabled plugin, restarted with back-off (1 s → 60 s) unless it exits with
  code 78 (config error). Each runs in its own process group, so a Ctrl-C
  reaches only the recorder, which then stops it. Changed settings restart
  the plugins: the new processes start once the old ones have exited, and
  events wait meanwhile. The engine thread takes the host once a turn
  (`Plugins::host`, an `Arc` clone), so a reload never waits on it; an old
  host is shut down once nothing holds it. The plugins are handed a call's
  files as the finish thread wrote them (an .m4a made for a plugin too).
- **Protocol.** JSON lines: `HostMessage` on stdin, `PluginMessage` on stdout,
  and stderr becomes log lines. Lines are read lossily (bytes that aren't
  UTF-8 become U+FFFD), so a stray byte never stops the reading.
  - The host first sends `hello` (settings, systems with their index and
    per-system settings, folders, formats); the plugin answers `ready`.
  - Topics: `call.start`, `call.end`, `call.concluded` (`ConcludedCall`:
    the typed `CallRecord`, every field named, and `files`: the json / wav /
    m4a paths), `unit`, `audio`, `status`. The session builds only the
    topics some plugin subscribes to.
  - The plugin replies with `log`, `status`, `metrics` and `call.result`
    (`ok`, `skipped`, `failed`).
  - On stop the host sends `shutdown` behind whatever is queued; each plugin
    gets its grace from when it has read it (after up to 30 s to drain),
    then is killed.
- **Back-pressure.** Each plugin has a 1024-deep queue fed with `try_send`.
  When it's full, events are dropped and counted, so the engine never waits
  on a plugin.
- **Delays** (`delay.rs`, `hold.rs`; `pluginDelays`). What a plugin is to
  have late goes to the hold instead of its queue, and is given to it when
  due (a finished call is kept on disk until then, across restarts).
- **Archive.** `Archive` counts `call.result`s to decide when a call's files
  can be deleted (`archiveWav: false`, `archiveM4a: false` / `callLog: false`). It is told at
  finish time whether a WAV was written.
- **Delivery timing** (`timing.rs`, #61). `Deliveries` is told when each
  finished call ended, how long the hold kept it from each plugin, and each
  `call.result` (with the plugin's `took_ms`, when it says). Each `ok` gives
  the call's age at delivery, split into held, waiting and upload, reported
  per plugin as running totals (`plg/<id>/ageMs`, `heldMs`, `waitMs`,
  `uploadMs`, `delivered`, `uploads`) and each tick's longest (`age`,
  `upload`). It's the same for every plugin, so none measures it.
- **WAV only when needed.** A call's `Plan` (`plugins::plan`) is the one
  place that says which of its files are written. The WAV goes to the
  encoder on stdin (afconvert gets a temporary file), so it's written only
  when it's kept, a plugin takes WAV, or encoding failed; `call.concluded`'s
  `wav` path may not exist for a plugin that asked for M4A.
- **RAM spool** (`spool.rs`, `recording.ramSpool`). Files that will be
  deleted once the plugins are done (and the .m4a made only for them) are
  written to a RAM disk instead: on macOS one the app makes with `hdiutil`
  (mounted `nobrowse` at `<data>/spool`, kept across restarts so queued
  retries still find their files), on Linux a folder in /dev/shm. Kept files
  go to the recordings folder directly. `Archive` moves a failed upload's
  files there; the monitor moves anything older than 2 h there too, and
  reports the spool's room (a `spool` disk row, `spoolLow` / `spoolFull` /
  `spoolRecovered` events). With no room, calls go to the recordings folder.
  Turned off, the spool is emptied into the recordings folder and removed.
- **Store** (`trunk-pro/src/store.rs`, shared with the interfaces' store:
  `plugins/store.rs`, `interfaces/store.rs`).
  - Packages are listed in the registry `index.json` (fetched, cached in
    `plugin-registry.json` / `interface-registry.json`, or the copy built
    into the binary), or come from a GitHub release's `SHA256SUMS`.
  - Install (`Shelf::install`, which reports each stage to whoever asked: the
    browsers as `installProgress`, or the CLI directly): download, check the
    SHA-256, unpack to a staging folder, re-check the manifest, rename into
    `plugins/<id>/`.
  - Plugin state lives in `plugin-data/<id>/`.
- **API versions.** The host speaks `API_VERSION` (1) and accepts plugins
  whose manifest `api` is 1 to `API_VERSION`.

## Identifiers that cross boundaries

A system's identity is its **short name**: no two systems share one,
trunked or conventional, on or off (`Config::name_problem`). It names the
folder, the companion files and the system's plugin settings, and plugins
key on it. So does the interface: its filters, `listen`, "now playing" and
system colours go by short name. Every message that carries a system's
number also carries its short name, except the live-audio frame, whose
number `status` maps to a name.

The numbers below are handles for one run, never saved. Keep them apart:
each indexes something different.

| Name | What it indexes | Where it's used |
|---|---|---|
| Position in `config.systems` / `config.conventional` | Every configured system, including disabled ones and ones with no control channel | The config's own order; `channelFile`'s `index` (conventional). Setup's cards are keyed by `cardKey`, not by it |
| Engine system index (`Call::system`) | **Active** systems only, from 0: the trunked ones (`Config::active_systems()`: enabled, with an enabled site that has a control channel), then the conventional ones (`Config::active_conventional()`: enabled, with an enabled channel), in config order. It indexes `EngineConfig.systems` | Calls, events, live-audio frames, `{sys_num}` in file names, plugin `CallInfo.system` / `SystemInfo.index`, `status.systems[].index`, `status.systemNames` |
| Engine site index (`Call::site: Option<u16>`) | Active sites of active trunked systems (`trunk::sites`), system by system; none for a conventional call | Calls, control channel events, `status.systems[].sites[].index` |
| Position in `Status.sites` | Sites that currently have a control channel (can skip some) | Nothing: each `SiteStatus` carries its `site` and `system` |
| `shortName` | Every system, unique | The identity: folders, companion files, plugin settings, what plugins key on |
| A site's `name` | Each site of a system, unique there (absent: the system's short name) | The talkgroup file's Preferred Site, call JSON `site`, band plan files, control channel stats |

`Config::call_systems()` is the one place a config's systems are given their
numbers (active trunked systems, then active conventional systems): the
lookup from short name to number, and the order `Config::engine_config`
lists them in for the engine. Plugins' `SystemInfo` and `status.systemNames`
come from it.

Outside the engine a site is named one way, by its `SiteKey { system, site }`
(`SystemConfig::site_key`): `own_name` (its name when it isn't its system's),
`stats_key` (`sys/<system>` or `sys/<system>/site/<site>`), `file_stem`
(`<system>` or `<system>.<site>`) and `Display` (`<system>` or
`<system>/<site>`, for log lines). Events about a site, such as
`controlLost`, carry the system's short name and the site's own name apart.

Other conventions:

- Frequencies are integer Hz (`u64`) in messages and `f64` Hz in config.
- TDMA / DMR slots are 0 / 1 internally and +1 only in display strings.
- Times inside the engine are seconds of sample time (`f64`). Times in JSON
  are Unix seconds or milliseconds, as the field name says.

## Where things live

| Concern | File(s) |
|---|---|
| Config schema, defaults, validation (`problem()`), source auto-centring | `trunk-app/src/config.rs` |
| Older configs upgraded when read (talkgroups moved out into files, older layouts rewritten) | `trunk-app/src/migrate.rs` |
| Config in the UI, Trunk Recorder import | `web/src/config.ts`, `web/src/Setup.tsx`; the rules it applies are the recorder's, as WebAssembly: `crates/trunk-web/src/rules.rs` → `web/src/rules.ts` |
| Call JSON (Trunk Recorder fields) | `trunk-core/src/trunk/record.rs` (`call_record`); the type, `CallRecord`: `crates/trunk-recorder-plugin/src/protocol.rs` |
| File names and folders | `trunk-app/src/filename.rs` |
| Talkgroup CSV / unit names, talker aliases (`UnitAliases`) | `trunk-core/src/trunk/talkgroups.rs`, `units.rs` |
| Talkgroup files beside the config (`talkgroups_path`, `TalkgroupFiles`) | `trunk-app/src/config.rs`, `trunk-pro/src/runtime.rs` (`set_talkgroups`, `load_talkgroups`) |
| Conventional channel CSV | `trunk-app/src/channels.rs` |
| Codes conventional frequencies carried | `trunk-app/src/heard.rs` |
| Control channels, one per protocol | `trunk-core/src/trunk/control.rs` |
| Control channel messages, TSBK parsing (`P25Parser`) | `trunk-core/src/trunk/message.rs` |
| Calls, the kind of voice (`Voice`), `RecorderHost`, call ids | `trunk-core/src/trunk/calls.rs` |
| Patches | `trunk-core/src/trunk/patches.rs` |
| Voice decoders, one per kind of voice | `trunk-core/src/trunk/voice.rs` (trackers: `tracker.rs`, `tdma.rs`) |
| Site identity and the site lock | `trunk-core/src/trunk/identity.rs` |
| Saving a call (cutting, levels, JSON) | `trunk-core/src/trunk/record.rs`, `frames.rs` |
| Multi-site dedupe | `trunk-core/src/trunk/multisite.rs` |
| Shared DSP / bit helpers | `trunk-core/src/dsp/filters.rs`, `bits.rs`, `ambe.rs` |
| Vocoders | `trunk-core/src/mbe/` |
| Dashboard measurements | `trunk-core/src/metrics.rs`, `trunk-app/src/stats/`, `trunk-pro/src/statstore.rs`, `monitor.rs`, `platform.rs` |
| Data folder | `trunk-pro/src/paths.rs` |
| Analysis tools (`trunk-pro tool` cc / voice / frames / p2 / snr / dmr / nxdn …) | `trunk-pro/src/tool.rs`, `toolkit.rs` (what they share), `snrtool.rs`, `dmrtool.rs`, `nxdntool.rs` |
| Interface protocol | `web/src/protocol.ts` → `docs/api/protocol.schema.json`; in Rust `trunk-app/src/protocol.rs` (`ToClient`, `Command`), checked against it by `trunk-pro/src/protocol_tests.rs` |
| Who may hear and send what (listen / control access) | `trunk-app/src/protocol.rs` (`audience`, `listen_may_send`), `trunk-pro/src/access.rs` |
| Radio drivers' run loop | `trunk-pro/src/sdr.rs` (`Radio`, `run_with`), `radio/` |
| Plugin protocol | `crates/trunk-recorder-plugin/src/protocol.rs` |
| Log format | `trunk-app/src/log.rs`, `trunk-pro/src/logging.rs` |
| Generated tables (IMBE / AMBE) | `trunk-core/src/tables.rs` (op25 / mbelib values, copied mechanically) |
