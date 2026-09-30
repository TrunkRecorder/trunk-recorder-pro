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
- **Conventional channels**: analog narrowband FM and P25, alongside a
  trunked system or on their own. Found by energy in the spectrum the
  channelizer already computes (an idle channel costs almost nothing), opened
  with pre-roll so transmissions start in full; per-channel mode, name,
  talkgroup and squelch (dB above the measured noise floor). Kept in the
  interface (with CSV import and export), in the config, or in a linked CSV
  channel file to edit in a spreadsheet; Trunk Recorder's channel file and
  config import.
- **Find my system**: a first-run survey for people who don't know their
  frequencies. It scans the land-mobile bands (800 / 700 / 900 MHz, UHF,
  VHF, optionally UHF federal and T-band) for carriers that never key down,
  checks each with the P25 receivers, then listens to the best control
  channel and reports its WACN / System ID / NAC / site, band plan,
  alternate control channels, neighbouring sites and voice channels. From
  the frequency the channel announces versus where it is heard, it measures
  the radio's frequency correction (ppm); on an RTL-SDR it also picks the
  gain. **Add this system** adds it with a site lock, and sets the ppm, gain
  and a center that covers the most voice channels seen. Also `trunk-lite survey`.
- **Several systems and sites at once**: each P25 system — or each site of
  a multi-site system — follows its own control channel, with its own short
  name (folder), band plan, talkgroups and modulation, sharing the sources
  and recorders. A **site lock** (NAC, WACN, System ID, RFSS, site; filled in
  by the survey) keeps each on its own control channel; neighbouring sites a
  control channel announces can be added with one click. The dashboard
  groups sites of one system and filters calls, history, log and live audio
  by system; sources left on Auto are placed over the systems not yet
  covered. Configs with one system carry over unchanged, and a Trunk
  Recorder config's P25 systems are all imported. `trunk-lite replay
  --system name:Hz[:nac=…,site=…]`.
- **Several RTL-SDRs** feeding the systems, over a pure-Rust USB driver (no
  librtlsdr / libusb to install).
- **USRP and Airspy** sources (optional): used when UHD / libairspy is
  installed, found at run time by the same binary.
- Captures in `cu8`, `cs16` or `cf32` (GNU Radio / UHD) formats.
- **Trunk Recorder–compatible output**: WAV + call JSON in Trunk Recorder's
  folder layout and field names; talkgroup CSV import; import of a Trunk
  Recorder config. `error_count` is the call's FEC-corrected bit errors,
  and `errorList` breaks them down per 10 s of audio (errors, repeated or
  muted frames, worst frame) so bursts stand out.
- **Vocoder frame capture** (setting "Save vocoder frames"): each call's
  decoded voice frames and error counts as `<call>.frames.jsonl`;
  `trunk-lite tool revoice` vocodes one again.
- **Browser interface**: setup, live status, waterfall per dongle, active
  calls with live listening, recent recordings.
- **Unattended use**: start recording at launch (`--start` / a setting),
  Ctrl-C / SIGTERM / Quit save calls in progress, a systemd service file;
  opening the app again shows the running instance.
- **Browser version**: the same engine as WebAssembly, WebUSB dongles, calls
  stored in the browser and exportable to a folder.
- Packages: macOS app (universal, DMG), Linux x86-64 / ARM64 (glibc 2.28+)
  with an installer, Windows 64-bit, and the browser build.
