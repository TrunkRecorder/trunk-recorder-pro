# Architecture

How Trunk Recorder Pro is put together: the crates, the threads, and the
path a sample takes from the radio to a `.wav` / `.json` pair on disk. For
the config file see [configuration.md](configuration.md); for the interface
protocol see [api/README.md](api/README.md).

## The crates

```text
          trunk-core      DSP, decoders, trunking engine. No I/O, no threads, no clock.
              ▲           Builds for native and wasm32.
              │
          trunk-app       Config, Session (engine → interface messages, call files,
              ▲           plugin events), survey session, filenames, log records.
       ┌──────┴──────┐    Platform-independent too.
   trunk-pro      trunk-web
   desktop app:   browser build: Session + RTL-SDR over WebUSB,
   threads, USB,  exported with wasm-bindgen to web/src/web/engine.worker.ts
   web server,
   plugins, CLI
       │
   trunk-recorder-plugin   The plugin wire protocol (used by trunk-pro with
                           default-features = false) and a Rust SDK for plugin
                           authors (feature "sdk"). Published on crates.io.

   web/            The React interface. It is embedded in the trunk-pro binary
                   and also runs the browser build.
```

The rule that holds this together is that **`trunk-core` and `trunk-app` never
do I/O**. They take samples and a wall-clock time and return values: events,
JSON strings, WAV bytes. The platform crates (`trunk-pro`, `trunk-web`) own
devices, threads, files and sockets. This is why the same recording logic runs
in a desktop process and in a Web Worker, and gives bit-identical calls on the
same capture.

| Crate | Main entry points |
|---|---|
| `trunk-core` | `trunk::Engine` (everything), `dsp::Channelizer`, `dsp::{C4fm, Cqpsk}`, `p25`, `dmr`, `smartnet`, `mbe`, `survey::Survey` |
| `trunk-app` | `Config`, `Session`, `Output`, `survey::SurveySession`, `filename`, `channels` (conventional CSV), `log::Record` |
| `trunk-pro` | `main.rs` (CLI), `runtime.rs` (threads), `server.rs` (HTTP + WebSocket), `sdr.rs` + `radio/` (drivers), `plugins/` (host, store, manage) |
| `trunk-web` | `WebSession`, `WebRtl`, `WebSurvey` |
| `trunk-recorder-plugin` | `protocol` (`HostMessage`, `PluginMessage`, `Manifest`, topics), SDK (`Plugin`, `run`, `CallQueue`) |

## From the antenna to a file

```text
 USB / driver thread           engine thread                                         finish thread
 ───────────────────           ─────────────────────────────────────────────────     ─────────────
 RTL-SDR (nusb)   ┐            Session::push ─► Engine::push_u8 / push_iq
 USRP (UHD)       │ SourceMsg      │
 Airspy           ├──────────►     ▼
 SoapySDR         │ sync_channel  Channelizer (one per source: shared FFT, many heads, pre-roll)
 capture file     ┘   (256)        │
                                   ├─ control-channel head ─► receivers ─► framer ─► TSBK / OSW / CSBK
                                   │                                              ─► Message ─► CallManager
                                   │                                                     │ grant
                                   │                     ┌───────────────────────────────┘
                                   │                     ▼
                                   ├─ voice heads (opened per grant, with pre-roll)
                                   │    ─► receivers ─► framer ─► voice tracker ─► vocoder ─► 8 kHz audio
                                   │
                                   └─ conventional: energy in the shared spectrum ─► open head with
                                        pre-roll ─► NBFM / P25 / DMR voice
                                   │
                         call ends ▼
                         MultiSite (hold copies; keep the best one)
                                   │
                                   ▼
                         Event::Concluded {json, audio}
                                   │
                         Session::poll ─► Output::File {wav, json}
                                   │
                         runtime::deliver ────────────────────────────────────►  write .wav/.json
                                                                                 `concluded` to browsers
                                                                                 .m4a (ffmpeg…)
                                                                                 plugins: call.concluded
                                                                                 Archive: delete when uploaded
```

### 1. Sources

Each source runs on a thread of its own (`source-N`), started by
`runtime::start`:

| Kind | Code | Notes |
|---|---|---|
| `rtlsdr` | `trunk-pro/src/sdr.rs` | Pure Rust over `rtlsdr-nusb`. R820T / R828D tuners only. Sends raw `u8` IQ (`SourceMsg::Data`). Reopens after USB errors. |
| `usrp` | `radio/uhd.rs` | UHD's C API, loaded with `libloading` at run time. Float IQ (`SourceMsg::Iq`). |
| `airspy` | `radio/airspy.rs` | libairspy, loaded at run time. Its samples arrive on libairspy's own callback thread. |
| `soapy` | `radio/soapy.rs` | SoapySDR 0.7 / 0.8, loaded at run time. |
| `file` | `runtime::run_file` | `cu8`, `cs16`, `cf32` captures, paced to real time or as fast as possible. |

Every source sends into **one** `sync_channel::<SourceMsg>(256)` that the engine
thread reads. When the engine falls behind, sends block. The driver then loses
samples at its own level and reports them in `dropped`; the channel itself
never drops anything silently.

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

The channelizer keeps about 1 s of past spectra. `add_head` replays them, so a
voice channel opened when a grant arrives starts with the air *before* the
grant (**pre-roll**). It also returns the absolute input-sample index of the
head's first output, which is how frame times are tied back to a source clock.

The same spectra give the waterfall (`spectrum`), the noise floor
(`noise_profile`) and, for conventional channels, the energy detector, so
watching a conventional frequency costs almost nothing.

### 3. Receivers (`dsp/`)

A receiver implements `dsp::Receiver`: channel IQ goes in, `Symbol { dibit,
sample, rel_hi, rel_lo }` comes out. Soft reliabilities feed the FEC.

| Receiver | Used for |
|---|---|
| `C4fm` (4800 Bd, RRC, block timing, cluster levels, optional multi-symbol detector `msd`) | P25 Phase 1 C4FM, DMR |
| `Cqpsk` (π/4-DQPSK, Gardner timing, optional CMA equaliser) | P25 Phase 1 CQPSK / simulcast (4800 Bd), Phase 2 H-DQPSK (6000 Bd) |
| `smartnet::Fsk2` (3600 Bd 2FSK; emits `Bit`, not `Symbol`) | SmartNet control channel |
| `fm::Nbfm` (discriminator, de-emphasis, 300 Hz high-pass, squelch gate) | Analog voice: conventional FM and SmartNet analog grants |

For P25 Phase 1, `p25::diversity::Bank` runs **several receivers on the same
channel**: CQPSK, CQPSK with equaliser, and C4FM, as `modulation` allows. It
takes the best of each frame (`best_frame`, `best_tsbks`). Frames from the
receivers are matched by their sample instant.

### 4. Framers and decoders

| Protocol | Framer → unit | Decoding |
|---|---|---|
| P25 Phase 1 | `p25::Framer` → `Frame` (NID, bits, soft bits) | `p25::tsbk` (soft Viterbi + CRC) for TSBKs; `p25::fec` / `voice` for LDU, HDU, LC, ES, TDULC; IMBE codewords |
| P25 Phase 2 | `p25::phase2::Framer` → `Packet` (one 180-dibit slot) | Descrambling (seed = WACN, System ID, NAC from the control channel), ISCH / DUID, MAC PDUs, AMBE+2 codewords |
| DMR | `dmr::Framer` → `Burst` → `dmr::Channel` (CACH picks the slot) → `SlotDecoder` ×2 → `SlotEvent` | BPTC, RS, link control, CSBKs, AMBE+2 (shares `phase2::decode_vcw`) |
| SmartNet | `smartnet::osw::Framer` → `Osw` → `smartnet::Parser` | OSW deinterleave + Viterbi; channel numbers mapped through a band plan |

The control-channel decoders all end in the same type, **`trunk::Message`**
(grants, updates, IDEN, identity, patches, affiliations…). This is what lets
one `CallManager` serve P25, SmartNet and trunked DMR.

### 5. Trunking (`trunk/`)

`trunk::Engine` owns everything above the radio:

- **`Radio`** is shared by all systems. It holds the sources and their
  channelizers, the open voice channels (`HashMap<(system, freq), Channel>`),
  the recorder pool (`recordings`, capped by `maxRecorders`), what each
  system's control channel tells its voice channels (`VoiceParams`: the
  Phase 2 scrambler key), and the AutoTune ppm per source.
- **`Trunk`** is one per active trunked system, or site, and knows nothing
  of any protocol. Its `ControlChannel` (`control.rs`) decodes the protocol;
  the `Trunk` does what every protocol shares:
  - hunting through the control channels, or watching every carrier, as the
    protocol's `CarrierPlan` says;
  - the site lock (`identity.rs`), on the fields the protocol states;
  - AutoTune;
  - the system's clock;
  - its `CallManager` (`calls.rs`: turns `Message`s into `Call`s, à la Trunk
    Recorder's `monitor_systems.cc`);
  - the unit aliases and the adjacent sites.
- **`Conventional`** (`conventional.rs`) covers energy-detected channels,
  tone / NAC / colour-code matching, and their calls.
- **`MultiSite`** (`multisite.rs`) holds the copies of one call heard on
  several sites. When the last copy ends it keeps the best one (most cleanly
  decoded voice, or the talkgroup's preferred site).

**Control channels** (`control.rs`): one `ControlChannel` implementation per
protocol.

| Protocol | Carriers | Decoding | Site lock fields |
|---|---|---|---|
| `P25` | one, hunted | receiver bank → framer → TSBKs → `TsbkParser` | NAC, WACN, System ID, RFSS, site |
| `SmartNet` | one, hunted | `Fsk2` → OSW framer → `smartnet::Parser` | System ID, site |
| `Dmr` | every carrier of the site, watched | `dmr::Site`: bursts → CSBKs / link control | none (colour codes) |

Each one turns IQ into `Step`s (messages, plus what the site has said about
itself so far). `Trunk::follow` checks every step against the site lock
before the call manager sees it. A system's protocol is
`SystemConfig::protocol` (`Protocol::{P25, SmartNet, Dmr}`). What the
dashboard shows beyond the common fields is `ProtocolStatus` (e.g. a DMR
site's channel table).

**Voice channels** (`voice.rs`): a `CallManager` reaches the radio through the
`RecorderHost` trait, which `SysHost` implements.
1. A grant calls `start_recording`, which takes a recorder from the pool and
   calls `open_channel`.
2. `open_channel` adds a head on whichever source covers the frequency and
   replays the pre-roll.
3. It then builds the call's `VoiceDecoder`. Which decoder depends on the
   call, not the control channel (`VoiceKind::of`): a P25 system grants
   Phase 1 and Phase 2 calls, a SmartNet system analog and P25 ones.

| `VoiceKind` | Path |
|---|---|
| `Fdma` | `Bank` → `VoiceTracker` (`tracker.rs`) → IMBE |
| `Tdma` | `Cqpsk` (6000 Bd) → `phase2::Framer` → `TdmaTracker` (`tdma.rs`), both slots on one head |
| `Analog` | `Nbfm`, squelched; MDC1200 / FleetSync IDs via `dsp::signalling` |
| `Dmr` | `C4fm` → `DmrVoice` (both slots) |

Conventional channels use the same decoders, with their own squelch and
carrier meter around them. A decoder's outputs (`VoiceOut`: slot, air time,
`TrackerOut`) are queued and applied after the block, so a voice channel
never borrows a `Trunk` mutably.

For each source block the engine works in this order:

1. Run that source's voice heads.
2. Run the trunks whose control channel is on that source.
3. Apply pending tracker output.
4. Run conventional channels.
5. Emit call events.

**Time.** The engine has no wall clock. Every time comes from a source's
sample count, plus that source's clock offset (`Engine::set_clock_offset`):
- each `Trunk` keeps `now_s` from its control channel's source;
- the engine's own `now_s` comes from source 0.

Wall time is `epoch_ms` (taken by the platform at start) plus engine time.
`Session` applies the local UTC offset for folder names.

`Session` sets each live source's offset by measuring its samples against
the wall clock (`ClockFit`):
- **At first data, at once.** This covers the time a radio took to open (a
  USRP's FPGA load).
- **Then from the least lag in each 10 s window**, slewed at most 1 ms a
  second. This covers crystal error, and time never runs backwards. (Samples
  only ever arrive late, so the least lag is the clock's own.)

A capture played as fast as it can be keeps its sample time. `now_ms`, which
the platform passes to `poll`, is Unix ms on a monotonic clock: the epoch
plus `Instant` / `performance.now()`.

Sample counts stay in step with the air only if no samples go missing, so
missing samples are fed as silence (`Engine::push_gap`):

- samples a driver reports it dropped;
- the time a running source sends nothing for over a second (unplugged,
  wedged, reopening), filled in by `Session::poll`.

Calls on a dead source therefore still time out, and sources' clocks stay
comparable for multi-site matching. Conventional channels don't learn their
noise floor from the silence or open on it.

**Output.** The engine does no I/O. Everything comes out of
`Engine::drain_events()` as an `Event`:

| Event | Meaning |
|---|---|
| `ControlChannel`, `Message`, `Note` | Control-channel activity and log-worthy notes |
| `CallStart`, `CallUpdate`, `CallEnd` | Call lifecycle (`Call`, `calls.rs`) |
| `Audio` | Live audio for a call, 8 kHz f32 (one copy per multi-site conversation) |
| `Concluded` | A saved call: `Call`, Trunk Recorder–format JSON, short name, base name, 8 kHz audio, optional vocoder frames |
| `NotSaved`, `Duplicate` | A call that wasn't saved, and why (too short / silent, or another site's copy kept) |
| `UnitAlias` | A talker alias learned over the air |
| `ConvSkipped` | A conventional transmission that no channel row took (another tone / NAC) |

`Engine::status()` returns a snapshot of every system and source for the
dashboard.

#### Saving a call

`record::save_call` turns a finished call into a `Concluded`:
- it leaves out transmissions shorter than `minTransmissionS`;
- it refuses a call that is too short or silent;
- it normalises loudness (`loudness.rs`, −16.5 dBFS speech) and applies the
  digital / analog level;
- it writes the call JSON (Trunk Recorder's fields plus `signal`, `noise`,
  `snr`, `clean_voice_pct`, `errorList`, `freq_error`).

The engine only gathers the system's rules and names for it. `wav.rs` writes
16-bit mono 8 kHz.

#### Adding a protocol

A new trunking protocol (say NXDN) touches these places:

1. **Decoding:** a module of its own (`nxdn/`): receiver, framer, and a
   parser that produces `trunk::Message`s.
2. **Its control channel:** a `ControlChannel` implementation in
   `control.rs`. It sets the `CarrierPlan`, the identity fields its site lock
   can use, and its site group.
3. **Wiring it in:** a `Protocol` variant, and its line in `control::build`.
4. **Its voice:** if the voice is new, a `VoiceKind`, its `VoiceDecoder`, its
   line in `voice::build`, and how a grant marks it in `VoiceKind::of`.
5. **Its site's identity:** if the site states facts no protocol has yet, an
   `IdField` for each.
6. **The config:** a `type` in `trunk-app`'s config and Setup.

The call manager, multi-site, recording, plugins and the interface don't
change.

### 6. Session (`trunk-app/src/session.rs`)

`Session` wraps the `Engine` and is the only thing either platform talks to:

- `push` / `push_iq` feed samples. `source_error` / `source_ended` report
  source trouble.
- `poll(now_ms, &mut Vec<Output>)` drains the engine's events and turns them
  into `Output`s:
  - `Output::Text` carries interface messages: `status` every 500 ms,
    `spectrum` every 150 ms, `log`, `unitAlias`, `heard`.
  - `Output::Audio` carries live audio frames `[2][u16 system][u32 call
    id][u32 talkgroup][i16…]`, built only when `want_audio` is set.
  - `Output::File` carries a call to store: relative path from
    `filenameFormat`, WAV bytes, JSON, optional frames, and the history entry.
    The platform sends the entry as a `concluded` message once the files are
    stored, so the interface never lists a call that isn't on disk.
  - `Output::Plugin` carries `HostMessage`s, built only for the topics in
    `plugin_topics`.
  - `Output::Log` carries `log::Record`s, which the platform formats in Trunk
    Recorder's log style.
- `finish()` ends every call (stop / end of input).
- Persistence hooks: `bandplans()`, `units_changed()`, `heard_unsaved()` hand
  back text to save, and `new` / `load_units` / `load_heard` take it back next
  run. The platform decides where these files live.

### 7. The desktop process (`trunk-pro`)

```text
main ─► serve()
         ├─ Ctx (Arc): config (Mutex), phase, history, runner, survey, plugins, hub
         ├─ tokio runtime ─► axum server: /api/ws, /calls/, /builtin/, /ui/, /api/*
         │                    one task per WebSocket; blocking commands via spawn_blocking
         └─ runtime::start(cfg) on "start" (or --start / autoStart)
               ├─ source-0 … source-N threads ─► SourceMsg channel
               ├─ engine thread: owns Session; loop { recv 50 ms → push → poll → deliver }
               │     deliver(): Text/Audio → hub (tokio broadcast, 4096)
               │                Plugin → PluginHost   Log → logging
               │                File → Finish queue (1024)
               ├─ finish thread: write .wav/.json, `concluded` + history, .m4a,
               │                 PluginHost::concluded, Archive bookkeeping
               └─ PluginHost: one supervised child process per enabled plugin
```

- **Ctx** is the shared state between the web server and the recorder. The
  `hub` is a `tokio::broadcast` channel. Every WebSocket task subscribes to it
  and filters live audio by what that client asked to `listen` to.
- **Start / stop.** Start, stop, survey start and quit each hold
  `Ctx::lifecycle` from beginning to end, so two never interleave. Once
  quitting, nothing starts.
- **Stop.** `Runner::stop` sets a flag and joins every thread. The engine then
  runs `session.finish()`, delivers the last calls, joins the finisher, gives
  plugins 10 s to finish uploads, and saves band plans and aliases. Ctrl-C,
  SIGTERM and the interface's **Quit** all go through this path.
- **Persistence.**
  - `config.json` is written by `setConfig` (the interface always sends the
    whole config).
  - `<short>.bandplan` is written every 10 s when it changes, and at stop.
  - `<short>.units.csv` and `conventional.heard.json` are written when they
    change.
  - These companion files, installed plugins and plugin data all live
    beside the config file (`paths::data_dir()`), so recorders with their
    own `--config` don't share them.
  - Every one of these is written atomically (`config::write_atomic`: a
    temporary file, then a rename), so a crash never leaves half a file.
- **Survey** (`trunk-pro/src/survey.rs`, `trunk-core/src/survey.rs`) has the
  same shape: one source thread retuned on request, and one survey thread
  stepping through the bands. It never runs alongside recording.
- **Logging** (`logging.rs`) uses the `log` facade with Trunk Recorder's line
  format. It writes to stderr, daily or 100 MB log files, and syslog.

### 8. The browser build (`trunk-web` + `web/src/web/`)

```text
page (React UI) ─► WorkerTransport ─postMessage─► engine.worker.ts
                                                   ├─ WebRtl (WebUSB)  ─► WebSession.push
                                                   ├─ setInterval poll ─► WebSession.poll ─► messages / audio back to the page
                                                   └─ call files       ─► OPFS (opfs.ts); "Export to folder…" copies them out
```

The worker speaks the same `FromRecorder` / `ToRecorder` messages as the
desktop WebSocket (`web/src/protocol.ts`), so the interface code is the same
for both. Its config is kept in `localStorage`. These desktop-only features
are hidden or ignored in the browser:

- USRP / Airspy / SoapySDR sources
- plugins
- channel files
- `findRadios`, `listDir`, Trunk Recorder config import from disk
- `quit`

### 9. The interface protocol

`web/src/protocol.ts` is the source of truth. `docs/api/protocol.schema.json`
is generated from it (`npm run schema`), and
`trunk-pro/src/protocol_tests.rs` checks Rust's messages against that schema.

- **Recorder → client.**
  - On connect: `hello` (config, phase, history, aliases, heard codes,
    radios, survey), then `plugins`.
  - Broadcast to every client: `state`, `status`, `spectrum`, `log`,
    `concluded`, `unitAlias`, `heard`, `config`, `survey`,
    `surveySpectrum`, `pluginRuntime` / `pluginState` / `pluginResult` /
    `pluginInstall`, `quit`.
  - Replies to the sender only: `devices`, `radios`, `dir`, `trConfig`,
    `pluginStore`, `error`, `notice`.
- **Client → recorder.** `setConfig`, `start`, `stop`, `listen`,
  `channelFile`, `devices`, `findRadios`, `listDir`, `readTrConfig`,
  `survey*`, `plugins`, `addPlugin`, `removePlugin`, `pluginStore`,
  `installPlugin`, `quit`.
- **HTTP routes.**
  - `/api/ws`, `/api/version`, `/api/docs`, `/api/schema`, `/api/protocol.ts`,
    `/api/interfaces`, `/api/*` (the docs/api folder).
  - `/calls/<path>` (the recordings folder).
  - `/builtin/` (the embedded interface), `/ui/<name>/` (interfaces from
    disk), and `/` (the home interface).

### 10. Plugins (`trunk-pro/src/plugins/`, `trunk-recorder-plugin`)

A plugin is a separate executable that only *watches*: nothing it does
changes what is recorded.

- **Manifest.** `exe --describe` prints a `Manifest`: id, version, plugin
  `api`, the subscribed topics, extra audio formats, and JSON Schemas for its
  global and per-system settings. The Plugins page draws its forms from those
  schemas.
- **Process.** While recording, `PluginHost` keeps one supervised process per
  enabled plugin, restarted with back-off (1 s → 60 s) unless it exits with
  code 78 (config error).
- **Protocol.** JSON lines: `HostMessage` on stdin, `PluginMessage` on stdout,
  and stderr becomes log lines.
  - The host first sends `hello` (settings, systems with their index and
    per-system settings, folders, formats); the plugin answers `ready`.
  - Topics: `call.start`, `call.end`, `call.concluded` (with wav / json / m4a
    paths), `unit`, `audio`, `status`. The session builds only the topics
    some plugin subscribes to.
  - The plugin replies with `log`, `status` and `call.result` (`ok`,
    `skipped`, `failed`).
- **Back-pressure.** Each plugin has a 1024-deep queue fed with `try_send`.
  When it's full, events are dropped and counted, so the engine never waits
  on a plugin.
- **Archive.** `Archive` counts `call.result`s to decide when a call's files
  can be deleted (`audioArchive: false` / `callLog: false`).
- **Store** (`store.rs`).
  - Packages are listed in the registry `index.json` (fetched, cached in
    `plugin-registry.json`, or the copy built into the binary), or come from
    a GitHub release's `SHA256SUMS`.
  - Install: download, check the SHA-256, unpack to a staging folder,
    re-check the manifest, rename into `plugins/<id>/`.
  - Plugin state lives in `plugin-data/<id>/`.
- **API versions.** The host speaks `API_VERSION` and accepts plugins whose
  manifest `api` is no newer. While there is only version 1 that is enough;
  the first bump has to make the host speak each plugin's own version, or
  refuse older ones.

## Identifiers that cross boundaries

A system's identity is its **short name**: no two systems share one,
trunked or conventional, on or off (`Config::name_problem`). It names the
folder, the companion files and the system's plugin settings, and plugins
key on it. So does the interface: its filters, `listen`, "now playing" and
system colours go by short name. Every message that carries a system's
number also carries its short name, except the live-audio frame, whose
number `status` maps to a name.

The numbers below are handles for one run, never saved. Mixing them up has
been the most common kind of bug where modules meet.

| Name | What it indexes | Where it's used |
|---|---|---|
| Position in `config.systems` | Every configured trunked system, including disabled ones and ones with no control channel | The UI's Setup cards |
| Engine system index (`Call::system`, `Trunk::idx`) | **Active** systems only (`Config::active_systems()`: enabled, with a control channel), in config order | Calls, events, live-audio frames, plugin `CallInfo.system` |
| Position in `Status.systems` | Trunks that currently have a control channel (can skip some) | Nothing: each `SystemStatus` carries its engine index (`system`), which the `status` message sends as `index` |
| Conventional system id | `65535 − k` for conventional system *k* (at most 256) | Calls and plugins; `calls::conventional_system` / `conventional_index` |
| `shortName` | Every system, unique | The identity: folders, companion files, plugin settings, what plugins key on |

`Config::call_systems()` is the one place a config's systems are given their
numbers (active trunked systems, then enabled conventional systems with
channels): the lookup from short name to number. Plugins' `SystemInfo` comes
from it.

Other conventions:

- Frequencies are integer Hz (`u64`) in messages and `f64` Hz in config.
- TDMA / DMR slots are 0 / 1 internally and +1 only in display strings.
- Times inside the engine are seconds of sample time (`f64`). Times in JSON
  are Unix seconds or milliseconds, as the field name says.

## Where things live

| Concern | File(s) |
|---|---|
| Config schema, defaults, validation (`problem()`), source auto-centring | `trunk-app/src/config.rs` |
| Config in the UI, Trunk Recorder import | `web/src/config.ts`, `web/src/Setup.tsx` |
| Call JSON (Trunk Recorder fields) | `trunk-core/src/trunk/record.rs` |
| File names and folders | `trunk-app/src/filename.rs` |
| Talkgroup CSV / unit names | `trunk-core/src/trunk/talkgroups.rs`, `units.rs` |
| Conventional channel CSV | `trunk-app/src/channels.rs` |
| Control channels, one per protocol | `trunk-core/src/trunk/control.rs` |
| Voice decoders, one per kind of voice | `trunk-core/src/trunk/voice.rs` |
| Site identity and the site lock | `trunk-core/src/trunk/identity.rs` |
| Saving a call (cutting, levels, JSON) | `trunk-core/src/trunk/record.rs` |
| Multi-site dedupe | `trunk-core/src/trunk/multisite.rs` |
| Interface protocol | `web/src/protocol.ts` → `docs/api/protocol.schema.json` |
| Plugin protocol | `crates/trunk-recorder-plugin/src/protocol.rs` |
| Log format | `trunk-app/src/log.rs`, `trunk-pro/src/logging.rs` |
| Generated tables (IMBE / AMBE) | `trunk-core/src/tables.rs` from `scripts/gen_tables.ts` |
