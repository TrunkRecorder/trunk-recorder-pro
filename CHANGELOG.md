# Changelog

Each version's section is its GitHub release notes (CI copies the section
under `## [<version>]` when the tag `v<version>` is pushed).

## [0.1.0] — unreleased

The first release of the Rust rewrite: one self-contained program per
platform, with no GNU Radio, OP25 or other runtime dependencies.

- **P25 Phase 1** control channel and voice (C4FM and CQPSK/LSM simulcast),
  with receiver diversity and soft-decision FEC: 98–99 % of control messages
  on a simulcast site where op25's decoder gets 62 %.
- **P25 Phase 2 TDMA** voice (H-DQPSK, AMBE+2), descrambled with the
  system's WACN / System ID / NAC from the control channel.
- **Motorola SmartNet / SmartZone** (Type II) control channels: 3600 baud
  2FSK, soft-Viterbi OSW decoding, Trunk Recorder's OSW parser and band
  plans (800 standard / rebanded / splinter, 900, OBT `400_custom`). Voice
  follows the grant: P25 Phase 1 for digital grants, narrowband FM for analog
  (`defaultMode` for talkgroups never heard granted). Configured with Trunk
  Recorder's names (`type` "smartnet", `bandplan`, `bandplanBase`, …). On
  WMATA's OBT system: 90–99 % of OSWs where Trunk Recorder gets 40–60 %.
  SmartNet systems work alongside P25 ones (each system picks its type), and
  Trunk Recorder configs with SmartNet systems import.
- **DMR**: conventional channels (`mode` "dmr", each slot its own calls,
  with the colour code) and trunked sites (`type` "dmr"): Capacity Plus and
  Linked Capacity Plus followed by their link control on every repeater,
  Capacity Max / Connect Plus / Tier III by their grants. Logical channel
  frequencies are learned from the air (or Trunk Recorder's `lcnTable`);
  keyed checksums (restricted access) are recognised. Repeated blocks and
  embedded link control are soft-combined, which recovers most of them on a
  weak site. Voice through the Phase 2 AMBE+2 decoder. Trunk Recorder DMR
  configs import. `tool dmrscan` / `tool dmr` for captures.
- **DMR in Find my system and the dashboard**: the scan recognises trunked
  DMR sites (kind and colour code) and adds them; Business UHF / VHF bands.
  The dashboard shows a DMR site's rest channel, watched frequencies with
  each slot's call, and its channel table. Simplex / talkaround DMR (a
  mobile's bursts, nothing between them) is received: the 4FSK receiver
  leaves quiet stretches out of its timing and levels.
- **Weak signals**: the C4FM receiver (P25 C4FM and DMR) has a matched
  filter, takes its levels from the symbol clusters, and re-decides
  symbols by multi-symbol detection below the discriminator's threshold;
  SmartNet decides by tone energies. Half the codewords decoded at about
  6 dB less signal on P25 C4FM voice, 8 dB on DMR voice, 1.7 dB on
  SmartNet (`tool snr`, which measures it; see research/weak-signal.md).
- **Find my system finds SmartNet** too, and learns its band plan from the
  air (which carrier comes up when a channel number is granted) — on WMATA,
  exactly Trunk Recorder's hand-made `400_custom` plan, all four control
  channels and the voice channels, from one dongle.
- **Talker aliases**: the names radios send over the air with their unit
  ID (Motorola and Harris, Phase 1 and Phase 2 — Trunk Recorder's
  decoders), on trunked systems (SmartNet's P25 voice included) and
  conventional P25 channels. On DC's system: the same names Trunk Recorder
  decodes. The interface shows a radio's alias in place of its unit ID
  (the ID on hover), in live calls and history, and history can be
  filtered by it. Encrypted calls aren't recorded, but their voice channel
  is still followed (when a recorder is free) for the link control sent in
  the clear: who spoke and their aliases. Each system's aliases are kept between runs in
  `<shortName>.units.csv`, Trunk Recorder's `unitTagsOTA` format, and saved
  in each call's `srcList` as `tag_ota`.
- **Conventional channels**: analog narrowband FM and P25, alongside a
  trunked system or on their own. Found by energy in the spectrum the
  channelizer already computes (an idle channel costs almost nothing), opened
  with pre-roll so transmissions start in full; per-channel mode, name,
  talkgroup and squelch (dB above the measured noise floor). Kept in the
  interface (with CSV import and export), in the config, or in a linked CSV
  channel file to edit in a spreadsheet; Trunk Recorder's channel file and
  config import.
- **Unit IDs on analog calls**: MDC1200 and FleetSync (1200 and 2400 baud)
  bursts are decoded from the audio of analog calls, conventional and
  SmartNet, into `srcList`; an MDC1200 emergency flags the call. Always on,
  at about 0.02 % of a core per analog call being recorded.
- **Tones on analog calls**: the CTCSS tone (all 51) or DCS code (all 112,
  either polarity) each conventional FM call carries is identified and
  written to the call JSON as Trunk Recorder's `tone_detected` /
  `tone_confidence`, and shown in the call list.
- **Tones on conventional channels**: an analog channel's Tone (typed as
  RadioReference or Trunk Recorder write it: `151.4 PL`, `023 DPL`, `D023N`)
  records only transmissions carrying it. Several rows on one frequency
  split it by tone, each filed under its own talkgroup (numbered for you);
  a row with no tone takes the rest. Transmissions are held until their tone
  is known, so nothing is cut off. Trunk Recorder's channel file `Tone`
  column reads as is.
- **NAC and colour code on conventional channels**: the same Tone column
  takes a P25 NAC (`293 NAC`, `$293`) or DMR colour code, slot and
  talkgroup (RadioReference's `CC1 TS2 TG201`), so P25 users of one
  frequency are told apart by NAC and a DMR repeater's traffic picked by
  slot and talkgroup.
- **Heard codes**: under each conventional frequency, the tones, NACs and
  colour codes it has carried (recorded calls and transmissions no row
  took), kept between runs, each with an **Add** that makes a row for it.
- **Find my system**: a first-run survey for people who don't know their
  frequencies. It scans the land-mobile bands (800 / 700 / 900 MHz, UHF,
  VHF, optionally UHF federal and T-band) for carriers that never key down,
  checks each with the P25 receivers, then listens to the best control
  channel and reports its WACN / System ID / NAC / site, band plan,
  alternate control channels, neighbouring sites and voice channels. From
  the frequency the channel announces versus where it is heard, it measures
  the radio's frequency correction (ppm); on an RTL-SDR it also picks the
  gain. **Add this system** adds it with a site lock, and sets the ppm, gain
  and a center that covers the most voice channels seen. Also `trunk-pro survey`.
- **Several systems and sites at once**: each P25 system — or each site of
  a multi-site system — follows its own control channel, with its own short
  name (folder), band plan, talkgroups and modulation, sharing the sources
  and recorders. A **site lock** (NAC, WACN, System ID, RFSS, site; filled in
  by the survey) keeps each on its own control channel; neighbouring sites a
  control channel announces can be added with one click. The dashboard
  groups sites of one system and filters calls, history, log and live audio
  by system; sources left on Auto are placed over the systems not yet
  covered. Configs with one system carry over unchanged, and a Trunk
  Recorder config's P25 systems are all imported. `trunk-pro replay
  --system name:Hz[:nac=…,site=…]`.
- **Calls heard on several sites are saved once** (Trunk Recorder's
  `multiSite`). Sites are grouped into systems automatically, by the WACN /
  System ID (P25) or System ID (SmartNet) their control channels announce;
  a `siteGroup` name groups DMR sites or ISSI-linked systems. Where Trunk
  Recorder records only the first site's grant, every copy is recorded here,
  and the one decoded most cleanly is kept, so a site fading mid-call
  doesn't cost the call. The talkgroup CSV's **Preferred Site** (or Trunk
  Recorder's **Preferred NAC**) wins when its copy is nearly as good. Live
  audio plays one copy; the dashboard shows "also on …". On by default
  (`recording.dropDuplicateCalls`). Trunk Recorder imports keep their
  multi-site settings. `--system …:group=name` for replays.
- **Several RTL-SDRs** feeding the systems, over a pure-Rust USB driver (no
  librtlsdr / libusb to install). New dongles start at 25.4 dB gain: higher
  gains overload the front end near strong 800 MHz transmitters (about 3x
  the voice-frame errors at 38.6 dB).
- **USRP and Airspy** sources (optional): used when UHD / libairspy is
  installed, found at run time by the same binary.
- **Gain control per radio**: an AGC switch on every kind (RTL-SDR tuner,
  USRP, Airspy, SoapySDR); the Airspy's linearity or sensitivity step, or
  its LNA, mixer and VGA stages by hand; a SoapySDR device's gain stages
  (HackRF LNA / VGA / AMP, SDRplay IFGR / RFGR, …) one by one. Trunk
  Recorder's `agc`, `lnaGain`, `mixGain`, `ifGain` and `gainSettings` import.
- **AutoTune** (Trunk Recorder's `autoTune`, per source): each source's
  frequency error is measured on its P25 and SmartNet control channels and
  shown on the dashboard, with the ppm that would remove it; switched on,
  new voice channels open at the corrected frequency and a P25 control
  channel more than 150 Hz off is reopened (at most every 200 s). On the
  DCFD capture with the source set 1 kHz off: +1.31 ppm measured, every
  call's voice within 30 Hz of the 1.14 kHz expected. The call JSON's
  `freq_error` is now measured too (the voice's offset, averaged over the
  call).
- Captures in `cu8`, `cs16` or `cf32` (GNU Radio / UHD) formats.
- **Call rules for every system, and each system's own**: the Recording
  tab's call rules apply everywhere unless a system (or the conventional
  channels) sets its own under **Recording override**. New ones, after
  Trunk Recorder's: shortest call (`minDuration`), shortest transmission
  (`minTransmissionDuration`, key-ups and data bursts left out), longest
  call (`maxDuration`: saved in parts, nothing lost or repeated between
  them), digital and analog levels in dB (`digitalLevels` /
  `analogLevels`), an .m4a of every call (`compressWav`), audio and JSON
  deleted once every upload plugin has had the call (`audioArchive`,
  `callLog`, kept when an upload failed: `archiveFilesOnFailure`; a call no
  plugin takes is always kept), and folders and file names
  (`filenameFormat`, Trunk Recorder's tokens and time formats). An imported
  Trunk Recorder config brings these over: a setting the same on every
  system becomes the Recording tab's, the rest each system's own.
- **Ignore** column in the talkgroup file (`true`, `yes`, `1`, `x`; or
  Trunk Recorder's priority −1): those talkgroups are never recorded.
- **Unit names** (Trunk Recorder's `unitTagsFile` and `unitTagsMode`):
  `unit,name` lines or `/regex/,Name $1` patterns, per system; the call
  JSON's `srcList` `tag` and the interface use them.
- **Reception on every call**: the call JSON's `signal` and `noise` (dBFS),
  `snr` (dB) and `clean_voice_pct` (voice frames decoded cleanly); the call
  list shows the SNR.
- **A log, as Trunk Recorder's**: its line format and options (`logLevel`,
  `consoleLog`, `logFile` / `logDir`, `syslogFriendly`, `logColor`,
  `frequencyFormat`, `talkgroupDisplayFormat`, `statusAsString`,
  `controlWarnRate`), to stderr, daily files and the system log (syslog),
  with a status summary every 200 s. `--log-level` on the command line.
- **Trunk Recorder–compatible output**: WAV + call JSON in Trunk Recorder's
  folder layout and field names; talkgroup CSV import; import of a Trunk
  Recorder config. `error_count` is the call's FEC-corrected bit errors,
  and `errorList` breaks them down per 10 s of audio (errors, repeated or
  muted frames, worst frame) so bursts stand out.
- **Plugins**: programs of their own that the recorder runs while it records
  and tells what happens — calls starting, ending and landing on disk, radio
  activity, live audio, status — over JSON lines on stdin/stdout. They only
  watch; a plugin that crashes is restarted, and one that falls behind loses
  events rather than slowing the recorder. Calls are encoded to M4A once
  for every plugin that asks (ffmpeg, macOS's afconvert or fdkaac, whichever
  is there; WAV only without one). The `trunk-recorder-plugin` crate is the
  protocol and a Rust SDK.
  - **Plugin store**: the Plugins page installs, updates and removes plugins
    from the [plugin registry](https://github.com/TrunkRecorder/plugins),
    each download checked against the registry's SHA-256, and shows how
    each is running: calls handled, status, recent log. A plugin that isn't
    listed can be installed from its GitHub release, marked as not reviewed.
    `trunk-pro plugin search | install | update | uninstall` from the
    command line, and `list | describe | run` to try one against recorded
    calls.
  - **Set up in Setup**, in the config like everything else: a Plugins tab
    to turn each on and fill in its settings, and each system's card has
    its settings for every plugin that's on (an OpenMHz key, say). Settings
    are drawn from what the plugin describes; while recording, changes
    restart the plugins at once. Trunk Recorder's uploaders and their keys
    come over with an imported config.
- **Vocoder frame capture** (setting "Save vocoder frames"): each call's
  decoded voice frames and error counts as `<call>.frames.jsonl`;
  `trunk-pro tool revoice` vocodes one again.
- **Browser interface**: setup, live status, waterfall per dongle, active
  calls with live listening, recent recordings.
- **Unattended use**: start recording at launch (`--start` / a setting),
  Ctrl-C / SIGTERM / Quit save calls in progress, a systemd service file;
  opening the app again shows the running instance.
- **Browser version**: the same engine as WebAssembly, WebUSB dongles, calls
  stored in the browser and exportable to a folder.
- Packages: macOS app (universal, DMG), Linux x86-64 / ARM64 (glibc 2.28+)
  with an installer, Windows 64-bit, and the browser build.
