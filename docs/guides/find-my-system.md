# Find My System

Find my system scans the air for trunked control channels, listens to the best one, and sets the
system up from what it hears. This page explains how the scan works, what it reports, the buttons
in the interface, and the `trunk-pro survey` command.

You don't need to know any frequencies to use it. It finds P25, SmartNet, trunked DMR and NXDN
control channels. For P25 and SmartNet it goes further: it listens to the control channel and
learns the system's identity, band plan, alternate control channels, neighbouring sites and voice
channels, and measures your radio's frequency error (and on an RTL-SDR, the best gain).

## Where to find it

- On **Setup**, **Systems** tab: the **Find my system** panel at the top. It is open when no
  systems are set up yet; otherwise press **Show**.
- In the setup guide's **Find a system** step, which runs the same scan with fewer choices. See
  [Getting started](../getting-started.md#first-launch-the-setup-guide).

Recording has to be stopped to scan (*Stop recording to scan.*): the scan needs the radio to
itself, to retune it.

## Running a scan

The panel's options:

- **Radio**: which source scans, when you have more than one.
- **Find the best gain** (RTL-SDR only; adds about 15 seconds): try a range of gains once the
  control channel is found. See [Gain](#gain).
- **Bands to scan**: tick the bands to sweep. See [Bands](#bands).

Press **Scan**. While it sweeps, the panel shows *Scanning 800 MHz around 852.008 MHz* and *step 3
of 44*. Once it has found a control channel and is listening to it, the panel shows a waterfall of
what the radio hears, a checklist of what it has learned, and the signals it found. **Stop** ends
it; **Scan again** starts over; **Close** puts the panel away when it's done.

USRP, Airspy and SoapySDR radios can scan in the desktop app only. A capture file source can't be
retuned, so it is looked at once, at its center frequency (*Scanned at its center frequency*);
set the capture's center first.

## Bands

| Band | Range, MHz | Scanned by default |
|---|---|---|
| 800 MHz | 851–869 | yes |
| 700 MHz | 769–775 | yes |
| 900 MHz | 935–941 | yes |
| UHF | 450–470 | yes |
| VHF | 136–174 | yes |
| UHF federal | 380–420 | no |
| Business UHF | 451–470 | no |
| Business VHF | 150.8–174 | no |
| T-band | 470–512 | no |

These are the base stations' transmit frequencies, where control channels are. Bands are scanned
in that order, so the most common P25 bands come first. Business UHF and VHF lie inside UHF and
VHF; they're listed separately so you can scan just them (where DMR and NXDN systems mostly are).
Where ticked bands overlap, the overlap is scanned once. Bands outside the radio's tuning range
are skipped; if none is left, the scan stops with *None of the chosen bands is in this radio's
tuning range.*

## How it finds control channels

A control channel never stops transmitting. Voice channels key up and down; a control channel
is a carrier that is always there. The scan uses that.

The radio steps across each band. Each step uses 85 % of the radio's bandwidth (the edges roll
off), and steps overlap by 25 kHz, so an RTL-SDR at 2.4 MSPS moves about 2 MHz at a time. At each
step:

1. **Settle**: the first samples after retuning are thrown away (30 ms on an RTL-SDR, 150 ms on a
   USRP, 200 ms on an Airspy or SoapySDR device).
2. **Look** for 0.35 seconds. For each ~1 kHz slice of the spectrum it takes a low percentile of
   the power over that time, so only carriers that stayed on the whole time stand out. A peak at
   least 4 dB over the local noise floor and 3.5 to 30 kHz wide is a candidate, up to 16 per step.
3. **Decode** each candidate for 0.9 seconds with every receiver at once: P25 (C4FM and CQPSK),
   SmartNet, DMR, and NXDN at both 4800 and 9600 baud. A step with no candidates skips this.

So a step takes up to about 1.3 seconds, and the default bands take about a minute with an
RTL-SDR at 2.4 MSPS.

What each candidate is called, from what decoded:

| Shown as | Means |
|---|---|
| **Control channel** | P25 control channel: at least 2 control messages (TSBKs) passed their CRC |
| **SmartNet control channel** | at least 3 good SmartNet control words (OSWs) |
| **DMR control / rest channel** | DMR bursts whose control blocks say which kind of trunking it is |
| **NXDN control channel** | an NXDN Type-C control channel: at least 2 good control messages (CAC) |
| **P25 (voice / data)** | P25 frames, but no control messages: a voice channel or conventional P25 |
| **DMR (conventional / data)** | DMR bursts with no trunking control blocks |
| **NXDN (conventional / voice)** | NXDN frames but no control channel |
| **Other** | a continuous signal none of the receivers decoded |

When the sweep is done, it picks the P25 or SmartNet control channel with the best signal (signal
to noise, plus a bonus for the share of messages that decoded) and listens to it. It tunes the
radio a little to one side of it (a fifth of the sample rate, at most 500 kHz) to keep it off the
radio's center spike.

Trunked DMR and NXDN sites are found and can be added, but aren't listened to further: no
identity beyond what the scan saw, no frequency correction, no gain search.

If there's no P25 or SmartNet control channel, the scan ends with one of:

- *Found 1 trunked DMR control / rest channel(s) — add one below. No P25 or SmartNet control
  channel.*
- *P25, DMR or NXDN signals were found, but no control channel. Scan again, or add more bands.*
- *No P25 or SmartNet control channel found. Check the antenna and gain, and add more bands.*

## What it reports

While listening, the checklist fills in:

- **Control channel**: *decoding 98 % of 412 messages*, the modulation (C4FM, CQPSK, or 2FSK for
  SmartNet) and the signal to noise ratio.
- **System**: for P25, the WACN, System ID and NAC, and the RFSS and site number. For SmartNet,
  the System ID.
- **Band plan**: for P25, how many channel tables (IDEN messages) it has heard; the control
  channel broadcasts these, and they turn channel numbers into frequencies. For SmartNet, see
  [SmartNet band plans](#smartnet-band-plans).
- **Frequency correction**: *heard 1650 Hz high → -1.92 ppm*, with the whole number an RTL-SDR
  takes. See [Frequency correction](#frequency-correction).
- **Gain**: *trying settings...*, then the gain it chose (*28.0 dB (best of 7)*), or *no clear
  best; unchanged*.
- **Alternate control channels** the site announces.
- **Neighbouring sites** it announces (P25), each with its site number and control channel, and
  an **Add** button.
- **Voice channels in use**: every voice frequency it has seen a call granted on, noting when
  Phase 2 TDMA channels are among them. This list grows as people talk, so listening longer
  catches more.

Under **Signals found** is the list from the sweep: frequency (corrected by the measured ppm once
known; hover for the uncorrected one), signal strength, what it is, its identity (NAC / System ID
/ site, DMR colour code, NXDN RAN), and the share of messages decoded. **Show N other signals**
adds the ones nothing decoded.

### Frequency correction

The control channel announces its own frequency, and base stations are locked to GPS or a
rubidium clock, far better than any SDR's crystal. So the difference between where your radio
hears the control channel and where it says it is, is your radio's error. The scan measures the
carrier's offset over at least a second, re-centring its receiver on the carrier (up to three
times) if it was more than 250 Hz off, and reports the total correction to set: what the radio
already had plus what it measured.

For SmartNet the "announced" frequency comes from the band plan it learned. On a VHF/UHF (OBT)
plan, the control channel is assumed to sit on the 6.25 kHz channel raster, so the measurement is
only right if your radio is less than about 3 kHz off to begin with.

### Gain

On an RTL-SDR with **Find the best gain** on, once the control channel has decoded at least 20
messages and the scan has listened for 4 seconds, it steps through 19.7, 28.0, 33.8, 38.6, 42.1,
44.5 and 49.6 dB. At each gain it waits 0.3 seconds, then measures for 1.5 seconds: the signal to
noise ratio, the share of control messages that decoded, and how many samples hit full scale.

It keeps the **lowest gain within 1 dB of the best signal to noise ratio** that decodes about as
well as the best one, counting a gain that clips as 10 dB worse. Less gain leaves more headroom
for strong signals nearby, so it doesn't pick more than it needs. If nothing stands out, it leaves
the gain alone.

Without it, the scan uses the source's gain as set on the **Radios** tab.

## SmartNet band plans

A P25 control channel broadcasts its band plan. A SmartNet control channel doesn't: it names
channels only by number, and on VHF and UHF systems (OBT, "off band trunking") nothing on the air
says which frequency a number is. Trunk Recorder Pro works it out by watching.

While the control channel grants channel number *n*, one carrier in the spectrum is up that is
down when *n* is idle. For each channel number granted, the scan compares the spectrum while it
is granted with the spectrum while it isn't; the slice that rises at least 6 dB is where that
channel is. A few such channels, plus the number the control channel broadcasts for itself (at
the frequency it's heard), fix a straight line from channel number to frequency. The spacing is
snapped to a standard channel step.

If the line matches one of the standard plans (800 MHz standard, rebanded or splinter, or 900
MHz), it uses that. Otherwise it's an OBT plan, in Trunk Recorder's `400_custom` terms: a base
frequency, a spacing and the channel number the base belongs to (Motorola numbers OBT outbound
channels from 380), plus the highest channel. The checklist shows it as *learning — 2 of 5
granted channels located (this is channel 412)*, then for example *400_custom: channel 380 =
489.0875 MHz, 25 kHz steps*.

Once the plan is known, the alternate control channels and every voice channel granted get a
frequency, including ones outside the radio's view. It needs normal traffic: on WMATA, a UHF OBT
system, it takes a minute or so. A quiet system takes longer.

See [SmartNet](smartnet.md) for what the band plan settings mean.

## Adding the system

When the control channel has announced its own frequency, a **Ready to record** banner says what
it will set. Until everything is measured it also says *Still measuring...*; you can add the
system then, but waiting until the checklist is done gets you the band plan, correction and gain.

The drop-down next to the button chooses **as a new system** or **replacing** one already set up
(it picks the system that already has one of these control channels, if any). The button reads
**Add this system** or **Update it**.

It sets, on the system:

- the type (P25, or SmartNet with the learned band plan);
- the control channels: the one it announced for itself first, then the alternates;
- a site lock with the identity it announced (NAC, WACN, System ID, RFSS, site), so a control
  channel that turns out to be another system's isn't followed. See [P25](p25.md) and
  [Multi-site](multi-site.md);
- the voice channels it saw (used to place radios; shown on the system card as *voice 14/16
  covered*).

A new system gets a short name made from its identity, and if another site of the same system is
already set up, its talkgroup list.

And on the radio that scanned:

- **Frequency correction, ppm** (rounded to a whole number on an RTL-SDR);
- on an RTL-SDR, the gain it found, with AGC off;
- the center frequency (see below).

### Center frequency choice

The scan picks a center that keeps the control channel in range and covers as many of the voice
channels it saw as one radio can (then as many of the other control channels), off the radio's
center spike and inside its guard band. The banner says *Covers all 16 voice channels seen*, or
*Covers 11 of 16 voice channels seen; the system spans 4.2 MHz* when one radio can't cover the
system; then a second radio would catch the rest (see [Radios](radios.md#running-several-radios)).
With no calls seen yet, it centres on the control channel.

It won't move the radio if that would leave another system with no source covering its control
channel. Then the message says *source 1 unchanged (in use) — needs a source at 852.3000 MHz*,
and you add a radio there yourself.

### Listen and Add

In the **Signals found** list:

- **Listen** (P25 and SmartNet control channels) switches to listening to that one instead of the
  one the scan picked.
- **Add** on a P25 control channel adds it as a system straight away, with the other control
  channels the scan found for the same site (same NAC, System ID, RFSS and site) and a site lock.
  Frequencies are rounded to the 6.25 kHz channel raster. It doesn't set the radio.
- **Add** on a DMR control / rest channel adds a trunked DMR system with its colour code. For
  Capacity Plus, add the site's other repeaters under **Site frequencies**; for the others, the
  voice channels under **Voice frequencies**. See [DMR](dmr.md).
- **Add** on an NXDN control channel adds an NXDN Type-C system with its RAN; add its channel
  table or voice frequencies next. On an NXDN conventional signal it adds a conventional channel.
  See [NXDN](nxdn.md).
- A signal already in a system shows *in* and that system's name instead.

**Add** on a neighbouring site adds it as a system of its own, locked to that site. It needs a
source that covers its control channel.

You can still edit everything by hand afterwards on the **Systems** and **Radios** tabs.

## From the command line

`trunk-pro survey` runs the same scan with an RTL-SDR, or looks at a capture file, and prints
what it finds as JSON lines. It doesn't change your config.

```bash
trunk-pro survey --serial 200 --bands 800,700 --seconds 30
trunk-pro survey --serial 91 --bands t-band --seconds 120
trunk-pro survey capture.cu8 --center 858300000 --rate 2400000
```

| Flag | Default | Description |
|---|---|---|
| `<capture>` | | Look at this capture file once, instead of scanning with a dongle |
| `--serial S` | first RTL-SDR | The RTL-SDR to use, by serial number |
| `--rate Hz` | `2400000` | Sample rate (of the dongle, or the capture) |
| `--bands list` | `800,700,900,uhf,vhf` | Bands to scan, by id: `800`, `700`, `900`, `uhf`, `vhf`, `uhf-fed`, `biz-uhf`, `biz-vhf`, `t-band` |
| `--gain dB` | | A fixed gain; turns off the gain search |
| `--no-gain` | | No gain search (the dongle scans on its AGC) |
| `--ppm N` | `0` | The dongle's correction to start from, whole ppm |
| `--seconds N` | `30` | How long to listen once a control channel is found |
| `--center Hz` | | A capture's center frequency (required for a capture) |
| `--format cu8\|cs16\|cf32` | from the extension | A capture's sample format |

Without `--gain`, the dongle scans with its AGC on, then the gain search tries fixed gains while
listening. Values can also be written `--rate=2048000`.

Progress goes to stderr (`survey: scanning`, `survey: monitoring`, source errors). Stdout gets one
`{"found": {...}}` line per signal as it's found, with its `freqHz`, `correctedHz`, `band`,
`snrDb`, `widthHz`, `kind` (`control`, `smartnet`, `dmrControl`, `nxdnControl`, `p25`, `dmr`,
`nxdn`, `other`), `modulation`, `identity` and decode counts. The last line holds the system:

```json
{"monitor": {...}, "suggest": {...}, "message": "", "wallS": 94.2}
```

`monitor` has everything the checklist shows. `suggest` is what **Add this system** would set:
`type`, `controlChannels`, `bandplan` (SmartNet), `ppm` and `ppmApply`, `gainDb`, `centerHz`,
`voiceChannels`, `voiceCovered` / `voiceTotal`, `spanHz`, and `nac`, `sysId`, `wacn`, `rfss`,
`site`.

## When it finds nothing

- **Check the antenna and the radio.** The scan only sees what the radio hears. An antenna made
  for one band can be deaf on another; indoors, move it near a window or higher. Make sure no
  other program has the radio.
- **Check the gain.** The scan sweeps with the source's gain as set on the **Radios** tab. Far too
  little and weak control channels never stand out; far too much and strong signals swamp the
  rest. Try the default, then a few dB either way.
- **Add more bands.** Federal systems are often in **UHF federal**, some city systems in
  **T-band**, many DMR and NXDN systems in the business bands. None of those are scanned by
  default.
- **Scan again.** A busy step, or a fade, can hide a control channel for a moment.
- **Look it up.** Find the control channel on [RadioReference](https://www.radioreference.com/db/)
  and enter it by hand: **Add a system** on the **Systems** tab, or **I know the control channel**
  in the setup guide. See [Getting started](../getting-started.md#entering-a-system-by-hand).
- **Found it but can't add it.** The banner only appears once the control channel has announced
  its own frequency (and, for SmartNet, the band plan has been worked out from a few calls). Give
  it longer, or pick a stronger control channel with **Listen**.
- **Not every system can be found.** A control channel that decodes too poorly to pass a few CRCs
  in 0.9 seconds shows as *P25 (voice / data)* or *Other*. Improve reception, or enter it by hand.

Messages you may see with a capture file: *Set the capture's center frequency first.*, *The
capture ended before the scan finished.*, and *... MHz is outside the capture.* A source whose
sample rate is under 240 kHz can't scan (*The source's sample rate is too low to scan.*).
