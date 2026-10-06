# Radios

This page explains how Trunk Recorder Pro uses your radios (it calls them *sources*): how much
spectrum each covers, where it is tuned, gain, frequency correction, and running several at once.
Installing drivers for USRP, Airspy and SoapySDR radios is covered in
[Other radios](../install/other-radios.md); every source key is listed in the
[configuration reference](../configuration/sources.md).

All of these settings are on the **Radios** tab of **Setup**, one card per source. **Add a
source** adds another; the buttons at the top of a card pick its kind: **RTL-SDR**, **USRP**,
**Airspy**, **SoapySDR** or **Capture file** (USRP, Airspy and SoapySDR only in the desktop app).

## What a source is

A source is one radio (or one capture file) delivering a slice of spectrum. Trunk Recorder Pro
doesn't tune a radio to each call. It tunes each radio once, to a center frequency, and the radio
hands over everything within its bandwidth. The recorder then picks out the control channel and
every voice channel from that stream in software, as many at once as there are calls.

So the questions for each radio are: how wide a slice does it deliver (sample rate), and where is
that slice (center frequency)?

## Sample rate and coverage

The sample rate, in millions of samples per second (MSPS), is how wide a slice the radio
delivers: at 2.4 MSPS it covers 2.4 MHz, from 1.2 MHz below the center to 1.2 MHz above.

Not all of that is usable. Every radio filters its band before sampling it, and near the edges
that filter rolls off and signals from outside the band fold back in. Trunk Recorder Pro leaves a
**guard band** unused at each edge (75 kHz by default, see [Guard band](#guard-band-and-roll-off)).
At 2.4 MSPS that leaves 2.25 MHz you can record from.

The sample rates offered for each kind:

| Kind | Offered | Notes |
|---|---|---|
| RTL-SDR | 2.4, 2.048, 2.56, 3.2, 1.92, 1.024 MSPS | 2.4 MSPS is the default |
| USRP | 2.4 to 20 MSPS, or any rate typed in | any rate its master clock divides to; default 8 MSPS |
| Airspy | 10, 6, 3, 2.5 MSPS | R2: 10 or 2.5; Mini: 6 or 3 |
| SoapySDR | 2 to 10 MSPS, or any rate typed in | one the device supports |

Wider costs more CPU, but not by much: see [CPU](#cpu). A system whose frequencies spread wider
than one radio can cover needs a wider radio or several radios.

## Center frequency and Auto

**Center frequency, MHz** is the middle of the radio's slice. Leave it blank and the recorder
places the radio itself (**Auto**); the hint under the field shows where it went
(`Auto: 858.3000 MHz`), or *Auto: no uncovered system to place over* when there's nothing left
for it to cover.

How Auto chooses, for each source in order whose center is blank:

1. It looks at what the sources before it (and any source with a center you typed) don't cover
   yet: each system whose control channels aren't in any radio's usable range yet, and each
   conventional system's channels.
2. It tries to fit, in one radio, all of those systems' channels: control channels and the voice
   channels it knows about (the survey saves the voice channels it saw). If they don't fit, it
   tries just their control channels; then all channels of the first uncovered system; then that
   system's control channels alone.
3. The center is the middle of what it's fitting, rounded to the kHz and then nudged in 12.5 kHz
   steps so that no channel sits within 25 kHz of it. Every channel must end up at least 10 kHz
   inside the usable range.

The nudge is there because most SDRs have a spike at their exact center frequency (the DC
offset), and a channel sitting on it decodes badly.

If nothing fits, Start is blocked with *Set a center frequency for source N — it couldn't be
placed automatically*. Then type a center yourself.

Auto is a good choice for a single radio on a single system. With several radios, or when you
want a radio over the busiest voice channels rather than in the middle, set the centers yourself;
Find my system and the setup guide both do this for you.

### The coverage bar

Under each source card is a bar showing its band. The highlighted middle part is the usable range
(the band less the guard band at each edge), with the range in MHz under it. Each system's
channels inside it are drawn as ticks in the system's colour: a tall tick for a control channel,
a short one for a known voice channel (conventional channels in plain text colour). The legend
names the systems with channels inside, or says *nothing in range*. Hover over a tick to see what
it is.

On the **Systems** tab, each system card says which source its control channel is on, and, when
the survey has found voice channels, how many are covered ("voice 14/16 covered").

## Guard band and roll-off

The **Guard band** is the part of each edge of a radio's band that is left unused, in kHz at each
edge. It defaults to 75 kHz, and can be set from 0 up to a quarter of the sample rate. A channel
inside the guard band isn't recorded from that radio.

Why have one: near the edges of the band the radio's anti-alias filter rolls off, and signals
just outside the band fold back over the edge. A wider guard band is safer; a narrower one covers
more. If you're coming from Trunk Recorder, its usable bandwidth was set by its own filter chain;
here it is this one number per source.

### Profile roll-off

**Profile roll-off…** measures where your radio's edges sag. It opens a panel:

1. Under **Look at**, type a frequency to tune the radio to. A quiet part of the spectrum is best,
   with the antenna connected and the gain as you'll record. (A capture file is looked at where it
   was recorded.)
2. Press **Measure**. The recorder opens the radio by itself for about 1.5 seconds, so recording
   has to be stopped first.
3. It shows a waterfall and the averaged spectrum, with the noise floor in blue and the guard band
   shaded at each edge. It reports how far in from the low and high edge the noise floor is 3 dB
   down, and how far down it is at the very edge.
4. It suggests a guard band: the larger of the two 3 dB points plus half a channel (7.5 kHz),
   rounded up to 5 kHz. Drag on the plot or use the slider to try other values; the channels your
   systems use are drawn as ticks, and any that would fall in the guard band are listed. Press
   **Use ... kHz** to keep it.

When you add a source, its card offers *New radio — measure its edge roll-off?* You can
**Skip** it.

Treat the suggestion as conservative. The roll-off turns signal and noise down alike, so a gentle
sag at the edges doesn't by itself cost decoding; the real problem is signals folding in from
outside the band, which the noise floor can't show. On an RTL-SDR at 2.4 MSPS the floor sags
slowly, so the suggestion comes out well over the 75 kHz default. If a channel near an edge
decodes fine at the default, there's no need to give it up.

The same measurement from the command line (an RTL-SDR, or a capture file), printed as JSON:

```bash
trunk-pro rolloff --serial 200 --center 858300000 --rate 2400000 --gain 28
trunk-pro rolloff capture.cu8 --center 858300000 --rate 2400000
```

`--full` adds the spectra and waterfall to the output. See [Command line](../command-line.md).

## Gain and AGC

Gain is how much the radio amplifies the signal before sampling it. Too little and weak control
channels don't decode; too much and strong signals (a nearby pager or broadcast transmitter, or
the system's own site) overload the radio, and everything gets worse. More gain also raises the
noise, so past a point it doesn't help.

Most kinds of radio have an **AGC** switch, which lets the radio set its own gain. A fixed gain
usually works better for trunked systems, so the default is AGC off (except on SoapySDR devices).

| Kind | Gain setting | Default |
|---|---|---|
| RTL-SDR | **Gain, dB**, 0 to 49.6 on most dongles, or **AGC** | 25.4 dB, AGC off |
| USRP | **Gain, dB** (B200/B210: 0 to 76), or **AGC** (B200 / B210 and E3xx only) | 40 dB, AGC off |
| Airspy | **Gain**: **Linearity** or **Sensitivity**, a step 0 to 21; or **Each stage**: **LNA** 0 to 14, **Mixer** and **VGA (IF)** 0 to 15, with **AGC** for the LNA and mixer | Linearity, step 14 |
| SoapySDR | **Gain, dB** overall (blank leaves it as the device has it), then **Gain stages, dB** one by one (HackRF LNA / VGA / AMP, SDRplay IFGR / RFGR...), or **AGC** | AGC on |

On an Airspy, **Linearity** is best near transmitters and **Sensitivity** for weak signals.

How to set it:

- **Let the survey do it.** On an RTL-SDR, Find my system tries gains from 19.7 to 49.6 dB while
  listening to the control channel and keeps the lowest one within 1 dB of the best signal that
  doesn't clip. See [Find my system](find-my-system.md#gain).
- **Watch the dashboard.** While recording, a source's line warns *the gain is too high* when more
  than 0.5 % of samples are at full scale, and *little headroom left* when peaks reach full scale.
  Lower it a few dB. The **RF** page shows each radio's headroom and noise floor over time.
- **Watch the control channel.** If it decodes most messages, more gain won't help. If it loses
  many, try a few dB more, or less if there are strong signals nearby.

If no gain gives a clean control channel, the trouble is usually elsewhere: an antenna that isn't
made for the band, a poor location, or a noisy USB hub or computer nearby.

## Frequency correction (ppm) and AutoTune

An SDR's tuning is set by a crystal, and cheap crystals are off by a few parts per million (ppm).
The error scales with frequency: 2 ppm is 1.7 kHz at 858 MHz. A narrow digital channel 2 kHz
off decodes badly or not at all, and crystals drift as they warm up.

**Frequency correction, ppm** tells the recorder how far off the radio is, so it can tune
correctly. On an RTL-SDR it's a whole number (the dongle takes whole ppm); other radios take
decimals.

There are three ways to find the number:

- **Find my system** measures it: the control channel announces its own frequency, and base
  stations are locked to GPS, so the difference between where your radio hears it and where it
  says it is is your radio's error. **Add this system** sets it.
- **While recording, Setup measures it.** Under the ppm field, a source card shows *Measured
  error: +1.40 ppm* once a control channel on it has been measured, and offers **Set correction
  to N ppm** when the error is 0.3 ppm or more. The error is measured whether AutoTune is on or
  not. The **RF** page shows it too, and the dashboard warns when a source is more than 2 ppm off
  with AutoTune off.
- **AutoTune** corrects for it automatically (see below).

### AutoTune

**AutoTune** follows crystal drift using the control channel. It works on P25 and SmartNet
control channels. Here is what it does:

- Every 10 seconds, while the control channel is decoding, it measures how far off the control
  channel comes in, as ppm, and keeps a running average of the last 20 measurements per source
  (measurements over 50 ppm are thrown away).
- With AutoTune on, every voice channel opened from then on is corrected by that average, on top
  of the ppm you set.
- When the correction has moved more than 150 Hz (at the control channel's frequency) from the
  one a P25 control channel was opened with, the control channel is reopened at the corrected
  frequency, at most once every 200 seconds. SmartNet control channels are measured but not
  reopened.

AutoTune corrects around the ppm you set, so it works best when the ppm is roughly right to start
with: a control channel that is too far off to decode can't be measured. Set the ppm (or let Find
my system set it), then turn on AutoTune to follow drift. While recording, the source card shows
how much AutoTune is correcting by (*corrected by +0.40 ppm*).

## Running several radios

More radios cover more spectrum: a second dongle can catch the voice channels the first can't
reach, or another system entirely.

### How channels are shared out

When a call comes in, its voice frequency is recorded from the **first source, in the order on
the Radios tab, whose usable range covers it**. Control channels are the same: each is opened on
the first source that covers it. Where two radios overlap, the one higher in the list gets the
channels in the overlap.

A call whose frequency no source covers isn't recorded; the **Calls** page says *outside tuned
range*, and the **Radio system** page counts these as *Out of band* (*No source covers the
frequency*). A system with no control channel inside any source can't start: *no control channel
falls inside a source's bandwidth — move the center frequency*.

### Placing several radios

- **Auto** places each radio over what the radios before it don't cover yet, one system per radio
  where they don't all fit (see [Center frequency and Auto](#center-frequency-and-auto)). It
  doesn't split one system's voice channels across radios.
- **The setup guide's Coverage step** does: the scanning radio covers the control channel, and
  each other radio goes over the busiest voice channels still out of reach, weighted by how many
  calls the scan saw on each.
- **By hand**, type each radio's center so their usable ranges sit side by side, and check the
  coverage bars. The **Decode** page will tell you when a frequency decodes worse because it is at
  the edge of a radio's band (*move the centre toward it*).

### Telling dongles apart

RTL-SDR dongles are told apart by their USB serial number. The **Dongle** list on a source card
shows each connected dongle as its product name and serial (`· SN 200`); one already used by
another source is marked *(in use)*. **First available** (an empty serial) opens the first
RTL-SDR found. **Refresh** looks again after you plug one in.

With more than one dongle, give each source a specific dongle, not **First available**: which
dongle is "first" can change from one start to the next.

Many dongles leave the factory with the same serial (often `00000001`), or none. Trunk Recorder
Pro can't tell those apart, and it has no way to change a dongle's serial. Use librtlsdr's
`rtl_eeprom` tool to give each one its own serial (for example `rtl_eeprom -d 0 -s 00000200` for
the first dongle), unplug and replug it, then pick it in Setup.

Airspys are picked by serial the same way. USRPs and SoapySDR devices are picked by their device
arguments (`serial=...`, `addr=192.168.10.2`, `driver=sdrplay,serial=...`); **Find** lists what's
connected.

`trunk-pro devices` lists the RTL-SDRs with their serials from a terminal.

## CPU

Trunk Recorder Pro splits each radio's band into channels once, with a shared FFT, so the cost
depends mostly on the sample rate, and very little on how many calls are being recorded. Measured
on live systems (see [Performance](../performance.md)):

| Computer | Radios | CPU, % of one core |
|---|---|---|
| Raspberry Pi 5 | one RTL-SDR at 2.4 MSPS (P25) | 4.2 % idle, 5.0 % with one call, 6.2 % with three |
| Mac mini, Apple M4 Pro | one USRP B200 at 8 MSPS (P25) | 11.7 % average |
| Mac mini, Apple M4 Pro | two RTL-SDRs at 2.4 MSPS each (SmartNet) | 7.6 % average |

Each extra call costs about 0.6 to 0.7 points. If a source drops samples (the dashboard says
*Dropping samples: the computer or USB bus can't keep up*), calls on it lose audio; check the
CPU load on the **Platform** page, and try another USB port or fewer dongles per hub.

## Capture files

A **Capture file** source replays a recording of raw IQ instead of a live radio. It's useful for
testing, for reporting a problem, or for trying settings on the same air again and again.

- **Capture file**: the path to the file on the recorder's computer (in the browser version,
  **Choose file...**, forgotten on reload).
- **Sample format**: **cu8** (`rtl_sdr`'s unsigned 8-bit), **cs16** (signed 16-bit) or **cf32**
  (32-bit float, as GNU Radio and UHD write). It's guessed from the file's extension: `.cf32`,
  `.cfile`, `.fc32`, `.complex` are cf32; `.cs16`, `.sc16` are cs16; anything else cu8. The browser
  version reads cu8 only.
- **Center frequency, MHz** and **Sample rate**: what the capture was recorded at. These must be
  right; the file doesn't say.
- **Real-time pace**: on, it plays back at the speed it was recorded; off, as fast as it decodes.

A capture can't be retuned, so Find my system looks at it once, at its center frequency.

To make a capture with an RTL-SDR:

```bash
trunk-pro capture dcfd.cu8 --freq 858300000 --rate 2400000 --gain 28 --serial 200 --seconds 30
```

Captures can also be replayed straight from the command line with `trunk-pro replay`, without
the interface. See [Command line](../command-line.md).
