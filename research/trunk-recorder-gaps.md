# Trunk Recorder features not in Trunk Recorder Pro

September 2026. Found by listing the settings Trunk Recorder's config reader
accepts (`config.cc`, `setup_systems.cc`), plus its recorders, decoders and
plugins, then searching this repo for each one. The Trunk Recorder source is
the fork in `~/Projects/Trunk Recorder/source`.

## Major gaps

| Feature | Trunk Recorder | Trunk Recorder Pro |
|---|---|---|
| DMR | Conventional DMR, plus a DMR trunking parser in the fork (`dmr_parser.cc`, `dmr_trunking.cc`, `dmr_recorder`) | None: P25, SmartNet, and conventional FM and P25 only |
| Upload and output plugins | Broadcastify Calls, Rdio Scanner, OpenMHz, simplestream / streamer (live audio over UDP/TCP), stat_socket (status to a server), unit_script, `uploadScript` | Plugins for OpenMHz, Broadcastify Calls, Rdio Scanner, simplestream and `uploadScript` (as the upload-script plugin). Not ported: stat_socket, unit_script, and streamer (gRPC; Trunk Recorder doesn't build it) |
| Talkgroup priority | Higher-priority talkgroups take a recorder when all are busy | Priority column read but unused; a full pool gives `no_recorder` |
| Duplicate calls across sites | `multiSite` drops the same call heard on several sites | Every site's copy is saved (on the roadmap) |
| Tones and NAC on conventional channels | CTCSS/DCS tones and P25 NAC tell users of one frequency apart | Tones filtered from the audio, not matched (on the roadmap) |

## Smaller gaps

**Recording rules**
- `minDuration`, `maxDuration` and `minTransmissionDuration` filters. The app only has "keep silent calls".
- `conversationMode: false`: each transmission saved as its own file.
- `hideEncrypted` and `hideUnknownTalkgroups`: log-only settings.

**Files and archive**
- `compressWav`: the app writes WAV only; M4A is made only for plugins.
- `audioArchive: false`, `archiveFilesOnFailure`, `filenameFormat`, `callLog`, and automatic deletion of old files.
- The call JSON writes `signal`, `noise` and `freq_error` as 0 instead of measuring them.

**Names and tables**
- A user-supplied unit names file (`unitTagsFile`, with regex modes). The app keeps only talker aliases learned over the air.
- `talkgroupDisplayFormat`, `customFrequencyTableFile` and `lcnTable`.

**Analog signalling**
- Star (GE, 400 bps PSK) and TPS (P25 frames in analog audio) unit IDs. MDC1200 and FleetSync are done (see below).
- FleetSync II's interleaved ECC framing: only plain FleetSync blocks are read.

**Debugging and hardware**
- Per-call IQ recording (`sigmfRecorders`, `conventionalSIGMF`) and `debugRecorder`. The app has only whole-band `trunk-pro capture`.
- Radios: Trunk Recorder uses osmosdr and SoapySDR (HackRF, SDRplay, bladeRF, LimeSDR and more). The app supports RTL-SDR, USRP and Airspy.
- Per-source `digitalLevels`, `analogLevels`, AGC and `autoTune`, and per-model gain stages (LNA, VGA, mixer).

## Suggested order

1. ~~Broadcastify and Rdio Scanner plugins~~ (done, with simplestream and upload-script).
2. Duplicate detection across sites.
3. Talkgroup priority when recorders run out.
4. CTCSS/DCS/NAC matching on conventional channels.
5. DMR: the largest job.


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

## Analog signalling (done, not yet committed)

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

