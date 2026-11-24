# Trunked DMR

This page explains how Trunk Recorder Pro records trunked DMR sites: Motorola Capacity Plus,
Capacity Max and Connect Plus, ETSI Tier III, and [Hytera XPT](#hytera-xpt). It covers what to list, colour codes, slots,
how channel numbers are learned, and restricted-access systems. Every key is listed in the
[trunked systems reference](../configuration/systems.md#dmr). Conventional DMR channels are
covered in [Conventional channels](conventional.md).

## How DMR trunking differs

A DMR carrier is 12.5 kHz wide and carries two calls at once, one in each of its two time slots.
Each slot carries its call's *link control*: which talkgroup, which radio, and whether it is
encrypted. Sites keep apart from their neighbours with a **colour code** (0 to 15).

The trunked flavours differ in how a call gets its channel:

- **Capacity Plus** (and Linked Capacity Plus) has no control channel and no grants. Idle radios
  sit on a *rest channel*, which moves from repeater to repeater. A call starts on the rest
  channel and the rest channel moves on.
- **Hytera XPT** shares home/free repeaters without a dedicated control channel.
  See [Hytera XPT](#hytera-xpt).
- **Capacity Max, Connect Plus and Tier III** have a control channel that grants calls to a
  *logical channel number* (LCN), as P25 does.

Trunk Recorder Pro doesn't hunt for a control channel on DMR. It watches **every frequency you
list at the same time**, with one receiver each (a small fraction of a CPU core apiece), and
works out which kind of system it is by listening. The log says so: "This is a DMR Capacity Max
system".

## Setting it up

In **Setup** → **Systems**, **Add a system** and set **Type** to **DMR (trunked)**. Its site (under
**Sites**) has these fields:

- **Site frequencies, MHz** (`controlChannelsHz`): for Capacity Plus, **every repeater** of the
  site. For the others, the control channel(s).
- **Watched frequencies, MHz** (`dmrChannelsHz`): for Tier III, Capacity Max and Connect Plus,
  voice frequencies listened to all the time, as the site frequencies are. A call on one is found
  by its own link control, and a grant naming a channel the table doesn't have is learned from it.
  This is Trunk Recorder's `channels`. Each must be inside a source.
- **Voice channels, MHz** (`voiceChannelsHz`): optional. The radios are placed over these, and
  over the channel table's frequencies, but nothing listens to one until a grant names it. Use it
  to place a radio for a site's voice channels without watching them.
- **Colour code** (`colorCode`): leave empty ("auto") to take the control channel's.
- **Channel table** (`lcnTableHz`): channel numbers you already know, as
  `101=452.275, 102=452.300`. This is Trunk Recorder's `lcnTable`. Leave it empty to learn them.

In the config file (frequencies in Hz):

```json
{ "shortName": "capplus", "type": "dmr", "sites": [{ "controlChannelsHz": [463375000, 463750000, 464350000] }] }
```

```json
{
  "shortName": "capmax",
  "type": "dmr",
  "sites": [{
    "controlChannelsHz": [452175000],
    "dmrChannelsHz": [452275000, 452300000],
    "lcnTableHz": { "101": 452275000 }
  }]
}
```

**Find my system** finds trunked DMR sites too (tick the **Business UHF** and **Business VHF**
bands, which are off by default), with their kind and colour code. It adds the frequency it
found; then add the rest yourself: the other repeaters under **Site frequencies** for Capacity
Plus, or the voice frequencies under **Voice frequencies** for the others. See
[Find my system](find-my-system.md).

Every frequency listed, site and voice, has to be inside a source's band, or the recorder won't
start ("DMR frequencies outside every source's bandwidth: … MHz").

## How calls are followed

**Capacity Plus.** Every repeater is watched. A call is found by its own link control, on
whichever repeater and slot it turns up, and recorded from there. The rest channel is tracked
from the site status messages, so the dashboard can show it.

**Capacity Max, Connect Plus, Tier III.** The control channel grants a talkgroup to a logical
channel and slot. The recorder turns the channel number into a frequency from, in this order:

1. your **Channel table** (it always wins);
2. the grant itself, when it carries an absolute frequency, or the site's channel announcements
   (some Tier III systems);
3. what it has learned.

**System code.** Tier III and Capacity Max control channels send a 16-bit System Identity Code
(in C_ALOHA and C_BCAST). Its model (Tiny, Small, Large or Huge) sets how the rest splits into a
network and a site, which become the site's identity beside its colour code: a site can be locked
to them with `expect` (`{ "network": 12, "site": 4 }`). The Decode page's line under the graphs
shows them with the code, its model and which category of radio may use the site, for example
`B013`, Large, network 12, site 4, category A+B. The log notes it when first heard and whenever it
changes.

**Learning a channel.** When a grant names a channel it doesn't know, the recorder notes the
talkgroup and slot and waits up to 4 seconds. If that talkgroup's link control then shows up on
the same slot of one of the watched voice frequencies, that frequency is the channel. The log
says so ("Channel 101 is 452.27500 MHz (talkgroup 5 granted there came up on it)"). Learning
only works for frequencies you've listed, so list them all.

Learned channels are kept in the recorder's store (for each site) and loaded on the next start, so
a known channel is followed from the first grant.

**Slots.** Each slot records its own call. File names end in `.0` or `.1`, and the call JSON has
`tdma_slot` and `color_code`.

**Private calls.** A slot's link control says whether the call is to a group or to one radio.
Radio-to-radio calls are recorded as unit-to-unit calls (on by default; **Unit-to-unit calls**
under **Call rules**).

## Hytera XPT

XPT is received only: no transmitter, registration, affiliation or decryption is implemented.
Select **DMR (trunked)**. XPT uses the existing 4800-baud DMR receiver,
burst framing, FEC, two-slot transport, AMBE+2 decoder and recording pool.

XPT is set up as Capacity Plus is: there is no dedicated control
channel, so list every repeater of the site and give the recorder SDR coverage
of all of them. A repeater is a physical carrier with two slots; its **LCN
(repeater number, from 1) is not its logical slot number (LSN)**:
`LSN = 2 * (LCN - 1) + slot`, where the displayed slot is 1 or 2.

Example site settings within a system with `"type": "dmr"`:

```json
{
  "controlChannelsHz": [451000000],
  "dmrChannelsHz": [451012500],
  "lcnTableHz": {"1": 451000000, "2": 451012500}
}
```

`controlChannelsHz` and `dmrChannelsHz` together are the watched repeaters;
which one is listed as the control channel doesn't matter. `lcnTableHz` gives
repeater numbers; a repeater left out is learned when a call setup for it is
followed by that call's voice on a listed frequency. **Do not enter logical
slot numbers as repeater numbers.** Nothing is learned from status target
hints, because private-target hashes are not unique IDs.

Hytera permits different colour codes on the repeaters of one site. Leave
`colorCode` empty in that case: once XPT is identified, every listed frequency
is taken as the site's, whatever its code. A `colorCode` that is set still
filters every carrier. Do not put unrelated systems or different sites in one
site configuration; co-channel sites with identical colour codes cannot be
separated by the implemented signalling.

Both slots and several repeaters are decoded at the same time; the recording
pool and source bandwidth decide how many calls. Calls are opened and
refreshed from voice LC on their actual carrier and slot, so a target hash, a
data preamble or an unconfirmed setup is never recorded as a call.

The dashboard shows an XPT site as it shows Capacity Plus: the variant (Hytera XPT), the rest
channel in the line beside it (`rest LSN n (MHz)`), and each carrier's slots with their calls.
The free repeater's frequency carries the **rest** chip. Every
repeater with a free slot sends status, so the chip is on the free
repeater when its frequency is known, else on the one taken first while it keeps
sending. The message log has the deduplicated status lines.

What is decoded, what has been checked on real captures and what is still unverified is in
[Hytera XPT: protocol notes](../developer/protocols/xpt.md).

## Colour codes

The site's colour code is the one its control channel (for Capacity Plus, any listed repeater)
sends, unless you set **Colour code**. Anything on another colour code is ignored, such as
a neighbouring system that happens to use one of your listed frequencies. Until the colour code is
known, only the frequencies in **Site frequencies** count.

The dashboard flags a watched frequency heard on a different colour code with a chip (for example
**CC 3**).

## Restricted access (keyed checksums)

Some systems, Capacity Max ones often, run Motorola or Hytera *restricted access*: they scramble
the checksums of their control messages with a key, so a receiver without the key sees every
message fail its check. The recorder notices this (several messages in a row with perfect error
correction but a failed checksum) and from then on accepts messages on their error correction
alone. Nothing to configure. The dashboard marks the site **keyed**.

## Encryption

Encrypted transmissions (the link control's privacy bit, or a privacy header) are marked and left
out of the audio. A call that was all encrypted is kept, without audio, when
`recordEncrypted` is `true` in the config file (there is no switch for it in Setup); a call the
grant didn't mark encrypted is also kept when **Calls with no audio** is on. The recorder
can't decrypt anything.

## Over-the-air radio names

DMR talker aliases are decoded from embedded link control on trunked and
conventional channels. Supported forms are standard FID `0x00` and Hytera FID
`0x68` (FLCO `0x04`–`0x07`), and Motorola FID `0x10` (FLCO `0x14`–`0x17`).
The four encodings are 7-bit ASCII, ISO-8859-1, UTF-8 and UTF-16BE. Lengths
are code units, not displayed characters; multibyte characters and UTF-16
surrogate pairs may cross fragments.

Complete, valid aliases with fresh voice LC identifying the radio use the
existing learned-radio-name store and name-priority settings. Assembly is
independent for each carrier and slot. Missing/out-of-order/conflicting blocks,
new headers, source/talkgroup changes, matching voice termination, protected LC
and four-second timeouts discard incomplete state. An alias cannot establish
a call's identity or change its encryption state.

`tool dmr` emits `alias` JSON records with text, format, declared/received code
units and `complete` (its `lc` records name the radio). Valid partial Unicode
prefixes are shown without replacement characters; partial or unattributed
results never overwrite saved radio names. For example, the supplied Hytera
capture declares 13 UTF-16 units but only supplies six (`Hytera`) before its
alias header repeats. This remains a partial diagnostic, not a learned name.

No alias is invented when the feature is disabled or fragments are absent.
Clear alias LC can be decoded even when audio is encrypted; protected/encrypted
LC itself is not decrypted. Protocol layouts follow ETSI TS 102 361-2 V2.5.1
§§7.1.1.4–5 and 7.2.18–19.

## Several sites

A DMR site doesn't announce which system it belongs to, so the recorder can't tell on its own that
two sites are the same system. If you record several sites of one system, add each under the
system's **Sites** (**Add site**) so a call heard on both is saved once:

```json
{ "shortName": "capmax", "type": "dmr",
  "sites": [{ "name": "north", "controlChannelsHz": [452175000] },
            { "name": "south", "controlChannelsHz": [452525000] }] }
```

There is no **Site lock** field in Setup for DMR; colour codes keep sites apart. On a Tier III or
Capacity Max site you can also lock to its network and site by hand in the config (`expect`,
above). See
[Several systems and sites](multi-site.md).

## What the dashboard shows

The **Decode** page has a **DMR** panel for each site (in the system's detail):

- the kind of system ("DMR Capacity Max", or "type unknown" until it's clear), the colour code,
  for Capacity Plus the rest channel (`rest LSN 2 (463.3750)`), and **keyed** for restricted access;
- a table of every watched frequency: whether it carries control (**control**, or **rest** on
  Capacity Plus), its channel number and whether that was **(learned)**, and the call on
  **Slot 1** and **Slot 2** right now (talkgroup and radio);
- the channels in the table that aren't watched frequencies.

On a system's page under **Radio system**, a DMR call shows its slot (`s0` / `s1`) after the frequency.

## Diagnostic tools

```bash
# Every DMR carrier in a capture, with its colour code
trunk-pro tool dmrscan site.cu8 --center 463500000 --rate 2400000

# One carrier: link control, control messages and calls as JSON lines
trunk-pro tool dmr site.cu8 --center 463500000 --rate 2400000 --freq 463375000
#   --bursts         every burst
#   --audio out.f32 --slot 0|1   that slot's voice, as raw 8 kHz float samples

# The whole recorder on a capture
trunk-pro replay site.cu8 --center 452500000 --rate 2400000 --cc 452175000 --dmr-trunk \
  --dmr-channels 452275000,452300000 --lcn 101=452275000 --out calls
```

`replay` also takes `--color-code N`. `tool dmrscan` takes `--seconds N` to look at only the start
of a capture. See [Command line](../command-line.md) for all flags.

## Common problems

**Grants for channels it never learns.** The frequency the channel is on isn't in **Voice
frequencies**, or isn't inside a source. List every voice frequency of the site (RadioReference
often has them), or fill in the **Channel table**.

**Capacity Plus calls missing.** Every repeater has to be listed under **Site frequencies**; a
call on an unlisted repeater isn't heard.

**Nothing decodes after adding a site from Find my system.** Check the colour code it found, and
that the other frequencies have been added.

**Calls from another system.** Set **Colour code** to the site's, so a neighbour on a shared
frequency is ignored.


## GPS positions and data services

Standard DMR and Hytera GPS Info embedded in voice calls are decoded for the
radio named by the current slot's voice link control. Positions use the existing
call JSON fields `srcList[].lat` and `srcList[].lon`, as P25 positions do; no
extra setting is required. This adds DMR decoding to the existing position
recording path rather than introducing another JSON format.

On Hytera systems with a dedicated data carrier, add that frequency to the
site's monitored frequencies (or a conventional DMR channel). CRC-checked
compressed GPS reports can then emit positions without a voice call. Matching
active calls retain the position on the radio's source entry. A standalone
report produces a position event/log entry and does not create a call JSON or
audio recording by itself. RRS registration and the observed data-channel
setup/end/status profiles are also exposed in diagnostic logs.

Compressed GPS and busy indications remain experimental for the observed
profiles. Other hemisphere/fix flags, motion units and unidentified control
fields stay raw. See [DMR GPS and data protocol notes](../developer/protocols/dmr-gps.md)
for supported formats, limitations and validation evidence.
