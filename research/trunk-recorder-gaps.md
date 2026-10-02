# Trunk Recorder features not in Trunk Recorder Pro

Re-evaluated 2 October 2026. Method: every key Trunk Recorder's config reader
takes (`config.cc`: global, source, system, `audio_postprocess`, plugins),
its recorders, decoders and plugins (`plugins/`, `docs/Plugins.md`), each
searched for in this repo (`crates/`, `web/src`, the `trunk-plugin-*` repos).
The Trunk Recorder source is the fork in `~/Projects/Trunk Recorder/source`.

## Major gaps

None left open. The three that were here are decided:

- **Talkgroup priority**: not needed for running out of recorders (a pool
  of 32 costs little CPU here). Its other use, never recording a talkgroup,
  is the new **Ignore** column (below).
- **One file per transmission** (`conversationMode: false`): researched,
  left out for now (below).
- **stat_socket, unit_script, streamer** and the community plugins
  (MQTT, Prometheus, decode-rate and daily logs): **won't be ported**.
  The plugin protocol's `status` and `unit` topics are there if someone
  writes one.

## Smaller gaps

**Recording rules**
- `hideEncrypted`, `hideUnknownTalkgroups` (log only).
- `newCallFromUpdate`: in the engine, always on; not a setting.
- Preferred site by SmartNet `multiSiteSystemNumber`: only Preferred NAC
  (as a NAC or RRRRssss) and Preferred Site (short name) are matched.

**Files and output**
- `audio_postprocess`: loudness here is our own normalisation. Missing:
  high-pass, low-pass and band-reject filters, `ffmpeg_filter`, the
  loudnorm targets, and `outputRawAudio` (an unprocessed copy).
- Call JSON: `source_num` and `spike_count` are 0; `freqList` has one
  entry; `srcList`'s `signal_system` is empty.
- `compressBitrate` per system: the M4A bitrate is one setting for all.
- Defaults differ: Trunk Recorder's `compressWav` is on by default, here
  off. An imported config that relied on the default gets no M4A files.

**Names and tables**
- `customFrequencyTableFile` (P25).

**Analog signalling**
- Star (`decodeStar`) and TPS (`decodeTPS`): researched, left out (below).
- FleetSync II's interleaved ECC framing: only plain FleetSync is read.

**Radios and DSP**
- The `iio` driver (PlutoSDR and similar, natively; through SoapySDR it works).
- `sigmf` file sources, `iqType`, and `repeat` (loop a capture).
- A source's `enabled` switch.
- `deemphasisTau`: fixed at 750 µs.
- AutoTune is measured on P25 and SmartNet control channels only, and not
  applied to conventional channels.

**Debugging — left out for now** (not clear anyone uses them)
- Per-call IQ (`sigmfRecorders`, `conventionalSIGMF`) and `debugRecorder`.
  Here: whole-band `trunk-pro capture`, vocoder-frame capture per call, and
  `trunk-pro tool`.
- `silenceFrames` (undocumented; not checked what it does).

**Logging**
- Windows has no system log here (console and files only).

## Won't be added

- Plugins: stat_socket, unit_script, streamer, and the community ones
  (MQTT status, Prometheus, decode-rate logger, daily log).
- Talkgroup priority as a recorder-shortage rule (Ignore covers priority −1).

## Deliberately different

- Recorder counts per source (`digitalRecorders`, `analogRecorders`,
  `dmrRecorders`): one shared pool, `maxRecorders`.
- `squelch` in dBFS and `signalDetectorThreshold`: squelch here is dB above
  the measured noise floor, per section and per channel.
- GNU Radio knobs: `qpskGainMu`, `qpskCostasLoopBw`, `filterWidth`, `maxDev`.
- `tempDir`: there are no temporary transmission files.
- `controlRetuneLimit`: the next control channel is tried after 5 s, always.
- `softVocoder`: replaced by the P25 voice decoder choice.
- `multiSite` / `multiSiteSystemName`: replaced by site groups; every
  site's copy is recorded and the best kept.
- `decodeMDC` / `decodeFSync`: always on.
- `error` (Hz): imported as ppm. osmosdr device strings become RTL-SDR,
  Airspy or SoapySDR sources.
- Plugins are separate programs from a registry, not C++ libraries.
- The call JSON's `signal` and `noise` are dBFS here (Trunk Recorder's
  are labelled dBm but come from the GNU Radio blocks' own scale).

## Closed since the September list

- **DMR**: conventional and trunked (Capacity Plus, Connect Plus, Tier III,
  Capacity Max), with `lcnTable`.
- **Plugins**: OpenMHz, Broadcastify Calls, Rdio Scanner, simplestream,
  upload-script, with talkgroup allow / deny and Broadcastify's OTA aliases.
- **SoapySDR** sources: HackRF, SDRplay, bladeRF, LimeSDR and the rest.
- **Duplicate calls across sites**: every copy recorded, the best kept.
- **CTCSS, DCS, NAC and colour code** on conventional channels (below).
- **Call rules** (October): `minDuration`, `maxDuration`,
  `minTransmissionDuration`, `digitalLevels` / `analogLevels` (as dB),
  `compressWav`, `audioArchive`, `callLog`, `archiveFilesOnFailure`,
  `filenameFormat`. Each is set on the Recording tab and may be set again
  per system (and for the conventional channels) under Recording override.
  `digitalLevels` and `analogLevels` are system settings in Trunk Recorder
  too (`config.cc`), not source settings as the September list said.
- **Sources** (October): `agc` on every kind; the Airspy's sensitivity mode
  and its LNA / mixer / VGA stages; SoapySDR gain stages one by one;
  `autoTune`; the call JSON's `freq_error`.
- **Ignore** (October): an `Ignore` column in the talkgroup file (`true`,
  `yes`, `1`, `x`) marks talkgroups never to record; Trunk Recorder's
  priority −1 counts too. Such calls show "ignored" and aren't followed.
  The system card says how many are ignored.
- **Unit names** (October): Trunk Recorder's `unitTagsFile` (`unit,name`
  lines; `/regex/,Name $1` patterns with groups, as boost's `$1` / `\1`)
  and `unitTagsMode` (user first, OTA first, user only, none), per system
  and for the conventional channels; loaded into the config. The call
  JSON's `srcList` `tag` is the name by the mode, `tag_ota` the alias
  heard; the interface names radios the same way. Imported with a Trunk
  Recorder config (a missing file is a to-do).
- **Reception** (October), on every saved call:
  - `signal`: the channel's power while it carried the call's voice, dBFS;
  - `noise`: the noise floor under the channel (from the source's
    spectrum), dBFS;
  - `snr`: their difference, dB;
  - `clean_voice_pct`: the share of voice frames decoded cleanly (not
    repeated, muted or lost), digital calls only.

  With `error_count` (bit errors FEC corrected) and `errorList`, that says
  how strong the call was and how well it decoded. On the DCFD capture the
  trunked calls read 26–30 dB SNR, the same voice channel set up as a
  conventional channel 27–28 dB. The call list shows the SNR (green from
  20 dB, amber from 10), with the levels and the clean share on hover.
- **Logging** (October): Trunk Recorder's line format,
  `[2026-10-02 10:43:53.822506] (info)   [dcfd]	1C	TG:       3747	Freq: 859.037500 MHz	Concluding Recorded Call - …`,
  with its options: `logLevel`, `consoleLog`, `logFile` / `logDir`
  (daily and 100 MB files, Trunk Recorder's names), `syslogFriendly` (one
  file for logrotate; SIGHUP reopens it), `logColor` (and `NO_COLOR`),
  `frequencyFormat`, `talkgroupDisplayFormat`, `statusAsString`,
  `controlWarnRate`; plus `syslog` (the C library's syslog(3): journald /
  rsyslog, macOS's unified log) and `--log-level`. The console is stderr,
  so it can be redirected apart from anything else. Every 200 s a status
  summary, as Trunk Recorder's: active calls and their states, recorders,
  decode rates. Set in the config's `log` section, or Setup → Recording →
  Log, applied at once. Imported from a Trunk Recorder config.

  How it is built: the session describes what happened as data
  (`trunk-app/src/log.rs`: a level, the system, the call, and what — a
  recording started, a call saved with its reception, a decode rate…); one
  function words it (`line`, with the format options); the desktop's
  logger (`trunk-pro/src/logging.rs`) stamps the time and sends it where
  the settings say. Messages from elsewhere in the app use the `log`
  crate's macros and reach the same logger. Nothing that logs knows about
  formats or destinations.
- **Several conventional systems** (October): `conventional` is a list;
  each system has its own short name, channels or channel file, squelch,
  Recording override, unit names and plugin settings, and calls numbered
  65535, 65534, … (`conventional_system`), so plugins see each as a system
  with its own settings. A frequency belongs to one system. The Trunk
  Recorder importer makes one per conventional system instead of merging
  them.
- **Wrong in the September list**: "automatic deletion of old files" isn't
  a Trunk Recorder feature (no retention keys in `config.cc`).

## conversationMode: researched, left out

**In Trunk Recorder today it does nothing.** `config.cc` reads
`conversationMode` (default true) and the system stores it, but nothing
calls `get_conversation_mode()` (checked: `call_impl.cc` defines it, no
caller in `trunk-recorder/` or `plugins/`). Since 2021 every call has been
its transmissions joined into one file whatever the setting. A real
implementation exists only on the unmerged branch `dev/conversation`
(May 2025, +115 / −28 lines in 7 files): the call concluder makes one call
per transmission, each with its own times and file, and uploads each.

**Who asked**: Broadcastify's owner (issue #326, 2020, still open: separate
clips per transmission make duplicate handling across nodes easier), and a
handful of users (#813, #385). Importing `conversationMode: false` today
behaves exactly like Trunk Recorder: one file per call.

**What it would take here**: little. Transmissions are already marked in
each call's audio (`record.rs` `Transmissions`, the same marks
`minTransmissionDuration` uses), and multi-site dedupe works on whole calls
first. `write_call` would cut the audio at the marks and save each piece as
a call (its own start time, the radios that spoke in it, a new call number;
the first keeps the original). About 150–200 lines in 5 files, a system
setting (and a per-talkgroup override, as the branch has), no CPU cost.
What grows is files and uploads: every piece is a WAV, a JSON and an
upload to each plugin, 3–10 times as many on a dispatch talkgroup. Risks:
quick back-and-forth can merge (the boundary is a 0.5 s gap in the audio;
a change of talker could split too), and file names need milliseconds.

**Decision**: leave it out. Trunk Recorder itself never shipped it; add it
when an upload service or a user asks.

## Star and TPS: researched, left out

**Star** is GE-Star ANI: a unit ID sent on legacy GE / Ericsson / M/A-COM
conventional analog radios (400 bps PSK on a 1600 Hz carrier, an 80-bit
block with a complement check and CRC-6). Trunk Recorder's decoder is
Matthew Kaufman's, 618 lines, **GPL-2.0-only**, so it can't be copied into
this GPL-3.0-or-later code. No Trunk Recorder issue reports anyone decoding
it; it appears only in configs that turn every decoder on. Writing one here
from the format: about 200–250 lines with tests, negligible CPU, but the
bit order, polarity and ID formats can't be confirmed without a recording
from a real radio.

**TPS** is Motorola's Tactical Public Safety (FDNY fireground): analog FM
voice with a P25 C4FM ID burst. Trunk Recorder's decoder (455 lines) feeds
its de-emphasised 16 kHz audio straight into OP25's slicer with no symbol
timing recovery or level scaling, so it **probably doesn't work**; its
issues (#894, #928) are crashes and log spam from people with no TPS signal
nearby, and nobody reports a decode. Here it would reuse the C4FM receiver
and P25 framing on the FM discriminator's output before de-emphasis (about
100–150 lines, one more C4FM demodulator per open analog channel), but it
can't be tested without a real fireground capture.

**Decision**: leave both out. Star if someone sends a recording; TPS if
someone near NYC sends a fireground capture.

## Suggested order

1. `audio_postprocess` filters and `outputRawAudio`, if anyone misses them.
2. AutoTune for conventional channels.
3. The `iio` driver (PlutoSDR) natively.

## Patching (now done, not yet committed)

The app now handles patches at least as well as Trunk Recorder:

- **Per-member expiry:** each talkgroup in a patch times out on its own. An M/A-COM delete removes only the talkgroup it names.
- **Patches on calls:** each call keeps every talkgroup patched with it during the call. It is written as `patched_talkgroups` in the call JSON, sent to plugins, and used for OpenMHz's `patch_list`. The UI shows active patches and the "patched with …" talkgroups on calls.
- **Late recording:** a call on an unknown supergroup starts recording when a known talkgroup is patched in, even if the patch arrives after the grant. Trunk Recorder decides only at the grant, and never connected its SmartNet patch code at all.

What a 5-minute WMATA capture showed:

- A patch is repeated about every 0.4 s (never more than 2 s apart) and cancelled three times when it ends.
- The grant arrives 0.1–0.4 s before the first patch message.
- Most of the traffic is multiselect: one console keying two talkgroups at once.

SmartNet patches now time out after 4 s without a repeat. P25 keeps Trunk Recorder's 10 s: DCFD had no patches in six days of logs, so it couldn't be measured.

Still missing: the Motorola patch-delete message isn't decoded, so Motorola patches end only by timing out.

## Analog signalling (done)

MDC1200 and FleetSync unit IDs are decoded on every analog call: conventional
FM channels and SmartNet analog grants. They go into the call's `srcList`;
an MDC1200 emergency (op 0x00) flags the call. There is no setting.

How it is built:

- **One file**, `dsp/signalling.rs` (about 335 lines plus 175 of tests). It
  takes the 8 kHz audio the FM demodulator already makes. Each of the two
  call sites (`conventional.rs`, `engine.rs`) adds about five lines: decode
  the audio, then report each ID the way P25 link control reports a source.
  Digital channels never touch it.
- **Written fresh, not ported.** Kaufman's MDC and FleetSync library, which
  Trunk Recorder uses, is GPL-2.0-only, so it can't be combined with this
  GPL-3.0-or-later code. It was read for the framing only, and used to check
  the new decoder in a scratch harness.
- **One front end for all three formats.** MDC1200 (1200/1800 Hz at 1200
  baud) and FleetSync (the same, or 1200/2400 Hz at 2400 baud) are all MSK.
  The audio is mixed down by 1200 Hz, then summed over each bit by 4 slicers
  with staggered bit clocks. That gives a complex value per bit with no
  timing loop. MDC compares each bit's phase with a reference taken from the
  values squared (coherent detection). FleetSync compares each bit's phase
  with the bit before.
- **A one-tap pre-emphasis comes first.** It undoes our de-emphasis. Without
  it, data a radio sends flat (not through its pre-emphasis) gets a
  wandering baseline after our de-emphasis, and 2400-baud FleetSync then
  never decodes.
- **Only MDC PTT IDs (op 0x01) and emergencies (0x00) count as the talker.**
  Radio checks, stuns and status messages can address another radio.

Cost: 0.11 s of CPU for 10 minutes of audio (0.018 % of a core), about 9 %
of what the FM demodulator itself costs, and only while an analog call is
open. The decoder state is 544 bytes, with nothing on the heap.

Against Trunk Recorder's decoders, on the same FM-demodulated audio (100
packets each, data at 2.5 kHz deviation, sent flat):

| Carrier-to-noise | MDC1200 here / TR | FleetSync 1200 here / TR | FleetSync 2400 here / TR |
|---|---|---|---|
| 16.5 dB | 100 / 78 | 100 / 100 | 100 / 0 |
| 11.4 dB | 100 / 83 | 100 / 100 | 99 / 0 |
| 8.2 dB | 100 / 74 | 98 / 89 | 90 / 0 |

Trunk Recorder runs its decoders on 16 kHz audio from its own FM chain, so
its numbers on its own audio would be higher. On clean audio at 8 kHz its
decoders do read 2400-baud FleetSync. Not yet tried on a real radio. The
framing was checked by having Trunk Recorder's decoders read packets from
this code's test encoder.

## Tones, NAC and colour code on conventional channels (done)

Trunk Recorder's PR #1137 (branch `dev/dcs`, not merged) adds CTCSS and DCS
to its channel file's `Tone` column. This app follows its model, with these
decisions (2026-10-01).

**One Tone field per row, read according to the row's mode.** Trunk
Recorder's channel file and RadioReference's listings both have one Tone
column for all of these. Input is forgiving, so whatever RadioReference
shows can be pasted:

| Mode | Accepted | Shown as | Empty means |
|---|---|---|---|
| FM, CTCSS | `151.4`, `151.4 PL`, `PL 151.4`, `151.4 Hz` | `151.4 Hz` | any |
| FM, DCS | `D023N`, `023 DPL`, `D023`, `023`, `D023I` | `D023N` | any |
| P25 | `293`, `293 NAC`, `$293`, `0x293` (`F7E` = any) | `NAC 293` | any |
| DMR | `CC1`, `1`, RadioReference's `CC1 TS2 TG201` | `CC 1` | any |

A tone can imply the mode (`293 NAC` → P25, `CC1` → DMR). Trunk Recorder's
`S` (search) reads as empty, because every analog call's tone is identified
anyway. Mistakes get a specific message ("151.5 isn't a standard tone — 151.4?").

**Several users of one frequency: one row each.** One receiver per
frequency. Each transmission goes to the row whose tone it carries. A row
with no tone takes everything else, so a channel listed with no tone records
everything, as now: only someone who adds tones can drop calls. With no
untoned row, an unmatched transmission is dropped (scanner tone squelch, as
in the PR). Same frequency with the same tone stays an error.

**Talkgroups.** RadioReference gives conventional frequencies no talkgroup
number (its export: Frequency Output/Input, Callsign, Agency/Category,
Description, Alpha Tag, PL Tone, Mode, Class Station Code). Only DMR
listings carry a TG, inside the Tone column. So no one has to type a number:
**+ Add tone** pre-fills the next free one (154325 → 1543251, 1543252, …) and
saves it, so it stays put when rows are reordered. `TG Number` still
overrides it; for DMR, a TG in the Tone field picks the row, since the air
names the talkgroup.

**In the engine** a matched tone, NAC or colour code picks the row, and the
row gives the talkgroup. That is the existing "talkgroup named on the air"
path P25 link control uses to label or split a call. On a frequency where a
transmission could be dropped, the call starts only once the tone is known
(about 0.2–0.4 s, the audio held).

**Steps**
1. Identify CTCSS and DCS on every analog call, into the call JSON and the
   call list. **Done** (below).
2. The Tone field: matching, rows sharing a frequency, CSV / config / UI.
   **Done** (below).
3. P25 NAC (the framer reads it; pass it to the call) and DMR colour code.
   **Done** (below).
4. A "Heard" list per frequency (tones seen, with call counts) and an
   **Add** button that makes a row for one: the way in for anyone who
   doesn't know their tones. **Done** (below).

### Step 1: identification (done, not yet committed)

`dsp/tones.rs` reads the 8 kHz audio before the 300 Hz voice high-pass
(`Nbfm::push_low`), cut to 1 kHz. Each conventional FM call has its own
detector, so the evidence resets with the call.

- **CTCSS:** each of the 51 standard tones is mixed to 0 Hz and summed over
  50 ms blocks. Every block, the strongest tone over the last 250 ms must
  stand 13 dB over the median of all 51, and 10 dB over the strongest one
  more than 8 Hz away. The second test matters: DCS's repeating words put
  lines every 5.84 Hz, and without it a DCS call read as 67.0 Hz. The
  frequency comes from how fast the winner's sums turn from block to block,
  snapped to the nearest standard tone within 1 Hz. That separates 150.0
  from 151.4 Hz and rejects DCS's 134.4 Hz turn-off tone.
- **DCS:** 134.4 baud, 8 staggered bit clocks. A code counts when a clock's
  last 23 bits are within one bit of its Golay word (generator 0xC75) and it
  is read again 23 bits later in the same position. Codes that are rotations
  of each other (D023N = D047I) are one signal; a class is named by its
  first normal-polarity member. All 112 code words match the PR's encoder
  bit for bit, and the PR's was checked against the published D023 word.
- **Output:** the call JSON gets the PR's `tone_mode` ("search" on
  conventional FM, "off" elsewhere), `tone_detected` (`151.4`, `D023N`)
  and `tone_confidence` (the share of the call that carried it, 0–1). The
  call list shows the tone after "FM".

Tested through the FM demodulator, with voice and noise: all 51 tones and
all 224 code/polarity pairs identified, nothing heard on voice alone or on
the turn-off tone. Cost: 0.07 s of CPU for 10 minutes of audio (0.012 % of a
core), only while an analog call is open. Not yet tried on a real radio.
DCS polarity follows the PR's (+deviation = 1), which was benched on real
radios. If it were wrong, every code would read as its inverted twin.

### Step 2: matching (done, not yet committed)

- **Parsing** (`Tone::parse` in Rust, `parseTone` in `web/src/tones.ts`):
  everything in the table above, stored in Trunk Recorder's form. A bare
  number is a CTCSS tone when it is one (`100`), else a DCS code (`023`).
  Mistakes are named (`151.5 Hz isn't a standard CTCSS tone — 151.4?`,
  `D024 isn't a DCS code`). The UI shows the error inline and tidies the
  text when the field is left. The Rust and TypeScript parsers give the
  same answers on the same 33 inputs.
- **Config:** a `tone` per channel, and the CSV `Tone` column, written after
  `Frequency` as Trunk Recorder orders it. Rows without a TG Number are
  numbered by their place on the frequency
  (`ConvChannel::default_talkgroup_at`, mirrored in `channelTalkgroups`).
  **+ tone** saves the next free number on the new row, so it never moves.
  `S` and P25 / DMR tones get a note when read.
- **Checks** (`check_channels`, mirrored in `startProblem`): rows on one
  frequency must share a mode. Only FM may have several. No two rows may be
  for the same signal (DCS aliases included) or both have no tone.
- **Engine:** rows on one frequency are one `Chan` with one receiver. A
  frequency whose only row has no tone runs exactly as before. Otherwise
  each transmission (it ends when the carrier does) gets its own
  `ToneDetector` and is held until `pick` names a row:
  - the row whose tone it carries;
  - else, once a tone is heard or after 1 s, the row with no tone;
  - else nothing, and it is dropped.
  The row's talkgroup goes in as the "air talkgroup", so another row's
  transmission ends the call and starts its own, and the same row's carries
  on. A transmission too short to decide goes by what was heard when it
  ends. At the end of a replay, one still held is dropped (up to 1 s).
- **JSON:** `tone_mode` is `ctcss` / `dcs` when the row has a tone.

Tests: three rows on one frequency (151.4, D023N, none) route 151.4, D023N,
untoned and 127.3 transmissions to rows 1, 2, 3, 3. Each keeps all but the
gate's attack of its 1.5 s. Two agencies 0.5 s apart (inside the call
timeout) make two calls. With no untoned row, only the D023N transmission
of three is recorded. Not yet tried on the air.

### Step 3: NAC and colour code (done, not yet committed)

`ConvChannel.tone` became `access: Option<Access>`. An `Access` is a
`Tone`, a `Nac(u16)`, or `Dmr { cc, slot, tg }` (any of the three), read
from the Tone column for the row's mode (`Access::parse`, mirrored by
`parseAccess` in `web/src/tones.ts`; the two agree on the same 27 inputs).

**Filing decision.** Trunk Recorder doesn't use the NAC, and files
conventional calls under the channel file's TG Number. This app filed
P25 / DMR calls under the talkgroup on the air. So:
- **P25, several rows on a frequency:** the row's talkgroup.
  Conventional radios mostly send a talkgroup that says little (often 1),
  which would merge the agencies.
- **P25, a lone row with a NAC:** a filter only; the air's talkgroup as
  before.
- **DMR:** always the air's talkgroup (DMR's are meaningful). Rows select
  and name; a row whose code names a talkgroup defaults to that number, so
  its names attach.

**Routing.** `route_digital`, per slot, per run: the most specific row
whose code fits the NAC / colour code / slot / talkgroup heard (what isn't
known yet doesn't fit), else the row with none, else dropped. Nothing is
held. A DMR row that names a talkgroup can't match until link control gives
it, so with late entry the first ~0.36 s goes to the catch-all row or is
dropped.

Also fixed: step 2 had stopped putting a P25 / DMR channel's names on calls
whose air talkgroup isn't in the talkgroup list. `info_for` falls back to
the row the call came on again.

**Tests**
- On the air (the 180 s DCFD capture, NAC 443, voice on 859.0375 MHz set
  up as a conventional P25 channel):
  - no NAC: 2 calls under TG 3747, named;
  - rows 443 / 293: both under the 443 row's TG 9001;
  - 293 only: nothing;
  - 443 alone: 2 calls under TG 3747.
- Synthetic DMR (CC 5, slot 2, TG 4321):
  - `CC5 TS2 TG4321` beats `CC5`, `CC5 TS1` and a catch-all;
  - `CC 5` alone keeps TG 4321 with its names;
  - `CC3` / `CC5 TS1` / `CC5 TG 999` with no catch-all record nothing;
  - with a catch-all, it takes the call.

### Step 4: the Heard list (done)

**Sources**
- **Recorded calls:** each conventional call's code (`heard_code`): the
  CTCSS / DCS heard ("" for none), its NAC (`Call.nac`, new), or its DMR
  colour code / slot / talkgroup.
- **Dropped transmissions:** a new `Event::ConvSkipped`, emitted by
  `Conventional::skip`. It fires once per call's worth: a new code, or the
  same after a pause longer than the call timeout. The memory lives on the
  channel, because a dropped transmission has no call to keep the receiver
  open. Only voice counts, since calls without audio are discarded too.
  On the DCFD capture, a NAC-293-only row on 859.0375 MHz reports 2, the
  2 calls the frequency records with no NAC (first try: 11).

**Store.** `trunk-app/src/heard.rs`: per frequency, up to 40 codes, each
with calls, skipped and last heard, most heard first. Codes are in the
form a row's Tone takes. Saved like talker aliases:
- desktop: `<conventional short name>.heard.json` in the config folder,
  loaded at start, saved with the aliases, sent in `hello`;
- browser: `heard-<name>.json` in its storage, through
  `WebSession::load_heard` / `heard_unsaved`;
- the UI gets a `heard` message whenever it changes.

**UI.** Under the last row of each frequency: *Heard: code, N calls,
M not recorded*, with **Add** (or ✓ when a row already has it; DCS aliases
count). The list shows only codes the frequency's mode can take, eight at
most. **Add** puts the row after the frequency's rows, with the next free
talkgroup (a DMR code naming a talkgroup files under that one).

Checked through the real desktop path (`serve --start` on the capture file,
then the saved JSON). The table was built and type-checked, not looked at.

