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
| Analog signalling decoders | MDC1200, FleetSync, Star, TPS: unit IDs on analog calls | None; analog calls have no unit IDs |
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

The analog signalling decoders only matter for users with analog systems.

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
