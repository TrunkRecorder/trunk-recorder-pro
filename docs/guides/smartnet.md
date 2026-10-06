# SmartNet systems

This page explains how Trunk Recorder Pro records Motorola SmartNet and SmartZone (Type II)
systems: band plans, including the custom VHF/UHF ones, analog and digital voice, and the
**Voice of unknown talkgroups** setting. Every key is listed in the
[trunked systems reference](../configuration/systems.md#smartnet).

## What it handles

A SmartNet control channel is a 3600 baud signal that sends short messages (*OSWs*) all the time:
grants, updates, the system ID, patches. Voice channels are either **analog FM** or **P25 Phase
1**, and a grant says which. Many systems mix the two.

Trunk Recorder Pro handles 800 MHz (standard, rebanded and splinter), 900 MHz, and VHF/UHF
systems with a custom band plan (often called OBT). It records group calls and patched
talkgroups. It recognises private (radio-to-radio) and interconnect calls on the control channel
but doesn't record them.

## Setting it up

**Find my system** in Setup finds SmartNet control channels and, unlike most tools, works out the
band plan from the air (see [SmartNet band plans](find-my-system.md#smartnet-band-plans)). That's
the easiest way to set up a VHF or UHF system.

To add one by hand, click **Add a system** on **Setup** → **Systems**, set **Type** to
**SmartNet / SmartZone**, and fill in:

- **Control channels, MHz**: the control channel, then the alternates ("Later ones are
  fallbacks").
- **Band plan**: see below.
- **Voice of unknown talkgroups**: see [Analog and digital voice](#analog-and-digital-voice).
- **P25 voice modulation**: how the system's P25 voice channels are modulated. It works as on a
  P25 system ([Modulation](p25.md#modulation-auto-c4fm-or-cqpsk)); **Auto (both receivers)** is
  safe. It doesn't apply to the control channel or to analog calls.
- **Talkgroups**: see [Talkgroups and units](talkgroups-and-units.md).

An 800 MHz system in the config file (frequencies in Hz):

```json
{ "shortName": "county", "type": "smartnet", "controlChannelsHz": [856112500], "bandplan": "800_reband" }
```

## Band plans

A SmartNet grant names a channel number, and unlike P25, nothing on the air says what frequency
that number is. You have to tell the recorder which numbering the system uses:

| **Band plan** in Setup | `bandplan` | Use for |
|---|---|---|
| **800 MHz standard** | `800_standard` (default) | 800 MHz systems that haven't been rebanded |
| **800 MHz rebanded** | `800_reband` | 800 MHz systems after rebanding (most US systems today) |
| **800 MHz splinter** | `800_splinter` | 800 MHz systems on splinter channels |
| **900 MHz** | `900` | 900 MHz systems |
| **VHF / UHF (custom, OBT)** | `400_custom` | anything else, with the four numbers below |

The names are Trunk Recorder's, so a Trunk Recorder config's band plan carries over. If you aren't
sure between the 800 MHz plans, Find my system tells you; otherwise try `800_reband` first, and if
the recorder follows grants to frequencies where nothing is talking, try the others.

### Custom (OBT) band plans

On VHF and UHF systems, channel numbers map onto frequencies by a straight line that the system
operator chose. Four numbers describe it:

| Setup field | Key | Meaning |
|---|---|---|
| **Base, Hz** | `bandplanBaseHz` | the frequency of the offset channel |
| **Spacing, Hz** | `bandplanSpacingHz` | the step between channel numbers |
| **Offset (channel)** | `bandplanOffset` | the channel number the base frequency belongs to |
| **High, Hz** | `bandplanHighHz` | the highest outbound (repeater transmit) channel |

So channel *n* is at `base + spacing × (n − offset)`, for channels from the offset up to the high
frequency. Channel numbers below the offset are the radios' transmit frequencies, which the
recorder doesn't need. Motorola usually numbers outbound channels from 380.

WMATA's UHF system, for example:

```json
{
  "shortName": "wmata",
  "type": "smartnet",
  "controlChannelsHz": [496437500],
  "bandplan": "400_custom",
  "bandplanBaseHz": 489087500,
  "bandplanSpacingHz": 25000,
  "bandplanOffset": 380,
  "bandplanHighHz": 496612500
}
```

All four are needed: Setup won't start with one missing ("SmartNet band plan 400_custom needs its
base, spacing and high frequency (high above base) and offset").

If you don't know the numbers, run **Find my system** on the control channel. While it listens it
watches the spectrum: each time the control channel grants channel *n*, one carrier comes up
somewhere. A few of those, plus the control channel's own number and frequency, fix the line. It
also recognises when a system is really on one of the fixed 800 or 900 MHz plans. The band plan it
finds is shown under **Band plan** in its checklist and goes into the system it adds.

The recorder doesn't learn or save a SmartNet band plan while recording; it uses the one in the
config.

## Analog and digital voice

Each grant on the control channel says whether the call is analog FM or P25. The recorder opens an
FM receiver or a P25 receiver to match, and remembers each talkgroup's mode.

Some messages don't say: the short "update" messages repeated while a call goes on. If the
recorder starts while a call is already up, it only ever hears updates for it, and has to guess.
For a talkgroup it hasn't yet seen granted, it uses **Voice of unknown talkgroups**
(`defaultMode`):

- **P25** (the default; `defaultMode` left out or `"digital"`)
- **Analog FM** (`"defaultMode": "analog"`)

Set it to whatever most of the system's talkgroups are. On an all-analog system, set it to
**Analog FM**, or the first calls after each start will be recorded as P25 noise.

A talkgroup whose **Mode** in the talkgroup file starts with `A` (RadioReference's analog) is
always recorded as analog, whatever the grant says, as in Trunk Recorder.

Analog calls need no squelch setting: the recorder opens on a carrier well above the channel's
noise.

## Encrypted calls, patches, site lock

- **Encryption.** The control channel flags encrypted talkgroups. Those calls, and talkgroups
  marked `E`, `DE` or `TE` in the talkgroup file, are followed but not recorded unless
  `recordEncrypted` is on (config file only). Encryption found in the P25 voice partway through a
  call is left out of the audio. See [P25: Encrypted calls](p25.md#encrypted-calls).
- **Patches.** When dispatch patches talkgroups together, the control channel announces it, and
  the call goes out on the patch's *supergroup*, which usually isn't in your talkgroup file. A
  supergroup call is recorded while it's patched with a talkgroup you do have (even with
  **Talkgroups not in the talkgroup file** off), and the call JSON lists the patched talkgroups.
  The **Decode** page shows the patches standing now.
- **Site lock.** A SmartNet control channel announces a **System ID** (and on some VHF/UHF
  systems a **Site**). The **Site lock** fields on the system card take the System ID in hex. See
  [Several systems and sites](multi-site.md#site-lock).

## What the dashboard shows

The **Decode** page's card for a SmartNet system shows **Control messages** per second, **Control
lost** (OSWs that failed their checksum), **Deviation** (the control channel's tone spacing:
"Normal is about ±2.4 kHz; less means weak or filtered", with the carrier offset under it) and
**Voice lost** for its P25 calls. The badge reads **SmartNet · 2FSK**. The system's detail adds the
frequencies table, channels open now and patches.

On the **Calls** page, an analog call shows `FM` after its frequency.

## Diagnostic tools

`trunk-pro tool smartnet` decodes a SmartNet control channel from a capture and prints its
messages as JSON lines (talkgroup, frequency, source, analog, encrypted), then the number of good
and lost OSWs, the carrier offset, the deviation and the System ID:

```bash
trunk-pro capture wmata.cu8 --freq 496637500 --rate 1024000 --seconds 60
trunk-pro tool smartnet wmata.cu8 --center 496637500 --rate 1024000 --cc 496437500 \
  --bandplan 400_custom --bp-base 489087500 --bp-spacing 25000 --bp-offset 380 --bp-high 496612500
```

`--osw` also prints every OSW with the frequency the band plan gives its channel number
(`rx_hz`), which is the quickest way to check a band plan: granted frequencies should be ones the
system actually uses.

`trunk-pro replay` runs the whole recorder on a capture: add `--smartnet <band plan>` (and the
`--bp-*` options for `400_custom`) and `--analog-default` for analog. See
[Command line](../command-line.md).

## Common problems

**Calls are recorded on the wrong frequencies, or as silence.** The band plan is wrong. Check it
with `tool smartnet --osw`, or let Find my system work it out.

**Setup won't start: "unknown SmartNet band plan".** The `bandplan` value isn't one of
`800_standard`, `800_reband`, `800_splinter`, `900` or `400_custom`.

**Analog talkgroups come out as noise (or P25 ones as hiss).** Set **Voice of unknown
talkgroups** to match the system, or mark the analog talkgroups `A` in the talkgroup file.

**Low deviation, many lost OSWs.** The signal is weak or off frequency: check the antenna, gain and
ppm correction (see [Radios](radios.md)).

**Private calls aren't recorded.** SmartNet private and interconnect calls aren't recorded.
