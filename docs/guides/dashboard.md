# The dashboard

The dashboard pages show how the recorder is doing: whether each system's control channel is
decoding as usual, how well the radios hear the air, how clean the voice is, what the system is
doing, and whether the computer and plugins are keeping up. This page goes through each one and
what its numbers mean. For the frame around them, Setup and the Listen and Live pages, see
[The web interface](interface.md).

Most numbers come with a small chart of the last 10 minutes, and many pages have an **Over time**
section with a **Span** of **1 h**, **24 h** or **7 days**. The history survives restarts (see
[Where the history is kept](interface.md#where-the-history-is-kept)). While stopped, the pages
show past data where they can.

Several numbers are compared with **the usual**: that series' average over the last 24 hours.
The shaded band on a chart is its usual range, and an arrow shows how far today is above or below
it. This is how the dashboard tells "the system is quiet tonight" from "reception got worse".

## Overview

While recording, the top row is today's big numbers ("today" is since local midnight, across
restarts):

| Number | What it is |
|---|---|
| **Calls today** | Calls saved today, with how many an hour right now and the last hour against the usual |
| **Airtime today** | Total length of today's calls, and how many are on the air at once on average |
| **Audio today** | Size of today's audio, and how much an hour right now |
| **Recorders** | Calls being recorded now, of **Recorders** in Setup, and how many channels are open. Amber when they're all busy. |
| **Missed, 24 h** | Calls not recorded because no recorder was free or no source covered the frequency. Click to see the Radio system page. |
| **Decoding load** | How much of one CPU core decoding takes. Over 100% means it's using more than one core, which is fine on a multi-core computer. |

Below that is a card for each system. Click its name to go to its Decode page.

- **Control channel** (trunked systems): control messages decoded a second, against the usual;
  the share decoded; the control channel's frequency (or `hunting` while it looks for one); and
  its SNR.
- **calls/min**, **on the air** now, **voice lost** (the share of voice frames that couldn't be
  decoded over the last 10 minutes; under 2% is clean), and **new TGs** (first heard in the last
  24 hours).
- Chips for the talkgroups on the air; click one for its page.
- **voice quality by frequency, 24 h**: one cell per voice channel, darker where more voice was
  lost. A dark cell or two at one end often means a source's band edge.

On the right are the **Sources** (as on the RF page, smaller), then **Status**: a **Computer**
line (CPU, memory, disk, RAM spool, internet), a **Plugins** line (sent, failed, queued for each
plugin that's on), and **Lately**, the event feed (see [Events](interface.md#events)).

## RF

How well each radio hears the air. While stopped, it lists your sources' settings.

**The band** draws every source's passband on one axis, with the control channels, conventional
channels and calls (recording and followed-only) marked on it. The darker part of each passband
is what the source actually uses, its sample rate less the guard band at each edge. Use it to see whether your
sources cover what they should.

Each source then has a card:

| Number | What it means |
|---|---|
| **Noise floor**, dBFS | The median power across the band. It rises with gain, and with interference. A sudden rise with no gain change means something nearby started making noise. |
| **Headroom**, dB | How far the strongest signals are below full scale (the point where the radio clips). Under the meter: the share of samples clipping, if any. |
| **Frequency error**, ppm | How far off the radio's frequency is, measured against the control channels (the towers' frequencies are very accurate). With AutoTune on it's corrected as it's measured; otherwise it says `not corrected (AutoTune off)`. It needs a control channel to measure. Steady: set the source's ppm. Drifting: temperature. See [Radios](radios.md). |
| **Dropped**, samples/s | Samples the driver lost because the computer or the USB bus didn't keep up. Anything but zero is red: calls on that source lose audio. |

Under the card: the gain as set, the sample rate actually delivered against the one asked for,
and the source's errors, if any (a USB error, a dongle that went away).

The **Waterfall** beside each source shows its whole band over time, with the control channels
and active calls marked. **Show** turns it off; the browser remembers that, and the recorder
stops sending that spectrum.

Click a source's name for its own page, which adds:

- **Across the band**: the noise floor across the band (bars) and each channel's power above it
  (stems). 10 dB or more above the floor is clean. A floor that sags at the edges is the radio's
  roll-off; keep channels out of it (see the guard band in [Radios](radios.md)).
- **Channels on this source**: each channel's power, floor, **SNR** (green at 15 dB or more, amber
  from 8, red below), **Offset** (how far its carrier is from where it should be, in Hz) and
  **Quality** (see [Decode](#decode)). If every carrier is off by about the same ppm, it says so:
  set the source's ppm or turn on AutoTune.
- **Over time**: noise floor and peaks, frequency error (measured and corrected), dropped samples,
  clipping.

## Decode

How well signals turn into messages and voice. A card per system:

| Number | What it means |
|---|---|
| **Control messages** /s | Control channel messages decoded a second, against the usual. How many there should be depends on the system: compare with its own usual. |
| **Control lost** % | Messages that didn't decode. A few percent is normal; amber over 10%, red over 30%. |
| **Eye opening** | C4FM (P25 Phase 1 and most control channels): how well separated the four symbol levels are. 10 or more is clean, under 4 means errors, about 1 is noise. Simulcast systems read low even when they decode well. |
| **Phase error**, ° rms | CQPSK (simulcast P25, and Phase 2): how far symbols land from where they should. Under 10° is clean, amber from 15°, red from 20°, about 25° is noise. |
| **Deviation**, Hz | SmartNet: how far the signal swings. About ±2.4 kHz is normal; less means weak or filtered. |
| **carrier** (under these) | The control channel's carrier offset in Hz. A steady offset is the radio's frequency error. |
| **Framing**, % missed | P25: frames whose header (NID) failed or whose sync was missed, and how often the equaliser reset a minute |
| **Voice lost**, % of frames | Voice frames too damaged to use, over 10 minutes. Under 2% is clean, amber from 2%, red from 8%. Under it: bit errors the error correction fixed per frame. |

Each card also has a **voice lost by frequency, 24 h** strip.

Click a system's name for its own page, which adds:

- its identity (NAC, WACN, SysID, site) and calls saved this run;
- **Channels now**: each open channel's SNR, offset and quality;
- **Over time**: decoded and lost control messages, voice frames lost, SNR with eye opening or
  phase error, carrier offset;
- **Frequencies, 24 h** (or 7 days): every voice frequency with calls, its source, **In band**
  (0% at the edge of the source's usable band, guard band left out, 100% at its centre), calls, SNR, offset, the share of voice
  frames decoded cleanly and lost, and an hour-by-hour strip. Above the table the page points out
  patterns: every frequency off by the same ppm, a frequency at a band edge decoding worse ("move
  the centre toward it"), or one frequency much worse than the rest (interference or a weak
  path);
- neighbouring sites, patches, and for DMR and NXDN the site's channels.

## Radio system

The system as heard. With several systems it first shows a card each (talkgroups and radios in
24 hours, new talkgroups, talkgroups not in its file, the busiest talkgroups, what's on the air);
**Open <system> ▸** goes to one. The **Span** is 24 hours or 7 days.

| Number | What it is |
|---|---|
| **Calls**, **Airtime** | In the span, with the average call length |
| **Talkgroups heard** | With how many are in your talkgroup file |
| **Radios heard** | With how many in the last hour |
| **New talkgroups** | First heard in the last 24 hours (`learning (first hour)` while it builds a baseline) |
| **Unknown** | Heard in 24 hours but not in the talkgroup file |

**What happened to calls** splits the calls by what became of them: **Recorded**, **Followed
only**, **Ignored**, **Encrypted**, **Not in the file**, **No recorder** and **Out of band**. The
last two are calls you missed; see the reasons table on [the Live page](interface.md#active-calls).

Then: **On the air now**, **Busiest … by hour**, **Who talks on what** (talkgroups and the
busiest radios, linked by how often each radio talks on each), the **Talkgroups** table, the
**Radios** table, and **Call lengths**.

The **Talkgroups** table can show **All**, **New**, **Unknown**, **Ignored** or **Encrypted** (more
than half their calls encrypted), sorted by **Airtime**, **Calls**, **Latest** or **Newest**. The
**Ignore** button stops recording a talkgroup by marking it Ignore in the system's talkgroup file;
it takes effect at once, even while recording. Click it again (**Ignored ✕**) to record it again.

Click a talkgroup or a radio anywhere for its own page: its last 7 days, who talks on it (or what
it talks on), what's affiliated with it, and its latest transmissions.

## Plugins

The **Health** tab has a card for each plugin that's on (**Install & settings** is covered in
[Plugins](../plugins/README.md)):

| Number | What it is |
|---|---|
| **Sent** | Calls it handled this run, and how many it skipped |
| **Failed** | Calls it failed on, as a share of tries. Hover for the last failure. |
| **Waiting** | Calls queued for it (amber over 20), and how many are waiting to retry |
| **Upload time** | How long its uploads take, and how much it has sent |

Under them: a bar per minute for the last hour (sent, skipped, failed), each service the plugin
talks to with its state, any figures of its own the plugin reports (simplestream's packets sent and
dropped, for example), and when it last succeeded and failed. **Restarted N×** means the plugin
crashed and was restarted; **Fell behind N events** means it was too slow to take events and some
were dropped. **Last 24 h and log ▾** shows its history and recent log lines.

A service shows as down after 3 failures in a row, or 5 minutes without a success.

## Platform

The computer the recorder runs on, recording or not:

| Number | What it is |
|---|---|
| **CPU** | All cores together (of the container's limit, in a container). Amber over 90%. |
| **Trunk Recorder Pro** | The app's own CPU use, in cores, and its memory |
| **Memory** | Memory in use, and how much of the time programs stall waiting for memory (Linux) or the system's memory pressure (macOS) |
| **Internet** | Whether the internet checks answer, and how long a connection takes |
| **Temperature** | The hottest sensor, and whether a Raspberry Pi is throttling (under-voltage or heat) |

**Over time** has CPU (with each core) and memory. **Disks** shows the **Recordings** disk, the
**App data** disk and the **RAM spool** if it's on, with free space, calls that overflowed from
the spool, and **About … of recordings left** at last week's daily average. **Network** shows each
internet check (`monitor.probeHosts`, by default `1.1.1.1:443` and `dns.google:443`), and **This
computer** the system, cores, uptime and anything the platform can't measure.

On macOS, if Spotlight is indexing the recordings folder, a note at the top explains how to
exclude it. See [Troubleshooting](../troubleshooting.md#spotlight).
