# Changelog

Each version's section is its GitHub release notes (CI copies the section
under `## [<version>]` when the tag `v<version>` is pushed).

## [Unreleased]

- **Saving never holds up recording or a click.** Large systems on slow disks (SD cards) waited on
  whole-file writes: an Ignore click could take seconds, and the recording paused while talker
  aliases, heard codes or the radio registry were written.
  - Everything kept beside the config is now written on a thread of its own. If a file is
    queued several times, only the latest copy is written.
  - A talkgroup edit is told to every browser right away, and its file is written afterwards.
  - The conventional codes heard (`conventional.heard.json`) are written once a minute instead
    of after every call.
  - Talker aliases, sharing and band plans are written within 10 s of a change. The radio
    registry is still written every 15 minutes.
  - Everything is written when recording stops.
  - The browser version saves on the same schedule.
  - A page that connects gets the latest heard codes from memory, not from the file.

  What's next for storage, including SQLite, is in `research/storage.md`.

- **An Ignore click, or a talkgroup's name edited, changes just that row.** Before, the whole
  talkgroup file was sent, rewritten and sent to every browser. The new `setTalkgroup` command
  and `talkgroupEdited` message are in the API docs, and `client.js` handles them. Several clicks
  in a row end where the last one left it, also with another browser editing at the same time, and
  an edit made while the connection was down is sent again.
- **Setup says whether your changes are saved.** "Saving…", "All changes saved" or "Couldn't save"
  shows beside its tabs. A file the recorder couldn't write in the background is reported too.
- **Plugin settings that would restart a running plugin wait for Apply.** Discard drops them.
  Plugins that take settings while running, and anything changed while not recording, save at
  once.
- **The Airspy gain sliders are saved once you let go**, not at each step dragged through.
- **The radio registry tracks what changed per row**, for storing it a row at a time later.
  `research/storage.md` measures the ways to store it on the Pi, including issue #100's SQLite
  design (`tools/storage-bench`).
- **Settings are saved once you finish typing, not per letter.** A text setting in Setup (a name,
  a short name, a path, a plugin's settings…) is saved when you leave the field or press Enter.
  Escape puts it back. Before, each pause in typing was a save: a system's short name typed as
  "fire4a" renamed it to f, fi, fir… on the way, moving its talkgroup file each time, and a
  plugin's settings restarted it on each pause. Switches and menus still act at once.

- **Which sites of a system can be heard here** (from #95, by Bill Johnson). Find my system can look
  for a system you know of instead of sweeping the bands. It listens to just that system's
  frequencies and says, site by site, which it heard: **Heard**, **Another site**,
  **Unconfirmed**, **Not heard** or **Not checked**, with why, coloured by how well each decoded.
  - **Check sites** on a system in Setup → Systems. Afterwards, **Turn off the sites not heard**.
  - **Check which I can hear** in the directory import. Afterwards, **Import the sites heard…**.
  - **RadioReference sites exports** (`trs_sites_NNNN.csv`) import through Setup → Systems →
    **Import…**: choose the kind of system, and optionally tick only the sites within some miles of
    a city or ZIP code.
  - **Licensed systems no one has shared** are now found by listening to their licensed frequencies
    rather than sweeping whole bands.

  Every frequency checked is probed, whether or not a carrier stands out there, with only the
  system's kind of receivers (about a third of the CPU), and its carrier is metered. A wide radio's
  probes are shared out over half the computer's cores. **Listen to each frequency for** sets how
  long: 5 s by default, 1 minute for DMR. The sites are judged together: a frequency several sites
  list confirms none by itself, and a DMR site of another kind of trunking than the rest is a
  neighbour, not the site. Command line: `trunk-pro survey --freqs 451.175,452.175 --type dmr`.

- **Scanning on a spare radio while recording.** Find my system can scan with a radio that's plugged
  in but no source uses, while the others keep recording. It's listed under **Radio** as *(spare)*,
  and a check started while recording uses one. A radio a source names is refused. `trunk-pro
  survey --radio '{…}'` scans with any radio, written as a source in `config.json`.

- **The survey recognises EDACS control channels**, and their site ID, alongside P25, SmartNet, DMR
  and NXDN.
- **Talkgroups, chosen the same way everywhere.** The uploaders (OpenMHz, Broadcastify, Rdio
  Scanner) have a **Talkgroups** menu on each system's card: **Every talkgroup**, **Only some** (with
  **Except these**) or **All but some**, with the lists shown only when they apply. Lists take
  numbers, ranges (`1000-1099`) and patterns (`507*`), with commas, spaces or semicolons between;
  the form flags an entry that isn't one of them, and the plugin won't start with one. **Breaking:**
  lists saved without a menu choice no longer apply; choose **Only some** or **All but some** on the
  card. Trunk Recorder's talkgroup list names (`broadcastifyAllow`, `…Whitelist`, …) are no longer
  read from a plugin's settings. **Import Trunk Recorder config…** sets the menu from the allow / deny lists it brings over. See
  [Choosing talkgroups](docs/configuration/plugins.md#choosing-talkgroups). OpenMHz 0.1.7 gains
  talkgroups; Broadcastify 0.1.7 and Rdio Scanner 0.1.6 move to the menu.

- **A delay for all but some talkgroups.** A plugin delay's **Talkgroups** is the same menu: every
  talkgroup, only some (less any excepted), or all but some (`exceptTalkgroups`). "Thirty minutes
  for everything on dcfd but 1001" is one rule.

- **Fixed: a range typed with spaces** (`1000 - 1099`) in a delay or a signal's talkgroups was saved
  as three entries, and matched nothing. It's one range now, as the recorder reads it.

- **Plugin SDK 0.8.0:** `Talkgroups` (the menu and its lists, for a `SystemConfig`),
  `TalkgroupFilter::parse`, `filter::talkgroup_list` and `check_talkgroup`, and the form's
  `"x-talkgroups"` hint. `TalkgroupFilter::describe` reads "only 507*; except 50799" / "all but 9".

- **The waterfall reads the frequency under the pointer.** Hover over a source's spectrum or
  waterfall to see the frequency there. Near a signal, the readout snaps to its peak and says how
  far over the noise floor it is. It works on the waterfall's history too, so a conventional
  channel that keyed up and went quiet can be identified without taking the recorder offline.

- **A switch for the network checks** (#37). The Platform page's Network card has a **Checks**
  switch (`monitor.networkChecks`, on by default): off, the recorder stops connecting to the
  internet check hosts every 15 s and asking the time server every 15 minutes, for a computer whose
  connection something else already watches. `probeHosts` and `timeServer` are kept for when it's
  turned back on. Applies at once.

## [0.2.10] — 2026-10-25

- **EDACS and SmartNet control channels lock every time** (#76). The 2FSK receiver (SmartNet,
  EDACS, ProVoice) keeps each tone's level as the mean of the symbols decided on its side. After a
  strong transient (a channel's first moments, a burst of interference several kHz off) those levels
  could settle wrong and never recover: one pulled most of the way to the other, or one past the far
  tone, the midpoint far off the carrier, a quarter to half of the bits wrong. EDACS (three samples a
  symbol) got there from its own start-up: no messages from a strong control channel, or no ProVoice
  audio, about one channel in fourteen, and the engine tests failed on Linux on aarch64 (Raspberry
  Pi). SmartNet got there after a strong burst off frequency. The levels are now watched against
  the quantiles of the last symbols, which don't depend on the decisions, and start again from them
  when they've walked away. On healthy signals nothing changes: the reference captures replay
  identically and weak-signal decoding is the same. The level fit is shared with the C4FM receiver
  (P25, DMR, NXDN), whose levels already start again from the quantiles every block.

- **Multi-site: the best copy of each transmission** (replaces #75; thanks to photounit for
  working out the problem). A system's sites don't always agree on what a call is: one keeps a
  voice channel through a conversation while another grants each transmission its own, so their
  copies didn't line up: transmissions were uploaded twice, or dropped with a shorter copy. Now:
  - **Conversations** (`conversationMode`, per system, on by default) can be turned off: each
    transmission is then a call of its own on every site, and the best copy of each is kept.
    Setup suggests it for a system with several sites.
  - A copy is dropped only when the copy saved holds all of it (the same time and talkers,
    decoded at least as well); anything else is saved too: better a duplicate than a
    transmission lost. A conversation has one copy from each site, so copies never chain.
  - Every call JSON has a `transmission_id` (`<shortName>-<talkgroup>-<radio>-<start ms>`), the
    same on every copy of one transmission saved.
- **The RAM spool in the service's own folder** (#88). Run as a systemd service with
  `RuntimeDirectory=` (the shipped `trunk-pro.service` now has it, with
  `RuntimeDirectoryPreserve=yes`), the Linux spool is `spool` in that folder under `/run` (using
  at most half of it) instead of `/dev/shm`, where systemd's `RemoveIPC` deleted it whenever
  someone logged out of the account it runs as, with the calls waiting in it for their uploads.
  docs/install/linux.md has a section on running it as a system service of your own.
- **Plugins can have a page of their own, buttons, and settings that change without a restart.** A
  plugin's manifest can ask for a page (`page`), listed under Plugins in the navigation, where its
  lists of settings are cards of their own, each item one line until opened, beside its log; for
  buttons elsewhere, such as a **Notify** on each of a system's signals in Setup that adds an item
  to its settings (`contribute`); and to be sent new settings as they change (`live_settings`)
  rather than every plugin restarting. Settings forms can tick systems, signals and lists of
  choices, name another list's items (`x-ref`, following renames), title and fold list items
  (`x-title`), and have buttons (`x-actions`) that ask the running plugin to do something, such as
  send a test, and show how it went; items can sum themselves up (`x-summary`), fields show only
  when they apply (`x-when`), and tones can be typed in with ▶ (`x-tones`). SDK 0.7.0:
  `Plugin::action`, `Plugin::settings`.
- **Plugins hear radios' and talkgroups' names** in their live events, looked up as each event is
  sent (as a call's JSON has them): a call's radios (`sources[].tag`), radio activity (`tag`, and
  the talkgroup's `alpha_tag`) and tone patterns (`src_tag`). No need to remember them from calls.
- **Airspy sample rate fixed** (#74, by photounit). The rate is now set by its IQ value even though
  real samples are streamed, so libairspy sends it as an index, which every firmware takes. Before,
  the doubled rate was sent by value, which firmware before v1.0.0-rc7 refuses. Worse, a Mini at
  3 MSPS asked for 6 (its first rate), so it ran at twice the rate it was set to.
- **Each radio says what it is and the rates it runs at** (#90, by auggiesoft). Setup lists an
  Airspy as "Airspy R2 · SN …" or "Airspy Mini · SN …" (from its firmware string: both report the
  same board id) and offers the rates that Airspy runs at. A bladeRF and a SoapySDR device say so
  once a source has opened them. Picking a different radio moves the rate to one it runs at, and a
  rate it can't do is flagged in Setup. A source refuses such a rate with one message for every
  kind of radio: "this Airspy R2 runs at 10 / 2.5 MSPS, not 6: pick one of those in Setup →
  Radios". A new Airspy source found by the setup guide starts at one of its rates.
- **Radios open one at a time.** libairspy (and librtlsdr, libhackrf) find a radio by opening every
  one of theirs, and skip any they can't open, so two sources starting together, or a device search
  beside one, could miss a radio (`AIRSPY_ERROR_NOT_FOUND` until the retry). Opens and searches
  for Airspy, bladeRF, RTL-SDR and SoapySDR radios now share one lock (the one SDRplay radios
  already used). An Airspy is opened to ask its model once per run, not on every page load.
- `trunk-pro devices` shows each Airspy's model and rates (and a bladeRF's once known).

- **Setup's FCC licenses fold.** A system's (or conventional system's) **FCC licenses** section
  folds to its name and a line ("20 frequencies, 5 call signs: …"); it starts folded once it has
  more than six, and remembers being opened or folded in this browser, as the cards do.
- **Pages, alert tones, DTMF and Morse IDs as call metadata** (#82, by taclane; #86). Every call's
  voice is searched for in-band tones: P25 Phase 2, DMR and NXDN tone frames name them exactly (as
  the vocoder plays them), and P25 Phase 1 and analog calls are measured. The call JSON lists them
  (`tones`) and what they make up (`tone_patterns`, each with `text` in words: a two-tone page with
  its timing format and likely paging codes, a long tone, a warble, a DTMF or KNOX string, a Morse
  ID). Each system's page has a **Tones** card listing what was heard, with ▶ to play it in the
  browser (again to stop) and **Add** to name a pattern as one of the system's **signals** (`signals`
  in its config, edited in Setup under the system's **Signals**). A tone counts as a signal's within
  ±1.5 % when measured in the audio and ±(15.7 Hz + 0.5 %) from tone frames, widened by its
  `extraTolerancePct`; the editor says what that comes to and which paging tones (any plan's) it would
  take in too. Calls that carry a signal list it (`signals`), the dashboard's event feed shows it, and
  plugins subscribed to `tone_pattern` hear every pattern as it ends, with the signals it is (one heard
  in decrypted voice only by plugins with `shareDecrypted`). Start refuses a signal it can't use.
  `trunk-pro tool tones` reads them out of saved calls (`.sdr`, `.frames.jsonl` or `.wav`).
- **In-band signalling is one path.** Analog unit IDs (MDC1200, FleetSync), CTCSS/DCS and the new
  tones are each a detector a call runs over its audio, and a call's recording is one type,
  trunked or conventional. Talkgroup lists everywhere (delay rules, signals, plugin filters) take
  numbers, ranges either way round and patterns alike: plugin filters now take ranges too.
- **Breaking, plugins:** SDK 0.7.0. The topic is `tone_pattern` (`TonePatternEvent`, the call JSON's
  `TonePattern` with the call's context); `TonePattern` gained `text`. KNOX signals are `knox: true`
  (no `KNOX ` prefix in `digits`).
- **A chart's values on hover sit beside the crosshair, not on the line** (#79). They covered
  the very peak you were trying to read; now they're to one side of it (the left past the
  middle), in the half of the chart away from the line. The activity-by-hour heatmap's tooltip
  likewise opens toward the middle instead of running off the card's edge.
- **A RAM spool deleted while running is made again, and said** (#88). On Linux, systemd's
  `RemoveIPC` (on by default, Raspberry Pi OS included) deletes a user's files in /dev/shm when
  their last login ends, so a recorder started over SSH lost its spool when that session closed:
  every call after was reported as **The RAM spool was full** and the spool's meter and charts
  went. It's now made again (a try a minute while that fails); calls turned away meanwhile
  aren't counted as full; a `spoolRemoved` event, and the Computer card and Platform page say
  how to stop it: `sudo loginctl enable-linger $USER` with the user service, or `RemoveIPC=no`
  in /etc/systemd/logind.conf.
- **On the air counts while calls are on the air.** A call's air time (`airMs`) was counted
  when it ended, all in that minute: a channel held open an hour drew a spike of 3600 s/min.
  It now counts a second at a time. The Decode page's **Calls** chart (conventional) reads
  On the air, s/min (every channel's together: 60 is one channel busy the whole minute) off
  its own scale on the right, and a chart's scale no longer stretches to a line's highest a
  minute when it doesn't draw that band. A talkgroup's airtime by hour is likewise spread over
  the hours its calls were on, not all in the hour each ended.
- **Charts in two units read each off its own scale**: Signal and demodulation's eye opening or
  phase error is on the right, beside SNR in dB. Dropped samples, Clipping, Lost /s and a
  plugin's Failed a minute show their band, so a short burst isn't averaged away.

## [0.2.9] — 2026-10-24

- **Add a system from the directory.** A system's page in the systems directory opens it in
  Trunk Recorder Pro (the web version, or the desktop app at its address), or downloads it
  for **Setup → Systems → Import…** (formerly "Import Trunk Recorder config…", which still
  takes those). Either way you see what comes first: its sites, licenses and named
  talkgroups, its short name, and, if you already have the system, updating it instead
  (nothing of yours is replaced). It keeps its directory link, so sharing it adds to the
  same record. See docs/guides/from-the-directory.md.
- **Which frequencies were out of band.** A system's Radio system page lists the frequencies of
  calls no source covered, in the span: how many calls, by hour, the last, and the nearest source
  with how far past its usable edge each is. **Out of band** in **What happened to calls**, and a
  warning of calls out of band (the Overview's system card too), open it. A warning that says
  where what it's about is listed links there. Kept between runs, in the radio registry.
- **Sharing says which voice channels were out of band**: each voice channel shared carries
  `outOfBand`, how many of its grants no source here covered.
- `radioQuery` `freqs`: `rows` are the frequencies with a call recorded (the Decode page's);
  `outOfBand` is new.
- **Sharing no longer rewrites the config.** A system's `directory` link is just its record's ID
  (and each site's `directoryId`), written once, by its first share. The record's version and
  when it was last shared are kept beside the config in `sharing.json` (the browser version keeps
  its own), so a config in git doesn't change every hour, and an edit in Setup isn't refused
  because a share saved the config meanwhile.
- **DMR calls have their frequency error** (`freq_error` in the call JSON), conventional and
  trunked: it was always 0. A conventional DMR channel's `autoTune` now corrects its source,
  as the docs said it did.
- **Trunked DMR, NXDN and EDACS sites measure their control channel's frequency error** for
  AutoTune and the dashboard's frequency error, as P25 and SmartNet sites do. They measured
  nothing from it before: a DMR radio's error went unmeasured, and an NXDN one's came only from
  calls.
- **Phase 2, DMR and NXDN tones no longer drop out on a weak signal.** A tone frame was recognised
  only if four unprotected bits at its end were zero, so a single bit error there lost it and the
  tone faded out for a frame. A tone frame is now recognised by its Golay-protected signature alone,
  as TIA-102.BABA-A §16 has a decoder do. A frame damaged past reading in the middle of a tone
  repeats the tone (up to 3 times, then fades, and stays faded while the damage lasts) rather than
  the speech from before it.
- **Choosing a location for Identify is a map.** Move it so its circle (5 km across) is roughly
  where you are: only your general area is needed. Or find a place by city ("Arlington, VA"),
  ZIP code or coordinates, or use your browser's location. The map is the systems directory's,
  from trunkrecorder.pro (its tiles, fonts and style), loaded only when the picker opens. In
  Setup's Identify, Share, the survey and the Setup Guide.
- **Setup shows a system's FCC licenses section only once a frequency has a license**
  (Identify, the air, or the config). **Sharing needs an FCC call sign**, for now: switching
  Share on without one opens Identify, and Share turns on only if a call sign is kept. A
  Motorola P25 system's call sign heard on the air counts at once; one found by Identify while
  recording does too.
- **Charts say what they show.** A chart of one line now shows its label
  and unit, as a chart of several does: the Clock card's ("this computer's
  clock against pool.ntp.org, ms"), the plugin queue and age at delivery,
  the internet connect time and a channel's frequency offset.
- **Charts drawn at their real width:** a chart's labels and dots are no
  longer stretched sideways on a wide screen (time charts, and the RF
  page's band and channel plots), and a y axis never shows two ticks that
  read the same (the Clock chart's "37, 37, 36, 36").
- **The radio systems' clocks read plainly** on the Clock card: "3.0 s
  behind them" (the system's clock against the calls' time) under a heading
  of their own, so they aren't taken for the time server's offset in ms.

## [0.2.8] — 2026-10-22

- **The clock, on the Platform page** (#61): how far off this computer's
  clock is, measured by one SNTP query to `monitor.timeServer` (default
  `pool.ntp.org`) every 15 minutes, and whether the operating system says
  its time service has it synchronized (Linux, macOS). The light turns
  amber at 250 ms off or when the OS says unsynchronized (red at 5 s), with
  an event. It only watches the clock: chrony, systemd-timesyncd, ntpd or
  macOS keep it. No new dependencies, no privileges; in a container it's
  the host's clock. A time server that doesn't answer (UDP port 123 is often
  blocked) isn't counted as the internet down.
- **How long calls take to reach each plugin's service** (#61): on each
  plugin's card, the age at delivery (from the call's end to the plugin's
  success) over the last 5 minutes or the hour, its parts (held by delay
  rules, waiting, the plugin's own upload) and the hour's longest. The same
  for every plugin (Broadcastify Calls, OpenMHz, Rdio Scanner, a script);
  measured by the recorder, by its own clock. Plugins built on SDK 0.6 tell
  their upload time apart; older ones show the age alone.
- **`clock_off` in a call's JSON** when the clock was off as it ended (250 ms
  or more, or the operating system said it wasn't synchronized).
- **The clock calls are stamped by, judged** (#61): calls keep the clock they
  started with, so a correction during a recording doesn't reach them. The
  Clock card now says how far that is from the clock now, and the warning
  goes by it ("Stop and Start again to stamp them by it").
- **Radio systems' own clocks** (#61): P25 Phase 2 systems broadcast their
  time to 7.5 ms (SYNC_BCST). Each site's offset from the call times is on
  the Clock card and in the site's status. Without an answer from the time
  server, the systems that vouch for their time are the reference.
- **The time service on the Clock card** (Linux: chrony, systemd-timesyncd):
  the server it follows, its stratum, when it last synchronized.
- **`monitor.clockWarnMs`** sets the clock warning (250 ms by default), and a
  jump of the clock is an event.
- **Plugin SDK 0.6.0:** `call.result` takes `took_ms`
  (`Host::call_result_took`; `CallQueue` sends it), and `CallRecord` has
  `clock_off`.
- **Share a system with the community directory** ([trunkrecorder.pro/directory](https://trunkrecorder.pro/directory/)):
  a switch in each system's Basics (`"share": true`). While it records, the recorder shares what it
  heard: the sites, their control and voice channels, neighbours, call signs heard on the air, and
  the talkgroups granted (numbers, how often, where, encrypted or not). Not talkgroup names or
  anything else typed in, and not a location. It shares after 5 minutes of decoding, then as it
  hears new talkgroups or voice channels (at most hourly), otherwise daily. Setup shows the last
  share and links to the system's page; the directory's IDs are kept as `directory` and each site's
  `directoryId`. P25, SmartNet, DMR and NXDN; desktop and the browser build.
- **Colours never look like a status light.** Green, amber, orange and red
  now mean only OK, a warning or a problem. Systems and chart lines share
  one palette of blues, teal, violets and magenta, and a system's mark is
  square where a light is round (#40).
- **A light names what it's about.** With several systems, radios or
  plugins, the navigation's lights say which one is in trouble: "ipscpend:
  Calls are being missed…". The Radio page shows the light and why on that
  system's card, its button in the picker and its "What happened to calls".
  The Overview's system cards include missed calls, and "Missed, 24 h" says
  which systems missed them (#41).
- **Two SDRplay radios in one process:** a second RSP source no longer
  fails with `SelectDevice() failed`. SoapySDRPlay3 opens one SDRplay API
  session per process, which can select only one radio, so trunk-pro now
  opens one more session whenever a source with `driver=sdrplay` in its
  device arguments opens while another radio is in use (as SDRTrunk does),
  including after Setup's Restart. Two radios on one USB bus
  can still run out of bandwidth: 3 + 3 MS/s failed in testing, and
  2.5 + 6 MS/s worked. A source at 2 MS/s or
  below runs its radio at 6 MS/s; use separate USB controllers if a
  pair doesn't fit.
- **Conventional systems on the dashboard** (#71): a conventional system has
  a working card on the Overview (its calls this run and in 24 h, and its
  channels) and on Decode (its calls, and each channel's mode, name, offset
  and quality), and it's in the status (`status.systems`).
- **AutoTune for conventional channels** (#71): they take up their source's
  frequency correction, as trunked channels do. A source with no control
  channel learns it from calls on the digital rows you turn **AutoTune** on
  for (a repeater output; `autoTune`, or the CSV's `AutoTune` column).
- **Live gain with several Airspys** (#69): checked and tested; an Airspy
  reopened after a USB stall comes back with exactly the gain last set while
  recording.

## [0.2.7] — 2026-10-22

- **EDACS** trunked systems (`"type": "edacs"`): Standard and Networked
  sites (GE / Ericsson / Harris), with their analog FM calls and **ProVoice**
  digital voice. The 9600 baud control channel is decoded (three copies of
  each message voted, BCH corrected; a word is kept only when two of its
  three copies agree), channel numbers turn into frequencies by each site's
  channel list (`lcnTableHz`, an **EDACS** section in Setup), and
  ProVoice's IMBE 7100 frames are decoded and spoken by the P25 vocoder.
  ESK 0xA0 systems are read with `esk`. Extended Addressing isn't decoded
  yet. Every frequency of a site is watched at once, so the control channel
  is found wherever it moves, and each LCN's frequency is checked against
  the air (the carrier that keys when a call is assigned it): a list whose
  numbers are off is corrected. A site is reported in failsoft only when its site ID says so and it has
  stopped assigning calls (some sites set the flag and trunk all the same).
  A site ID is believed once three in
  a row agree. A grant to the frequency the control channel is on is never
  followed. The 9600 baud burst at the start of an analog transmission is
  left out of the audio.
- **bladeRF** sources (optional; bladeRF 2.0 micro xA4 / xA9 and the
  original x40 / x115): used when libbladeRF 2.x is installed (`"type":
  "bladerf"`). Serial, any sample rate to 61.44 MSPS, overall gain or the
  board's AGC, analog filter bandwidth and bias-T; samples USB drops are
  counted from the board's timestamps. Trunk Recorder's osmosdr
  `bladerf=…` sources (and `"driver": "bladerf"`) import as bladeRF sources.
- **Gain changes apply while recording** on RTL-SDRs and Airspys (the
  gain and AGC on or off; an Airspy's preset or stages), and on USRPs,
  bladeRFs and SoapySDR devices (the gain in dB, AGC off), without a Stop
  and Start; the log says the gain each took, and a radio reopened after an error keeps
  the gain it was given. Other radio settings still apply when recording
  starts, which the log says too.
- **Who is licensed on each frequency.** A system, trunked or conventional, can keep an `fcc` map:
  the FCC licensee and call sign on each of its frequencies, in an **FCC licenses** table on its
  card. **Identify** on the card looks them up from a location you give (trunkrecorder.pro's FCC
  lookup; nothing is sent until you ask, and everything works the same without it). The survey's
  **Identify surveyed sites** adds a *Licensed to* column, and the setup guide names the systems it
  finds. Motorola P25 systems fill in their call signs from the air, from the base station ID they
  send on control and voice channels. Plugins get them in `hello`.
- **P25 Phase 2:** a Motorola `$83` MAC message no longer hides the messages after it in the same
  PDU.
- **Restart.** While recording, a Restart button beside Stop starts the run again with the
  settings as they are now. Changing a setting that only applies at start (radios, systems,
  recording options) shows a "Change requires restart" banner. Settings the recorder takes while
  recording (plugins and their delays, the log, a radio's gain) don't raise it.
- **A background tab that falls behind reconnects** (Safari and other browsers that slow
  background tabs instead of freezing them): once it is more than 10 s behind the recorder, it
  reconnects instead of replaying the backlog when brought forward.
- **Conventional rows say what they record and what they file it under**: a row's Tone is now
  **Code** (tone, NAC, colour code or RAN), **Slot** (DMR) and **Match TG** (P25, DMR, NXDN), and
  the most specific row that fits takes a transmission. P25 rows can now split a frequency by
  talkgroup, not just by NAC. An older config's `tone`, and a channel file's `Tone` column, are read
  as the three; RadioReference's `CC1 TS2 TG201` pasted into Code still works.
- **A typed TG Number on a digital row now wins** over the talkgroup the air names (before, it was
  used only when the air named none). Several P25 channels whose radios all send TG 1 can each file
  under a number of their own. An empty TG Number keeps the air's, as before. P25 rows that shared a
  frequency (and filed under their own number) have that number written in on upgrade, so nothing
  already recorded files differently.
- **`talkgroup_ota` in the call JSON**: a conventional P25, DMR or NXDN call keeps the talkgroup the
  air named, also when its row files it under another number. A new talkgroup on the air still
  starts a new call. Plugins see it on `CallRecord` and `CallInfo` (SDK 0.5.1).
- **Shown where calls are**: the console log's `TG:` reads `46012 (air 1)` when a row files the
  air's talkgroup under another number, as do Live calls and the Listen page (`air 1`); the
  archive path has a `{talkgroup_ota}` token.
- **Setup's channel table shows what each row records** as chips coloured by kind — tone / DCS,
  NAC / colour code / RAN, slot, talkgroup, or everything else — opening the row's Code, Slot and
  Match TG to edit; a code that doesn't read is marked. The CSV format's help is an example and a
  table of the columns.
- **A row's names come first**: a conventional call takes its row's names, if the row has any, else
  the system's talkgroup file's for the number it's filed under.
- **Every name on a conventional row can be edited in Setup**: **Edit** opens a row's codes and the
  Description, Tag and Category its calls carry (before, only the Name could be changed without
  editing the config), offering the tags and categories the system uses already. A row's tag and
  category show under its name. **Export CSV** writes them all.
- **The call JSON says what kind of call it was**: `system_type` (`p25`, `smartnet`, `dmr`, `nxdn`
  or `conventional`, as plugins have it), `voice_mode` (`p25`, `p25_phase2`, `dmr`, `nxdn48`,
  `nxdn96`, `provoice`, `fm` or `am`) and, for P25, `nac`: the NAC a conventional channel's frames carried, a
  trunked system's site NAC, in hex as channel files and Setup write it (`"293"`). The archive path has `{system_type}`, `{voice_mode}` and `{nac}`
  tokens. SDK 0.5.1 has the fields on `CallRecord`.
- **The Radio page names a conventional system's talkgroups** as Listen does (from #53's review; it
  showed bare numbers in 0.2.6 too): by the row, else the talkgroup file; a talkgroup filed under the
  air's number is named as its last named call was.
- **Setup keeps a system's card open while its Short name is typed** (from #53's review): a pause
  saved the edit, and the recorder's copy coming back closed the card.
- **P25 Phase 1 voice repeats fewer good frames.** A frame whose FEC corrected a lot of errors
  is repeated only when the errors fall in its pitch word *and* there are many in total, as
  TIA-102.BABA-A §7.7 has it, not on either one (an `||` inherited from op25, which taclane
  found). With noise added to a DCFD capture, the audio comes out closer to the clean decode's
  at every SNR tried. `tool snr --repeat` measures repeat rules.

## [0.2.6] — 2026-10-20

- **A tab woken from the background shows how things are now** (by taclane, #54), not minutes of
  calls and status replayed as if live: one that heard nothing from the recorder for 30 s (it
  hears from it every 2 s) reconnects, and the recorder closes a connection that stops reading
  for 10 s or falls behind, so it reconnects afresh.
- **A call times out from when its last update came in, not when it was on the air** (by
  taclane, #55). A control channel's messages come out some 0.4 s after they were sent (P25's
  diversity bank holds them 0.35 s), so every call ended that much early: a `callTimeoutS` of 1,
  carried over from Trunk Recorder, ended calls about half a second after their last update,
  shorter than the gap between a busy system's updates in hangtime, and split conversations into
  pieces with empty calls between them. `callTimeoutS` now means seconds after the last update
  came in; Trunk Recorder's once-a-second checks end calls up to 2 s later than its own setting.
- **Numbers show as they should**: an SNR, an airtime or a gain no longer reads like
  8.600000381469727 (in a frequency's tooltip on Overview and Decode, a USRP's or SoapySDR's gain
  on RF, a talkgroup's or radio's airtime). A gain the survey finds is saved as 49.6, not
  49.599998474121094.

## [0.2.5] — 2026-10-19

- **The Live page is each system's now**: a system's page under **Radio system** has its
  **Active calls** (with **Listen live** for that system, and its talkgroups and radios linking to
  their pages) in place of the "On the air now" card, and its **Control channel log** at the
  bottom. The Live page, which showed every system's calls and messages together, is gone; links
  to it open Radio system.
- **Platform comes before Plugins** in the side rail.

## [0.2.4] — 2026-10-19

- **Every page that drills down goes back the same way**: a system on Radio now starts with
  "← All systems" and the systems beside it, as Decode does with its sites, instead of a button on
  the right of the system's header. A talkgroup's or radio's page starts with "← <system>"; RF's
  source page with "← All sources".
- **A system's overrides in Setup are two groups**: **Plugins** (its plugin settings and plugin
  delays) and **Recording**, in two parts: **Recording** (what's recorded and kept, loudness) and
  **Archive** (folders and file names, the M4A, and what stays once the plugins are done).
- **Keep .wav and Keep .m4a, each on its own**, in place of **Keep audio** (`audioArchive`) and
  **Also save an M4A** (`compressWav`), on the Recording tab and in a system's Archive override:
  keep the M4A and not the WAV, or both, or neither (`archiveWav`, on by default; `archiveM4a`,
  off). An older config is upgraded as it's read, keeping what it kept. With OpenMHz (or another
  plugin that uploads M4A) on, an .m4a is made for every call either way, and Keep .m4a's hint
  says so; off, it's deleted once the plugins are done. An M4A to keep that couldn't be made
  leaves the WAV in its place, and **Keep when a plugin fails** now keeps every file of the call.

## [0.2.3] — 2026-10-19

- **Listen says why a system is missing** instead of "No recorded calls yet": a system whose calls'
  audio isn't kept (Keep audio off, its own or the Recording tab's) has nothing to play, so its
  talkgroups are left out of the Talkgroups tab, and a card above the calls names it and where to
  turn Keep audio on.

## [0.2.2] — 2026-10-18

- **A radio's delivered rate holds steady** on the RF page (by taclane, #43): it's measured over
  5 s from one buffer's arrival to another's, not over the status tick's second, which counted a
  whole RTL-SDR transfer (55 ms at 2.4 MS/s) in or out of it. Healthy dongles no longer warn that
  they deliver 5% off their rate; one that goes quiet still falls to zero.
- **Reinstall a plugin in place** (by taclane, #38): a plugin's card has **Reinstall** where it
  would say **Update to vX** with nothing newer, taking it again from where it came (the
  registry, its GitHub repository's latest release, or your build's file) with its settings kept.
  Adding your build's file again does the same, and restarts it if it's on. A build of your own
  says **Uninstall** too (its file stays).

## [0.2.1] — 2026-10-18

- **A dashboard card opens what it's about from anywhere on it**, not only its title: a system's
  card on the Radio page opens that system (its "Open …" link is gone), a site's on Decode, a
  source's on RF, and so on. A row of a system's busiest talkgroups opens that talkgroup, and a
  row of who talks on a talkgroup opens that radio. Buttons, links and charts in a card still do
  their own thing.
- **The Listen page's Talkgroups tab opens again** (0.2.0 failed with "s.talkgroups is
  undefined"): the page didn't take the talkgroup files from the store when they moved out of
  the config.
- **Every closed choice in the config is read in any case**, the few that weren't too: a source's
  `type` (`"RTLSDR"`), an interface's `access`, and the log's `level`, `frequencyFormat` and
  `talkgroupDisplayFormat` (`"warn"` is `"warning"`). They're saved in their own case, as before.

## [0.2.0] — 2026-10-18

A release with breaking changes, for interface and plugin authors more than for recording. An
older config is upgraded when it's read (its talkgroups are moved into files beside it), and
recording carries on as before. What to know before upgrading:

- **Update your plugins** from the Plugins page once the recorder is upgraded: the plugins in the
  store are rebuilt on plugin SDK 0.5.0 (OpenMHz 0.1.6, Broadcastify 0.1.6, Rdio Scanner 0.1.5,
  simplestream 0.1.5, Upload script 0.1.4, MQTT 0.1.7). Systems are numbered from 0 now,
  conventional ones too, and the call record names every field, so an older MQTT plugin would
  report a call's signal, noise and frequency error as 0.
- **Interfaces** that use `setConfig`, the talkgroups, the store messages or a site's identity
  need a version made for this one (Trunk City 2000 0.4.0 is; Trunk-tris needs no change).
- **A config written by hand** with a choice the recorder doesn't list (a system `type` of
  "P25x", say) is refused when it's read, where it used to fall back to the default without a
  word.
- **The dashboard needs a browser with WebAssembly SIMD** (Safari 16.4, Chrome 91, Firefox 89 or
  later).

- **Talkgroup files live beside the config, not in it** (breaking for interface authors). A system's
  talkgroup file (Trunk Recorder's CSV format) is `<shortName>.talkgroups.csv` in the config's
  folder, and `talkgroupsCsv` / `talkgroupsName` are gone from the config, so editing one no longer
  sends every system's file to the recorder and back to every connection. An older config is
  upgraded when it's read: its talkgroups are moved out into those files (the log says which).
  Renaming a system in Setup moves its file. In the API the recorder sends `talkgroups { system,
  csv }` for each system on connecting and whenever one changes (`""`: none), and an interface with
  control access changes one with `setTalkgroups { system, csv }`. client.js: `rec.talkgroupFiles`
  and `rec.setTalkgroups(shortName, csv)`.
- **A config edit names the version it edits** (breaking for interface authors). `config` carries a
  `version`, and `setConfig` is `{ config, version }`. If the config changed elsewhere meanwhile
  (another browser saved), the edit is refused rather than saved over the newer config. Every
  `setConfig` is answered to its sender with `configSaved { version, error? }`: `version` is the
  config's now, and `error` says the edit wasn't taken (an old version, or a bad config) or
  couldn't be saved. client.js tracks `rec.configVersion` and sends it; a refusal sets `rec.error`.
- **A connection is greeted with ordinary messages, and `hello` comes last** (breaking for
  interface authors). It used to carry a copy of the config, phase, history, aliases, heard codes,
  radios, survey, events, host, devices and keys, in its own shapes. Now a new connection is sent
  `state`, `config`, `talkgroups`, `units`, `heard`, `history`, `radios`, `survey`, `monitorEvent`
  (each recent event) and `host` (with control access also `devices`, `keys`, `plugins` and
  `interfaces`) like any later change, and then `hello`, which means "caught up" and is just
  `{ version, platform, api, configPath?, surveyBands }`. New messages: `history { entries }` (the
  latest calls, newest first) and `units { system, csv }` (a system's saved talker aliases).
  Wait for `hello` before treating something missing as missing. A listen-only connection gets the
  same messages as before, without plugin settings and key sources. client.js emits `change` once
  at `hello` during the greeting.
- **A conventional system has its own talkgroup file.** Its P25, DMR and NXDN channels name the
  talkgroups they hear by it (Setup shows it on a conventional system with digital channels). They no
  longer borrow the names, or the receiver settings, of the only trunked system when there is one:
  give the conventional system the file instead.
- **Conventional calls have their frequency error** (`freq_error` in the call JSON), measured from
  their decoder's offset as a trunked call's is (P25; analog and DMR receivers don't measure one).
- **Conventional DMR opens its channel as wide as trunked DMR** (6.25 kHz, was 7 kHz): a shade
  fewer bad frames on the test capture.
- **The recorder's own log lines reach the browsers** with their clock time and level (`LogLine.time`,
  `.level`): warnings and errors to every connection, the rest to those watching the `log` topic.
- **Calls are finished by a small pool of threads** that write every file a call's plan says (an
  .m4a made for a plugin too), so the plugins are handed finished files.
- **Plugin SDK: one `CallRecord`.** The call JSON is one typed record from the engine to the file,
  the history and the plugins, where it was a string parsed again four times. Every field the
  recorder writes is named (`freq_error`, `signal`, `noise`, `snr`, `clean_voice_pct`,
  `recorder_num`, `algid`, `key_id`, `color_code`, `ran`, the tone fields, `errorList`…), so a plugin
  no longer finds them in `extra`; flags are written back as 0/1, as the recorder's files have them.
  The files themselves are byte for byte as before.
- **One store for plugins and interfaces** (breaking for control-access interfaces). `pluginStore` /
  `interfaceStore`, `installPlugin` / `installInterface` and `pluginInstall` / `interfaceInstall` are
  `store { kind }`, `install { kind, … }` and `installProgress { kind, … }`, with `kind` `plugin` or
  `interface`; a store's list is `listings`. The Plugins and Interfaces pages share one store panel.
  `trunk-pro interface install` reports progress directly instead of reading it back off a hub.
- **One site identity everywhere** (breaking for interface authors). A site's identity in `status`,
  the survey's candidates and monitor, and the config's `expect` is one shape, `SiteIdentity`: a
  field not heard yet is left out (it was `null` in messages), so test with `!= null`. The survey's
  suggestion carries `identity` instead of `nac`, `sysId`… beside its other fields, and its learned
  SmartNet band plan uses the config's names (`bandplanBaseHz`, `bandplanSpacingHz`,
  `bandplanHighHz`).
- **`hello.capabilities`** says what a recorder can do beyond recording: `service`, `drivers`,
  `plugins`, `interfaces`, `host`, `keys` (the desktop app has all; the browser build none). The
  interface shows what the recorder can do instead of checking which build it is, and the browser
  build answers a command it can't do with an `error` instead of dropping it. client.js:
  `rec.capabilities`.
- **`status` is slimmer** (breaking for interface authors). It's sent to everyone twice a second,
  and each site carried its neighbour list, DMR and NXDN channels and carriers, and each system its
  patches, which only the Decode page's system view reads. They moved to `decodeDetail` (topic
  `decode:<shortName>`): `sites: [{ index, name, adjacent, dmr, nxdn }]` and `patches: [{
  supergroup, members }]`.
- **Setup applies the recorder's own rules.** Why Start would refuse a config, where an automatic
  source is centred, how a stored config is read (and an older one upgraded), and reading and writing
  a channel CSV were written twice, in the recorder and again in the interface, and had drifted:
  Setup let Start be pressed with an RTL-SDR's USB transfer size the recorder refuses, or a channel
  bandwidth or demodulation setting out of range. The interface now runs the recorder's code for
  these (as WebAssembly, which the browser version already used), so they can't disagree. The
  dashboard now needs a browser with WebAssembly SIMD (Safari 16.4, Chrome 91, Firefox 89 or
  later), and says so if it hasn't. The browser version starts from the recorder's default
  config, with the M4A, spool and monitor settings it lacked.
- **A config with a choice it doesn't list is refused when it's read**, as one with a wrong type is:
  a system `type`, a site's `modulation`, `nxdnType`, `nxdnRate`, `defaultMode`, `vocoder`, the M4A
  `encoder`, `unitNames.mode` and `log.color`. They used to fall back without a word (a `type` of
  "P25x" recorded as P25). A listed name is read in any case (`"P25"`, `"QPSK"`), and an empty value
  still means the default.
- **A file name format with a token nobody knows is refused at Start**, the config's or a system's
  own. Setup already said so; a config edited by hand or by another interface wasn't checked.
- **Renaming a system in the setup guide keeps its plugin settings pointing at it**, as renaming it
  in Setup does.
- **A command the recorder can't read is answered.** One with a field missing or of the wrong kind,
  or of a type the recorder doesn't know, used to be ignored without a word (or read with a
  default in place of what was missing); now the connection gets an `error` saying what was wrong.
- **Systems are numbered from 0, conventional ones too.** Conventional systems used to be numbered
  down from 65535; now the systems being recorded are numbered 0, 1, 2…, trunked first, then
  conventional. This is the number `{sys_num}` gives in a file name format and plugins see as
  `system`. The `status` message lists every system's short name by that number
  (`status.systemNames`); the API client's `systemName(status, system)` reads it, and
  `CONVENTIONAL` / `conventionalIndex` are gone (from the plugin SDK too: SDK 0.5.0, since 0.4.0
  on crates.io still has it; a plugin tells a conventional system by its `kind` in `Setup`). There
  is no longer a limit of 256 conventional systems.
  - Inside, a system's talkgroups, rules, unit names and keys are kept once for the whole system
    rather than copied into each of its sites, so a talkgroup file edited while recording reaches
    every site from one place.
- **A conventional DMR call is DMR from its start.** One whose colour code wasn't heard yet was
  logged as a P25 call and its files had no slot in their name; now it is DMR on its slot at once.
  (Inside, a call says what its voice is on — P25, P25 Phase 2 on a slot, DMR on a slot, NXDN,
  FM or AM — in one field, where five fields used to be read in order to tell.)
- **A lost control channel on a named site links to its system.** The `controlLost` and
  `controlRegained` events now carry the system's short name in `system` and the site's own name
  in `site`; `system` used to be "dcfd/south", so the event's link to the Decode page found no
  system.

## [0.1.15] — 2026-10-17

- **A call to a radio isn't taken for a copy of a talkgroup call.** Multi-site matched copies of a
  call by number alone, and a unit-to-unit call's number is the radio called: one to radio 1234 on
  one site and a call on talkgroup 1234 on another, within 3 s, were taken for one call and one of
  them wasn't saved.
- **A system has sites.** Sites of one radio system used to be systems of their own, joined while
  recording (a site group, or the same WACN and System ID on the air), and anything set or counted
  by short name — talkgroups, unit names, recording rules, plugin settings and delays, Ignore, the
  Listen page — went by whichever site a call was kept from. Now a system has a list of sites, each
  a control channel to follow, and everything else is the system's: one talkgroup file, one Listen
  list, one folder, one Overview card (a row per site); the Decode page has a card per site. A call
  heard on several sites is still saved once, now only among a system's own sites; the call JSON
  says its `site`, and `{site}` can go in a filename format.
  - **Setup**: a system's **Sites** (Add site; each with its own Record switch, control channels,
    modulation, protocol settings and site lock). A neighbouring site the control channel announces
    is added as a site of its system, and the survey can add a control channel as a site or replace
    one. Two systems set up apart that the air says are one get **Make them one system**.
  - **Your config is updated on the first start** (the old one kept as `config.json.before-sites`):
    each system becomes a site named as it was, and those joined by a site group or by site locks
    naming one system become one system, with the first one's name and settings; the log says what
    wasn't kept. Systems joined only from the air are offered in Setup.
  - From Trunk Recorder: systems with `multiSite` and one `multiSiteSystemName` are one system with
    a site each.
  - Plugin SDK 0.4.0: `site` on calls, unit events and the call record, `site_name` in status; a
    call is started and ended once for plugins however many sites hear it.
  - The API: `status.systems[]` is one per system with its `sites[]` (control channel, identity,
    decoding); calls carry `site`. `--system name/site` in `trunk-pro replay`.
  - Also fixed on the way: switching on an avoided talkgroup on Listen left it avoided.
- **RTL-SDR USB transfer size** (`bufLen`, **USB transfer size** in Setup): Trunk Recorder's
  `buflen`. Four or more dongles at 2.4 MSPS on one USB 2.0 bus overflow with the driver's
  256 KiB transfers (the control channel goes unreliable first); 64 KiB transfers let them take
  turns. Imported from a Trunk Recorder config's `buflen=` / `buffers=`, and suggested in the log
  and in Setup once 4 or more dongles run at full rate. The RTL-SDR driver (`rtlsdr-nusb`) is now
  a fork in the workspace to allow it.
- **Airspy sample packing.** Airspys now send 12-bit samples packed over USB (`packing`, on by
  default; **Pack samples** in Setup): a quarter less of the bus. An Airspy at 10 MSPS nearly
  fills a USB 2.0 bus by itself; with several radios on one bus, that's what overflows.
- **Losses a radio can't report are caught.** An RTL-SDR has no way to say its FIFO overflowed
  (its USB bus too busy to read it in time). The recorder now notices a source's samples falling
  behind the wall clock beyond its crystal's drift, counts them as dropped and logs "lost without
  its driver saying so", so the dashboard's dropped count and alert cover RTL-SDRs too.
- RTL-SDR and capture-file buffers are reused rather than allocated per transfer.

- **Fewer Airspy drops.** libairspy converted the Airspy's samples to IQ on the same thread that
  drains its USB queue (about half a core at 10 MS/s), and dropped a buffer whenever that thread
  ran ~50 ms late. The recorder now asks for the raw samples and converts them on the source's
  decoding thread, with no allocation per buffer on libairspy's. `TRUNK_PRO_AIRSPY_LIBRARY_IQ=1`
  goes back to libairspy's conversion. Also fixed: drops reported by libairspy were under-counted
  when they came back to back.
- **Sources don't wait on the dashboard.** With many sources, the threads that read status, the
  channel list and the history for the dashboard held the engine while waiting on a busy source,
  and every other source's decoding waited behind them.

- **Each source decodes on its own core.** With many sources one core used to do all the
  decoding while the others sat idle (and samples were dropped once it couldn't keep up). Each
  source now has a decoding thread of its own — its channelizer, its voice and control channels —
  while the calls are followed in one place. Results are the same as before, fed from one thread
  bit for bit; `trunk-pro replay --threads` feeds each source from its own thread, as live.
- **NXDN trunked sites cost far less.** A Type-D site watches every repeater at once; each
  watched NXDN48 carrier now decimates before demodulating, 42 % less CPU with the same decodes.
- **P25 `modulation: "auto"` detects the modulation.** Instead of running the CQPSK and C4FM
  receivers on every channel all the time, each channel runs both until one clearly decodes
  better (a few seconds), then only that one — about 70 % less CPU on a P25 channel. The log says
  what was detected and the setting that skips detecting it; Setup's option reads **Detect**. On a
  weak simulcast signal (9–11 dB) this decodes up to 1 % fewer control messages than running both
  kept doing.
- **Faster 4FSK receivers.** Multi-symbol detection (P25 C4FM, DMR, NXDN) does the same work in
  7–12 % less time.

- **AM channels (airband).** Conventional channels can now be analog AM: VHF airband (118–137 MHz)
  and UHF military air (225–400 MHz). The audio level follows each transmission's own carrier, so
  a fading aircraft keeps its volume; impulse noise is clipped; and a transmitter a few kHz off
  frequency is heard as it is. Pick **Analog AM** in Setup, `"mode": "am"`, `am` in a channel CSV,
  or `--am` on `trunk-pro replay`. See [Airband (AM)](docs/guides/conventional.md#airband-am).
  AM channels listen for the carrier's spectral line as well as the channel's power, which opens
  them about 10 dB sooner, so distant aircraft are heard; the steady whistles of SDR spurs and
  nearby electronics are learned and ignored.
- **Channel bandwidth.** Each FM or AM channel can set its filter width (`bandwidthKhz`, Setup's
  new **Bandwidth** column, the CSV's `Bandwidth kHz`): wide FM for 25 kHz channels, 6 kHz for
  8.33 kHz airband, wider for UHF military air (its default from 225 MHz).
- **Demodulation settings per conventional system** (`demod`, Setup's **Demodulation** group): FM
  de-emphasis on or off, AM level speed and low cut, and the squelch's open delay and hang. A
  longer hang bridges fades on weak signals without keeping the noise after a transmission: the
  squelch now cuts the audio from just before the carrier dropped, whatever the hang.

## [0.1.14] — 2026-10-16

- **Interfaces for people who listen, from an interface store.** Besides its own interface, the
  recorder now serves other web interfaces — a player, a scanner wall, a phone page — each on a
  port of its own, so each is a separate site to the browser. The new **Interfaces** page
  installs them from the interface store ([TrunkRecorder/interfaces](https://github.com/TrunkRecorder/interfaces),
  checksums pinned as the plugin store's are) or a GitHub release, adds folders of your own,
  updates and removes them, and sets their ports; changes take effect at once, no restart.
  `trunk-pro interface …` does the same from the command line. See
  [docs/interfaces](docs/interfaces/README.md). The store shows each interface's screenshots
  (fetched and checked by the recorder) and who made it; plugin cards show their author too.
  The first listed is [Trunkamp](https://github.com/ge0metrix/TrunkAmp) by ge0metrix, a
  Winamp-style player.
- **Listen access.** An interface gets **listen** access unless you give it **control**: it sees
  and plays everything, and can't change settings, start or stop, quit, or manage plugins; the
  config it sees leaves out plugin settings and key sources. The recorder's own port keeps full
  control. Share a listener's interface without sharing the controls.
- **Changed:** interfaces are no longer served at `/ui/<name>/`, `/builtin/` and `/ui/` are gone,
  and `/` is always the recorder's own interface (`server.home` is dropped). `server.interfaces`
  is now a map by id (`port`, `enabled`, `access`, `path`); an old list is read and given ports.
  `--ui <folder>` serves the folder on the next free port and opens it. `/api/interfaces` is
  replaced by the `interfaces` message. The interface panel moved from Setup to the Interfaces
  page.

## [0.1.13] — 2026-10-16

- **P25 decoding checked against the TIA-102 standards.** The control channel, Phase 1 voice,
  Phase 2 and encryption were gone through against the standards' own message layouts and test
  vectors, which the tests now follow. What you'll notice:
  - **Phase 2 calls hold together.** Grant updates for both slots of a TDMA carrier now come
    through, so a call being followed no longer ends and starts again partway.
  - **Phase 2 calls joined late name their talkers.** The source IDs come from the voice channel
    as well as the call's start, so a call heard after it began no longer has an empty `srcList`.
  - **Unit-to-unit calls aren't wrongly skipped.** On some systems every unit-to-unit call was
    taken for encrypted (or an emergency) and skipped.
  - **Telephone interconnect calls are recorded**, with `recordUnitToUnit`.
  - **No more noise from encrypted calls joined late.** Voice heard before it's known whether it's
    encrypted is held until it is, instead of being played as if clear.
  - **A wrong key is caught.** Phase 1 voice a key decrypts to noise is no longer saved as
    decrypted: the log says which key ID doesn't fit, and that voice is left out.
  - **More decoded in weak signal.** Phase 2 signalling survives about twice as many errors, and
    Phase 1 headers and link control use what the error correction already knows is damaged.
  - **Keys:** with keys loaded, encrypted link control is read (talkgroup and talker). A 3DES key
    whose three parts are the same also decrypts DES ($81), as the standard says; DES keys with a
    mistyped (wrong-parity) byte are warned of; from a KFDtool container, a key in both the active
    and an inactive keyset is taken from the active one; a second key for the same talkgroup is an
    error. ALGID $85 is shown as AES-128 and $89 as AES-128 (Motorola).
- **Each radio's own flags in the call JSON.** Every `srcList` entry now has `encrypted`, `duplex`,
  `mode` and `priority` beside `emergency`: what that radio's transmission said of itself (its
  grant, then the voice channel while it talked), so one talker's emergency isn't another's.
- **Plugins** (SDK 0.3.1, additive): `CallInfo` has `sources`, each radio with those flags, and the
  call's `duplex`, `packet` and `priority`; `SrcEntry` reads the new `srcList` fields;
  `UnitEvent` has `emergency`.

## [0.1.12] — 2026-10-16

- **Import several Trunk Recorder configs into one setup.** With systems already set up, the import
  now asks whether to add the config to them (the default) or replace them, so a setup split over
  several configs (700 MHz, 800 MHz, VHF…) can be imported one after another. Radios and systems
  already here aren't added twice; your Recording and log settings stay. See
  [Running the import](docs/migrating-from-trunk-recorder.md#running-the-import).
- **`srcList` positions are in the audio.** Each source's `pos` in the call JSON is where it starts
  in the saved audio, as in Trunk Recorder, not the time since the call was granted: the gaps
  between transmissions and the transmissions left out (short key-ups, encrypted ones) are no
  longer counted, so a position can't fall past the end of the file.

- **Plugin delays.** Plugins can be given calls late: for a whole system, some of its talkgroups,
  one plugin on every system, or one plugin on one system (`pluginDelays`, at the top of the config
  and in each system). A rule that narrows another replaces it; otherwise the longest delay wins.
  Held calls are kept on disk until they're given, so a restart doesn't lose them; a delayed call's
  live audio isn't streamed. In Setup, under a system's Overrides and on each plugin's card. See
  [Delays](docs/configuration/plugins.md#delays).

- **Key sources.** The config's new top-level `keys` names each keys file once, and a system's
  `keys` names a source instead of a file, so systems on the same radios share one and it is read
  once. A config whose systems name a file is read as one source per file, named after it.
- **Encryption keys from a KFDtool container:** a key source's `file` can be a KFDtool key
  container (`.ekc`), with `passwordFile` naming a file that holds its password. A password file
  others can read is refused. Keys the recorder can't decrypt (key-encryption keys, DES-XL) are
  listed in the log and left out. `replay` takes `--keys-password-file` too.
- **Unlock a key container in the interface.** A container with no `passwordFile` no longer stops
  the start: Setup → Recording → Key containers asks for its password. Unlocked keys are held in
  memory until Lock or the app stops, and reach calls already in progress.

- **Decrypted calls go only to the plugins you allow.** A call decrypted with your keys (its live
  audio, files and JSON) is no longer given to every plugin: turn on **Share decrypted calls** on a
  plugin's card in Setup → Plugins (`plugins.<id>.shareDecrypted`) for each one that should have
  them. Voice of a call that ends in the same moment it is decrypted is no longer taken for clear.
  See [Decrypted calls](docs/plugins/README.md#decrypted-calls).

- **Encryption keys are wiped from memory:** the keys in a system's keyring,
  the keys file's text and its parsed contents, and the cipher's key-derived
  state are overwritten when they are dropped, so a freed page doesn't keep a
  key for a crash dump or a memory scan to find.

- **No CTCSS hum on analog audio.** The voice high-pass now removes every CTCSS tone. The higher
  tones (roughly 150–254 Hz) used to come through as a low hum; a 250 Hz tone was cut by only
  10 dB, and every tone is now cut by 50 dB or more. Voice and two-tone paging tones from 288 Hz
  up are kept, as before.

- **Phase 2, DMR and NXDN tone frames now play as tones:** a dispatch tone-out sent
  as AMBE+2 tone frames (a pure tone or a pair, with a tone ID and a level)
  came out as silence with a click at each frame edge, because the decoder
  took the tone ID for part of a pitch and faded to nothing. It now reads the
  frame by TIA-102.BABA-A §2.10 and plays the Annex T tone at the level it
  names, continuous from frame to frame (checked against the reference
  vocoder's amplitude to within 4 %). A tone frame the FEC damaged keeps the
  tone going if its four ID copies still name it, and otherwise repeats it up
  to 3 times, as voice does. Tone frames no longer count as bad frames. The
  mbelib profile still plays none, as mbelib doesn't.

## [0.1.11] — 2026-10-14

- **Setup, reorganized.** Each Setup tab is now sections of cards, and
  each card's settings are in named groups with a line on what they are
  for: a system's Basics, On the air, Talkgroups and units, Sites and
  Overrides; a radio's Device, Tuning, Gain and Frequency correction; the
  Recording tab's Recorder, Call rules and Advanced (M4A audio, Log,
  Interfaces). Headings step down in size from section to card to group,
  so it's clearer what belongs together. System, conventional and radio
  cards fold to a one-line header (name, type, whether its control
  channel is covered, Record), with Expand all / Collapse all; with
  several, they start folded, and each is remembered in the browser.
  Show, from an import's to-do list or a plugin's system chips, opens the
  card it points into. Nothing about the settings themselves changed.
- **A system can override a plugin's settings.** Each plugin on a system's
  card in Setup now has a section for that system's own values of the
  plugin's settings, such as the server it uploads to; anything left empty
  stays as set for every system. It works for a single copy of a plugin or
  for each of several, so one system can upload to two Rdio Scanner servers.
  In the config this is `override` in the system's settings for the plugin.
  A required setting, such as Rdio Scanner's server, can be left empty for
  the whole recorder when every system that uses the plugin sets it.
- **Plugin SDK 0.3.0.** Each system in `Setup` carries `settings`: the
  plugin's settings with that system's overrides. `CallQueue::start_by`
  gives each server a queue of its own (workers, retries, capacity and
  endpoint status), so a server that's down doesn't hold up the others.
  Breaking: `System` takes the config type too, `System<C, S>`.

## [0.1.10] — 2026-10-13

- **Encryption heard:** a talkgroup's page on the Radio dashboard lists
  the ciphers its encrypted P25 calls used, with each algorithm (ADP,
  DES, AES-256, …), key ID, number of calls and when it was last heard.
  Hover over a talkgroup's **ENC** badge to see the same. A call's JSON
  now has `algid` and `key_id`.
- **Decrypting P25 (experimental):** if you are authorized to monitor an
  encrypted P25 system and hold its keys, give the system a `keys` file
  (ADP, DES, 3DES, AES-128 or AES-256; see Configuration → Systems →
  Encryption keys). Its encrypted P25 calls, Phase 1 and Phase 2, are
  then recorded, decrypted and saved, marked `"decrypted": 1` in the call
  JSON, and can be played in Listen. A key of the wrong length stops
  recording from starting, with a message saying which key to fix.
  Plugins get `algid`, `key_id` and `decrypted` with each call.
  `trunk-pro replay --keys` does the same for captures, and `trunk-pro
  tool p25synth` makes an encrypted test capture with a key of yours, to
  check a setup without an encrypted system on the air. The decryption
  was checked against OP25 and dsd-fme on such captures.
- **Fixed:** conventional channels on a source running below about
  1 MS/s never opened (at 480 kS/s and under, none did): the noise floor
  was measured in slices narrower than a channel, so a transmission read
  as noise. Slices are now at least 30 kHz wide. The analog squelch of
  trunked calls on such sources used the same floor and is fixed too.
  Sources at 1.92 MS/s and above behave as before.
- **Archive settings, clearer and predictable.** The three settings that
  decide what of a call stays in the recordings folder are now under
  **Archive** in Setup's Recording tab: **Keep audio**, **Keep call JSON**
  and **Keep when a plugin fails** (the config keys are unchanged). Off, a
  Keep setting deletes that file once the plugins are done with the call;
  a plugin that skipped a call is done with it, just as one that uploaded
  it. Only a failure keeps the files, with **Keep when a plugin fails** on,
  which is greyed out when both Keep settings are on. Changes: with no
  plugin taking calls, the Keep settings now apply too (before, every call
  was kept whole), and Setup says when that means nothing is saved; a call
  a plugin never answered on counts as failed after an hour (before, it was
  always kept).
- **P25: multi-block control channel messages.** Some sites send part of
  their control channel as multi-block (MBT) messages, which were
  dropped. Those sites now show their WACN and System ID (so a site lock
  and multi-site grouping work), and multi-block grants, patch grants,
  affiliations and registrations are followed like the single-block ones.
  On a site that uses them, control messages a second go up: CLMRN-I
  went from about 12 to 22.
- **Control channel coverage.** The Decode page (a tile on each P25
  system's card, and a chart under Over time) and Find my system show how
  much of the control channel decoded: an estimate from timing, of the
  air decoded against the most the channel could carry. It reads the same
  on every system, unlike messages a second, which are lower where a site
  sends multi-block messages (one takes the air of two or three).
- **P25: manufacturers' messages.** Motorola and L3Harris radios' GPS
  positions, sent while they talk, are kept on the radio in the call JSON
  (`lat`, `lon` in `srcList`). Motorola patch deletes end a patch when the
  site says so; Motorola's base station ID logs the site's callsign;
  Motorola emergency alarms are logged and sent to plugins as unit events
  (`emergency_alarm`); denied and queued requests are logged with the
  reason. On Phase 2, a Motorola patched call names its talker.
- **P25 fixes.** On Motorola systems, a supergroup being set up (and a
  queued request) was taken for a unit-to-unit call, and a patch-add in a
  voice channel's link control for a talker. An L3Harris regroup of a
  radio was taken for a talkgroup patch; an encrypted L3Harris patch is
  now marked. A refused registration or affiliation isn't counted as one.
- **NXDN talker aliases:** Kenwood radios' programmed names are read from the
  traffic channel and shown as each radio's name (`tag_ota`, the Calls page,
  `<shortName>.units.csv`), as P25's are. ASCII aliases only; Icom's format
  isn't read.

## [0.1.9] — 2026-10-09

- **Plugins: more than one copy for a system.** In a system's plugin
  settings, **Add plugin** runs a plugin again for that system with
  settings of its own, for example to upload to two OpenMHz or
  Broadcastify accounts. Each copy runs separately and is shown under
  its plugin on the Plugins page; **Remove** takes a copy away. A call's
  files are deleted only after every copy has finished with it.
- **Listen:** ticking a talkgroup, group or tag in a system that's turned
  off now turns the system back on with just those talkgroups. A system
  that's off no longer shows its talkgroups ticked.
- A new developer guide, `docs/developer/architecture.html`, explains how
  the recorder works with interactive diagrams, with a text version
  beside it.

## [0.1.8] — 2026-10-08

- **Listen**, a new page right under Overview: a scanner over the
  recorded calls, like rdio-scanner. Turn on **Live feed** and each new
  call plays as it's recorded, one after another, with **Hold sys**,
  **Hold TG**, **Pause**, **Replay**, **Skip** and **Avoid** (until let
  go, or 30/60/120 minutes). Pick the systems, talkgroups, groups and tags
  to hear; the choice is kept in your browser. Click any recent call to
  play it, and **Load older** goes back through the last 24 hours.
  Encrypted calls and calls whose audio wasn't kept are left out.
- The **Calls** page is now **Live**: the calls on the air now (with
  Listen live) and the control channel log. Its recent calls table moved
  to Listen.
- **RF → The band** draws one row per band in use (VHF, UHF, 700 MHz,
  800 MHz…), so sources in different bands no longer squeeze each other
  into slivers.
- Recorded calls are served with byte ranges, so Safari can play them.
- **captureFrames** now saves each digital call's vocoder frames as a
  MimoSDR `.sdr` file (DigitalStream) instead of `.frames.jsonl`; NXDN
  calls stay `.frames.jsonl`. `tool revoice` reads either, and `tool sdr`
  prints an `.sdr` file.

## [0.1.7] — 2026-10-07

- **Louder calls:** speech is normalised to about −14 LUFS (−12 dBFS), 4.5 dB
  louder than before, for digital and analog calls alike. Peaks are still
  limited to −2.5 dBFS, so nothing clips.
- `digitalLevelDb` and `analogLevelDb` now go through the limiter: turning
  them up makes a call louder without clipping it (it used to clip anything
  past about +2.5 dB).

## [0.1.6] — 2026-10-07

- **Analog calls** no longer end with a burst of noise (a squelch tail) when
  the transmitter's carrier drops. It was most noticeable on SmartNet analog
  talkgroups. The squelch now closes within a few milliseconds of the carrier
  going away, and the audio is held back slightly so the fade-out finishes
  before any noise is recorded.

## [0.1.5] — 2026-10-06

- **New documentation** in [docs/](https://github.com/TrunkRecorder/trunk-recorder-pro/blob/main/docs/README.md):
  installing on each platform, getting started, a guide for each kind of
  system (P25, SmartNet, DMR, NXDN, multi-site, conventional, tones), the
  interface and dashboard, recordings, plugins, the configuration reference
  split by section, the command line, coming from Trunk Recorder, and
  troubleshooting. The README is now a short overview with links.
- **Windows:** closing the console window, Ctrl-Break, signing out or
  shutting down now saves the calls in progress before quitting, as Ctrl-C
  does (closing the window used to lose them).
- **Upload script:** the WAV is always written for it, so its first argument
  is never a missing file (with an M4A encoder installed and Keep the audio
  off, it used to be).
- A plugin that needs a recorder-wide setting (an API key, say) isn't started
  until it's set; the log and the Health tab say which setting is missing.
- File name formats: `{signal}` and `{noise}` give the call's levels in whole
  dBFS, as Trunk Recorder does (they were always 0).
- Informational protocol notes (a DMR site's colour code and kind, NXDN
  RAN and site, channels learned) are logged as information, not as errors.
- The browser version keeps settings it doesn't know when it saves, as the
  desktop app does.
- **Import Trunk Recorder config** also brings Broadcastify's `broadcastifyOTA`
  and talkgroup allow / deny lists, Rdio Scanner's talkgroup allow / deny
  lists, and the global `defaultMode` (to each SmartNet system).
- Setup: the **multi-site with …** chip groups sites as the recorder does
  (SmartNet, NXDN, and sites grouped by what their control channels
  announce); a headerless talkgroup file downloads with a header row; the
  conventional CSV help lists every mode and the Tone column; the Linux dongle
  hint covers both RTL-SDR USB IDs.
- Dashboard: the RF page's band and the Decode page's **In band** use each
  source's guard band; the setup guide's coverage step does too. Plugin
  Health cards show a plugin's own figures (simplestream's packets sent and
  dropped).
- A call whose uploads never report back within an hour has its spooled files
  moved to the recordings folder at once, instead of staying in the RAM spool
  for another hour.
- `survey` and `rolloff` refuse an unknown `--format` (they read it as cu8);
  `rolloff` on a capture takes `--format`. `--help` lists every command and
  option.
- Linux: `install.sh` creates the `plugdev` group where the distribution has
  none (Fedora, Arch) and adds you to it, so a headless service can open the
  dongles.
- The Docker image is published to Docker Hub as
  `robotastic/trunk-recorder-pro` for each release (`latest`, `0.1.5`, `0.1`).

## [0.1.4] — 2026-10-04

- **NXDN** (Kenwood NEXEDGE, Icom IDAS), at both rates — NXDN48 (6.25 kHz)
  and NXDN96 (12.5 kHz):
  - conventional channels, `"mode": "nxdn48"` / `"nxdn96"`, split by RAN
    and group in the Tone column (`RAN 5 TG 201`);
  - trunked `type` `"nxdn"` systems: Type-C (a control channel; channel
    numbers from `lcnTableHz`, the site's Direct Frequency Assignment, or
    learned from the voice frequencies in `nxdnChannelsHz`) and Type-D (IDAS
    distributed: every repeater watched, calls found by their SCCH);
  - Find my system recognises NXDN control channels and carriers; the
    dashboard shows a site's system and site code, RAN, channel table, the
    channel numbers not known yet, and its carriers;
  - call JSON `"ran"`, file name `{ran}`; `tool nxdnscan` / `tool nxdn` / `tool nxdnsynth`,
    `replay --nxdn48 / --nxdn96 / --nxdn-trunk`.

  Checked on recorded NXDN48 / NXDN96 signals and synthesized control and
  traffic channels; not yet on a live trunked system.
- The 4FSK receiver no longer counts the noise just before a signal comes
  up towards its levels (it decided those symbols late, by the levels of
  the time), so a transmission's first frames decode.
- Building from source needs Rust 1.95 or newer (the workspace is on the
  2024 edition).

## [0.1.3] — 2026-10-04

- **Folders and file names, built by dragging.** Setup → Recording (and a
  system's Recording override) has an editor for `filenameFormat`: drag in
  the call's fields, its start time (local or UTC), folder separators and
  your own text, with an example path as macOS / Linux or Windows would
  write it. The format is checked as you go — unknown tokens, a leading `/`
  or drive, empty folders, `..`, characters or names Windows won't take.
  A format with `\` for folders works as well as one with `/`. Time codes
  take `-` for no padding (`{time:%-m}`), so Trunk Recorder's
  `<year>/<month>/<day>` layout can be written as a format.
- **Talkgroups pasted from RadioReference** in Setup → Systems: copy the
  talkgroup tables from the system's page, paste, check the preview. Each
  category heading becomes the Category. Each system keeps its own list
  ("Copy from…" is gone), and Download saves it as a Trunk Recorder CSV.
  RadioReference's `De` / `Te` (only sometimes encrypted) are no longer read
  as `DE` / `TE`, which aren't recorded.
- **Talkgroup CSVs as Trunk Recorder reads them:** commas, semicolons, tabs
  or `|`, a spreadsheet's byte-order mark, headers in any case, and
  RadioReference's `DEC` / `Group` headings. After loading, Setup says what
  Trunk Recorder itself would refuse (no header row, no Mode or Description
  column, unknown columns) and which rows had no talkgroup number.
- Call rules: "Default (…)" in place of "As Recording (…)", "Reset to
  Defaults", and "Digital / Analog Level Adjustment, dB".
- **RAM spool** (`recording.ramSpool`, macOS and Linux): a call's files that
  only the upload plugins need wait in memory — a RAM disk on macOS, /dev/shm
  on Linux — and reach the recordings folder only when they're kept (an
  upload failed, or Keep the audio / call JSON is on). When it's full, calls
  go to the folder as before. The dashboard shows how full it is, with
  `spoolLow` / `spoolFull` events.
- **Spotlight** (macOS): the dashboard says when Spotlight is indexing the
  recordings folder — disk writes for every call — and how to exclude it.
- **CQPSK phase error** (`sys/<short>/cc/phaseErr`, RMS degrees: under 10
  clean, ~25 noise) as CQPSK's quality figure, where C4FM has its eye
  opening; the carrier offset graph reads correctly for CQPSK.
- **USRP:** a bigger receive buffer (`num_recv_frames=256` unless the
  device args set it) and, on macOS, the receive thread at interactive
  priority — a B200 at 8 MSPS was losing samples whenever the computer was
  busy.
- Dashboard: each source's waterfall is back on the RF page; with several
  systems, Radio system shows each one separately.
- Web build: the screen stays awake while recording (phones and tablets
  froze the tab), and calls can be downloaded as one .zip where there's no
  folder picker (Android Chrome, Firefox).

## [0.1.2] — 2026-10-03

**Breaking: config keys renamed.** Every key now carries its unit, and a
v0.1.1 config won't load until it's updated (the app says which field it
couldn't read). Rename in your config.json:

| v0.1.1 | v0.1.2 |
|---|---|
| a source's `kind` | `type` |
| `controlChannels` | `controlChannelsHz` |
| `voiceChannels` | `voiceChannelsHz` |
| DMR `channels` | `dmrChannelsHz` |
| `lcnTable` | `lcnTableHz` |
| `bandplanBase` / `bandplanSpacing` / `bandplanHigh` | `bandplanBaseHz` / `bandplanSpacingHz` / `bandplanHighHz` |
| Airspy `gain` / `lnaGain` / `mixerGain` / `vgaGain` | `gainStep` / `lnaStep` / `mixerStep` / `vgaStep` |
| `controlWarnRate` | `controlWarnRatePerS` |

Frequencies are in Hz everywhere (the MHz some DMR and SmartNet fields
accepted is gone).

- **Guard band per source** (`guardHz`, default 75 kHz at each edge) in place
  of the fixed 90 % of the sample rate, which left 120 kHz per edge at
  2.4 MSPS and 1 MHz at 20 MSPS. Trunk Recorder left 32–64 kHz, so a config
  with channels near the edge of a source may need its centre moved, or a
  smaller guard.
- **Profile roll-off** (Setup → Radios, offered when a source is added; also
  `trunk-pro rolloff`): listens to the radio for a moment, finds where its
  noise floor sags at each edge, and suggests a guard band — shown on a
  waterfall and spectrum, adjusted by dragging or with a slider, with the
  channels it would leave out. The suggestion is conservative: on an
  RTL-SDR at 2.4 MSPS the floor sags gently and the 3 dB point is
  140–180 kHz in.
- **A dashboard for running the recorder**, in place of the single page:
  - **Overview**: every system at a glance — today's calls, airtime and
    audio, recorders in use, calls missed; a card per system with its
    control channel's rate against its usual, what's on the air, and voice
    quality by frequency; the sources, the computer, the plugins, and what
    happened lately.
  - **RF**: each source's noise floor, headroom and clipping, frequency
    error against the control channels, dropped samples; the band with
    every channel's level above the floor; the waterfall on demand; a week
    of history.
  - **Decode**: control channel messages decoded and lost, the receiver's
    eye opening, P25 framing misses, voice frames lost — per frequency, with
    hints when a pattern points at the RF (a common offset: the source's
    ppm; a bad channel at a band edge).
  - **Radio system**: talkgroups by hour (new, unknown, encrypted, ignored —
    and an Ignore switch that takes effect at once), radios with their
    affiliations and who they talk with, call lengths, what happened to
    calls; a page per talkgroup and per radio.
  - **Plugins**: each plugin's results minute by minute, queue, upload time,
    the services it talks to, restarts.
  - **Platform**: CPU (all, and the recorder's), memory pressure, disks and
    the days of recordings left, internet reachability minute by minute —
    container limits respected, what a platform can't tell shown as such.
- **History**: a week of one-minute figures kept in `<data>/stats/`, and the
  radio registry (talkgroups, radios, frequencies heard) in `<data>/radio/`.
- **Events**: control channel lost / regained, a talkgroup or radio heard for
  the first time, a source dropping samples or clipping, a plugin's health
  changing, the internet going down, a disk filling — in the dashboard, and
  through the `Watcher` hook alert rules will use.
- **API**: `subscribe` (`spectrum` and the control channel `log` now come only
  to connections that ask), `stats`, `host`, `rfDetail`, `decodeDetail`,
  `monitorEvent`, `statsQuery`, `radioQuery` (docs/api).
- **Plugin SDK 0.2**: `metrics` messages (`Host::metrics`); `CallQueue`
  reports its queue, upload time and the service's state by itself.
- Talkgroup file changes (an Ignore flag, a tag) apply while recording.
- A source's last error is shown for 5 minutes after it.

## [0.1.1] — 2026-10-02

- **Faster on Raspberry Pi**: about half the CPU for the same output (the
  C4FM multi-symbol detector, Hamming soft decoding and the CQPSK receivers
  vectorised and shared). On a Raspberry Pi 5 recording a P25 site from one
  RTL-SDR: 5 % of a core with one call, under 1 point more for each further
  call; Trunk Recorder needs 4–6× as much (`docs/performance.md`).
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
- **Several conventional systems**, as in Trunk Recorder: each with its own
  short name (folder), channels or channel file, squelch, Recording
  override, unit names and plugin settings (its own upload keys), switched
  on or off as a whole. Imported one for one from a Trunk Recorder config.
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
- **Plugin store**: the Plugins page installs, updates and removes plugins
  from the [plugin registry](https://github.com/TrunkRecorder/plugins),
  each download checked against the registry's SHA-256, and shows how
  each is running: calls handled, status, recent log. A plugin that isn't
  listed can be installed from its GitHub release, marked as not reviewed.
  `trunk-pro plugin search | install | update | uninstall` from the
  command line, and `list | describe | run` to try one against recorded
  calls.
- **Plugins set up in Setup**, in the config like everything else: a
  Plugins tab to turn each on and fill in its settings, and each system's
  card has its settings for every plugin that's on (an OpenMHz key, say).
  Settings are drawn from what the plugin describes; while recording,
  changes restart the plugins at once. Trunk Recorder's uploaders and their keys
  come over with an imported config.
- The built-in plugin list: OpenMHz 0.1.2, Broadcastify, Rdio Scanner and
  simplestream 0.1.1.
- **Interfaces of your own**: the WebSocket and call files the built-in
  interface uses, documented for developers (`docs/api/README.md`, served at
  `/api/docs`) and for an LLM building one (`/api/llms.txt`), with
  `client.js` (connection, state, live-audio player; `/api/client.js`),
  four examples (`/api/examples/`), the protocol's TypeScript types
  (`/api/protocol.ts`) and its JSON Schema (`/api/schema`, checked against
  what the recorder sends). The recorder serves any number of them — folders
  in `server.interfaces` (Setup → Recording → Interfaces) at
  `/ui/<name>/`, one of them at `/` if `server.home` says so, or
  `--ui <folder>` for one run — with the built-in interface always at
  `/builtin/`. Web pages from other sites are refused unless listed in
  `server.allowedOrigins`.

## [0.1.0] — 2026-10-02

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
- Captures in `cu8`, `cs16` or `cf32` (GNU Radio / UHD) formats.
- **Trunk Recorder–compatible output**: WAV + call JSON in Trunk Recorder's
  folder layout and field names; talkgroup CSV import; import of a Trunk
  Recorder config. `error_count` is the call's FEC-corrected bit errors,
  and `errorList` breaks them down per 10 s of audio (errors, repeated or
  muted frames, worst frame) so bursts stand out.
- **Plugins**: programs of their own that the recorder runs while it records
  and tells what happens — calls starting, ending and landing on disk, radio
  activity, live audio, status — over JSON lines on stdin/stdout. They only
  watch; a plugin that crashes is restarted, and one that falls behind loses
  events rather than slowing the recorder. Settings in `plugins.json`
  beside the config. Calls are encoded to M4A once for every plugin that
  asks (ffmpeg, macOS's afconvert or fdkaac, whichever is there; WAV only
  without one). `trunk-pro plugin list | describe | run` to look at plugins
  and run one against recorded calls. The `trunk-recorder-plugin` crate is
  the protocol and a Rust SDK. A **Plugins** page lists them: turn each on
  or off, fill in settings drawn from what the plugin describes (its own and
  for each system), see whether it's running and how many calls it handled,
  its recent log, and the M4A encoder. Changes apply at once while recording.
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
