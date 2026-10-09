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
| App | `trunk-app` | `Config`, `Session` (engine events → interface messages, call files, plugin events), stats, file names, log records, conventional channel CSV. | No |
| Platform | `trunk-pro` | The desktop program: source threads and SDR drivers, the engine thread, the web server (axum), the plugin host, the CLI and analysis tools. | Yes |
| Platform | `trunk-web` | The browser build: `WebSession` + RTL-SDR over WebUSB, exported with wasm-bindgen into a Web Worker. | Yes (WebUSB, OPFS) |
| Protocol | `trunk-recorder-plugin` | The plugin wire protocol (`HostMessage`, `PluginMessage`, `Manifest`) and a Rust SDK for plugin authors. Published on crates.io. | SDK only |
| UI | `web/` | The React interface. Embedded in the `trunk-pro` binary and also runs the browser build. | — |

The rule that holds this together: **`trunk-core` and `trunk-app` never do I/O.** They take
samples plus a wall-clock time and return values (events, JSON strings, WAV bytes). The
platform crates own devices, threads, files and sockets. That is why the same recording logic
runs in a desktop process and in a Web Worker and gives bit-identical calls on the same capture.

### 1.2 Threads in the desktop process

| Thread | Count | What it does |
|---|---|---|
| `source-N` | one per source | Reads the SDR (or capture file) and sends `SourceMsg` blocks into the one engine queue. Airspy samples arrive on libairspy's own callback thread instead. |
| `engine` | one | Owns the `Session` (and so the `Engine`). Loop: receive one block (50 ms timeout) → `push` → `poll` → `deliver`. **All DSP and decoding for all sources runs here**, single-threaded. |
| `finish` | one | Writes call files (.json, .wav when kept, .m4a), sends `concluded`, hands calls to the plugin host and the Archive. |
| `encode-N` | 1–3 | Makes M4A for plugins (only if a plugin asks for M4A). |
| `plugin-<id>` + writer + stderr reader | per plugin process | Supervises one plugin child process, feeds its stdin, reads its stdout/stderr. |
| tokio runtime | pool | The axum web server; one task per WebSocket client. |
| `monitor`, net probe | one each | Samples the computer and plugins every 2 s; TCP connectivity checks every 15 s. |
| `stats-load`, `stats-store`, `history` | one each | Load/append stats rollups; fill the call list from disk at start. |

The browser build has no threads: `engine.worker.ts` pulls blocks from `WebRtl::next()` and
calls `WebSession.push` / `poll` directly, and keeps call files in OPFS.

---

## 2. How samples flow from the SDR to a saved call

Stages in order. Thread in brackets.

1. **SDR driver** [`source-N`]. Produces a block of IQ:
   - RTL-SDR (`trunk-pro/src/sdr.rs`, pure-Rust `rtlsdr-nusb`): 262,144-byte USB transfers
     (131,072 IQ samples, about 55 ms at 2.4 MSPS), 8 transfers in flight. Sent as raw `u8`
     (`SourceMsg::Data`).
   - USRP (`radio/uhd.rs`, UHD loaded at run time): about 10 ms per block, float IQ
     (`SourceMsg::Iq`). Adds `num_recv_frames=256` to the device args.
   - Airspy (`radio/airspy.rs`): whatever libairspy hands its callback, float IQ.
   - SoapySDR (`radio/soapy.rs`): about 10 ms per block, float IQ.
   - File (`runtime::run_file`): 32,768 samples per chunk, paced to real time or as fast as the
     engine takes them.
   Each block is stamped with `Instant::now()` (`at`) and carries the driver's `dropped` count.
2. **Source queue** [between threads]. One `sync_channel::<SourceMsg>(256)` shared by all
   sources. Senders block when it is full (see section 7).
3. **`Session::push` / `push_iq`** [`engine`]. Records drops (`push_gap` fills them with faint
   noise), feeds the wall-clock fit (`ClockFit`), then calls `Engine::push_u8` / `push_iq`.
4. **Channelizer** [`engine`]. One per source. Collects 12,288 new samples (one block), runs one
   16,384-point FFT, stores the spectrum for pre-roll, then calls `Engine::on_block(source)`.
5. **`Engine::on_block`** [`engine`], in this order:
   1. Every open voice channel (head) on this source: head IQ → `VoiceDecoder::push` → queued
      `VoiceOut`s in `radio.pending`.
   2. Every trunk whose control channel is on this source: control head IQ → `ControlChannel::push`
      → `Step`s → `Trunk::follow` (site lock) → `CallManager::handle` → may open voice channels.
   3. `apply_pending()`: decoded audio appended to recordings, `Event::Audio` for live audio,
      call info and talker aliases applied.
   4. Conventional channels: energy detection on the shared spectrum, opening heads as needed.
   5. `apply_pending()` again, then `emit_call_events()`: `CallStart`, `CallUpdate`, `CallEnd`.
      An ended call goes through `conclude()` → `MultiSite` → `record::save_call` →
      `Event::Concluded` (or `NotSaved` / `Duplicate`).
6. **`Session::poll`** [`engine`]. Runs after every received block (or every 50 ms when idle).
   Drains engine events and turns them into `Output`s: `Text` (status every 500 ms, stats every
   1 s), `Topic` (spectrum, logs, RF detail, only while watched), `Audio` (live frames, only
   while someone could listen), `File` (a call to store), `Plugin` (plugin events, only for
   subscribed topics), `Log`, `Rollup`.
7. **`runtime::deliver`** [`engine`]. Routes each output:
   - `Text`, `Topic`, `Audio` → the hub (a tokio broadcast channel of 4096) → each WebSocket task.
   - `Audio`, `Plugin` → `PluginHost` (per-plugin queues).
   - `File` → the finish queue (1024).
   - `Log` → logging; `Rollup` → stats store.
8. **Finish** [`finish`]. Writes `.json` (and `.wav` when it is kept or a plugin takes WAV),
   encodes the kept `.m4a`, publishes `concluded` to browsers (so the UI never lists a call
   that isn't on disk), adds it to history, registers it with the Archive and sends
   `call.concluded` to plugins.
9. **Plugins** [plugin processes]. Upload or stream the call, answer `call.result`. The Archive
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
| P25 control and voice, conventional FM / P25 / DMR | 7,000 Hz |
| Trunked DMR | 6,250 Hz |
| NXDN48 | 3,400 Hz |
| NXDN96 | 6,250 Hz |
| SmartNet control channel | 5,000 Hz |
| SmartNet analog voice | 7,000 Hz |

**Pre-roll.** Every block's full spectrum is kept in a ring holding
`max(prerollS, conventional pre-roll 0.3 s, 0.1 s)` of history (1 s by default; about 196
blocks × 16,384 × 8 bytes ≈ 26 MB per source at 2.4 MSPS). `add_head` replays up to `prerollS`
of stored blocks (M-point work only) so a voice channel opened when a grant arrives starts with
the air *before* the grant. It also returns the absolute input-sample index of the head's first
output, which ties frame times to the source clock. Live and replayed output are identical.

**Other outputs from the same spectra:** the waterfall (`power_spectrum`), the noise floor per
slice (`noise_profile`: median |X|² / ln 2), and the conventional energy detector
(`band_power`), which costs no head while a channel is idle.

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

1. Head (6.25 kHz cutoff for trunked DMR) → C4FM at 4,800 Bd, RRC α 0.2, MSD with decision
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
4. Layer 3 messages for Type-C and Type-D trunking.
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
1. 64-tap Blackman channel filter, 5,500 Hz; also the carrier power meter (τ 10 ms).
2. Discriminator (5 kHz deviation = full scale).
3. De-emphasis, 750 µs.
4. Audio low-pass 128 taps at 3,400 Hz; resample to 8 kHz.
5. 300 Hz 6th-order Butterworth high-pass (removes CTCSS).
6. Squelch gate: opens after 30 ms above threshold, closes after 2 ms below; 10 ms ramps; audio
   delayed 15 ms so the squelch tail is never heard. Trunked threshold: noise + 6 dB.
7. MDC1200 / FleetSync unit IDs decoded from the gated audio (`dsp/signalling.rs`); CTCSS/DCS
   from the pre-high-pass audio (`dsp/tones.rs`, conventional only).

### 4.7 Conventional detection (`trunk/conventional.rs`)

1. Noise floor per source: `noise_profile` in 64 slices every 0.1 s (EMA 0.3), not updated
   during gap fill.
2. Channel power: `band_power` ±5 kHz (±3.125 kHz NXDN48), smoothed with a 20 ms time constant.
3. Opens when SNR ≥ the row's `squelchDb` (default 8 dB): `add_head` with 0.3 s pre-roll, then the
   same `voice::build` decoder as a trunked call.
4. A carrier meter with 3 dB hysteresis confirms the carrier; a false open raises the bar 3 dB.
5. The head closes 0.5 s after the carrier is gone and no call is live.
6. Several rows on one frequency are told apart by CTCSS / DCS (FM: held up to 1 s while the
   tone detector decides), NAC (P25), colour code / slot / talkgroup (DMR) or RAN / talkgroup
   (NXDN). Traffic no row takes is reported as `ConvSkipped`.

### 4.8 Vocoders and saving

- `mbe/`: IMBE 7200×4400 (P25 Phase 1) and AMBE+2 3600×2450 (Phase 2, DMR, NXDN), ported from
  mbelib in double precision, with TIA-102.BABA-A concealment. 160 samples (20 ms) at 8 kHz per
  frame.
- Profiles: `fixed` (Pavel Yazev's fixed-point IMBE, the app default for Phase 1), `enhanced`,
  `mbelib`. AMBE always uses `enhanced`.
- Scaling: ×7/32768, then a tanh soft limiter above −3 dBFS.
- `record::save_call`: drops transmissions shorter than `minTransmissionS` and encrypted ones,
  refuses too-short or silent calls, normalises loudness (−12 dBFS speech, about −14 LUFS; gain
  clamped −18..+24 dB; look-ahead limiter at −2.5 dBFS), writes the call JSON. `wav.rs`
  writes 16-bit mono 8 kHz.

---

## 5. How trunking messages are processed

### 5.1 The path

1. **Control head** on the source that covers the control channel (`Radio::source_for`).
2. **`ControlChannel::push`** (one implementation per protocol in `trunk/control.rs`): IQ →
   receiver → framer → parser → `Step { good, msgs: Vec<Message>, identity }`.
3. **`Trunk::follow`**, for each step:
   1. Record the site's identity and adjacent sites.
   2. Compare the identity with the configured `expect` (the site lock); on a new mismatch log
      "Control channel X MHz is not this system".
   3. Emit every message as `Event::Message` (the log and the plugin `unit` topic).
   4. If mismatched: skip the step and do not refresh `last_good_s`, so hunting moves on.
   5. If the step decoded cleanly, refresh `last_good_s`.
   6. Hold grants until every locked field has been heard.
   7. Pass the Phase 2 scrambling key to voice channels (`radio.voice_params`).
   8. **`CallManager::handle(msgs, SysHost)`**.

### 5.2 Per protocol

| Protocol | Carriers | Receive and decode | Site lock fields | Site group |
|---|---|---|---|---|
| P25 | one, hunted | `Bank` → `p25::Framer` → TSDU → `TsbkParser` | NAC, WACN, System ID, RFSS, site | `p25:<wacn>.<sysid>` |
| SmartNet | one, hunted | `Fsk2` → OSW framer → `smartnet::Parser` | System ID, site | `smartnet:<sysid>` |
| DMR | every carrier, watched | `C4fm::dmr` per carrier → bursts → CSBK / LC | none (colour code gate) | none (configured only) |
| NXDN | every carrier, watched | `C4fm` (NXDN) → frames → CAC / SACCH → layer 3 | Type-C: System ID, site; Type-D: none | `nxdn:<sysid>` |

`CarrierPlan::Hunt` (P25, SmartNet) listens to one control channel at a time and moves through
the list. `CarrierPlan::Watch` (DMR, NXDN) opens a head on every listed carrier at once.

### 5.3 `Message` and `MessageType`

`trunk::Message` is one struct for every protocol. Its `kind: MessageType` is one of:
`Grant`, `Status`, `Update`, `ControlChannel`, `Registration`, `Deregistration`, `Affiliation`,
`SysId`, `Acknowledge`, `Location`, `PatchAdd`, `PatchDelete`, `DataGrant`, `UuAnsReq`,
`UuVGrant`, `UuVUpdate`, `CallAlert`, `Adjacent`, `Unknown`.

P25 TSBK opcodes (`TsbkParser::parse`):

| Opcode | Standard | Motorola (MFID 0x90) |
|---|---|---|
| 0x00 | Grant (group voice) | PatchAdd |
| 0x02 | Update (two if ch2 differs) | Grant (patch grant) |
| 0x03 | Update (explicit) | Update (patch) |
| 0x04 | UuVGrant | — |
| 0x05 | UuAnsReq | traffic channel ID (ignored) |
| 0x06 | UuVUpdate | — |
| 0x14 | DataGrant | — |
| 0x1f | CallAlert | — |
| 0x20 | Acknowledge | — |
| 0x28 | Affiliation | — |
| 0x29, 0x39 | ControlChannel (secondary CCs) | — |
| 0x2b | Location | — |
| 0x2c | Registration | — |
| 0x2f | Deregistration | — |
| 0x30 (MFID 0xa4) | M/A-COM PatchAdd / PatchDelete | — |
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
| `UuVGrant` | `grant(m, unit-to-unit)` if `recordUnitToUnit` |
| `UuVUpdate` | `update(m)` if `recordUnitToUnit` |
| `PatchAdd` | add to `Patches`; calls fold in patch members; a call skipped as unknown talkgroup is re-admitted |
| `PatchDelete` | remove from `Patches` |
| everything else | ignored by the call manager; `Session` turns registrations, affiliations, etc. into plugin `unit` events |

A call's identity is (talkgroup, frequency, TDMA slot, Phase 2). `grant()` ignores
`freq_hz == 0`; if a matching call exists it is refreshed (`last_update_s`, patches,
encrypted/emergency latch, a new source unit); otherwise a new `Call` gets the next id from the
shared `CallIds` counter (one sequence for every system and conventional channel) and is admitted.

### 5.5 Hunting

- `CC_HUNT_S` = 5 s: if no step decoded cleanly for 5 s and the system has more than one control
  channel, `tune(next)`: remove the control head, add one at the next covered frequency, reset
  the protocol's state and forget the site identity.
- Steps from a mismatched site never refresh `last_good_s`, so a foreign control channel is
  hunted away from.
- Watched plans (DMR, NXDN) never hunt; they report when the active control carrier moves
  (Capacity Plus rest channel).

### 5.6 AutoTune

Every 10 s, if the control channel decoded in the last second, the engine reads the receiver's
frequency offset and records ppm = applied ppm + offset/f × 1e6 (|ppm| ≤ 50 kept, last 20
averaged). With `autoTune` on, new heads open at `f × (1 + ppm·1e-6) − center`. P25 control
channels more than 150 Hz off are reopened at the corrected frequency, at most every 200 s. The
measured error is always reported (`SourceTune`).

### 5.7 Patches

`Patches` maps supergroup → member → last heard. Members not heard for 10 s (P25) or 4 s
(SmartNet) expire; a patch with one member left is dropped. `members_of(tg)` fills
`Call.patched_talkgroups`.

---

## 6. How recorders are managed and assigned

### 6.1 The pool

`Radio` in `trunk/engine.rs` is shared by every system:
- `channels: BTreeMap<(system, freq_hz), Channel>`: open voice channels. Systems never share one.
- `recordings: HashMap<CallId, Recording>`: calls being recorded. Their count is capped by
  `maxRecorders` (default 32).
- `free_nums` / `next_num`: recorder numbers (the call JSON's `recorder_num`), reused lowest-free.

A "recorder" is not a pre-built object as in Trunk Recorder. It is a `Recording` (audio buffer,
vocoder frames, transmissions, reception meter) plus a head and decoder that exist only while
the call does.

### 6.2 Admitting a call (`CallManager::admit`)

In order, the first that applies wins:
1. The talkgroup row has `Ignore` (or priority < 0) → not recorded, reason `ignored`.
2. The talkgroup is not in a non-empty talkgroup file (nor a patch member) and `recordUnknown`
   is off → `unknown_tg`.
3. Encrypted and `recordEncrypted` is off → `encrypted`; the engine may **follow** it (open a
   channel without a recording, to collect units and aliases) while recorder room is spare.
4. Otherwise `RecorderHost::record(call)` (`SysHost::record`):
   1. `recordings.len() ≥ maxRecorders` → `no_recorder`.
   2. `Radio::source_for(freq)` finds no source covering it → `no_source`.
   3. Take a recorder number, insert a `Recording`, `open_channel`.

There is no priority and no preemption: a call refused for `no_recorder` stays monitored until it
times out and is not retried. The talkgroup CSV `Priority` column only matters when negative
(ignore). `Message.priority` is copied to the call JSON and used nowhere else.

### 6.3 Opening the channel (`Radio::open_channel`)

1. Decide the voice kind from the call (`VoiceKind::of`): analog → `Analog`; NXDN → `Nxdn`;
   colour code → `Dmr`; Phase 2 → `Tdma`; otherwise `Fdma`.
2. Slot: `tdma_slot & 1` for Tdma and Dmr, else 0.
3. If `(system, freq)` already has a channel of **the same kind**: put the call in that slot and
   return. Phase 2 and DMR calls on the two slots of one carrier share one head and one decoder.
   A newer call on an occupied slot takes it over; the older call stops getting audio and ends by
   timeout.
4. If it has a channel of **another kind** (P25 dynamic dual mode, SmartNet analog ↔ digital):
   remove that channel and head; its calls time out.
5. Otherwise: `add_head(offset with AutoTune ppm, cutoff, prerollS)`, build the decoder with
   `voice::build(VoiceSpec)`, decode the replayed pre-roll at once.

| `VoiceKind` | Decoder chain |
|---|---|
| `Fdma` | `Bank` → `VoiceTracker` → IMBE |
| `Tdma` | `Cqpsk` 6000 Bd → `phase2::Framer` → `TdmaTracker` (both slots) → AMBE+2 |
| `Dmr` | `C4fm::dmr` → `DmrVoice` (both slots) → AMBE+2 |
| `Nxdn(rate)` | `C4fm` (NXDN) → `NxdnVoice` → AMBE+2 |
| `Analog` | `Nbfm` → `Signalling` (MDC1200 / FleetSync IDs) |

### 6.4 While recording

Decoder outputs (`VoiceOut { slot, t, out: TrackerOut }`) are queued in `radio.pending` and
applied after the block (`apply_pending`), so a voice channel never borrows a `Trunk` mutably:
- `Audio` / `AnalogAudio` → appended to the `Recording`, `last_audio_s` moved, live
  `Event::Audio` if this copy leads its multi-site conversation;
- `Info` → source unit, encrypted, emergency → `CallUpdate`;
- `Alias` → talker alias learned.

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

The recorder is freed in `Engine::conclude`, at the end of the block, when the recording is
removed and its number returned to `free_nums`.

### 6.6 Multi-site

`trunk/multisite.rs`. Sites are twins when they are in the same site group but not the same
site or control channel. A call that starts on a twin site with the same talkgroup within 3 s is
linked into one conversation. **Every copy is recorded.** Live audio comes from one copy until it
has been quiet 0.5 s. When the last copy ends, one is kept: the talkgroup's preferred site if its
clean voice is at least 90 % of the best, otherwise the most clean voice, then fewest FEC errors,
then earliest start. The others become `Duplicate` events.

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

- **One engine thread, push-driven, run to completion.** The engine thread takes one block from
  the source queue and calls `push`. Inside, each completed 12,288-sample channelizer block runs
  every stage for that source synchronously, in a fixed order (section 2, step 5), before the
  next block is read. There are no buffers between stages: a head's output is overwritten every
  block, and decoders keep only their own state.
- **One FFT for all channels.** Opening a channel adds an M-point inverse FFT, not a full-rate
  filter. The recorder count costs little until channels actually decode.
- **One bounded queue in front of the engine, which blocks.** `sync_channel::<SourceMsg>(256)`.
  If the engine is slower than real time, the queue fills (about 14 s of RTL data at 2.4 MSPS,
  about 2.5 s for 10 ms blocks), the source thread blocks in `send`, the driver stops collecting,
  and the device's own buffer overflows. The driver reports what it lost (`dropped`) where it can
  tell; the session fills it with faint noise (`push_gap`, up to 10 s) so sample time stays in
  step with the air. Loss a driver can't see is caught by the wall-clock fit: if samples are
  more than 0.25 s off, the clock is re-anchored and the log says so.
- **Everything after the engine is non-blocking, with an explicit policy per queue.** Nothing
  downstream can stall decoding:

| Queue | Capacity | Producer → consumer | When full |
|---|---|---|---|
| Source → engine | 256 blocks | source threads → engine | sender blocks; driver overflows; drops reported and gap-filled |
| rtlsdr-nusb USB transfers | 8 × 256 KiB | USB → source thread | device FIFO overflows |
| UHD receive frames | 256 frames | USRP → UHD | overflow `O`; counted from the timestamp gap |
| Hub (tokio broadcast) | 4,096 messages | engine, finish, monitor → each WebSocket | a lagging client loses its oldest messages (silently); others unaffected |
| Finish | 1,024 calls | engine → finish thread | the engine thread writes the call itself, without M4A |
| Plugin events | 1,024 lines per plugin process | engine / finish / encoders → plugin stdin | the event is dropped and counted (`dropped`), logged at most every 30 s |
| Encode jobs | 256 | finish → encode threads | the call goes to plugins as WAV only |
| Stats rollups | unbounded | engine → stats-store | — |
| SDK `CallQueue` (inside a plugin) | 10,000 calls | plugin | `call.result failed` "too many calls waiting" |

- **Topic gating** keeps cost proportional to what's watched: spectra, control-channel logs, RF
  and decode detail are built only while some client subscribes; live audio frames only while a
  browser is connected or a plugin wants audio; plugin events only for topics some plugin
  subscribes to.

### 7.3 What happens when one part is slow

| Slow part | Effect |
|---|---|
| The engine (CPU can't keep up) | Source queue fills → sources block → driver loses samples → `dropped` in status, gap filled, calls carry on with a hole; load shown on the dashboard. |
| One browser | Its WebSocket task falls behind the 4,096-message ring and skips messages. Recording and other browsers are unaffected. |
| Disk / finish thread | Finish queue fills (1,024 calls) → the engine writes calls itself without M4A, which slows decoding until it drains. |
| One plugin | Its 1,024-line queue fills → its events are dropped and counted. The engine and other plugins are unaffected. The Archive keeps files a plugin never answered for (gives up after 1 h). |
| M4A encoder | Encode queue fills → calls go to plugins as WAV. |
| A radio stalls | No data for 1 s → silence filled in by `Session::poll` so calls on that source time out; the driver reopens after 2–3 s. |

### 7.4 Side by side

| | Trunk Recorder (GNU Radio) | Trunk Recorder Pro |
|---|---|---|
| Scheduling | a thread per block | one engine thread; stages called in order per block |
| Inter-stage buffers | circular buffer per block output | none; stage outputs reused every block |
| Unit of work | whatever items are available (`noutput_items`) | one channelizer block (12,288 samples) |
| Channel extraction | a full-rate filter per recorder | a shared FFT; an M-point IFFT per channel |
| Recorders | pre-built at start, enabled by selector | built per call; a count cap only |
| Control → recorder | message queue polled by the main loop | direct call (`CallManager` → `RecorderHost`) in the same block |
| Back-pressure | propagates block by block to the source | one blocking queue at the input; downstream queues never block the engine |
| Overload | hardware overflow; any block can stall all | hardware overflow, measured, gap-filled, clock re-anchored |
| Multi-core | automatic | one core for DSP; I/O, encoding, plugins and the server on other threads |
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
| `host.rs` | `PluginHost`: processes, queues, dispatch, encoding, shutdown |
| `manage.rs` | runtime state for the UI, reloads, stats, installs |
| `store.rs` | registry, download, SHA-256 check, unpack, install |
| `archive.rs` | counts `call.result`s to decide when files can be deleted |
| `encode.rs` | ffmpeg / afconvert / fdkaac M4A |
| `cli.rs` | `trunk-pro plugin …` commands |

Per plugin process: a supervisor thread (`plugin-<id>`) reading stdout, a writer thread feeding
stdin from the 1,024-line queue, and a stderr reader. Events are serialised once and shared.

### 8.3 Conversation

1. Host → `hello` { api, host, config, systems [ {index, short_name, kind, config} ],
   capture_dir, data_dir, audio_formats }.
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

For each call the finish thread calls `archive.expect(call, takers)`, where takers = live
processes subscribed to `call.concluded`. Each `call.result` counts down. At zero the call's
`audioArchive`, `callLog`, `archiveFilesOnFailure` and `compressWav` rules are applied (delete,
or move out of the RAM spool). A call no plugin takes keeps all its files. An unsettled call is
kept after 1 h.

### 8.6 RAM spool

With `recording.ramSpool.enabled`, files that will be deleted once the plugins are done go to a
RAM disk (macOS: one made with `hdiutil`; Linux: `/dev/shm`) to save SSD wear. No room → the
recordings folder. Files older than 2 h are moved out.

### 8.7 Store

- Registry `index.json` from the TrunkRecorder/plugins repository (cached 15 min; built-in copy
  as fallback); assets keyed by target triple.
- Install: download (≤ 200 MiB) → SHA-256 must match → unpack to staging (no links, no path
  escape, ≤ 1 GiB) → `--describe` must match id and version → atomic rename into
  `plugins/<id>/`.
- A GitHub release not in the registry can be installed if it has `SHA256SUMS` (tier
  "unlisted").

### 8.8 Several copies of one plugin (in progress on this branch)

A system's `plugins.<id>` may be an array of settings objects: one process per element
(`openmhz`, `openmhz#2`, …), each with its own queue, back-off, data folder
(`plugin-data/openmhz#2/`) and drop counter. Copy *k* gets the same recorder-wide settings and,
per system, element *k* (or `null` for systems with fewer copies). The Archive waits for a
result from every copy. The UI shows one row per plugin with the worst state and summed metrics.

### 8.9 SDK (`trunk-recorder-plugin`, feature `sdk`)

- `Plugin` trait: associated `Config` and `SystemConfig` types (serde + schemars), `manifest()`,
  `start(Host, Setup)`, optional `call_start`, `call_end`, `call_concluded`, `unit`, `audio`,
  `status`, `shutdown`. `run::<P>()` handles `--describe`, `--version`, the handshake and exit
  codes.
- `CallQueue`: worker threads (default 2), retries after 10 s, 60 s, 300 s, 900 s, capacity
  10,000, endpoint health (Up / Degraded / Down), metrics every 5–30 s, and `queue.jsonl`
  persistence across restarts.
- `TalkgroupFilter` (allow / deny globs), `schema.rs` (form hints such as `x-secret`,
  `x-system`, `x-required`), `testing.rs` (drive a plugin with messages; HTTP mock server).

---

## 9. Data objects

Lineage of a call: `Symbol` → `Frame` / `Packet` / `Burst` → `Message` → `Call` →
`TrackerOut` / `VoiceOut` → `Recording` → `Concluded` → `Event::Concluded` → `Output::File` →
call JSON + WAV on disk → `HostMessage::CallConcluded` → plugin → `PluginMessage::CallResult`.

### 9.1 Samples and symbols

**`SourceMsg`** (`trunk-pro/src/sdr.rs`): what a source thread sends the engine.

| Variant | Fields |
|---|---|
| `Data` | `source: usize`, `bytes: Vec<u8>` (u8 IQ), `dropped: u64`, `at: Instant` |
| `Iq` | `source`, `samples: Vec<Complex32>`, `dropped`, `at` |
| `Error` | `source`, `error: String` |
| `End` | `source` |
| `Tuned` | `center_hz` (survey only) |

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
| `phase2_tdma`, `tdma_slot` | bool, u8 | |
| `sys_id`, `wacn`, `rfss`, `site` | u32 | identity |
| `nac` | u16 | |
| `analog` | bool | SmartNet analog voice |
| `patch` | `Option<Patch { sg: u32, ga: [u32; 3] }>` | |
| `color_code` | `Option<u8>` | DMR |
| `nxdn`, `ran` | `Option<Rate>`, `Option<u8>` | NXDN |
| `opcode` | u8 | |
| `meta` | String | human summary |

**`Step`**: `good: bool`, `msgs: Vec<Message>`, `identity: Identity`.

**`Identity`**: `BTreeMap<IdField, u32>`; `IdField` = `Nac`, `Wacn`, `SysId`, `Rfss`, `Site`.

**`FreqTable`** (P25 IDEN): `offset_hz: i64`, `step_hz: u64`, `base_hz: u64`,
`phase2_tdma: bool`, `slots_per_carrier: u32`, `bandwidth_khz: f64`.

**`Talkgroup`**: `number: u32`, `mode`, `alpha_tag`, `description`, `tag`, `group: String`,
`priority: i32`, `preferred_nac: u32`, `preferred_site: String`, `ignore: bool`.

### 9.3 Calls

**`Call`** (`trunk/calls.rs`):

| Field | Type | Meaning |
|---|---|---|
| `id` | `CallId(u32)` | one sequence for all systems |
| `system` | u16 | engine system index; conventional = 65535 − k |
| `talkgroup` | u32 | |
| `freq_hz` | u64 | |
| `phase2_tdma`, `tdma_slot` | bool, u8 | |
| `unit_to_unit` | bool | |
| `recording` | bool | has a recorder |
| `reason` | `Option<Reason>` | why not recorded: `ignored`, `unknown_tg`, `encrypted`, `no_source`, `no_recorder` |
| `encrypted`, `emergency` | bool | latched |
| `priority` | u8 | |
| `duplex`, `mode`, `analog` | bool | |
| `start_s`, `last_update_s`, `last_audio_s` | f64 | sample-clock times |
| `sources` | `Vec<CallSource { src: u32, time_s: f64, emergency: bool }>` | units heard |
| `talkgroup_info` | `Option<Talkgroup>` | from the CSV |
| `patched_talkgroups` | `Vec<u32>` | |
| `color_code`, `nxdn`, `ran`, `nac` | options | protocol details |
| `tone`, `tone_set` | options | conventional CTCSS/DCS heard / configured |

**`CallEvent`**: `Start(Call)`, `Update(Call)`, `End(Call)`.

**`RecorderHost`** trait: `record`, `follow`, `release`, `record_continued`.

**`Channel`** (engine, internal): `system`, `source`, `head`, `calls: [Option<CallId>; 2]`,
`voice: Box<dyn VoiceDecoder>`, `freq_hz`, `tune_ppm`, `meter`, `noise`.

**`Recording`** (engine, internal): `audio: Vec<f32>`, `frames: CallFrames`,
`recorder_num: u32`, `tx: Transmissions`, `freq_error: (f64, u32)`, `reception: Reception`.

**`TrackerOut`**: `Audio(Vec<f32>, VoiceFrame)` (20 ms at 8 kHz), `Info { source, emergency,
encrypted }`, `AnalogAudio(Vec<f32>)`, `Subaudible(Vec<f32>)`, `Alias(Alias)`,
`AliasLc(AliasLc)`. **`VoiceOut`**: `slot: u8`, `t: f64`, `out: TrackerOut`.

**`VoiceFrame`**: `codec` (Imbe | Ambe), `bits: Vec<u8>`, `e0`, `errs: u32`, `erased: bool`,
`kind`.

**`VoiceSpec`**: `kind`, `rate`, `t0`, `seed`, `vocoder`, `bank`, `squelch`, `subaudible`.
**`VoiceParams`**: `tdma_key: Option<(nac, sysid, wacn)>`.

**`SaveRules`**: `keep_silent`, `keep_encrypted`, `min_call_s`, `min_transmission_s`,
`normalize`, `digital_gain_db`, `analog_gain_db`.

**`Concluded`** (`trunk/record.rs`): `call: Call`, `json: String`, `short_name`, `base_name`
(`<tg>-<startEpoch>_<freq>[.slot]`), `audio: Vec<f32>` (8 kHz), `frames: Option<Capture { ext:
"sdr" | "frames.jsonl", bytes }>`.

### 9.4 Engine output

**`Event`** (`trunk/engine.rs`):

| Variant | Fields | Meaning |
|---|---|---|
| `ControlChannel` | `system`, `freq_hz` | tuned a control channel |
| `Message` | `system`, `msg: Message` | every decoded control message |
| `Note` | `system`, `level` (Info / Warning), `text` | log-worthy note |
| `CallStart` / `CallUpdate` / `CallEnd` | `Call` | call lifecycle |
| `Audio` | `call_id`, `system`, `talkgroup`, `samples: Vec<f32>` | live audio, 8 kHz |
| `UnitAlias` | `system`, `unit`, `alias`, `talkgroup` | talker alias heard |
| `Concluded` | `Concluded` | a call to save |
| `NotSaved` | `Call` | too short / silent |
| `Duplicate` | `call`, `kept` | another site's copy kept |
| `ConvSkipped` | `freq_hz`, `code` | conventional traffic no row took |

**`Status`**: `now_s`, `systems: Vec<SystemStatus>`, `active_calls`, `recording`,
`channels_open`, `conventional_open`, `calls_concluded`, `sources: Vec<SourceTune
{ error_ppm, applied_ppm }>`.

**`SystemStatus`**: `system`, `short_name`, `now_s`, `control_channel_hz`, `identity`, `good`,
`bad`, `modulation`, `active_calls`, `recording`, `calls_concluded`, `mismatch`,
`adjacent: Vec<AdjacentSite { sys_id, rfss, site, freq_hz }>`, `patches`,
`protocol: ProtocolStatus`, `site_group`.

**`ChannelSnapshot`**: `system`, `freq_hz`, `source`, `kind` (control | voice | carrier |
conventional), `power_db`, `noise_db`, `offset_hz`, `quality`, `phase_err`, `calls`.

### 9.5 Session output (`trunk-app/src/session.rs`)

**`Output`**:

| Variant | Fields | Goes to |
|---|---|---|
| `Text` | `String` (JSON message) | every browser |
| `Topic` | `topic`, `text` | browsers subscribed to the topic |
| `Audio` | `system`, `short_name`, `tg`, `frame: Vec<u8>` = `[2][u16 system][u32 call][u32 tg][i16 LE…]` | listening browsers, plugins with `audio` |
| `File` | `rel`, `system`, `wav: Vec<u8>`, `json`, `frames`, `entry` | finish thread |
| `Plugin` | `HostMessage` | plugin host |
| `Log` | `log::Record` | logging |
| `Rollup` | a minute of dashboard series | stats store |

**`log::Record`**: `level`, `system: Option<String>`, `call: Option<CallTag { num, talkgroup,
tag, encrypted, freq_hz }>`, `body: Body` (`Text`, `Recording`, `NotRecording`, `Concluded`,
`Dropped`, `ControlChannel`, `DecodeRate`, `State`).

### 9.6 Call JSON (written by `record::call_record`)

Trunk Recorder's keys plus reception figures, in this order: `call_num`, `freq`,
`freq_error`, `signal`, `noise`, `snr`, `clean_voice_pct`, `source_num` (always 0),
`recorder_num`, `tdma_slot`, `phase2_tdma`, `start_time`, `stop_time`, `start_time_ms`,
`stop_time_ms`, `emergency`, `priority`, `mode`, `duplex`, `encrypted`, `call_length`,
`call_length_ms`, `talkgroup`, `talkgroup_tag`, `talkgroup_description`,
`talkgroup_group_tag`, `talkgroup_group`, `color_code` (−1 if none), `ran` (−1 if none),
`tone_mode`, `tone_detected`, `tone_confidence`, `audio_type` (analog | digital tdma |
digital), `short_name`, `patched_talkgroups` (only when more than one), `freqList` [{freq,
time, pos, len, error_count, spike_count}], `errorList` [{pos, len, frames, error_count,
bad_frames, max_frame_errors}], `srcList` [{src, time, pos, emergency, signal_system, tag,
tag_ota}].

### 9.7 Plugin protocol (`trunk-recorder-plugin/src/protocol.rs`)

Constants: `API_VERSION` = 1, `EXIT_CONFIG` = 78, conventional system index base 65535.

**`Manifest`**: `id`, `name`, `version`, `description`, `api`, `subscribe`, `audio_formats`,
`config` (JSON Schema), `system_config` (JSON Schema), `homepage`, `repository`, `authors`,
`license`.

**`HostMessage`** (tag `type`):

| `type` | Payload fields |
|---|---|
| `hello` | `api`, `host { name, version }`, `config`, `systems: [SystemInfo { index, short_name, kind, config }]`, `capture_dir`, `data_dir`, `audio_formats` |
| `call.start`, `call.end` | `CallInfo { id, system, short_name, talkgroup, talkgroup_tag, freq_hz, tdma_slot, analog, encrypted, emergency, recording, reason, start_time, units, patched_talkgroups }` |
| `call.concluded` | `path`, `system`, `call: CallRecord` (the call JSON), `files { json, wav, m4a }` |
| `unit` | `system`, `short_name`, `kind` (registration, deregistration, affiliation, acknowledge, location, data_grant, answer_request, call_alert), `unit`, `talkgroup`, `time` |
| `audio` | `call_id`, `system`, `short_name`, `talkgroup`, `sample_rate`, `pcm` (base64 i16 LE) |
| `status` | `time`, `systems [{ index, short_name, control_channel_hz, decode_rate, active_calls, recording }]` |
| `shutdown` | `grace_s` |

**`PluginMessage`** (tag `type`): `ready`; `log { level, message }`; `status { state: ok |
warning | error, message }`; `call.result { path, outcome: ok | skipped | failed, message, url }`;
`metrics { queued, retrying, in_flight, retries, bytes_sent, latency_ms, last_ok, last_error,
last_error_text, endpoints [{ name, state, latency_ms, last_ok, last_error }], extra }`.

---

## 10. The configuration file

### 10.1 Where it is

- macOS `~/Library/Application Support/trunk-pro/config.json`; Windows `%APPDATA%\trunk-pro\`;
  Linux `$XDG_CONFIG_HOME/trunk-pro/` or `~/.config/trunk-pro/`; Docker
  `/data/config/trunk-pro/config.json`; browser `localStorage["trp.config"]`.
- `--config file.json` picks another file. Its folder becomes the data folder, holding every
  companion file, so two configs in different folders share nothing.
- Written by `setConfig` (the interface always sends the whole config) through `write_atomic`
  (temporary file, `sync_all`, rename).
- Keys are camelCase. Unknown keys are kept and written back at the top level and in each
  system, conventional system, `recording`, `server` and `log`. A value of the wrong type makes
  the file unreadable ("not a config this version reads").

### 10.2 Top level

| Key | Type | Default | Meaning |
|---|---|---|---|
| `sources` | array | one RTL-SDR at 2.4 MSPS, auto centre | radios and capture files |
| `systems` | array | `[]` | trunked systems, one entry per site |
| `conventional` | array | `[]` | conventional systems (≤ 256) |
| `recording` | object | see 10.6 | where and how calls are saved |
| `server` | object | see 10.8 | web server |
| `plugins` | map id → `{enabled, settings, path}` | `{}` | plugins on/off and recorder-wide settings |
| `log` | object | see 10.9 | log options |
| `monitor` | `{probeHosts}` | `["1.1.1.1:443","dns.google:443"]` | internet checks |

### 10.3 `sources[]`

`type` is `rtlsdr`, `usrp`, `airspy`, `soapy` or `file`. Every type has `centerHz` (required;
0 = auto-centre), `rateHz` (required), `autoTune` (false) and `guardHz` (75,000).

| Type | Other keys |
|---|---|
| `rtlsdr` | `serial` (required; "" = first free), `gainDb` 25.4, `agc` false, `ppm` (integer, required) |
| `usrp` | `args` "", `gainDb` 40, `agc` false, `antenna` "", `ppm` 0 |
| `airspy` | `serial` "", `gainMode` linearity / sensitivity / manual, `gainStep` 14, `lnaStep` / `mixerStep` / `vgaStep` 10, `agc`, `biasTee`, `ppm` |
| `soapy` | `args` "", `agc` true, `gainDb` null, `gains` {}, `antenna` "", `settings` "", `ppm` 0 |
| `file` | `path` (required), `realtime` (required), `format` cu8 / cs16 / cf32 (else from the extension) |

### 10.4 `systems[]`

| Key | Default | Meaning |
|---|---|---|
| `shortName` | `sys1` | identity: folder, companion files, plugin key; unique across all systems |
| `name` | "" | display name |
| `type` | `p25` | `p25`, `smartnet`, `dmr`, `nxdn` |
| `enabled` | true | |
| `controlChannelsHz` | [] | P25/SmartNet: hunted in order; DMR/NXDN: all watched |
| `modulation` | `auto` | `auto`, `fsk4`, `qpsk` (P25 receiver bank) |
| `talkgroupsCsv` / `talkgroupsName` | "" | talkgroup file contents and its name |
| `unitNames` | — | `{csv, name, mode: user / ota / user_only / none}` |
| `expect` | {} | site lock: `nac`, `wacn`, `sysId`, `rfss`, `site` (decimal) |
| `voiceChannelsHz` | [] | from the survey; only for auto-centring |
| `recording` | {} | per-system overrides (10.7) |
| `siteGroup` | "" | multi-site grouping |
| `plugins` | {} | per-system plugin settings; an array = several copies |
| SmartNet | | `bandplan` (800_standard, 800_reband, 800_splinter, 900, obt…), `bandplanBaseHz`, `bandplanSpacingHz`, `bandplanOffset`, `bandplanHighHz`, `defaultMode` (analog / digital) |
| DMR | | `lcnTableHz` {LCN: Hz}, `dmrChannelsHz`, `colorCode` |
| NXDN | | `nxdnType` (typeC / typeD), `nxdnRate` (nxdn48 / nxdn96), `nxdnChannelsHz`, `ran`, `lcnTableHz` |

A system is **active** when it is enabled and has at least one control channel. Active systems
are numbered 0, 1, … in config order; that number is `Call.system`.

### 10.5 `conventional[]`

| Key | Default | Meaning |
|---|---|---|
| `shortName` | `conv` | unique identity |
| `name`, `enabled` | "", true | |
| `squelchDb` | 8 | open threshold above the noise floor |
| `channelFile` | "" | CSV path; when set, `channels` is read from it |
| `channels[]` | [] | `{freqHz (required), mode: fm/p25/dmr/nxdn48/nxdn96, name, talkgroup, tone, description, tag, group, squelchDb, enabled}` |
| `recording`, `plugins`, `unitNames` | | as for systems |

`tone` examples: CTCSS `151.4`, DCS `D023N`, `NAC 293`, `CC 1 TS 2 TG 201`, `RAN 5 TG 201`.
Without a talkgroup, a channel is numbered by its kHz (then kHz×10 + k for more rows on the
same frequency).

### 10.6 `recording`

| Key | Default | Per system? | Meaning |
|---|---|---|---|
| `captureDir` | `~/TrunkRecorderPro` | | recordings folder |
| `prerollS` | 1.0 | | pre-roll for trunked calls |
| `maxRecorders` | 32 | | recorder pool size |
| `callTimeoutS` | 3 | yes | silence before a call ends |
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
| `compressWav` | false | yes | keep M4A instead of WAV |
| `audioArchive`, `callLog` | true | yes | keep audio / JSON after upload |
| `archiveFilesOnFailure` | true | yes | |
| `filenameFormat` | "" | yes | "" = `<short>/<Y>/<M>/<D>/<tg>-<epoch>_<freq>` |
| `dropDuplicateCalls` | true | | multi-site dedupe |
| `vocoder` | `fixed` | | `fixed`, `enhanced`, `mbelib` (IMBE) |
| `m4a` | `{encoder: auto, bitrateKbps: 32}` | | auto / ffmpeg / afconvert / fdkaac / none |
| `ramSpool` | `{enabled: false, sizeMb: 256, dir}` | | see 8.6 |

### 10.7 Per-system `recording`

Any key marked "per system" above can be set in a system's or conventional system's own
`recording`; `Recording::with(override)` copies the global settings and replaces what is set.
Other keys there are dropped.

### 10.8 `server`

`bind` 127.0.0.1, `port` 8080, `autoStart` false, `allowedOrigins` [], `interfaces`
[{name, path}], `home` "" (built-in UI).

### 10.9 `log`

`level` info (trace, debug, info, warning, error, fatal), `console` true, `file` false, `dir`
"" (`logs` beside the config), `syslogFriendly`, `syslog`, `color`, `frequencyFormat` mhz
(exp, mhz, hz), `talkgroupDisplayFormat` id (id, id_tag, tag_id), `statusAsString` true,
`controlWarnRatePerS` 10.

### 10.10 From config to engine

`Config::engine_config` builds trunk-core's `EngineConfig`: active systems → `SystemConfig`
(control channels, `CallConfig`, `SaveRules`, talkgroups, unit tags, receiver bank, `expect`,
protocol, site group); every conventional entry → `ConvSystem`; enabled channels →
conventional channel rows; sources with resolved centres; `prerollS`, `maxRecorders`,
`captureFrames`, `dropDuplicateCalls`, `vocoder`. File names, archive rules, M4A, spool,
server, log, monitor and plugins stay in the platform.

`Config::call_systems` numbers every system for plugins: active trunked systems 0, 1, …, then
enabled conventional systems with channels as 65535 − k.

### 10.11 Validation at Start (`Config::problem`, first failure wins)

1. No sources.
2. A file source without a path.
3. Nothing to record (no active system, no enabled conventional channel).
4. A missing or duplicate short name.
5. A source whose centre can't be resolved (auto-centre fails).
6. An active system's control channels (and DMR/NXDN carriers) outside every source; a bad
   SmartNet band plan.
7. More than 256 conventional systems.
8. One frequency in two conventional systems.
9. A channel frequency ≤ 0 or outside every source.
10. A tone that doesn't parse for its mode.
11. Rows sharing a frequency that can't be told apart.

**Auto-centring** (`resolved_centers`, `auto_center`): sources with `centerHz` 0 are placed, in
order, over the systems not yet covered. Start at the midpoint (rounded to 1 kHz), step ±12.5 kHz
until no channel is within 25 kHz of DC, and accept if every frequency is inside
half-width − 10 kHz.

### 10.12 Companion files (in the config's folder)

| File | Contents |
|---|---|
| `<short>.bandplan` | learned P25 IDEN tables / DMR & NXDN channel maps; saved every 10 s on change and at stop |
| `<short>.units.csv` | talker aliases heard over the air (Trunk Recorder's unitTagsOTA format) |
| `conventional.heard.json` | codes heard per conventional frequency |
| `radio/<short>.json` | the radio registry (units, talkgroups, affiliations); every 15 min and at stop |
| `stats/YYYY-MM-DD.jsonl` | a dashboard rollup per minute; gzipped after the day, kept 8 days |
| `plugins/<id>/` | installed plugins |
| `plugin-data/<id>/` | plugin state |
| `plugin-registry.json` | cached plugin index |
| `logs/` | log files |

Talkgroup CSV (stored inside the config): headed (`Decimal, Hex, Mode, Alpha Tag, Description,
Tag, Category, Priority, Preferred NAC, Preferred Site, Ignore`, any order) or Trunk Recorder's
headerless form. Channel CSV: `TG Number, Frequency, Tone, Mode, Alpha Tag, Description, Tag,
Category, Squelch dB, Enable`.

### 10.13 Example

```json
{
  "sources": [
    { "type": "rtlsdr", "serial": "00000101", "centerHz": 0, "rateHz": 2400000,
      "gainDb": 25.4, "agc": false, "ppm": 1, "autoTune": true, "guardHz": 75000 }
  ],
  "systems": [
    {
      "shortName": "county-p25", "name": "County Public Safety (Site 3)", "type": "p25",
      "enabled": true, "controlChannelsHz": [851012500, 851262500], "modulation": "auto",
      "talkgroupsCsv": "Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category\n1201,4b1,D,FD Disp,Fire Dispatch,Fire Dispatch,County Fire\n",
      "talkgroupsName": "county.csv",
      "expect": { "nac": 659, "wacn": 781824, "sysId": 989, "rfss": 1, "site": 3 },
      "recording": { "minCallS": 1, "recordUnknown": false },
      "plugins": { "openmhz": { "apiKey": "REPLACE_ME", "systemName": "countyp25" } }
    },
    {
      "shortName": "city-smartnet", "type": "smartnet", "enabled": true,
      "controlChannelsHz": [851637500], "bandplan": "800_reband", "defaultMode": "analog",
      "expect": { "sysId": 18205 }
    }
  ],
  "conventional": [
    {
      "shortName": "county-fire-conv", "enabled": true, "squelchDb": 8,
      "channels": [
        { "freqHz": 852087500, "mode": "fm", "name": "FG North", "tone": "151.4" },
        { "freqHz": 852087500, "mode": "fm", "name": "FG South", "tone": "D023N" },
        { "freqHz": 851812500, "mode": "p25", "name": "PD Tac 2", "tone": "NAC 293", "squelchDb": 12 }
      ]
    }
  ],
  "recording": { "captureDir": "/home/me/TrunkRecorderPro", "maxRecorders": 32, "compressWav": true },
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
`Session` keeps it for each live source with `ClockFit` (`trunk-app/src/session.rs`).

### 11.1 The clock path

1. **The driver hands over a block**, stamped `at = Instant::now()` in the source thread (by the
   worker in the browser), before it waits in any queue. Time spent queued for a busy engine never
   counts as clock error.
2. **`Session::push`**: the source's sample count grows by the block's length.
3. **`ClockFit::observe`**: lag = (at − epoch) − samples ÷ rate. Buffers only ever arrive late
   (USB, driver buffering), so the least lag is the clock's own.
4. **10 s window** (`CLOCK_WINDOW_MS`): the first block sets the target at once (this absorbs the
   radio's open time, such as a USRP's FPGA load); after that, each window's least lag is the new
   target.
5. **Steer, each poll**: within 0.25 s (`REANCHOR_S`) of the offset already set, the difference is
   drift and the offset slews toward it at most 1 ms per wall second (`CLOCK_SLEW`, 1000 ppm).
   Further than 0.25 s, samples were lost unreported (or invented): the offset is set at once and
   the log says "clock re-anchored".
6. **`Engine::set_clock_offset`**: the only place wall time enters the engine.
7. **`Source::time()`** = samples ÷ rate + offset: every frame and message is timed this way.
8. **Each `Trunk`'s `now_s`** comes from its control channel's source (timeouts, call starts and
   ends); the engine's own `now_s` from source 0. Multi-site matching compares these across
   sources, which is why their clocks must agree.
9. **Wall time** = `epoch_ms` (taken at start) + engine time. `Session` applies the local UTC offset
   for folder names.
10. **Gaps** (`push_gap`, `fill_quiet`): samples a driver reports dropped (up to 10 s,
    `MAX_GAP_S`), and silence for a running source quiet for over 1 s (`QUIET_MS`), are fed as
    faint noise and counted as samples, so sample time keeps pace with the air. Calls on a dead
    source still time out. Conventional channels don't learn their noise floor from it or open on
    it.

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
| Position in `config.systems` | every configured system | the Setup cards |
| Engine system index (`Call::system`) | active systems only | calls, events, live-audio frames, plugin `system` |
| Conventional system id | 65535 − k | calls and plugins |
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
| `Rollup` | `History` (a week in memory) and `stats/YYYY-MM-DD.jsonl` | `statsQuery` → `statsResult` |
| Session events → `Stats::observe` | `Registry` | `radioQuery` → `radioResult`; `radio/<short>.json` every 15 min and at stop |
| Session events → `Stats::observe` | `Monitor` | watchers (the hook for alert rules; none yet) and `monitorEvent` |
| monitor thread (desktop, every 2 s): the computer and the plugins | its own aggregator | `host` message every 2 s; rollups into the same history |
| `VoiceDecoder::report`, control-channel level separation | `Engine::channels()` | `rfDetail` / `decodeDetail`, only for subscribers |

### 12.2 Parts

- **`Sink`**: `counter(name, total)` is a running total (it may restart at 0 when a part is
  replaced, such as a retuned control channel) that the reader turns into a rate; `gauge(name,
  value)` is a current value. Names are `/`-separated and become series keys, for example
  `sys/<shortName>/cc/good`; renaming one loses its history.
- **`Aggregator`**: rates from counters; each series' value now; per minute its average, lowest
  and highest. A rollup line is `{"t": minute, "s": {key: [avg, min, max]}}`.
- **`History`**: a minute grid per series, a week in memory. `statsQuery` takes names or prefixes
  ending in `*`, a range (10m, 1h, 6h, 24h, 7d) or from/to, and is answered downsampled (360
  points by default).
- **stats files**: UTC days, appended and never rewritten, a finished day gzipped, days past 8
  deleted; read back into `History` in the background at start (`statsResult.loading` meanwhile).
- **`Registry`**: per system, bounded maps of talkgroups (5,000), units (20,000), unit pairs,
  frequencies and affiliations, with a week of hourly activity.
- **`Monitor`**: notable events (talkgroup or unit first seen, talkgroup active, call volume,
  control channel lost / regained, source drops, clipping, disk and spool space). The last 200 are
  sent in `hello`.
- **`stats::Shared`** holds the registry and monitor so they outlive a recording session; the
  platform answers queries from it.

---

## 13. The browser build

The same `Session` and `Engine`, compiled to WebAssembly (`wasm32-unknown-unknown`, `simd128`,
wasm-bindgen), run in a Web Worker. The RTL-SDR driver is the same `rtlsdr-nusb` on nusb's WebUSB
backend. The worker speaks the same `FromRecorder` / `ToRecorder` messages as the desktop
WebSocket, so the interface code is shared.

### 13.1 Parts

- **Page (main thread)**: the React interface; `WorkerTransport` (starts the worker, forwards
  commands over `postMessage`, plays live audio, keeps the config in
  `localStorage["trp.config"]`, since workers have no localStorage); a screen wake lock while
  recording (phones and tablets freeze a tab once the screen sleeps).
- **Worker** (`web/src/web/engine.worker.ts`): owns `WebSession` and the `WebRtl` dongles.
  - `pumpRtl`: `await rtl.next()`, then `session.push(source, bytes, dropped, wallMs())`.
  - Capture files: read in 128 KiB slices, paced to real time or not (the browser forgets the file
    on reload).
  - A 50 ms `setInterval` calls `session.poll`: protocol messages and audio frames go to the page
    (`{msg}` or `{audio, tg}`); call files, talker aliases, heard codes and (once a minute) the
    radio registry go to OPFS.
- **OPFS**: the origin's private file system; **Export to folder…** copies calls out
  (`showDirectoryPicker`).
- wasm exports: `WebSession`, `WebRtl`, `WebSurvey`, `WebProfiler`. Build: `cargo build -p trunk-web
  --target wasm32-unknown-unknown --profile dist` with `-C target-feature=+simd128`, then
  `wasm-bindgen --target web` (`npm run build:wasm`).

### 13.2 Desktop versus browser

| | Desktop | Browser |
|---|---|---|
| Threads | source threads, engine, finish, encoders, plugins, web server | one worker |
| Back-pressure | a 256-block queue before the engine | none needed: the pump asks for the next block only after pushing the last, so a slow engine leaves USB transfers waiting and the dongle overflows (reported as `dropped`) |
| Transport | WebSocket and binary audio frames | `postMessage` |
| Config | `config.json` | `localStorage["trp.config"]` |
| Files | recordings and data folders | OPFS |
| Sources | RTL-SDR, USRP, Airspy, SoapySDR, files | RTL-SDR over WebUSB, picked files |
| Not available | | plugins, M4A, channel files, `findRadios`, `listDir`, Trunk Recorder import from disk, `quit` |

---

## 14. The interface protocol

One WebSocket, `/api/ws`: JSON text both ways plus binary frames for live audio.
`web/src/protocol.ts` is the source of truth; `docs/api/protocol.schema.json` is generated from it
(`npm run schema`); `trunk-pro/src/protocol_tests.rs` checks the Rust side against the schema. The
full reference is `docs/api/README.md`.

### 14.1 A typical conversation

1. Client connects to `/api/ws` (the access guard runs first; see 14.5).
2. Recorder → `hello`: version, platform, config and its path, devices, phase, the last 300 calls,
   talker aliases, heard codes, radios, survey state, recent events, the computer, plugin
   runtimes. Then `plugins`.
3. Recorder → everyone: `status` (about every 500 ms while recording), `stats` (every second),
   `host` (every 2 s, desktop).
4. Client → `subscribe {topics: ["spectrum:0", "log"]}` (replaces the last; up to 64); recorder →
   `subscribed`; then topic messages to this client only.
5. Client → `listen {on, system, talkgroup}`; recorder → binary audio frames, filtered per client.
6. Recorder → `concluded {entry}` for each call, once its files are on disk.
7. Client → `statsQuery {id, series, range}`; recorder → `statsResult {id, …}` to the asker.
8. Client → `setConfig {config}` (always the whole config); recorder → `config` to everyone.
9. Client → `start` / `stop`; recorder → `state` to everyone.

### 14.2 Who gets what

| Delivery | Messages |
|---|---|
| On connect | `hello`, then `plugins` |
| Every client | `state`, `status`, `stats`, `host`, `concluded`, `callFiles`, `monitorEvent`, `unitAlias`, `heard`, `config`, `survey`, `surveySpectrum`, `pluginRuntime`, `pluginInstall`, `quit` |
| Topic subscribers | `spectrum` (`spectrum:<source>`, about 7/s, 512 bins), `log` (`log`), `rfDetail` (`rf:<source>`, 1/s), `decodeDetail` (`decode:<shortName>`, 1/s), per-core detail in `host` (`platform`) |
| The sender only | `devices`, `radios`, `dir`, `trConfig`, `pluginStore`, `subscribed`, `statsResult`, `radioResult`, `olderCalls`, `sourceProfile`, `error`, `notice` |
| Binary, to listeners | live audio frames |

Nothing is computed for a topic nobody watches. A client more than 4,096 messages behind on the
hub skips the oldest without notice.

### 14.3 Commands (client → recorder)

| Area | Commands |
|---|---|
| Recording | `setConfig`, `start`, `stop`, `quit`, `channelFile` |
| Watching | `subscribe`, `listen`, `statsQuery`, `radioQuery`, `olderCalls` |
| Setup | `devices`, `findRadios`, `listDir`, `readTrConfig`, `profileSource`, `surveyStart`, `surveyListen`, `surveyRescan`, `surveyStop` |
| Plugins | `plugins`, `pluginStore`, `installPlugin` (by id, or repository and tag), `addPlugin`, `removePlugin` |

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
| `/api/version`, `/api/schema`, `/api/protocol.ts`, `/api/docs`, `/api/interfaces`, `/api/*` | version, schema, TypeScript types, API docs and examples, custom interfaces |
| `/calls/<path>` | recordings (folder, then RAM spool), WAV ↔ M4A substitution, Range requests |
| `/builtin/`, `/ui/<name>/`, `/` | the embedded interface, interfaces from disk, the home interface |

Every request passes a guard. Programs that aren't browsers send no `Origin` and are let in. A
browser page must be the recorder's own origin or listed in `server.allowedOrigins` (which also
gets CORS headers); anything else gets 403. Bound to localhost, the `Host` must be localhost too,
which defeats DNS rebinding.

---

## 15. Where things live

| Concern | File(s) |
|---|---|
| Channelizer | `trunk-core/src/dsp/channelizer.rs` |
| Receivers | `trunk-core/src/dsp/{c4fm,cqpsk,msd,fm,tones,signalling}.rs`, `smartnet/rx.rs` |
| P25 framer / FEC / bank | `trunk-core/src/p25/{frame,tsbk,voice,fec,diversity,phase2}.rs` |
| DMR | `trunk-core/src/dmr/` |
| NXDN | `trunk-core/src/nxdn/` |
| SmartNet | `trunk-core/src/smartnet/` |
| Vocoders, loudness | `trunk-core/src/mbe/`, `loudness.rs` |
| Engine, sources, recorder pool | `trunk-core/src/trunk/engine.rs` |
| Control channels per protocol | `trunk-core/src/trunk/control.rs` |
| Messages, TSBK parsing | `trunk-core/src/trunk/message.rs` |
| Calls, `RecorderHost`, ids | `trunk-core/src/trunk/calls.rs` |
| Voice decoders | `trunk-core/src/trunk/{voice,tracker,tdma}.rs` |
| Site lock | `trunk-core/src/trunk/identity.rs` |
| Saving a call | `trunk-core/src/trunk/{record,frames}.rs` |
| Multi-site | `trunk-core/src/trunk/multisite.rs` |
| Conventional | `trunk-core/src/trunk/conventional.rs` |
| Config | `trunk-app/src/config.rs` |
| Session, outputs | `trunk-app/src/session.rs` |
| Threads, deliver, finish | `trunk-pro/src/runtime.rs` |
| SDR drivers | `trunk-pro/src/sdr.rs`, `trunk-pro/src/radio/` |
| Web server | `trunk-pro/src/server.rs` |
| Plugin host | `trunk-pro/src/plugins/` |
| Plugin protocol and SDK | `trunk-recorder-plugin/src/` |
| Interface protocol | `web/src/protocol.ts` → `docs/api/protocol.schema.json` |
