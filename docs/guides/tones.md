# Tones and access codes

This page explains how to record only some of the traffic on a shared conventional frequency, using
CTCSS tones, DCS codes, P25 NACs, DMR colour codes and NXDN RANs; how to find out which codes a
frequency carries; and how analog unit IDs (MDC1200 and FleetSync) are picked up.

It builds on [Conventional channels](conventional.md); read that first.

## Why frequencies are shared

Several agencies often share one conventional frequency. Each one's radios send a code along with
the voice, and only open their speakers for their own code, so each agency hears only its own
traffic. A scanner (or recorder) without the code hears all of them.

- **Analog FM** uses a sub-audible tone: a CTCSS tone (a steady low tone such as 151.4 Hz, also
  called PL) or a DCS code (a slow digital pattern such as D023N, also called DPL).
- **P25** frames carry a NAC (network access code), three hex digits such as 293.
- **DMR** carries a colour code (0-15), and each transmission is on slot 1 or 2 with a talkgroup.
- **NXDN** carries a RAN (radio access number, 0-63) and a group.

Give a row a code in its **Code** column (`code` in the config) and it records only transmissions
carrying that code. DMR rows also have **Slot** (`slot`, 1 or 2), and P25, DMR and NXDN rows
**Match TG** (`matchTalkgroup`): record only transmissions naming that talkgroup. Configs from
before these keys, with a `tone` key, are upgraded when loaded.

## Typing a code

Type the code the way RadioReference or Trunk Recorder shows it. It is tidied to one form (the bare
code), and a mistake is pointed out as you type (for example `151.5 Hz isn't a standard CTCSS tone — 151.4?`).

| Mode | Code | You can type | Shown as |
|---|---|---|---|
| Analog FM | CTCSS tone | `151.4`, `151.4 PL`, `PL 151.4`, `151.4 Hz`, `100` | `151.4`, `100.0` |
| Analog FM | DCS code | `D023N`, `023 DPL`, `D023`, `D023I` | `D023N`, `D023I` |
| P25 | NAC | `293`, `293 NAC`, `NAC 293`, `$293`, `0x293` | `293` |
| DMR | colour code | `CC1`, `1`, `CC1 TS2 TG201`, `CC 1 TG 201 SL 2` | `1` |
| NXDN | RAN | `RAN 5`, `5`, `RAN5 TG 201` | `5` |

A combined string such as RadioReference's `CC1 TS2 TG201` or `NAC 293 TG 1` typed into Code is
split into the three boxes: code `1`, slot `2`, match talkgroup `201`.

Some values mean "any code", the same as leaving the box empty: `0`, `S` (Trunk Recorder's search),
`CSQ` and `none` for analog; `F7E` and `F7F` (a radio's "receive any" NAC) for P25; `RAN 0` for
NXDN. A bare number on an analog row is read as a CTCSS tone when it is one, otherwise as a DCS
code. RadioReference's scan-list field (`SL 2`) on a DMR code is ignored.

## Splitting a shared frequency

To split a frequency, list it once per code. In the channel table, the **+ tone** button on a row
(**+ NAC** on P25 rows, **+ code** on DMR and NXDN rows) adds another row on the same frequency,
with its own talkgroup number already filled in. You can also add the rows in a CSV or the config:

```csv
TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag
,154.3250,D223N,,,fm,County A Fire
,154.3250,118.8,,,fm,County B Fire
,154.3250,,,,fm,County other
```

Each transmission goes to the row whose code it carries. Otherwise it goes to the row with no
code, if there is one. Otherwise it isn't recorded:

| Rows on 154.325 MHz | A transmission with D223N | with 118.8 | with another tone, or none |
|---|---|---|---|
| one, no tone | that row | that row | that row |
| D223N, 118.8 | the D223N row | the 118.8 row | not recorded |
| D223N, 118.8, and one with no tone | the D223N row | the 118.8 row | the row with no tone |

So a channel listed with no code records everything. Only rows with codes leave anything out.

Each row files its calls under its own talkgroup number (see [The number a call is filed
under](#the-number-a-call-is-filed-under)), so each agency's calls get their own
name, folder (with a [file name format](recordings.md#choosing-folders-and-file-names) that uses
the talkgroup) and upload settings. Left blank, the first row on 154.325 MHz is talkgroup 154325,
the next 1543251, then 1543252.

Two rules keep the rows unambiguous, and recording won't start until they're met:

- Rows on one frequency must share a mode.
- At most one row may have no code, and no two rows may have the same code. Two DCS codes that are
  the same signal on the air count as the same code (see below).

## How each mode decides

### Analog FM

A transmission is held until its tone is identified, usually within a few tenths of a second, then
filed under the matching row from its beginning, so nothing is cut off. A transmission with no
tone waits up to a second before going to the row with no code. When two agencies key up one after
the other, each transmission is told apart, so they become two calls.

A frequency with a single row and no tone isn't held at all.

### P25

Every P25 frame carries the NAC, so nothing needs holding. Rows split by NAC, and also by
**Match TG**: a row with a talkgroup records only transmissions naming it. Conventional P25 radios
often send a talkgroup that says little (frequently 1), so give each row its own **Talkgroup**; the
call is then filed under it and the air's number is kept in `talkgroup_ota`.

### DMR

Each slot is handled on its own. The most specific row that fits wins: `CC1 TS2 TG201` beats `CC1`.
Slot and **Match TG** narrow a row further. Calls are filed under the row's **Talkgroup** if it has
one, otherwise the talkgroup on the air.

### NXDN

As DMR, with the RAN in place of the colour code, and no slots.

## The number a call is filed under

The call JSON's `talkgroup` is, in order:

1. the row's **Talkgroup** (TG Number), if it gives one;
2. else the talkgroup the air named (P25, DMR and NXDN);
3. else the frequency default: the kHz, then kHz and a digit for further rows on the frequency.

The names (alpha tag and so on) are the row's own if it has any, else the conventional system's
talkgroup file entry for the number filed under. On P25, DMR and NXDN calls the talkgroup the air
named is also written as `talkgroup_ota`, even when it equals `talkgroup`; it is absent for FM and
AM and for calls whose air named none. A new talkgroup on the air within an open call still starts a
new call, even when the row files both under the same number.

## Finding a frequency's codes

If you don't know which codes a frequency carries, list it with no code and let it record for a
while. Under the frequency's rows, the channel table shows what it heard, most heard first:

```text
Heard: 151.4 Hz 42 calls [Add]   D023N 3 calls [Add]   no tone 5 calls ✓
```

- **Add** makes a row for that code, with its own talkgroup number.
- A ✓ means a row for that code is already listed.
- Transmissions that no row recorded are counted too ("2 not recorded"), so you can see what your
  codes are leaving out.
- Hovering over a code shows when it was last heard.

The first eight codes are shown, then "and N more". Up to 40 codes per frequency are remembered;
the least recently heard are dropped first. The list is kept between runs: on the desktop in
`conventional.heard.json` in the app's data folder (beside the config unless you've moved the data
folder), and in the browser version in the browser's own storage.

While a channel file is linked, the **Heard** line still shows, but without **Add**: add the row to
the file instead.

## How tones are identified

Every analog call on a conventional channel has its tone identified, whether or not its row has
one. The voice audio is high-passed at 300 Hz, which removes the tone from what you hear; the
detector works on the audio below that.

- **CTCSS:** all 51 standard tones are measured together. A tone counts when it stands well clear
  of the rest (about 20 dB above their median), and its exact frequency is measured and snapped to
  the nearest standard tone, so close pairs such as 150.0 and 151.4 Hz aren't confused.
- **DCS:** the 134.4 baud code is decoded and checked against the 23-bit Golay code word of each
  of 112 DCS codes (Motorola's 83 plus scanners' extended set). The same code must repeat before it
  counts.

A DCS word repeats continuously, so read from a different starting bit it can look like another
valid code: D023N and D047I, for example, are the same signal on the air. Such codes are treated as
one; a row set to either records both.

The identified tone is shown in the call list while the call is in progress (as `FM 151.4 Hz`), and
written to the [call JSON](recordings.md#the-call-json) with Trunk Recorder's field names:

| Field | Value |
|---|---|
| `tone_mode` | `ctcss` or `dcs` when the row has that kind of tone; `search` for an analog conventional call whose row has none; `off` otherwise |
| `tone_detected` | The tone heard, as `151.4` or `D023N`; empty when none was |
| `tone_confidence` | 0 to 1: the share of the call that carried the tone |

A tone is reported only when it was heard for at least 30% of the call. Trunked analog calls
(SmartNet) don't have their tone identified.

## MDC1200 and FleetSync unit IDs

Many analog radios send a short data burst when they key up or unkey, carrying the radio's ID.
Trunk Recorder Pro decodes the two common kinds from the audio of every analog call, on
conventional FM channels and on SmartNet analog calls:

- **MDC1200** (Motorola): 1200 baud. The 16-bit unit ID is used as it is, as Trunk Recorder
  writes it.
- **FleetSync** (Kenwood): 1200 or 2400 baud. The ID is written as fleet × 10000 + unit, so fleet
  101, unit 1234 becomes 1011234. Only the first block (which holds the sender) is read; FleetSync
  II's error-corrected framing isn't decoded.

The ID goes into the call's `srcList`, the same as a digital radio's unit ID, so unit names apply
to it (each conventional system has its own unit names). An MDC1200 emergency flag marks the call
as an emergency.

There is nothing to turn on: Trunk Recorder's `decodeMDC` and `decodeFSync` settings aren't needed
and don't exist here.

## Coming from Trunk Recorder

- Trunk Recorder's channel file `Tone` column reads as it is (as **Code**), including DCS codes. `S` (search)
  needs no setting here: every analog call's tone is identified and written to its JSON anyway.
- Trunk Recorder doesn't filter by NAC, colour code or RAN; here the same column does.
- The JSON tone fields (`tone_mode`, `tone_detected`, `tone_confidence`) use the names from Trunk
  Recorder's pull request #1137.
