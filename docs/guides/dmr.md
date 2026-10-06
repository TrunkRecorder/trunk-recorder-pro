# Trunked DMR

This page explains how Trunk Recorder Pro records trunked DMR sites: Motorola Capacity Plus,
Capacity Max and Connect Plus, and ETSI Tier III. It covers what to list, colour codes, slots,
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
- **Capacity Max, Connect Plus and Tier III** have a control channel that grants calls to a
  *logical channel number* (LCN), as P25 does.

Trunk Recorder Pro doesn't hunt for a control channel on DMR. It watches **every frequency you
list at the same time**, with one receiver each (a small fraction of a CPU core apiece), and
works out which kind of system it is by listening. The log says so: "This is a DMR Capacity Max
system".

## Setting it up

In **Setup** → **Systems**, **Add a system** and set **Type** to **DMR (trunked)**. The fields are:

- **Site frequencies, MHz** (`controlChannelsHz`): for Capacity Plus, **every repeater** of the
  site. For the others, the control channel(s).
- **Voice frequencies, MHz** (`dmrChannelsHz`): for Tier III, Capacity Max and Connect Plus, the
  site's voice frequencies. This is Trunk Recorder's `channels`.
- **Colour code** (`colorCode`): leave empty ("auto") to take the control channel's.
- **Channel table (optional)** (`lcnTableHz`): channel numbers you already know, as
  `101=452.275, 102=452.300`. This is Trunk Recorder's `lcnTable`. Leave it empty to learn them.
- **Site group**: see [Several sites](#several-sites).

In the config file (frequencies in Hz):

```json
{ "shortName": "capplus", "type": "dmr", "controlChannelsHz": [463375000, 463750000, 464350000] }
```

```json
{
  "shortName": "capmax",
  "type": "dmr",
  "controlChannelsHz": [452175000],
  "dmrChannelsHz": [452275000, 452300000],
  "lcnTableHz": { "101": 452275000 }
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

**Learning a channel.** When a grant names a channel it doesn't know, the recorder notes the
talkgroup and slot and waits up to 4 seconds. If that talkgroup's link control then shows up on
the same slot of one of the watched voice frequencies, that frequency is the channel. The log
says so ("Channel 101 is 452.27500 MHz (talkgroup 5 granted there came up on it)"). Learning
only works for frequencies you've listed, so list them all.

Learned channels are saved to `<shortName>.bandplan` in the folder the config file is in, and
loaded on the next start, so a known channel is followed from the first grant.

**Slots.** Each slot records its own call. File names end in `.0` or `.1`, and the call JSON has
`tdma_slot` and `color_code`.

**Private calls.** A slot's link control says whether the call is to a group or to one radio.
Radio-to-radio calls are recorded as unit-to-unit calls (on by default; **Unit-to-unit calls**
under **Call rules**).

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
out of the audio. A call that was all encrypted is kept, without audio, only when
`recordEncrypted` is `true` in the config file; there is no switch for it in Setup. The recorder
can't decrypt anything.

## Several sites

A DMR site doesn't announce which system it belongs to, so the recorder can't tell on its own that
two sites are the same system. If you record several sites of one system, give them the same
**Site group** name so a call heard on both is saved once:

```json
{ "shortName": "capmax-north", "type": "dmr", "siteGroup": "capmax", "controlChannelsHz": [452175000] },
{ "shortName": "capmax-south", "type": "dmr", "siteGroup": "capmax", "controlChannelsHz": [452525000] }
```

There is no site lock for DMR; colour codes keep sites apart. See
[Several systems and sites](multi-site.md).

## What the dashboard shows

The **Decode** page has a **DMR** panel for each site (in the system's detail):

- the kind of system ("DMR Capacity Max", or "type unknown" until it's clear), the colour code,
  for Capacity Plus the rest channel (`rest LSN 2 (463.3750)`), and **keyed** for restricted access;
- a table of every watched frequency: whether it carries control (**control**, or **rest** on
  Capacity Plus), its channel number and whether that was **(learned)**, and the call on
  **Slot 1** and **Slot 2** right now (talkgroup and radio);
- the channels in the table that aren't watched frequencies.

On the **Calls** page, a DMR call shows its slot (`s0` / `s1`) after the frequency.

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
