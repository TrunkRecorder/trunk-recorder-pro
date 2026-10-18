# Conventional channels: what a row matches, and what it files under

A design note (2026-10-18), after a user's proposal for a "channel file v2" and the question of
how to handle talkgroups that arrive over the air. Nothing here is built.

## Two kinds of conventional channel

- **Named ahead of time.** Analog FM and AM: the frequency (and a tone or DCS code, if the
  frequency is shared) *is* the talkgroup. Nothing arrives over the air that names it.
- **Named per transmission.** P25, DMR and NXDN: every transmission carries a talkgroup (and a
  NAC, colour code or RAN) that the row couldn't know ahead of time. An FM row set to search for
  tones is halfway: the tone arrives per transmission, but it names no number.

The engine already models both. `Conventional::new` groups the config's rows by frequency into
one `Chan` each (the head to open, its mode, shape and squelch) with the rows inside it as
matching rules; `Heard` carries what the air said (`tg`, `nac`, `color_code`, `ran`, the tone);
`route_digital` / `pick` choose the row. That is the "unique channel/mode pairs to monitor +
matching rules" split the proposal asks for, and it doesn't need a second kind of system or a
second pipeline. What is muddled is the **row**: one column means three things.

## What is wrong today

A row is frequency + mode + `tone` (one selector string) + `talkgroup` + names. The number a
call is filed under depends on the mode and on how many rows share the frequency:

| Mode | Rows on the frequency | Filed under |
|---|---|---|
| FM / AM | any | the row's `talkgroup` (default: kHz, then kHz+digit) |
| P25 | one | the talkgroup the air names; the row's only when it names none |
| P25 | several (split by NAC) | the row's `talkgroup` — the air's is thrown away |
| DMR / NXDN | any | the air's; the row's only when it names none |

So `talkgroup` means "filed under" for analog, "default when the air is silent" for digital, and
"filed under after all" for a NAC-split P25 frequency. A user who types 46012 on a single P25
row is ignored. There is no way to say "TG 1 on this channel is Public Works, TG 2 is Parks,
anything else is 'other'" (P25's selector is the NAC only; DMR's and NXDN's can name a TG but
then file under that same TG). And the case the user hit: three P25 conventional channels in one
system, each radio sending TG 1, all filed under talkgroup 1 — one talkgroup in the history, on
the Listen page, and on OpenMHz, for three channels.

The conventional system's own talkgroup file (0.2.0) names calls by the air's number, so it has
the same collision.

## Proposal: match and output, with blanks meaning the predictable default

Keep one row model for every mode. Split the row into three parts:

| Part | Columns | What it does |
|---|---|---|
| **Channel** | `Frequency`, `Mode`, `Squelch dB`, `Bandwidth kHz`, `Enable` | What to monitor. Rows sharing a frequency share one head and must agree on the mode (as today). |
| **Match** | `Code`, `Slot`, `Match TG` | Which transmissions on the frequency this row takes. Blank = any. `Code` is the mode's: a CTCSS/DCS code, a NAC, a colour code, a RAN. `Slot` is DMR's (1/2). `Match TG` is a digital mode's talkgroup. |
| **Output** | `TG Number`, `Alpha Tag`, `Description`, `Tag`, `Category` | What a matched call is filed under and called. |

The rules, the same for every mode:

1. **Matching.** A row takes a transmission when every selector it gives matches. Among the rows
   on a frequency that match, the most specific wins (most selectors given; today's `fields()`
   ranking); a row with no selector is the catch-all. Rows on a frequency keep today's checks
   (one mode, one catch-all, codes that can be told apart).
2. **Filed under** = `TG Number` if given; else the talkgroup the air named, if it did; else the
   frequency default (kHz, kHz+digit for further rows — as today). So blank keeps the air's
   number on a digital channel and the frequency's on an analog one, and a number typed always
   wins. "Match TG 1 → TG Number 45351" remaps; "Match TG blank, TG Number 46012" folds every
   talkgroup heard on the channel into one.
3. **Names** = the row's, if it has any; else the system's talkgroup file by the number filed
   under; else none. (Today's `info_for` also looks for *another* row whose `talkgroup` equals the
   air's number; that goes.)
4. **A new talkgroup on the air is a new call**, as today — compared on the air's number, not the
   output, so a catch-all that folds TG 1 and TG 2 into 46012 still cuts between them.

The user's example reads directly:

```csv
Frequency,Mode,Code,Slot,Match TG,TG Number,Alpha Tag,Description,Tag,Category,Squelch dB,Enable
460.1250,p25,,,,46012,Townville PD,,Law Dispatch,Police,,true
453.5000,p25,293,,1,45351,Townville PW,,Public Works,Public Works,,true
453.5000,p25,293,,2,45353,Townville Parks,,Parks,Public Works,,true
453.5000,p25,293,,,,Townville (other),,Public Works,Public Works,,true
451.5000,dmr,CC1,1,1,9001,Dispatch,,Business,Business,,true
451.5000,dmr,CC1,,,,451.500 Repeater,,Business,Business,,true
```

The last 453.5 row files under whatever the air says (TG 3 → 3), named "Townville (other)"; the
last 451.5 row files under the air's talkgroup with the repeater's name, unless the talkgroup
file names it.

### Keeping the air's number

When a row remaps, the number the radio sent is worth keeping: add `talkgroup_ota` (or so) to the
call record, the way `site` and `decrypted` were added as this recorder's fields, written only
when it differs from `talkgroup`. Plugins that key on talkgroup see the output; the file keeps
the truth. Optional, and a small SDK change.

## What changes where

**Engine (`conventional.rs`)** — small.
- `Access` becomes one shape for every mode: `{ code: Option<Code>, slot: Option<u8>, tg:
  Option<u32> }`, `Code` being `Tone | Nac | ColorCode | Ran`. P25 gains `tg`; the FM arm of
  `admits` matches the detected tone against `code`.
- `ConvChannel.talkgroup: u32` becomes `talkgroup: Option<u32>` beside the computed frequency
  default; `slot_call` applies rule 2. The `rows.len() > 1` special case for P25 in
  `route_digital` goes; `route_digital` and `pick` share one matcher.
- `info_for` becomes rule 3.

**Config (`config.rs`, `migrate.rs`, `channels.rs`)**.
- `Channel.tone` (the combined string) becomes `code`, `slot`, `matchTalkgroup`; `tone` is read
  as the old combined form and split (the parser exists: `Access::parse`). RadioReference's
  `CC1 TS2 TG201` pasted into `Code` still works, by the same split.
- `channel_talkgroups` (the Setup placeholder and the engine default) becomes "what this row
  files under when the air names nothing".
- CSV: columns `Code`, `Slot`, `Match TG` (and `TG Number` as today); `Tone` accepted as the old
  combined column. No version flag: a file with today's columns reads as today. The writer
  (`Export CSV`, creating a linked file) writes the new columns. `rules.rs` exports the reader and
  writer already, so the browser version and Setup get it for free.
- Upgrade, so no existing config files differently: a P25 row on a frequency with several rows
  and no `talkgroup` gets today's default written in as its `TG Number` (that is what those rows
  file under now). A DMR / NXDN row whose `tone` names a TG and has no `talkgroup` is unchanged
  under the new rules (air's number = matched number).

**Behaviour changes to note in the changelog** (deliberate: the column now means what it says):
a typed `TG Number` on a single-row P25, DMR or NXDN channel wins over the air's number, where
it was ignored before.

**Setup**: the channel table shows `Code`, `Slot` (DMR rows) and `Match TG` (digital rows) in
place of the one tone input; the `TG Number` placeholder says what a blank files under ("from
the air, else 460125"). The "+ NAC / + code" button becomes "+ rule on this frequency". The
browser version has the same rows.

**Docs**: `docs/configuration/conventional.md` (keys, the CSV, the filing rules as the table
above), `docs/guides/conventional.md` (present the two kinds as two workflows: "a channel you
name" and "a channel whose talkgroups come over the air", both on the same row), the talkgroup
file section.

## Why one model and not two

Two kinds of system (one for named channels, one for over-the-air ones) would mean two CSV
formats, two Setup tables and two routing paths, for rows that differ only in which columns
apply — and a frequency that carries P25 and gets named by a row (46012 above) belongs to both.
The architecture review just folded conventional's pipeline into the engine's; this keeps it
one. The user sees the difference where it matters: which columns a mode shows, and what a blank
`TG Number` means for that mode.
