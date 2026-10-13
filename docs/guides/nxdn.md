# Trunked NXDN

This page explains how Trunk Recorder Pro records trunked NXDN sites (Kenwood NEXEDGE, Icom
IDAS): the two rates, Type-C and Type-D trunking, RANs, and how channel numbers are found. Every
key is listed in the [trunked systems reference](../configuration/systems.md#nxdn).
Conventional NXDN channels are covered in [Conventional channels](conventional.md).

> **Not yet tried on a live system.** NXDN support was released in v0.1.4, but there is no NXDN
> system near the developer. It has been tested on recordings (the sigidwiki NXDN48 and NXDN96
> IQ files) and on synthesized control and traffic channels only. If you have a real NXDN system,
> reports and captures are very welcome; see [Diagnostic tools](#diagnostic-tools) for what to
> capture.

## What it handles

NXDN comes at two rates, and both are decoded:

| Rate | Setup | `nxdnRate` | Channel width |
|---|---|---|---|
| NXDN48 (4800 bps) | **NXDN48 (6.25 kHz)** | `nxdn48` (default) | 6.25 kHz |
| NXDN96 (9600 bps) | **NXDN96 (12.5 kHz)** | `nxdn96` | 12.5 kHz |

Voice is AMBE+2, as on DMR. There are two kinds of trunking:

- **Type-C** (NEXEDGE, IDAS Type-C): a control channel assigns each call to a channel number,
  much like P25.
- **Type-D** (IDAS distributed trunking): no control channel. Each repeater says in its own
  signalling when it is idle and who is talking on it.

As with DMR, the recorder doesn't hunt for a control channel: it watches every frequency you list
at once, one receiver each.

## Setting it up

In **Setup** → **Systems**, **Add a system** and set **Type** to **NXDN (trunked)**. The fields
are:

- **NXDN type** (`nxdnType`): **Type-C (control channel)** (the default) or **Type-D / IDAS (no
  control channel)** (`"typeD"`).
- **Control channels, MHz** (Type-C) or **Repeaters, MHz** (Type-D) (`controlChannelsHz`): the
  control channel, or every repeater of the site.
- **Rate**: the control channel's rate (Type-D: the repeaters').
- **RAN** (`ran`, 0 to 63): leave empty ("auto") to take the control channel's.
- **Voice frequencies, MHz** (Type-C) or **More repeaters, MHz** (Type-D) (`nxdnChannelsHz`):
  for Type-C, the site's voice frequencies, which are watched so channel numbers can be learned.
  For Type-D, optional: the repeaters may all go in the first field.
- **Channel table (optional)** (`lcnTableHz`): channel numbers you know, as `20=451.0125, 21=…`
  (Type-D: repeater numbers).

In the config file (frequencies in Hz):

```json
{ "shortName": "nexedge", "type": "nxdn", "controlChannelsHz": [451018750],
  "nxdnChannelsHz": [451118750, 452381250], "lcnTableHz": { "12": 451118750 } }
```

```json
{ "shortName": "idas", "type": "nxdn", "nxdnType": "typeD", "nxdnRate": "nxdn48",
  "controlChannelsHz": [452012500, 452062500, 452112500] }
```

**Find my system** recognises NXDN control channels, with their rate, RAN, system and site, and
adds a Type-C site from one. Then add its channel table or voice frequencies, as the notice says.
It also offers busy NXDN carriers as conventional channels. See [Find my system](find-my-system.md).

Every listed frequency has to be inside a source's band, or the recorder won't start ("NXDN
frequencies outside every source's bandwidth: … MHz").

## How calls are followed

### Type-C

The control channel assigns a call to a channel number. Its frequency comes from, in this order:

1. your **Channel table**. RadioReference lists the channel numbers (as LCNs) for many systems;
2. **Direct Frequency Assignment** (DFA): if the site uses it, it says so in its site
   information, and each assignment carries enough to work out the frequency itself;
3. learning: a grant for a channel it doesn't know, then the same group's call showing up on one
   of the watched **Voice frequencies** within 4 seconds, ties the channel to that frequency.

Learned channels are saved to `<shortName>.bandplan` in the folder the config file is in and
loaded on the next start. The log reports what it learns ("Channel 12 is 451.11875 MHz (…)"), and
a channel number it can't place yet ("Channel 12 isn't in the channel table: listening for the
call on the watched frequencies").

The DFA field layout comes from other open-source decoders (the specification isn't public), so
it is one of the least certain parts of the NXDN support.

### Type-D

Every repeater is watched. A call is found on the repeater carrying it, by what that repeater says
in its signalling. Repeater numbers are read from the air; the channel table isn't needed.

### Unit-to-unit calls

Calls to a single radio are recorded as unit-to-unit calls, like on P25 (on by default; **Unit-to-unit
calls** under **Call rules**).

## RAN

The RAN (Radio Access Number) is NXDN's colour code: a number from 0 to 63 that keeps a site apart
from neighbours on the same frequencies. The site's RAN is the first control channel's (the log
says "RAN 5"), unless you set **RAN**. Frames with another RAN are ignored. The dashboard marks a
watched frequency heard on a different RAN with a chip (for example **RAN 7**).

The call JSON has `ran`, and file name formats can use `{ran}`.

## Site lock and several sites

A Type-C site announces a system code and a site code. The **Site lock** on the system card takes
**System ID** (in hex) and **Site**; a control channel announcing something else isn't followed.
Type-C sites with the same system code are grouped as one system, so a call heard on two of them
is saved once.

Type-D sites announce nothing to lock to or group by. To record several Type-D sites of one
system, give them the same **Site group**. See [Several systems and sites](multi-site.md).

## Encryption

Encrypted transmissions (scrambler, DES or AES) are marked and left out of the audio, as on P25
and DMR. A call that was all encrypted is kept, without audio, only when `recordEncrypted` is
`true` in the config file.

Not decoded: full-rate (EFR) voice and data calls.

## Talker aliases

Kenwood NEXEDGE radios can send their programmed name ("E12 CAPT") while they talk, a few
characters at a time in the SACCH and FACCH1 of the traffic channel. Trunk Recorder Pro puts
the pieces together once the call has named its radio, checks the alias's checksum, and learns it
as it does a P25 talker alias: it appears as the radio's `tag_ota` in the call JSON and on the
**Calls** page, and is kept in `<shortName>.units.csv`. See
[Talker aliases](talkgroups-and-units.md#talker-aliases) for the file and for how a unit names
file combines with it. It works on trunked (Type-C) and conventional channels, at both rates.

Only plain ASCII aliases are read: one with other characters (a Japanese or Chinese name in
Shift-JIS or Big5, say) is dropped, as is one whose checksum doesn't match. Icom's version
isn't read either.

## What the dashboard shows

The system's detail on the **Decode** page has an **NXDN** panel:

- the kind (**NXDN Type-C** or **NXDN Type-D**), the rate, the system and site, the RAN, and the
  DFA base and step if the site uses it;
- a table of every watched frequency: whether it is the **control** channel, its channel number
  (marked **(learned)** if learned) or Type-D repeater number, and the call on it now;
- channels in the table that aren't watched;
- a warning listing channel numbers that were granted but aren't in the channel table yet.

On the **Calls** page an NXDN call shows its rate and RAN after the frequency.

## Diagnostic tools

What helps most with a real system is a capture: 60 to 120 seconds of raw IQ while the system is
busy, centred so the control channel and some voice channels are inside it.

```bash
# Record it (RTL-SDR, 2.4 MS/s by default)
trunk-pro capture nxdn.cu8 --freq 451000000 --seconds 120

# Every NXDN carrier in it, at both rates, with RAN and what it carries
trunk-pro tool nxdnscan nxdn.cu8 --center 451000000

# One channel's messages (on a control channel: grants, site information)
trunk-pro tool nxdn nxdn.cu8 --center 451000000 --freq 451018750 --nxdn 48 > cc.jsonl
#   --frames          every frame
#   --audio out.f32   its voice, as raw 8 kHz float samples

# The whole recorder on it
trunk-pro replay nxdn.cu8 --center 451000000 --rate 2400000 --cc 451018750 \
  --nxdn-trunk typeC --nxdn-rate 48 --nxdn-channels 451118750,452381250 --out calls
```

`replay` also takes `--lcn 12=451118750,…` and `--ran N`; use `--nxdn-trunk typeD` for a Type-D
site. The receiver works out the signal's polarity itself, so spectrally inverted recordings
decode too.

`trunk-pro tool nxdnsynth out.cu8 --center 451000000 --kind typeC|typeD|conv --nxdn 48|96` writes
a synthetic capture (a Type-C control channel granting group 3001 on channel 12, a Type-D pair of
repeaters, or a conventional call) for checking a setup or comparing with another decoder.

If you can, send the capture (or its first 30 seconds), the `tool nxdn` output, the system's
RadioReference page, and what the recorder got wrong. Every flag is in
[Command line](../command-line.md).

## Common problems

**"Channels … were granted but aren't in the channel table".** List the site's voice frequencies
so they can be learned, or add the channel numbers to **Channel table**.

**Nothing decodes.** Check **Rate**: an NXDN48 control channel set as NXDN96 (or the other way)
won't decode. `tool nxdnscan` tells you which it is.

**Calls from a neighbouring site.** Set **RAN** to your site's.
