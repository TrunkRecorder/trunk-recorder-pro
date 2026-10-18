# Architecture review — 2026-10-17

A top-to-bottom design review at v0.1.15 (`9ce22dc`), after the per-source
decoding threads, "a system with sites" (#51) and AM landed. The question
was not "where are the bugs" but "where has the design drifted, and what
change to the model would remove a set of workarounds at once". Backwards
compatibility is not a goal, so config keys, the interface protocol, file
layouts and the plugin SDK may all change.

Locations name the file and function rather than line numbers, which drift.
Tick an item when it's merged; strike or annotate one that's been decided
against, with a word on why, so the list stays honest.

**Verdict.** The skeleton is right and the leaf modules are good. The debt is
concentrated at the seams where each of the last three restructures
(encryption, per-source threads, sites) pushed its model into the app or the
platform but stopped short of the engine. Almost nothing is marked TODO or
"for now", so none of this is flagged; it shows up as the same fact kept in
several places.

**Suggested order.** Theme 1 first, since 2, 3 and 8 ride on it. Then 5 and
6 together, because generating TypeScript from Rust types solves both the
protocol and the config duplication with one mechanism. Then 4, then 7. The
second tier can go whenever a nearby change touches it.

## The big ones

### 1. "System" is not a type in the engine

- [x] `EngineConfig.systems: Vec<SystemConfig { short_name, policy…, sites }>` (no sites: conventional; its channels name it) — bb880da
- [x] `Call::system` a plain index for both kinds; `Call::site: Option<u16>`; the `65535 − k` sentinel, `MAX_CONVENTIONAL`, `conventional_index` gone; `status.systemNames` maps numbers to names — bb880da
- [x] One `SiteKey { system, site }` with `own_name`, `stats_key`, `file_stem`, `Display`; `MonitorEvent::ControlLost { system, site, … }` (fixed the dashboard link to Decode) — 1d26965
- [x] `control::build` takes a `SiteConfig`, which now holds only the site's own fields — bb880da
- [ ] Conventional borrows "the only trunked system" for talkgroup names and the receiver bank (`Config::engine_config`): make it an explicit setting or drop it

**Evidence.** #51 was done in the app; the engine kept a flat site list
(`research/multi-site-systems.md`: "the engine's config stays a flat list")
and recovers the system by string equality. `trunk::engine::SiteConfig`
carries the system's talkgroups, rules, unit tags and keys, and
`Config::engine_config` clones the whole talkgroup table into every site.
`system_numbers` regroups sites by short name; `Core::set_talkgroups` writes
four copies (`t.cfg`, `t.calls`, `cfg.sites[i]`, then `Shared.sites`);
`AliasBook::new` is built from a second dedup and relies on the same order.
Conventional systems are a parallel second kind, so `write_call`,
`learn_alias`, `set_keys`, `wrong_key`, `AliasBook` (`trunked` and
`conventional` vectors) and the concluded counters each have a "trunk else
conv" branch, and `Core::call_rules` rebuilds a `Vec<CallRules>` every block.
`conventional_index` / `conventional_system` are consulted in ten files across
three crates. A site's identity is spelled five ways: `bandplan_key`
(`dcfd.south`), `SiteConfig::stats_key` (`sys/dcfd/site/south`),
`Session::site_label` (`dcfd/south`), `Stats::edges` (`dcfd/south` again,
sent as the `system` of `ControlLost`), `Config::problem` (`dcfd (site
south)`). `Config` has five helpers that each re-walk the config to map name,
index and site (`call_systems`, `active_systems().nth`, `site_of`,
`short_name_of`, `recording_of`).

**Blast radius.** engine.rs, control.rs, units.rs, conventional.rs, calls.rs,
record.rs, bench.rs; trunk-app config.rs, session.rs, stats; trunk-pro main.rs
(replay), plugins; SDK docs ("65535 and down"); web `SystemStatus.index`.
Large but mechanical.

### 2. The kind of voice is a bag of flags

- [x] `Message.voice` / `Call.voice: Voice { P25, P25Tdma { slot }, Dmr { slot, color_code }, Nxdn(rate), Fm, Am }` (tuning stays with the conventional channel; RAN, NAC and tone stay as data heard) — ea241f1
- [x] `VoiceKind::of`, the cutoff in `SysHost::open_channel` (`VoiceKind::cutoff_hz`), `SdrMode` in `record::capture`, `Call::slot`, `Call::decryptable`, conventional's `heard_code` all become one `match` — ea241f1
- [ ] `VoiceSpec` loses the analog-only fields (`squelch`, `subaudible`, `analog: AnalogTuning`, `ignore_hz`); `VoiceDecoder` loses `set_squelch` / `set_ignore` / `carrier`
- [ ] `TrackerOut::Subaudible` becomes an FM-decoder accessor the conventional router reads, not a tracker output

**Evidence.** `Message` and `Call` carry `analog`, `phase2_tdma`,
`tdma_slot`, `color_code`, `nxdn`, `ran`, `nac`; "which protocol" is a
precedence chain of `if`s in `voice::VoiceKind::of`, `record::capture`,
`engine::SysHost::open_channel`, `calls::Call::slot` / `decryptable` and
`conventional::heard_code`. Nothing stops a message with both `color_code`
and `nxdn`. `VoiceKind::Am` exists but `VoiceKind::of` can never return it.
`dsp::AnalogTuning` is a union of FM (`deemph_tau_s`) and AM (`level_tau_s`,
`low_cut_hz`) knobs threaded through `VoiceSpec` into every digital decoder,
which fills them with defaults. `TrackerOut::Subaudible` is a no-op arm on the
trunked path.

**Blast radius.** calls.rs, message.rs, voice.rs, record.rs, sdr.rs,
engine.rs, conventional.rs, the four parsers, trunk-app `call_view`, plugin
protocol (field values unchanged if derived).

### 3. Conventional re-implements the trunked call pipeline

- [ ] `Conventional` becomes a grant source only (energy detection + row routing) and hands its heads' `TrackerOut`s to the engine's `Recording` / `pending` / `apply_pending` path
- [ ] `Live`, the info half of `Heard`, `ConvOut` and the second half of `slot_call` go
- [ ] One `CarrierMeter` per head and one threshold function from the noise profile; `VoiceKind::cutoff_hz()` replaces the two `match`es
- [ ] One `ToneDetector` per open head per transmission (today it runs twice, in `Tx` and `Live`)
- [ ] `ChannelShape { detect_half_bw, head_cutoff, meter_bw }` built once, instead of three functions offsetting a mode's width against the FM constants
- [ ] `Chan.cfg` is a mutated copy of `rows[0]`: split `ConvChannel { freq, mode, analog, system, rows: Vec<Row> }`

**Evidence.** `conventional::slot_call`'s `for (src, flags) in h.infos`
block is line for line the `TrackerOut::Info` arm of `engine::apply_pending`;
`Live { call, audio, frames, tx, reception }` mirrors `engine::Recording`;
`ConvOut::{Start, Update, Audio, End, Alias, Position, WrongKey}` is a second
event vocabulary mapped 1:1 onto `Event` in `Core::emit_conv`. Conventional
calls never get `freq_error` (`Held { freq_error_hz: None }`) though their
decoder exposes `offset_hz()`. The squelch threshold is computed in
`SysHost::open_channel` (noise × 6 dB), in `conventional::Chan` every block
(`base − CLOSE_BELOW_DB`, floor 3 dB) and pushed into the decoder each run;
`ChannelFilter` meters run inside the analog demod, again for digital
conventional channels, and a third time for trunked channels. The noise floor
under a frequency is computed four ways (`engine::slice_at`, inline in
`open_channel`, in conventional.rs, in survey.rs with a hard-coded 64).

**Blast radius.** conventional.rs (~400 lines), engine.rs (~100), voice.rs
trait, fm.rs / am.rs push signatures. Conventional's tests run through
`Engine`, so coverage survives.

### 4. The per-source thread split left mirrors and a shared-mutex decoder

- [ ] Drop `Radio.chans` (a `Call.followed` flag set by `admit` lets `follow()` count from `t.calls`)
- [ ] `VoiceParams` travels with the tap (or in `BlockDsp`) instead of being mirrored in `Radio` and every `SourceRx`
- [ ] One `Vec<SourceConfig>` (today: `Radio.cfgs`, `Shared.sources`, `SourceRx.src.cfg`)
- [ ] The tap owns its `ControlChannel`; `Step` / `CcResult` carry what the trunk needs each block (good/bad deltas, notes, `control_hz`, `offset_hz`, `voice_params`); cold queries (`bandplan`, `status`, `report`) through a snapshot. The `Arc<Mutex<Box<dyn ControlChannel>>>` and its lock-order comment go
- [ ] Carry `VoiceOut.t` through `routed` / `pending` and use it for transmissions, timeouts and `audio_lead`; set `Trunk.now_s = dsp.time` in both modes and delete `cc_start_s`, `cc_samples`, `cc_offset_s`, `TapTime` (or delete `t` and say the control channel's clock is authoritative)
- [ ] `enum Listening { Hunting { source, head, index, hz, ppm, … }, Watching(Vec<Carrier>) }` decided once in `Trunk::start`; `plan()` becomes `Copy` and is fetched once (today re-asked on every `measure()` and `channels()`)
- [ ] `apply_pending`: resolve `(trunk, recording, call)` once per `(site, id)` and dispatch (today up to three linear scans per output)
- [ ] `Quality` sink used as an RPC (`report()` fished by series name) → `VoiceDecoder::quality() -> ChannelQuality`; `ChannelSnapshot.kind` and `modulation()` as enums, not `&'static str`

**Evidence.** `Radio.chans` mirrors `SourceRx.channels[key].calls` and is
hand-synced in `open_channel` and `release`, which take the source lock
anyway; `voice_params` is kept in `Radio` and fanned out under every source's
lock in `Trunk::follow`. `type Cc = Arc<Mutex<Box<dyn ControlChannel>>>` is
locked by the source thread to push and by the trunk 13 times (`self.cc()`),
most on the per-block path, with the ordering rule in a comment. Every voice
decoder computes `VoiceOut.t` ("air time") and `Radio::run_channel` drops it;
audio is stamped with `Trunk.now_s`, the control channel's clock, though the
voice channel may be on another source. Three clocks coexist (`Trunk.now_s`,
`Core.now_s` advanced only by source 0, `Source::time`). `Trunk` has ten
hunting-only fields and one watching-only field, with the mode inferred from
`carriers.is_empty()`.

**Blast radius.** engine.rs, control.rs, voice.rs, calls.rs (one flag),
conventional.rs, bench.rs.

### 5. The interface protocol has no typed home in Rust

- [x] One serde enum each way in Rust (`trunk_app::protocol::{ToClient, Command}`), with `audience()` and `listen_may_send()`; `publish(ToClient)` the only way onto the hub (`deliver` and `survey.rs` go through it). Bodies still built as JSON ride as `Body`; typing them is follow-up work
- ~~Generate `protocol.ts` and `protocol.schema.json` from the Rust types~~ — decided against: protocol.ts stays the documented contract that third-party interfaces read; the enums are checked against it both ways instead
- [x] `hello` becomes `{ version, platform, api, capabilities }` followed by a replay of the ordinary messages through `publish`; `redact_hello`'s field list goes — 94fde4c (hello is last; `capabilities` is still to come)
- [x] `protocol_tests.rs` compares `ToClient::TYPES` / `Command::TYPES` with protocol.ts instead of scraping server.rs, and parses every documented command with `Command::parse`
- [x] `server::command` matches on `Command` with typed fields; a command that doesn't parse gets an `error`
- [ ] `radio_query` / `stats_query` move into trunk-app over `Shared` + a `UnitAliases` store (today the server re-reads every units CSV from disk per query because the `Session` is unreachable from it; `span()` is duplicated in trunk-web)
- [ ] One `SiteIdentity` (today `SiteIdentity`, `Identity`, `SurveyIdentity`, and `SurveySuggestion`'s flattened copy); `SmartnetBandplan` keys match `Site`'s so `applySurvey` stops renaming
- [x] `status` (every 500 ms, to everyone) = calls + a per-site summary; DMR/NXDN carriers, channel tables, adjacent and patches move to the topic-gated `decodeDetail`; counters live only in `stats` — df7b696 (counters still in status)
- [ ] Plugin and interface stores: one store concept keyed by kind (`store {kind}`, `install {kind, …}`, `installProgress {kind, key, stage}`), one state slice, one `<StoreCard kind>`; `Shelf::install` takes `&dyn Fn(Stage)` so the CLI doesn't fake a hub
- [x] `hello.capabilities` replaces the scattered `web` checks and the hand-kept drop list in `workerTransport.ts` — six (`service`, `drivers`, `plugins`, `interfaces`, `host`, `keys`); `web` stays only where the page itself hosts the engine (WebUSB, the wake lock, browser storage, picking files)

**Evidence.** Around 160 `json!` sites build messages by hand (session.rs 37,
server.rs 32, stats/radio.rs 37, runtime.rs 18, survey.rs 17…);
`Session::status_json` builds site Values then mutates them to group into
systems by linear search. `hello` carries `phase`, `survey`, `host`, `keys`
with their `type` tags embedded, plus config, devices, 300 history entries,
unit CSVs, heard, radios, events; the controller stashes `pluginRuntime`
until the separate `plugins` message arrives. Access gating is spread over
`access::LISTEN_COMMANDS` / `CONTROL_MESSAGES`, `runtime::publish`, the
per-socket filter in `server::session`, an inline list in `hello_json` and
`redact_hello`; `runtime::deliver` sends `Out::Text` straight to the hub,
bypassing classification (safe only because the Session never emits a
control-only type). `protocol_tests::every_message_type_is_declared` reads
server.rs as text and scrapes `"type": "` literals and the `command()` match
arms. `interfaces/cli.rs::install` builds a throwaway broadcast hub, calls
the browser-facing `Interfaces::install`, and parses its own progress JSON
back out of the hub.

**Blast radius.** Large but mechanical; removes a whole class of drift. The
store registry's third-party interfaces (Trunkamp, Trunk City 2000,
Trunk-tris) read the protocol, so land it in one release and update them.

### 6. Config logic lives twice, in Rust and TypeScript

- [x] trunk-web exports the config rules (`rules.rs`: normalize with migration, problem, name problem, centres, auto-centre, usable width, band plan, channel CSV read/write); `web/src/rules.ts` loads them before render and keeps answers for the store's config; config.ts −280 lines, migrate.ts's system migration gone. Left in TS on purpose: the filename chip editor (stricter, UI), tone/talkgroup parsers used for inline hints, `normalizeSystem`/`normalizeSite` as builders for new cards and the Trunk Recorder import
- [x] Rust config enums: `SystemKind`, `SmartnetMode`, `NxdnType`, `Modulation`, `NxdnRate`, `Vocoder`, `M4aEncoder`, `UnitNamesMode`, `LogColor`; a wrong value fails at load like a wrong type ("" still the default). Left: `Site` stays one struct with every protocol's fields (a `SiteProtocol` enum would change the JSON shape Setup and the store's interfaces read)
- [x] Talkgroup files as companion documents (`<short>.talkgroups.csv` beside the config; the browser keeps them in its storage), `talkgroups { system, csv }` / `setTalkgroups`; older configs' moved out by migrate.rs
- [ ] `setConfig` as a patch (per-section objects or `{ path, value }`) with a `version` the server bumps and rejects on mismatch; drop `controller.updateConfig`'s echo suppression (the version and the refusal are done, 7a49384; not as a patch yet)
- [ ] Declare the per-system `Recording` overrides once (today the 16 fields appear in `Recording`, `RecordingOverride`, the `take!` macro, the TS type and the docs table); resolve each system's rules once at start instead of `recording_of` per concluded call in both session.rs and runtime.rs
- [x] Onboarding shares Setup's rules (the rename bug is fixed: one `renameSystem` for both): one centre planner (today `planCenters` is a third algorithm with 15 kHz steps and 10 kHz DC clearance), `useShortName` (today `NameStep` doesn't call `renameSystemRefs`, so a rename there leaves plugin `x-system` references stale — a bug), `<RadioReferencePaste>` — done: `plan_centers` beside `auto_center` in config.rs (one off-the-DC-spike rule, exported as `planCenters`); one `renameSystem` in the controller moves the talkgroup file and plugin references for Setup (trunked and conventional) and the guide; removing or merging a system drops or moves its talkgroup file. The paste box stays two presentations over one parser (`parseRadioReferencePaste`): inline in Setup, a numbered walkthrough in the guide
- [ ] Setup cards keyed by a stable id instead of position + list length + `configEpoch`; delete `configEpoch` and `forgetCard`
- [x] `migrate_keys`, the `server.home` removal and the old `interfaces` list form moved into `migrate.rs` as JSON rewrites, so the documented delete procedure is true

**Evidence.** About 700 lines of `web/src/config.ts` mirror config.rs, each
marked "the recorder's X": defaults, `normalizeSystem` / `normalizeSite`,
`storedConfig`, `migrateSystems`, `resolvedCenters` / `autoCenter`,
`channelTalkgroups`, `nameProblem`, `startProblem`, `bandplanProblem`,
`siteLockFields`, `watchedChannels`, `parseChannelCsv` / `channelsToCsv`,
`filenameFormat.ts`, `tones.ts`, `talkgroups.ts`. They have already drifted:
the RTL transfer-size rule (`bufLen % 512`, `buffers ≤ 256`) is Rust-only;
`filename::problem` had no caller in any crate (fixed: `Config::problem` checks every format), so a bad format set by hand
or by a third-party interface passes `Config::problem()`; the two validators
check the capture-file path in different positions. Rust config enums are
strings with silent fallbacks (`kind_name()` falls through to P25;
`modulation` anything-else → both receivers; `Rate::parse(..).unwrap_or(N48)`)
while the TS side has string-literal unions. `talkgroupsCsv` lives inside
the config and is shipped whole on every 300 ms-debounced keystroke, to the
server and back to every client; `controller.ts` drops any `config`
broadcast while a save is pending (last writer wins on the whole document).

**Blast radius.** config.ts (~800 → ~200 lines), migrate.ts, filenameFormat.ts,
tones.ts, talkgroups.ts, Setup.tsx keys, Onboarding (~400 lines), server.rs
setConfig, runtime `EngineCmd::Talkgroups`, docs/api.

### 7. trunk-pro's lifecycle and engine inlets

- [ ] `Ctx.lifecycle: Mutex<Lifecycle>` with `enum Lifecycle { Idle, Recording(Runner), Surveying(SurveyRunner), Profiling, Quitting }` and `begin_recording` / `begin_survey` / `stop` / `quit` that own the transition and publish `state`; `phase` derived
- [ ] One `mpsc::Sender<EngineCmd>` in `Ctx` (`Talkgroups`, `Keys`, `Topics`, `Plugins(Option<Arc<PluginHost>>)`, `SourceError`, `SourceEnd`); the engine loop is `recv_timeout` on one channel; `topics_gen`, `engine_cmds` and the `host.read()` in the loop go
- [ ] `plugins.host: Mutex<Option<Arc<PluginHost>>>` (or `arc_swap`): clone once per iteration, hold no lock during work; `finish_one` takes the clone; `reload` is a plain store
- [ ] `Plan` is the one authority for a call's files (`AudioDisposition { Wav, M4a, M4aElseWav }`); `finish_one` materialises it before handing `CallAudio` to the host, so `PluginHost::concluded`, `encode_worker` and `CallAudio::write_wav` never write files; the Finish-queue-full fallback keeps the rules intact (or blocks for a bounded time now that decoding is off the engine thread)
- [ ] `logging::init` takes the `Hub`; `record()` publishes the `log` message with its real time and level; the six hand-built `publish(json!({ "type": "log", … }))` sites become `log::warn!`
- [ ] One generic `run_with<R: Radio>` in sdr.rs owning the reopen loop, back-off, ctl handling and `Tuned` reply; `runtime::start` and `survey::spawn_source` share one `spawn_source`
- [ ] `plugins/cli.rs` and `interfaces/cli.rs`: `search` / `update` / `say_source` into store.rs generic over `Index`; both CLIs thin
- [ ] Analysis tools share `Capture::from_args`, `channelize`, `hex`, `AudioOut`; `tool::run` becomes a `match`
- [ ] `replay` runs through the `Session` / `Feeder` / finish path (a `Config` of `Source::File` entries with a headless sink) instead of driving `Engine` directly with its own pacing and call writing

**Evidence.** The real state is one enum but lives in `lifecycle:
Mutex<bool>` (the lock is the mutual exclusion; the bool is one flag),
`runner`, `survey` and a `phase` string; the twenty-line take-the-locks dance
is copied into the `start`, `surveyStart` and `profileSource` arms of
`server::command`. `Ctx` has 21 fields, 13 of them locks; the engine loop
polls five inlets every 20 ms (the feeders' ctl channel, `plugins.host`
RwLock, `hub.receiver_count()`, `topics_gen`, `engine_cmds`). The
`PluginHost` read guard is held across `poll`, `deliver` and, when the finish
queue is full, a synchronous `finish_one` on the engine thread, so a plugin
reload waits on disk; `finish_one`'s signature threads `host:
Option<&PluginHost>` down so nobody re-locks. Whether a WAV exists on disk is
decided in `archive::plan`, `host::concluded`, `encode_worker`,
`runtime::finish_one` and `CallAudio::write_wav`, and `archive::settle`
re-derives it by probing the filesystem; the queue-full fallback rewrites the
call's `FileRules` (`compress_wav: false`), so that call is archived under
different rules than its system's. `run` / `run_with` and the retune block
are copied in sdr.rs, uhd.rs, airspy.rs, soapy.rs with different back-off
constants and already-diverged behaviour. `capture()` is byte-identical in
dmrtool.rs and nxdntool.rs; the channelizer feed loop appears seven times;
`hex` three times.

**Blast radius.** runtime.rs, server.rs, manage.rs, host.rs, archive.rs,
logging.rs, the four drivers' outer shells, both cli modules, the tools,
main.rs replay. Each item is small to medium on its own.

### 8. The call record travels as a string

- [ ] One serde `CallRecord` in trunk-core that `record::save_call` produces and the finish thread serialises once when writing `<rel>.json`; the SDK re-exports it; TS generated from it (theme 5)
- [ ] `Concluded { record: CallRecord, … }`, `Output::File { record, … }`; drop the parallel `entry: Value`
- [ ] `Stats::observe` and `Session::handle` read fields, not JSON keys (`"errorList"`, `"start_time"`)

**Evidence.** `record::call_record` writes the call JSON with a format
string; `Concluded.json: String` is re-parsed in `Session::handle` (for
`start_time`), `Stats::observe` (for `errorList`), `plugins/host.rs` (as
`CallRecord`) and `plugins/cli.rs`; `Output::File` carries the same record
as both a string and a `Value`. The SDK's `CallRecord` re-types ~20 fields
with a flag deserializer, so a plugin round-tripping a record writes
`true/false` where the file had `0/1`; `protocol.ts` re-types all ~45 fields,
beside `CallInfo`, `CallView` and `CallEntry`.

**Blast radius.** record.rs, session.rs, stats/mod.rs, runtime.rs finish,
plugins host/cli, SDK protocol.rs, protocol.ts, protocol_tests.

## Second tier

Local cleanups; none changes the model. Do them when a nearby change touches
the file.

**Protocol decoders**
- [ ] DMR and NXDN "watched site" are one skeleton written twice (~250 lines: `Site`, `Carrier`, `Pending`, `LEARN_WINDOW_S`, `CONTROL_HOLD_S`, `resolve`, `set_learned` byte-identical, `learn`, `status`, the rate-changed guard, the four-way `(group, repeat)` table ×4). A `trunk::watched::Site<P: WatchedProtocol>` and `MessageType::voice(group, update)`. The two have already drifted (DMR `Pending` has `slot`/`low8`; NXDN has `unknown` and typed notes)
- [ ] `dmr::trunking::Site::grant` takes 13 positional arguments, five of them `bool`/`u8`: a `Grant` struct
- [ ] `P25Parser::parse` (245 lines) does field copy and `meta` formatting in one 40-arm match: split `message(&Osp)` and `meta(&Message, &Osp)`; `smartnet::Parser::process` (196 lines of nested if/else) as a table of (first-OSW predicate, arity, handler)
- [ ] `trunk/` isn't protocol-neutral: `P25Parser` (message.rs), `VoiceTracker` (tracker.rs) and `TdmaTracker` (tdma.rs) are P25-only and import `p25::*`; move to `p25/trunking.rs`, `p25/voice_tracker.rs`, `p25/tdma_tracker.rs`. `Message` carries P25-only `wacn, nac, rfss, duplex, mode, priority, callsign` and SmartNet-only `analog`
- [ ] DMR, NXDN and P25 Phase 2 framers are the same grid framer with different constants (`buf`, `win`, `next_end`/`next_fsw`, lock/grid/unlock thresholds, the dibit/rel emit loop, `bit`/`soft`/`voice_frame`): `dsp::GridFramer<const LEN>` + `SoftFrame<const N>`. P25 Phase 1's framer is genuinely different and stays
- [ ] Block codes: Golay/Hamming by syndrome tables, Chase-II and brute-force ML in `p25/fec.rs`; `dmr::fec::Small` (the right shape) covering Hamming(7,4), Golay(20,8), QR(16,7); `hamming15_decode_soft` is `Small::decode` with k=11; GF(2^6) hand-written in `p25/frame.rs` and built at runtime in `p25/fec.rs::gf`; `ambe.rs` (shared by three protocols) imports `p25::fec`. A crate-level `fec/` with `BlockCode { n, k, parity }` and one `Gf<const M>`
- [ ] Three Viterbi decoders (nxdn/fec.rs 16-state, smartnet/osw.rs 2-state, p25/tsbk.rs 4-state) and five CRC routines (`bits::crc_ccitt`, `nxdn::fec::crc`, `smartnet::osw::crc`, `p25::mbt::crc32`, `p25::phase2::crc12` which allocates per PDU): `viterbi<const S>` and `Crc { width, poly, init, xorout }`
- [ ] `ControlChannel`: `Step.good` is a CRC pass for P25/SmartNet but "produced a Message" for DMR/NXDN (an idle Capacity Plus site reads as dead); `Step.identity` cloned per TSBK; 8 of 20 methods matter to one protocol (`restart`, `forget_site`, `reopens`, `identity_fields`, `voice_params`, `offset_hz`, `patch_hold_s` for Hunt; `control_hz` for Watch; `push(carrier, …)` ignores `carrier` for Hunt). Derive `good` from `counts()`; `identity: Option<Identity>` on change; split the trait by `CarrierPlan`
- [ ] Soft decisions carried four ways (`Option<&[f32]>`, empty-Vec-means-hard, `rel_hi = -1.0` sentinel, `BankConfig.soft` / `TdmaTracker.soft`); P25 `Frame` keeps `bits`, `raw`, `soft`, `raw_soft` and clones all four per frame per receiver. `Symbol` always carries reliability; one signed soft array; remap `VOICE_CODEWORD_BITS` etc. to stripped positions at compile time as `TB` already is
- [ ] `tables.rs` (1322 lines): `VOICE_CODEWORD_BITS`, `HDU_CODEWORD_BITS`, `LDU_LS_DATA_BITS` (formulaic interleave positions), `ISCH_CODEWORDS`, `DUID_LOOKUP` (from which `phase2::duid_codewords` reverse-engineers the 16 codewords at runtime), `LFSR_SEED_MATRIX`, `GF_EXP`/`GF_LOG`, `INVERSE_ODD`: `const fn` generators next to their decoders; keep the vocoder and spec-data tables
- [ ] Bit-field helpers re-rolled per module (`Octets` in osp.rs is the good one; `mac.rs`, `lc.rs`, `layer3.rs`, `bits.rs`, `nxdn/fec.rs`, `dmr/fec.rs`, `p25/voice.rs` each have their own); `fn mhz` ×3 with two formats; 15 xorshift/LCG copies in tests and synths; the dibit→level match ×5
- [ ] Names: `smartnet::ControlChannel` (a struct that does not implement `trunk::ControlChannel`); `Channel` is four unrelated things (`p25::osp`, `nxdn::layer3`, `dmr::slot`, `trunk::engine`); `Site` is three; `Alias.source` / `Position.source` carry Trunk Recorder's strings ("MotoP25_FDMA", "KenwoodNXDN") as `&'static str`; `NxdnVoice::take_messages` has no caller
- [ ] `C4fmOptions::legacy` / `"legacy"` is the only "legacy" in the engine; decide whether the first receiver is still worth keeping as an option

**Conventional, DSP, survey**
- [ ] `dsp/am.rs` copies `squelch::Gate::meter`'s running-sum boxcar verbatim (same comment) to OR in the line detector; `smartnet/rx.rs` hand-rolls it a third time: a `filters::Boxcar` and `Gate::meter(also_up)`
- [ ] `fm.rs` is the analog commons (`AUDIO_RATE`, `ChannelFilter`, `AudioResampler` imported by am.rs, squelch.rs, tones.rs, signalling.rs) and `Nbfm` is the wide-FM path too: `dsp/analog.rs`, `Fm`, `Am`
- [ ] The survey duplicates the control-channel decoders, not the channelizer: `survey::Decode::group` ≈ `control::P25::groups`, `SnRx` ≈ `smartnet::ControlChannel`, `survey::modulation()` ≈ `P25::modulation` with different thresholds (≥4 vs ≥8 frames, `>` vs `>=`) so the survey and the dashboard can disagree. A probe holds a `Box<dyn ControlChannel>` from `control::build`; the survey keeps `find_peaks`, the hop plan, gain stepping, the ppm fit, the SmartNet plan finder. `noise: vec![0.0; 64]` predates `channelizer::floor_slices`
- [ ] `Receiver` is implemented only by `C4fm` and `Cqpsk` (`Fsk2` emits `Bit`, `Am`/`Nbfm` have their own `push`), used polymorphically once; `offset_hz()` declared on four traits, `quality()` re-exported by hand as `report("sep")`. Either `type Out` or rename `SymbolReceiver`; one `Measured { offset_hz, quality }`
- [ ] `dsp/mod.rs` header says "the P25 symbol receivers and analog FM and AM"

**Session and stats**
- [ ] `Session` keeps two logs of every event (`log: Vec<Value>` for the topic, `records: Vec<Record>` for the platform), built with different site labels (`site_label` vs `site_name`) and `self.engine.status()` called just to stamp `timeS`: `Record` grows `site` and `time_s`, `flush_log` renders to JSON only while someone subscribes
- [ ] Per-source counters in `session::SourceStats` and `stats::SampleMeter` behind two mutexes, both updated by `Feeder::fed` / `gap` and `source_error`: one `SourceMeter`
- [ ] `Session::heard_file(_cfg)` ignores its argument; interface topics live in `Stats.topics` and are read as `self.meas.topics.has("log")`; `radio_query` re-parses every talkgroup CSV per query; `Channel::engine_channel` sets `system: 0` and `analog` only for `engine_channels` to overwrite both
- [ ] `dash/data.ts::siteKey` mirrors `SiteConfig::stats_key`'s "a site named as its system is under the system's key" special case; with no back-compat, `sys/<x>/site/<name>` always is simpler

**Plugin SDK**
- [ ] `Status.systems[]` stayed flat per site after #51 (a plugin sees "system dcfd" N times): `systems[].sites[]`
- [ ] `Metrics` / `Endpoint` are camelCase, every other message snake_case
- [ ] `SystemInfo.kind`, `CallInfo.reason`, `UnitEvent.kind` are `String` where trunk-core has enums
- [ ] `API_VERSION = 1` with a gate (`sdk::run`, `plugins/mod.rs`) that can never reject anything; `docs/plugins/writing-plugins.md` still says `trunk-recorder-plugin = "0.2.1"`. Drop `api` and compare the crate semver, or make the gate real
- [ ] `CallQueue`: `Attempt::Skip` belongs before enqueue; `QueueOptions.noun` / `endpoint` are presentation strings in a queue

**Docs and comments that no longer match**
- [ ] `docs/developer/architecture.md`: "All DSP and decoding for all sources runs here, single-threaded" / "One engine thread, push-driven"; the threads table has no `dsp-N` row; §7's queue table says "Source → engine"
- [ ] `trunk/engine.rs` module doc: "Each site of a multi-site system is a system of its own"; `Channel.site` "The system whose grant opened it"; `SourceRx.channels` "Keyed by (system, frequency)"; `remove_taps(system)`, `decode_control`'s `(system, …)` take a site; `Engine::load_bandplan(system)` indexes sites; `Engine::twins` doc says "(call, system)" and returns the site; `Trunk.cfg: SiteConfig` is read for system policy; `Call.analog` doc "(a conventional FM channel)" though SmartNet sets it
- [ ] `trunk-pro/src/main.rs` header: `--ui` "at `/` instead; the built-in one is at `/builtin/`" (0.1.14 removed both)
- [ ] `radio/airspy.rs` header: "libairspy does the real-to-IQ conversion" (0.1.15 converts on the dsp thread by default)
- [ ] `server.rs` `Listen` has two stacked doc comments, the first ("65535: conventional") from before short-name listening
- [ ] `session.rs` `threaded: bool` keeps both the single-threaded `push` path (trunk-web only) and the `Feeder` path alive, with `fill_quiet` branching per source: give the web build a `Feeder` or document `push` as wasm-only
- [ ] `runtime::older_calls`'s `migrate::old_names` is the only place the server knows the pre-sites layout; date it so it can go with migrate.rs
- [ ] `rtlsdr-nusb`: the fork is 102 insertions (`RxBuffers`, `rx_stream_with`) and the README says so; offer it upstream so the vendored copy can go

## What is already clean

Worth knowing so the refactors above don't disturb it.

- The core/app boundary: trunk-core and trunk-app have no I/O and no wall
  clock; `Output` is a small, honest enum that trunk-pro's runtime and
  trunk-web's `to_js` consume without peeking into the session.
- The threaded pipeline `push_iq → decode_block (source lock) → BlockDsp →
  decode_control (per-cc lock) → after_block (core lock)` is clear, the lock
  order is written where it's decided, and
  `sources_fed_from_threads_decode_as_from_one` pins the bit-identical
  promise. `Feeder`, `ClockFit`, gap filling, `Air` pacing and `Pool`ed
  buffers are coherent and documented in code; only the prose drifted.
- The channelizer is the one spectrum: conventional detection, the survey,
  the dashboard and pre-roll all read the same per-block FFT, with one
  calibrated noise model (`noise_profile` / `noise_in_band`).
- `VoiceDecoder` + `voice::build` and `ControlChannel` + `control::build`
  are real seams: AM slotted in as a `Demod::Am` arm; DMR's and NXDN's
  trackers are peers of P25's; `TrackerOut` is a vocabulary every protocol
  speaks. `CallManager` + `RecorderHost` lets the call lifecycle be tested
  with a stub host.
- `multisite.rs`, `identity.rs`, `patches.rs`, `Transmissions`,
  `CallFrames::keep`, `save_call`, `squelch::Gate`, `AudioResampler`,
  `ChannelFilter`, `filters.rs`, `real_iq.rs`, `loudness.rs`, `wav.rs`,
  `ambe.rs`, `bits.rs` are small, single-purpose, tested.
- The typed message layers (`p25::osp::Msg` with `Octets`, `p25::lc`,
  `p25::mac`, `nxdn::layer3`) each have a `build` module, so every decoder
  has its encoder beside it and the synths are exercised end to end through
  `Engine`. `nxdn::fec::Coding` and `dmr::fec::Small` are models of
  table-driven design. Talker-alias assembly is not duplication (three wire
  formats, one `Alias` output). `Keyring` / `Keystream` zeroize keys;
  `keys.rs` / `ekc.rs` never echo one in an error.
- `metrics::Sink` / `Instrumented` pull their weight; `Aggregator`,
  `Rollup`, `History`, `Registry`, `Monitor`, `Shared` are each under 200
  lines and know nothing about key names.
- The plugin subsystem's pieces are well bounded and tested on their own:
  `delay.rs`, `hold.rs` (`Outlet` trait), `archive.rs` (the exhaustive keep
  table), `encode.rs`, `spool.rs`; `store.rs` is a real shared core with thin
  plugin / interface specialisations. `interfaces/serve.rs` (reconcile with a
  watch stop per listener, `Door`, path containment) is tidy. `access.rs` is
  the right home even if the gating is sprinkled.
- The web controller is a small hand-rolled store done right
  (`useSyncExternalStore`, `useSelect`, reference-counted `useTopic`,
  `ask()` with ids and a timeout, `series.ts` rings outside React);
  `listen.ts` follows the same shape; `fieldText.ts` / `inputs.tsx` separate
  codecs from inputs. The sites restructure landed in web/ without stitching:
  none of `multiSiteGroup`, `bySite`, `regrouped`, `siteSiblings`,
  `MultiSiteChip` survive.
- `config.rs` keeps `other` maps at every level so unknown keys survive
  round-trips; `write_atomic` is used consistently; `migrate.rs` is isolated
  at the JSON level with a delete procedure (modulo the three stragglers in
  `Config::load`).
