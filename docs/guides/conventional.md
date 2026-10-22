# Conventional channels

This page covers recording conventional (non-trunked) channels: how to set them up, how Trunk
Recorder Pro decides that someone is transmitting, how to tune the squelch, and what to check when
a channel records too much or too little.

## What a conventional channel is

A trunked system hands out voice channels from a control channel. A conventional channel is
simpler: one frequency, always the same, and whoever keys up talks on it. Fire dispatch on a VHF
frequency, a public works channel, a business radio repeater. There is no control channel to tell
the recorder when a call starts, so it has to listen to the frequency itself and notice when a
signal appears.

Each channel has a mode:

| Mode | Config value | What it is |
|---|---|---|
| Analog FM | `fm` | FM voice, narrowband (12.5 kHz) or wide (25 kHz) |
| Analog AM | `am` | AM voice: VHF airband (118–137 MHz) and UHF military air (225–400 MHz) |
| P25 | `p25` | P25 Phase 1, C4FM or CQPSK |
| DMR | `dmr` | DMR Tier II; both slots, each recording its own calls (the channel is opened 6.25 kHz wide, as for trunked DMR) |
| NXDN48 | `nxdn48` | NXDN at 4800 bps (6.25 kHz) |
| NXDN96 | `nxdn96` | NXDN at 9600 bps (12.5 kHz) |

One list can mix modes. A config can have trunked systems, conventional channels, or both. With
no trunked system at all, it records conventional channels only.

## Conventional systems

Channels are grouped into **conventional systems**, the way Trunk Recorder groups them. Each one
has its own:

- short name, which is the folder its calls go to and the name plugins see,
- squelch (for its channels that don't set their own),
- channel list, or a linked channel file,
- unit names, plugin settings and delays (**Plugins**), and recording rules (**Recording**),
- on/off switch for the whole group.

A frequency can belong to only one conventional system.

Plugins treat each conventional system as one more system. Systems are numbered from 0: the
trunked systems being recorded first, then the conventional ones. That number is what
`{sys_num}` gives in a [file name format](recordings.md#tokens), and what upload plugins see. It
can change when you add, remove or switch off a system, so plugins go by short name.

## Adding channels

### In the interface

Open **Setup**, then the **Conventional** tab, and click **Add a conventional system**. In the
panel that appears:

1. Give it a **Short name** (letters, digits, `-`, `_` and `.`; it becomes a folder name) and,
   if you like, a **Name** such as "County Fire".
2. Type frequencies in MHz into **Add frequencies, MHz** (for example `154.430, 155.100,
   460.125`), pick the mode beside it, and click **Add**. Or click **Add a channel** for one row.
3. Fill in the table. Each row has **On**, **Frequency, MHz**, **Mode**, **Bandwidth** (analog
   only; see [Bandwidth](#bandwidth)), **Records** (what it takes, as chips), **Name** (the alpha
   tag), **TG Number** and **Squelch**. **Edit** opens the rest of the row: its **Code**, DMR's
   **Slot** and a digital row's **Match TG**, and the **Description**, **Tag** and **Category** its
   calls carry (offering the tags and categories the system uses already). A row's tag and category
   show under its name.
4. Leave **Squelch, dB above noise** at 8 to start with. See [Tuning the squelch](#tuning-the-squelch).

The **TG Number** column is the number calls are filed under: in file names, the call JSON and
uploads. Leave it blank and it defaults to the frequency in kHz (154.430 MHz becomes 154430),
which stays the same however you reorder the list. A second row on the same frequency gets that
number with a digit added (1543251, 1543252, ...). A digital (P25, DMR or NXDN) row with no
TG Number is filed under the talkgroup the radio sends, when it sends one. The air's number is also
written to the call JSON as `talkgroup_ota`.

**Code** limits a row to transmissions carrying one CTCSS tone, DCS code, NAC, colour code or RAN,
written bare (`151.4`, `D023N`, `293`, `1`, `5`). **Slot** (DMR) and **Match TG** (P25, DMR, NXDN)
narrow it further. Leave them blank to record everything on the frequency. Splitting a shared
frequency is covered in [Tones and access codes](tones.md). A config with an older `tone` key is
upgraded when it loads.

**Import CSV…** reads a channel file and asks whether to **Replace the list** or **Add to the
list**. **Export CSV** writes the whole list, codes and names included, as a CSV you can edit and
import again (or link as a [channel file](#in-a-spreadsheet-desktop-app)).

### In a spreadsheet (desktop app)

If you'd rather keep the list in Excel, Numbers or LibreOffice, link the system to a CSV file.
Under **Channel file (optional)**, type a path such as `fire-channels.csv` and click **Use this
file**. If the file doesn't exist yet, it is created from the current list. From then on the
channels come from the file: the table becomes read-only, and you edit the file instead.

- Click **Reload** after editing the file. Recording also re-reads it every time it starts.
- **Unlink** stops reading the file and keeps the channels in the app's settings.
- A relative path is taken from the folder the config file is in.
- If the file can't be read when recording starts (missing, empty, or no `Frequency` column),
  recording doesn't start, and the error names the file.

Channel files aren't available in the browser version.

### In the config file

```json
"conventional": [{
  "shortName": "county",
  "squelchDb": 8,
  "channels": [
    { "freqHz": 154430000, "mode": "fm",  "name": "County Fire Dispatch", "talkgroup": 1001 },
    { "freqHz": 155100000, "mode": "fm",  "name": "EMS Ops", "code": "151.4" },
    { "freqHz": 460125000, "mode": "p25", "name": "PD Tac 2", "squelchDb": 12 },
    { "freqHz": 453000000, "mode": "fm",  "enabled": false }
  ]
}, {
  "shortName": "air",
  "demod": { "squelchHangMs": 150 },
  "channels": [
    { "freqHz": 119100000, "mode": "am", "name": "Tower" },
    { "freqHz": 132650000, "mode": "am", "name": "ATIS" },
    { "freqHz": 269000000, "mode": "am", "name": "Mil Approach" }
  ]
}, {
  "shortName": "pd",
  "channelFile": "pd-channels.csv",
  "recording": { "minCallS": 2 }
}]
```

Frequencies in the config are in Hz. Every key is listed in the
[conventional configuration reference](../configuration/conventional.md).

### The channel CSV

A header row names the columns, in any order and any case; then one channel per row:

```csv
TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Bandwidth kHz,Enable,AutoTune
1001,154.4300,,,,fm,County Fire Dispatch,,Fire Dispatch,Fire,,,true
,154.3250,D223N,,,fm,County A Fire,,,Fire,,,true
,154.3250,151.4,,,fm,County B Fire,,,Fire,,,true
,460.1250,,,,p25,PD Tac 2,,Law Tac,Police,12,,true
,119.1000,,,,am,Tower,,Aircraft,Airport,,,true
```

- `Frequency` is MHz when it has a decimal point, otherwise Hz.
- `Mode` is `fm`, `am`, `p25`, `dmr`, `nxdn48` or `nxdn96`; empty means `fm`, unless `Code` holds
  a NAC (then `p25`), a colour code (`dmr`) or a RAN (`nxdn48`). An old `Tone` column still reads,
  as `Code`. A combined string such as `CC1 TS2 TG201` in `Code` is split into `Code`, `Slot` and
  `Match TG`.
- `Squelch dB` is 3-40 dB above the noise floor; empty uses the system's.
- `Bandwidth kHz` is an `fm` or `am` row's filter width, 4-20; empty uses the mode's default.
- `Enable` set to `false`, `no`, `0` or `off` turns a row off.
- `AutoTune` set to `true`, `yes`, `1` or `on` lets a digital row's calls correct its source's
  frequency error; empty is off. See [Frequency correction](#frequency-correction).
- Commas, semicolons or tabs all work, as do decimal commas and the byte-order mark Excel adds in
  some locales. Blank rows and lines starting with `#` are skipped.

Rows that can't be read are reported by row number, and the rest are used. A Trunk Recorder
channel file reads as it is; see [Coming from Trunk Recorder](#coming-from-trunk-recorder). The
full column list is in the [configuration reference](../configuration/conventional.md).

### Covering the channels with a radio

Every enabled channel must be inside a source's bandwidth. If a source's center is left on
**Auto**, it is placed to cover the channels when they fit. If they don't, recording won't start
and says which ones are outside: `Conventional channel(s) outside every source's bandwidth: ...
MHz — move a center frequency or disable them.` See [Radios](radios.md) for centers, sample rates
and guard bands.

### Frequency correction

A source's [AutoTune](radios.md#autotune) corrects the conventional channels on it as it does
trunked ones: each channel is placed by the source's correction, taken up between calls. A source
with a control channel learns the correction there. A source that carries only conventional
channels has none, so it learns it from calls on the rows you ask:

- Turn on **AutoTune** in a digital row's editor (Setup, the row's **Edit**), or put `true` in the
  CSV's `AutoTune` column. Its calls of a second or more then measure the source.
- Ask it of a **repeater output**: the base station holds its frequency, so every call says the
  same. Leave it off for simplex, talkaround or a repeater's input, where each radio is off by its
  own amount.
- The source needs AutoTune on. These calls count as a trunked call does: one measurement each,
  kept with the control channels' and trunked calls' on the same source, and the correction is the
  middle (median) of the last 20, so one radio off its frequency doesn't move it.

Each call's JSON keeps the whole offset (`freq_error`, Hz, against the ppm you set), corrected or
not, so the **Decode** page's frequency table shows the source's error either way.

## How a transmission is detected

Trunk Recorder Pro doesn't run a receiver on every conventional channel all the time. Each source
already computes a spectrum of everything it receives (the channelizer needs it anyway), and the
conventional channels are watched from that. A quiet channel costs almost nothing, so hundreds per
source are fine.

Here is what happens, step by step:

1. **The noise floor is measured.** Each source's band is cut into 64 slices. Every 0.1 s, the
   median level of each slice is taken and smoothed over time. Using the median means a few
   signals in a slice don't raise it, and measuring per slice follows the SDR's passband, which is
   usually lower near the edges.
2. **Each channel's energy is compared with the floor.** For every block of samples, the power
   within ±5 kHz of the channel (±3.125 kHz for NXDN48; an analog channel's bandwidth less 1 kHz) is averaged
   (AM also looks for its carrier's line: see [Airband](#airband-am)), lightly smoothed (20 ms), and
   compared with the floor under it. The difference in dB is the channel's SNR.
3. **The channel opens at the squelch.** When the SNR reaches the channel's squelch (default
   8 dB), a receiver is started on that frequency. It begins 0.3 s in the past: the air from just
   before the detection is replayed into it (pre-roll), so the first syllable isn't lost.
4. **A narrower filter confirms the carrier.** The receiver measures power through a ±5.5 kHz
   channel filter (an analog channel's own [bandwidth](#bandwidth)). This filter is narrower than the spectrum view, so energy leaking over from a
   strong neighbour 12.5 kHz away doesn't count. If the filter never sees a carrier, the detection
   was leakage or a spur: the receiver closes, and that channel's threshold is raised to 3 dB above
   whatever set it off until the band quietens down again. This stops a neighbour from reopening the
   channel over and over.
5. **The audio is decoded.** Analog FM and AM are demodulated to 8 kHz audio. Audio passes only while the
   carrier, measured through that channel filter, stays above a level 3 dB lower than the squelch
   (and never lower than 3 dB above the floor), so the gaps between transmissions are cut out.
   P25, DMR and NXDN go through the same decoders trunked calls use, and the same level decides
   whether their carrier is still there.
6. **The call ends after the call timeout.** A call starts with the first audio (or, for P25, the
   first link control). It ends when there has been no audio for the call timeout, 3 s by default.
   A transmission after a shorter pause continues the same call. The receiver closes 0.5 s after
   the carrier goes away with no call in progress.

Things that follow from this:

- The squelch is relative to the measured noise floor, not an absolute power level. Changing gain
  or swapping the dongle moves the floor and the signal together, so the same squelch keeps working.
- The 3 dB gap between opening and keeping the audio going (hysteresis) means a signal that opens
  the channel doesn't chatter on and off as it fades a little.
- Pre-roll for conventional channels is fixed at 0.3 s. The **Pre-roll, s** setting on the
  Recording tab is for trunked calls (audio before each grant) and doesn't change it.
- A call longer than **Longest call, s** is saved and a new one started. With no limit set, a
  stuck carrier is saved in 600 s parts.

## Airband (AM)

Aircraft and air traffic control talk in AM: VHF airband from 118 to 137 MHz on 25 kHz channels
(8.33 kHz in Europe), and UHF military air from 225 to 400 MHz. Add these channels with mode
**Analog AM** (`am`). An RTL-SDR at 2.4 MSPS covers about 2 MHz of airband, enough for a tower,
ground, approach and ATIS together.

How AM differs from FM here:

- **The volume follows the modulation, not the signal strength.** An aircraft's signal rises and
  falls as it turns and moves away; the audio level is measured against its own carrier, so a
  weak or fading aircraft sounds as loud as the tower. **AM level** sets how fast that follows
  (see [Demodulation settings](#demodulation-settings)).
- **No tones or codes.** AM has nothing to split a frequency by, so an AM frequency has one row.
- **Off-frequency transmitters are fine.** AM is detected from the envelope, which doesn't care
  where in the channel filter the carrier sits. Aircraft radios can be a couple of kHz off; UHF
  military radios more, which is why UHF channels get a wider filter by default.
- **Impulse noise is limited.** Clicks from ignition and power lines are clipped at 150 %
  modulation, above any voice, so they don't blast through.
- **Weak aircraft are heard by their carrier.** At least half of an AM signal's power is its
  carrier, a single line in the spectrum, while noise spreads across the channel. Besides the
  channel's power, an AM channel listens for that line, which stands out of the noise about
  10 dB sooner: a transmission whose power in the channel is no higher than the noise's opens it
  at the default squelch of 8 dB. The squelch applies to both, and so does its audio gate.
- **Spurs are learned.** A line detector also hears the steady, unmodulated whistles ("birdies")
  that SDRs and nearby electronics put in many channels. An AM channel learns them: in its first
  3 s (when it doesn't open), and afterwards any line that stays put for about a minute. It then
  ignores them. A birdie that starts later can open a channel for that first minute. A carrier
  strong enough to open the channel by its power, such as a nearby ATIS, is still recorded
  however steady it is; a weak continuous broadcast is learned like a birdie.

A good first test is the airport's ATIS, which transmits all the time: it should record as one
long call, cut into parts at **Longest call, s** if you have set one.

## Bandwidth

An analog channel's **Bandwidth** is the width of the filter its audio and carrier meter come
through. Leave it at **Default** unless one of these applies:

| Mode | Width | Use for |
|---|---|---|
| `fm` | 11 kHz (default) | Narrowband FM: 12.5 kHz channels, ±2.5 kHz deviation. Almost all US public safety and business radio since 2013 |
| `fm` | 16 kHz | Wide FM: 25 kHz channels, ±5 kHz deviation. Older systems, some federal, and many outside the US. Loud voice distorts through the narrow filter |
| `am` | 6 kHz | 8.33 kHz airband channels (Europe) |
| `am` | 11 kHz (default below 225 MHz) | 25 kHz airband channels |
| `am` | 16 kHz (default from 225 MHz) | UHF military air, a transmitter that is well off frequency, or offset-carrier sites |

Wider takes in more noise and makes a strong neighbour more likely to be heard; narrower cuts a
transmitter that is off frequency, or the high end of wide FM's voice. Rows sharing a frequency
share one bandwidth: changing it on one row in Setup changes them all.

## Demodulation settings

Each conventional system's **Demodulation** group (in its card on the Conventional tab, shown
when it has analog channels) applies to all of its FM and AM channels. Everything left at
**Default** behaves as before. To use other settings for some channels, put them in a
conventional system of their own.

| Setting | Default | What it does |
|---|---|---|
| **FM de-emphasis** | 750 µs | FM transmitters boost the treble (pre-emphasis) and receivers cut it back. Set **Off** for a channel sent without pre-emphasis: with de-emphasis on it sounds muffled |
| **AM level** | Fast (50 ms) | How fast the volume follows a fading carrier. **Fast** keeps a moving aircraft steady. **Slow** (0.5 s) pumps less on a steady signal such as ATIS |
| **AM low cut, Hz** | 200 | A high-pass under the voice, for carrier hum and the flutter of a fading signal. 0 turns it off; 300 helps a humming ground transmitter |
| **Squelch open delay, ms** | 30 | How long the carrier must hold before audio passes, so a neighbour's splatter as it keys up doesn't open the channel. Lower catches the first syllable of very short transmissions |
| **Squelch hang, ms** | 2 | How long audio keeps passing after the carrier drops. Raise it (100-300) for weak or fluttering signals, such as distant aircraft, whose audio is cut into pieces |

The hang doesn't add noise to the end of a transmission: audio runs a little behind the air, and
when the gate closes, everything since the carrier dropped is removed. A fade shorter than the
hang keeps playing (as the noise it is), and live audio runs the hang later than the air.

## Tuning the squelch

The squelch is how far above the noise floor, in dB, a signal must be to open a channel.

- **Too low**, and noise, interference, or splatter from busy neighbouring channels opens the
  channel. You get short calls with hiss or nothing in them.
- **Too high**, and weak transmissions never open it, or open late and lose their first words.

The default of 8 dB suits most channels. The interface accepts 3 to 40.

### Where to set it

- **For a whole system:** **Squelch, dB above noise** in the system's panel (`squelchDb` on the
  system in the config).
- **For one channel:** the **Squelch** column in the table (`squelchDb` on the channel, or the
  `Squelch dB` CSV column). Leave it blank to use the system's.

If several rows share one frequency (to split it by tone), the frequency opens at the lowest
squelch any of them sets.

### Finding a good value

1. Start recording with the default.
2. Open the **RF** page and click the source that covers the channel. The **Channels on this
   source** table lists every channel with its **Power**, **Floor** and **SNR**. The **Across the
   band** chart shows the floor as bars and each channel as a stem rising above it.
3. Watch the channel's **SNR** while it is idle, and again while someone is talking.
4. Set the squelch between the two: comfortably above the idle reading, comfortably below the
   reading during a transmission. If idle sits at 2-3 dB and transmissions show 25 dB, the default
   8 is fine. If a nearby transmitter keeps the idle reading at 9 dB, try 14.

The SNR on the RF page is measured over a slightly narrower band (±3 kHz) than the detector uses
(±5 kHz), so treat it as a close guide, not an exact match.

Each saved call also records how strong it was: `signal`, `noise` and `snr` in its
[call JSON](recordings.md#the-call-json), and on the log line when the call is saved.

### Symptoms and fixes

| What you see | Try |
|---|---|
| Lots of very short calls with only noise | Raise the channel's squelch a few dB. Set **Shortest call, s** under the system's **Recording** override to drop what's left. |
| Calls appear whenever a strong neighbouring channel is busy | Raise that channel's squelch. Check that your gain isn't so high the dongle is overloading. |
| Weak units are missing, or calls start mid-word | Lower the squelch, a few dB at a time. Improve the antenna or gain; see [Radios](radios.md). |
| One conversation is split into many calls | Raise **Call timeout, s** under the system's **Recording** override. |
| Separate conversations are merged into one call | Lower **Call timeout, s**. |
| Calls are cut into 10-minute pieces | A carrier is stuck on. Raise the squelch, or set **Longest call, s** if the channel really is that busy. |

## Debugging a channel

If a channel isn't recording as you expect, work through these:

1. **Is it on?** The system's **Record** switch and the row's **On** box must both be set.
2. **Is it covered?** Recording refuses to start if a channel is outside every source; the RF page
   shows where each source's band lies, with conventional channels marked.
3. **Is the frequency right?** A wrong ppm correction puts the signal off-center, so less of it
   falls inside the ±5 kHz the detector measures and the channel filter passes. See
   [Radios](radios.md) for ppm.
4. **What does the SNR do?** On the RF page, watch the channel during a transmission. If the SNR
   never gets near the squelch, lower the squelch or improve reception. If it sits above the squelch
   while idle, raise it.
5. **Is a tone filtering it out?** If the row has a **Code**, transmissions with a different code
   (or none) aren't recorded. The **Heard** line under the channel counts them as "not recorded".
   See [Tones and access codes](tones.md).
6. **Are calls dropped after recording?** **Shortest call, s**, **Shortest transmission, s** and
   **Calls with no audio** decide which finished calls are kept. A dropped call logs `Call not
   saved - no audio, or shorter than the minimum`.

In the log, a conventional call looks like a trunked one: a `Starting ... Recorder` line when it
starts, and `Concluding Recorded Call - Call Length: ...` with its signal, noise and SNR when it is
saved. There is no separate log line for each detection.

### Messages that stop recording from starting

| Message | Meaning |
|---|---|
| `Conventional channel(s) outside every source's bandwidth: ... MHz` | Move a source's center (or use Auto), add a source, or turn the channel off. |
| `Conventional channel ... MHz is in both A and B` | A frequency can belong to only one conventional system. |
| `Conventional channel ... MHz is listed with different modes` | Rows sharing a frequency need the same mode. |
| `Conventional channel ... MHz is listed twice without a tone` (or NAC, or code) | Give each row its own code; one may have none. |
| `Conventional channel ... MHz has two rows for ...` | Two rows ask for the same code (or two DCS codes that are the same signal). |
| `A conventional channel has no frequency yet.` | A row was added and left empty. |
| `Channel file ...: ...` | The linked CSV couldn't be read. |

## Talkgroup names on digital channels

P25, DMR and NXDN channels can carry a talkgroup number. A call's names are the row's it was
recorded under, if the row has any; else the conventional system's own talkgroup file's
(`<shortName>.talkgroups.csv` beside the config; Setup shows it on a conventional system with
digital channels), looked up by the number the call is filed under. The Radio page names
talkgroups the same way; a talkgroup filed under the air's number, on a row with names, is named
once a call of it has been heard.

Several P25 channels whose radios all send talkgroup 1 would all be filed under 1. Give each row
its own **TG Number**: calls are filed and named under it, and the air's 1 stays in
`talkgroup_ota`. A new talkgroup on the air within an open call still starts a new call, even when
the row files both under the same number.

## Coming from Trunk Recorder

- **Squelch is different.** Trunk Recorder's `squelch` is an absolute level (like `-50`) that
  depends on your dongle and gain. Here it is dB above the measured noise floor. Squelch values
  aren't carried over when you import a config or a channel file; start with 8 and adjust.
- **No signal detector settings.** There is no `signalDetectorThreshold` and no `Signal Detector`
  column to set: detection always works from the noise floor as described above.
- **Channel files read as they are.** **Import CSV…** and channel files accept Trunk Recorder's
  format, with its `Tone` column read as **Code**. Its `Comment` and `Signal Detector` columns are ignored, and its
  `Squelch` column isn't read; use `Squelch dB` instead. Add a `Mode` column to mix analog and
  digital in one file.
- **System types merge.** `conventional`, `conventionalP25` and `conventionalDMR` systems become
  conventional systems here, each channel with its own mode. **Import Trunk Recorder config…**
  brings them in with their short names, channels or channel files, rules and unit names; see
  [Migrating from Trunk Recorder](../migrating-from-trunk-recorder.md).
- **Replaying captures.** `trunk-pro replay` takes conventional channels with `--fm`, `--p25`,
  `--dmr`, `--nxdn48`, `--nxdn96` or `--channels channels.csv`, and `--squelch` (default 8). See
  the [command line](../command-line.md).
