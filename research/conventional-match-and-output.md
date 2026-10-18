# Proposal: conventional rows that say what they match and what they file under

A design proposal (2026-10-18), prompted by a user's "channel file v2" sketch and the question of
what to do with talkgroups that arrive over the air. Nothing here is built; the last section
lists what would change.

## Summary

A conventional row today is frequency + mode + one selector string (`tone`) + a `talkgroup` +
names. The `talkgroup` column means three different things depending on the mode and on how
many rows share the frequency, P25 rows can't match on a talkgroup at all, and a number the
user types on a digital row is ignored whenever the air names one. The result users hit: three
P25 conventional channels in one system, every radio sending TG 1, all filed under talkgroup 1 —
one talkgroup in the history, on the Listen page and on OpenMHz, for three channels.

The proposal keeps one row model for every mode and splits a row into three parts — **channel**
(what to monitor), **match** (which transmissions the row takes) and **output** (what a matched
call is filed under and called) — with one rule for the number a call is filed under:

> **`TG Number` if the row gives one; else the talkgroup the air named, if it did; else the
> frequency's default.**

A blank keeps today's behaviour for the common cases (an analog row files under its frequency, a
single digital channel keeps the air's numbers); a typed number always wins; and a digital row
can match one talkgroup and file it under another. The CSV gains `Code`, `Slot` and `Match TG`
columns; the old `Tone` column is still read. No new file format, no second kind of system.

## Two kinds of conventional channel

- **Named ahead of time.** Analog FM and AM: the frequency (and a tone or DCS code, if the
  frequency is shared) *is* the talkgroup. Nothing arrives over the air that names it.
- **Named per transmission.** P25, DMR and NXDN: every transmission carries a talkgroup, and a
  NAC, colour code or RAN, that a row couldn't know ahead of time. An FM row set to search for
  tones is halfway: the tone arrives per transmission, but it names no number.

The engine already models both. `Conventional::new` groups the config's rows by frequency into
one `Chan` each (the head to open, its mode, shape and squelch) with the rows inside it as
matching rules; `Heard` carries what the air said (`tg`, `nac`, `color_code`, `ran`, the tone);
`route_digital` / `pick` choose a row. That is the "unique channel/mode pairs to monitor +
matching rules" split the user's sketch asks for, and it needs no second pipeline. What is
muddled is the row.

## What is wrong today

| Mode | Rows on the frequency | Filed under (today) |
|---|---|---|
| FM / AM | any | the row's `talkgroup` (default: kHz, then kHz+digit) |
| P25 | one | the air's; the row's only when the air names none |
| P25 | several (split by NAC) | the row's — the air's is thrown away |
| DMR / NXDN | any | the air's; the row's only when the air names none |

- `talkgroup` is "filed under" for analog, "default when the air is silent" for digital, and
  "filed under after all" on a NAC-split P25 frequency.
- A user who types 46012 on a single P25 row is ignored (`slot_call`: `air_tg.unwrap_or(row.talkgroup)`).
- P25's selector is the NAC only (`Access::Nac`), so "TG 1 on this channel is Public Works, TG 2
  is Parks, anything else is other" can't be said. DMR and NXDN selectors can name a TG, but a
  matched call then files under that same number: no remapping.
- The conventional system's own talkgroup file (0.2.0) names calls by the air's number, so it
  collides across channels the same way.
- The Setup placeholder (`channel_talkgroups`) and the engine default agree, but neither says
  "from the air" where that is what happens.

## The row

| Part | Columns | What it does |
|---|---|---|
| **Channel** | `Frequency`, `Mode`, `Squelch dB`, `Bandwidth kHz`, `Enable` | What to monitor. Rows sharing a frequency share one head and must agree on the mode (as today). |
| **Match** | `Code`, `Slot`, `Match TG` | Which transmissions on the frequency the row takes. Blank = any. `Code` is the mode's: a CTCSS tone or DCS code (FM), a NAC (P25), a colour code (DMR), a RAN (NXDN). `Slot` is DMR's, 1 or 2. `Match TG` is a digital mode's talkgroup. |
| **Output** | `TG Number`, `Alpha Tag`, `Description`, `Tag`, `Category` | What a matched call is filed under and called. |

In the config a channel row gains `code`, `slot` and `matchTalkgroup` in place of the one `tone`
string (which is still read, see Upgrade); `talkgroup` keeps its name and becomes "TG Number".

### The rules

1. **Matching.** A row takes a transmission when every selector it gives matches. Among the rows
   on a frequency that match, the most specific wins (the most selectors given — today's
   `Access::fields` ranking); a row with no selector is the catch-all. A transmission no row
   takes is dropped and reported once (`convSkipped`), as today. Rows on a frequency keep today's
   checks: one mode, at most one catch-all, codes that can be told apart.
2. **Filed under** = `TG Number` if given; else the talkgroup the air named, if it did; else the
   frequency default — the frequency in kHz for the first row, kHz with a digit added for further
   rows (154430, 1544301, 1544302…), as today.
3. **Names** = the row's, if it has any (with the number filed under); else the system's
   talkgroup file's entry for that number; else none.
4. **A new talkgroup on the air is a new call**, as today, compared on the air's number, so a
   catch-all that folds TG 1 and TG 2 into 46012 still cuts between them. The first transmission
   that names a talkgroup labels the call it arrived in.
5. **The air's number is kept** when a row changes it: the call record gets `talkgroup_ota`
   (this recorder's field, like `site` and `decrypted`), written only when it differs from
   `talkgroup`. Plugins key on `talkgroup`; the file keeps the truth.

## Processing

Reading the config, as now but with the three parts explicit:

```text
read_channels(system):
    for each row in system.channels (enabled):
        chan  = channels[row.freq]            # one per frequency: mode, squelch (lowest), bandwidth
        rule  = Rule {
            match:  { code: row.code, slot: row.slot, tg: row.match_talkgroup },   # each Option
            output: { tg: row.talkgroup,                                           # Option
                      default_tg: default_talkgroup_at(row.freq, k),               # kHz, kHz+digit
                      names: row.names },                                          # Option
        }
        chan.rules.push(rule)
    check(chan): one mode; ≤ 1 rule with an empty match; distinguishable codes
```

Routing one transmission (`Heard`: what the air said on this frequency, this slot):

```text
route(chan, heard) -> Option<&Rule>:
    candidates = chan.rules.filter(|r| r.match.admits(heard))
    candidates.max_by(|r| r.match.given())      # most selectors given wins; ties: first row

Match.admits(heard):
    code.is_none()  or code == heard.code       # tone/DCS, NAC, colour code, RAN — the mode's
    and slot.is_none() or slot == heard.slot
    and tg.is_none()   or tg   == heard.tg      # digital modes; FM has no tg
```

FM rows are decided the same way, just later: a transmission on a frequency with tones is held
until its tone is read (today's `Tx`), then routed; one with no tone rows is routed at once.

What a matched transmission files under and is called:

```text
file_under(rule, heard) -> u32:
    rule.output.tg                              # typed: wins
    .or(heard.tg)                               # else the air's, when it named one
    .unwrap_or(rule.output.default_tg)          # else the frequency's

names(system, rule, tg) -> Option<Talkgroup>:
    rule.output.names.with_number(tg)           # the row's, if it has any
    .or(system.talkgroup_file.get(tg))          # else the file's entry for that number
```

The call on a slot, per run of a channel (today's `slot_call`, with rule 2 in place of the
mode-dependent choice):

```text
slot_call(chan, slot, heard):
    rule = route(chan, heard) or return        # (reported as skipped)
    tg   = file_under(rule, heard)
    live = chan.open.live[slot]

    if live and live.air_tg is Some and heard.tg is Some and heard.tg != live.air_tg:
        end(live)                               # a new talkgroup on the air: a new call
        live = None
    if live and live.air_tg is None and heard.tg is Some:
        live.air_tg = heard.tg                  # the first named transmission labels the call
        relabel(live, tg, names(system, rule, tg))   # only changes anything when tg differs
    if live is None:
        live = start_call(tg, names(system, rule, tg), air_tg = heard.tg, voice = chan.mode.voice(slot, heard))
    feed(live, heard)                           # audio, frames, sources, positions
    at end: record.talkgroup = live.tg; record.talkgroup_ota = live.air_tg if != live.tg
```

## Examples

Each example gives the CSV row(s), the same as config JSON, what the air sends, and what happens.
`→ filed under N "name"` is what the call record carries (`talkgroup`, `talkgroup_tag`).

### 1. A plain FM channel — named ahead of time

```csv
Frequency,Mode,Code,Slot,Match TG,TG Number,Alpha Tag,Description,Tag,Category,Squelch dB,Enable
154.4300,fm,,,,,County Fire Dispatch,,Fire Dispatch,Fire,,true
```

```json
{ "freqHz": 154430000, "mode": "fm", "name": "County Fire Dispatch", "tag": "Fire Dispatch", "group": "Fire" }
```

The carrier comes up. No selector, no number: the catch-all files under the frequency default.
→ filed under **154430** "County Fire Dispatch". Unchanged from today.

With `TG Number` 1001 → filed under **1001**. Unchanged from today.

### 2. An FM frequency shared by tone, with a catch-all

```csv
154.3250,fm,D223N,,,,County A Fire,,,Fire,,true
154.3250,fm,151.4,,,,County B Fire,,,Fire,,true
154.3250,fm,,,,,154.325 (other),,,Fire,,true
```

```json
{ "freqHz": 154325000, "mode": "fm", "code": "D223N", "name": "County A Fire", "group": "Fire" },
{ "freqHz": 154325000, "mode": "fm", "code": "151.4", "name": "County B Fire", "group": "Fire" },
{ "freqHz": 154325000, "mode": "fm", "name": "154.325 (other)", "group": "Fire" }
```

One head on 154.325. A transmission with DCS 223N → row 1 → filed under **154325**
"County A Fire". With 151.4 Hz → row 2 → **1543251** "County B Fire". With 100.0 Hz, or no tone
→ the catch-all → **1543252** "154.325 (other)", `tone_detected` 100.0 in the record. Unchanged
from today; `Code` is the old `Tone` column.

### 3. One P25 channel, blank TG Number — the air's numbers, named by the talkgroup file

```csv
460.1250,p25,,,,,,,,,,true
```

```json
{ "freqHz": 460125000, "mode": "p25" }
```

The system's talkgroup file `townville.talkgroups.csv` has `1,...,PD Dispatch` and `2,...,PD Tac`.

A transmission with NAC 293, TG 1 → the catch-all; no `TG Number` → the air's: filed under
**1** "PD Dispatch" (from the file). Next transmission TG 2 while the call is open → a new
call, **2** "PD Tac". Unchanged from today. This is the row for a channel whose talkgroups are
globally meaningful to its users.

### 4. Three P25 channels in one system, every radio sending TG 1 — the overload case

```csv
460.1250,p25,,,,46012,Townville PD,,Law Dispatch,Police,,true
460.1750,p25,,,,46017,Townville FD,,Fire Dispatch,Fire,,true
460.2250,p25,,,,46022,Townville PW,,Public Works,Public Works,,true
```

```json
{ "freqHz": 460125000, "mode": "p25", "talkgroup": 46012, "name": "Townville PD", "tag": "Law Dispatch", "group": "Police" },
{ "freqHz": 460175000, "mode": "p25", "talkgroup": 46017, "name": "Townville FD", "tag": "Fire Dispatch", "group": "Fire" },
{ "freqHz": 460225000, "mode": "p25", "talkgroup": 46022, "name": "Townville PW", "tag": "Public Works", "group": "Public Works" }
```

TG 1 on 460.125 → filed under **46012** "Townville PD", `talkgroup_ota: 1`. TG 1 on 460.175 →
**46017** "Townville FD". Three channels, three talkgroups on OpenMHz and the Listen page.
**Today**: all three file under 1, the typed numbers ignored.

TG 2 on 460.125 after TG 1, same call still open → a new call (rule 4), also filed under
**46012** — the output folds them, the call boundary doesn't.

### 5. A P25 frequency split by talkgroup, with "other" — the user's example

```csv
453.5000,p25,293,,1,45351,Townville PW,,Public Works,Public Works,,true
453.5000,p25,293,,2,45353,Townville Parks,,Parks,Public Works,,true
453.5000,p25,293,,,,Townville (other),,Public Works,Public Works,,true
```

```json
{ "freqHz": 453500000, "mode": "p25", "code": "293", "matchTalkgroup": 1, "talkgroup": 45351, "name": "Townville PW", "tag": "Public Works", "group": "Public Works" },
{ "freqHz": 453500000, "mode": "p25", "code": "293", "matchTalkgroup": 2, "talkgroup": 45353, "name": "Townville Parks", "tag": "Parks", "group": "Public Works" },
{ "freqHz": 453500000, "mode": "p25", "code": "293", "name": "Townville (other)", "tag": "Public Works", "group": "Public Works" }
```

One head on 453.5. NAC 293, TG 1 → rows 1 and 3 match; row 1 is more specific → **45351**
"Townville PW" (`talkgroup_ota: 1`). TG 2 → **45353** "Townville Parks". TG 7 → row 3 only; no
`TG Number` → the air's: **7** "Townville (other)". NAC 4F1, any TG → no row → dropped,
reported once as `convSkipped` "NAC 4F1" (as today). **Today** none of this can be said: a P25
row matches on NAC only.

### 6. A P25 frequency split by NAC — today's rows, unchanged

```csv
453.5000,p25,293,,,,Townville,,,Public Works,,true
453.5000,p25,4F1,,,,Riverside,,,Public Works,,true
```

```json
{ "freqHz": 453500000, "mode": "p25", "code": "293", "talkgroup": 453500, "name": "Townville", "group": "Public Works" },
{ "freqHz": 453500000, "mode": "p25", "code": "4F1", "talkgroup": 4535001, "name": "Riverside", "group": "Public Works" }
```

Today these rows (with blank `talkgroup`) file under the frequency defaults 453500 and 4535001,
the air's TG thrown away. The upgrade writes those defaults in as `TG Number` (the JSON above),
so they keep filing the same. A user who would rather keep the air's numbers clears the two
numbers; one who wants them named by talkgroup adds rows as in example 5.

### 7. DMR: one talkgroup remapped, the rest of the repeater as it comes

```csv
451.5000,dmr,CC1,1,1,9001,Dispatch,,Business,Business,,true
451.5000,dmr,CC1,,,,451.500 Repeater,,Business,Business,,true
```

```json
{ "freqHz": 451500000, "mode": "dmr", "code": "CC1", "slot": 1, "matchTalkgroup": 1, "talkgroup": 9001, "name": "Dispatch", "tag": "Business", "group": "Business" },
{ "freqHz": 451500000, "mode": "dmr", "code": "CC1", "name": "451.500 Repeater", "tag": "Business", "group": "Business" }
```

Colour code 1, slot 1, TG 1 → row 1 (three selectors) → **9001** "Dispatch" (`talkgroup_ota:
1`). CC 1, slot 2, TG 1 → row 2 (slot doesn't match row 1) → the air's: **1** "451.500
Repeater" (or the talkgroup file's name for 1, if the row had no names). CC 1, slot 1, TG 42 →
row 2 → **42**. CC 3 → no row → dropped, reported "CC 3 TS 1 TG 42". **Today** the first row
can be written (`tone` "CC1 TS1 TG1") but files under 1, not 9001.

The same `Dispatch` row on a second repeater (452.300, TG 1 → 9001) files both under 9001, as
the user's sketch intends: one logical talkgroup on two frequencies.

### 8. DMR: the two slots as two channels

```csv
462.5500,dmr,CC2,1,,,Yard,,,Business,,true
462.5500,dmr,CC2,2,,,Office,,,Business,,true
```

```json
{ "freqHz": 462550000, "mode": "dmr", "code": "CC2", "slot": 1, "name": "Yard", "group": "Business" },
{ "freqHz": 462550000, "mode": "dmr", "code": "CC2", "slot": 2, "name": "Office", "group": "Business" }
```

Slot 1, TG 5 → row 1; no `TG Number` → the air's: **5** "Yard". Slot 2, TG 5 → **5** "Office" —
the same number, two names; a user who wants them apart on OpenMHz gives each a `TG Number`
(say 4625501 and 4625502), after which `talkgroup_ota` keeps the 5.

### 9. NXDN by RAN

```csv
451.8000,nxdn48,5,,,,Transit,,,Transit,,true
```

```json
{ "freqHz": 451800000, "mode": "nxdn48", "code": "5", "name": "Transit", "group": "Transit" }
```

RAN 5, group 201 → **201** "Transit". RAN 0 (any) on the row would take every RAN. Unchanged
from today except that a `TG Number` would now win.

### 10. A talkgroup changes mid-call on a folding row

Row as in example 4 (`460.1250,p25,,,,46012,...`). A radio keys up on TG 1 (call A, filed under
46012); 4 s later, with the call still open, another keys up on TG 2.

TG 2 ≠ call A's air talkgroup → call A ends, call B starts: both filed under **46012**, A with
`talkgroup_ota: 1`, B with `talkgroup_ota: 2`. The output number doesn't hide the change of
conversation. (Today: the same cut, both filed under 1 and 2.)

### 11. An old-style row, upgraded

Today's row (config `tone`, CSV `Tone`):

```json
{ "freqHz": 451500000, "mode": "dmr", "tone": "CC 1 TS 2 TG 201", "name": "Dispatch" }
```

is read, by the existing parser (`Access::parse`), as

```json
{ "freqHz": 451500000, "mode": "dmr", "code": "CC1", "slot": 2, "matchTalkgroup": 201, "name": "Dispatch" }
```

and files under **201** as before (no `TG Number`, the air's 201). A CSV with a `Tone` column
reads the same way; RadioReference's `CC1 TS2 TG201` pasted into `Code` is split the same way
too, so the paste keeps working. `Export CSV` writes the new columns.

### 12. AM airband

```csv
119.1000,am,,,,,Tower,,Aircraft,Airport,,true
```

```json
{ "freqHz": 119100000, "mode": "am", "name": "Tower", "tag": "Aircraft", "group": "Airport" }
```

AM carries no code and names nothing: `Code`, `Slot` and `Match TG` must be blank (as `tone`
must be today), one row per frequency. → **119100** "Tower". Unchanged.

### What a blank `TG Number` means, by mode

| Mode | Blank files under |
|---|---|
| FM, AM | the frequency default (kHz; kHz+digit for further rows) |
| P25, DMR, NXDN | the air's talkgroup; the frequency default when a transmission names none |

Setup shows this as the field's placeholder: `460125` on an FM row, `from the air, else 460125`
on a P25 row.

## Upgrade and compatibility

- **Config**: `tone` is read and split into `code`, `slot`, `matchTalkgroup`; saved back in the
  new fields. A P25 row on a frequency with several rows and no `talkgroup` gets today's default
  written in as `talkgroup` (example 6), so nothing already recorded files differently. DMR and
  NXDN rows whose `tone` named a TG are unchanged by the new rules (the air's number is the
  matched number).
- **CSV**: no version flag. `Tone` is accepted as the old combined column; `Code`, `Slot`,
  `Match TG` are new; a file with today's columns reads as today. The writer writes the new
  columns.
- **One deliberate behaviour change**, for the changelog: a typed `TG Number` on a digital row
  wins over the air's number, where it was ignored before. It is what the column says.
- **Call record**: `talkgroup_ota` is new (SDK: one optional field, left out when absent, like
  `site`). Nothing else in the record changes.
- **Plugins and interfaces**: see `talkgroup` as before; a plugin wanting the air's number reads
  `talkgroup_ota`.

## What changes where

**Engine, `crates/trunk-core/src/trunk/conventional.rs`** — small.
- `Access` becomes one shape for every mode, `Match { code: Option<Code>, slot: Option<u8>, tg:
  Option<u32> }` with `Code = Tone | Nac | ColorCode | Ran`; `admits` as in the pseudocode (the FM
  arm compares the detected tone with `code`); `fields` → `given`.
- `ConvChannel.talkgroup: u32` → `talkgroup: Option<u32>` beside `default_talkgroup`;
  `slot_call` applies `file_under`; `Live` keeps `air_tg` beside the call's number (today's
  `tg_from_air` flag, with the number).
- `route_digital` and `pick` become one `route`; the `rows.len() > 1` P25 special case goes.
- `info_for` becomes `names`.
- `Call` / the record carry `talkgroup_ota`.

**Config, `crates/trunk-app/src/config.rs`, `migrate.rs`, `channels.rs`.**
- `Channel`: `code`, `slot`, `matchTalkgroup`; `tone` read as the old form (migrate.rs, using
  `Access::parse`); the P25 default written in on upgrade.
- `channel_talkgroups` → what each row files under when the air names nothing, plus whether the
  air may (for the placeholder).
- The CSV reader and writer: the new columns, `Tone` accepted. `rules.rs` already exports both,
  so Setup and the browser version follow.
- `Config::problem`: the per-frequency checks on the new shape; AM rows with any selector refused.

**Setup, `web/src/Setup.tsx`**: the channel table shows `Code`, `Slot` (DMR rows) and `Match
TG` (digital rows) in place of the one tone input; the `TG Number` placeholder as above; "+ NAC /
+ code" becomes "+ rule on this frequency".

**SDK, `crates/trunk-recorder-plugin`**: `CallRecord.talkgroup_ota: Option<u32>`.

**Docs**: `docs/configuration/conventional.md` (keys, the CSV, the filing table), the
conventional guide (the two kinds as two workflows on one row), the talkgroup-file section, the
recordings guide (the new record field), CHANGELOG.

## Why one model and not two

Two kinds of system — one for channels named ahead of time, one for channels named over the air
— would mean two CSV formats, two Setup tables and two routing paths, for rows that differ only
in which columns apply; and a P25 frequency a user names with a row (example 4) belongs to both.
The architecture review just folded conventional's pipeline into the engine's; this keeps it
one. The user sees the difference where it matters: which columns a mode shows, and what a blank
`TG Number` means for it. The guide presents them as two workflows on the same row.

## Open questions

- **`talkgroup_ota`'s name.** Trunk Recorder has no such field; `talkgroup_ota` follows
  `tag_ota`. Alternatives: `air_talkgroup`, `talkgroup_heard`.
- **A `Match TG` on an FM row** is meaningless (FM names no number); refuse it at Start, or
  ignore it? Refusing is consistent with the AM rule.
- **The Setup table width.** Three match columns instead of one makes the row wide; the `Slot`
  and `Match TG` inputs show only for the modes that have them, which keeps FM rows as they are.
- **Should a catch-all P25 row with names (example 5's "other") name every talkgroup it takes
  "Townville (other)"**, or only when the talkgroup file has no entry? The rule above says the
  row's names first. The other order (file first, row as fallback) suits example 3 with a
  partial file better. Either is a one-line choice in `names`; the proposal picks the row's
  because a row is the more specific statement.
