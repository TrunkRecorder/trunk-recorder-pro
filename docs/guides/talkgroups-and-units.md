# Talkgroups and units

This page explains how to give a trunked system its talkgroup names (from a CSV file or pasted
from RadioReference), how to choose which talkgroups are recorded, how to name radios, and how
talker aliases are learned over the air. The talkgroup and unit name keys are listed in the
[trunked systems reference](../configuration/systems.md).

## The talkgroup file

A trunked system works without a talkgroup file: every call is recorded and filed by talkgroup
number. With one, calls get names (the alpha tag, description and category go into the call JSON
and can be used in file names), and you can choose what to record.

Each system has its own. It's the **Talkgroups** field on the system card in **Setup** →
**Systems**:

- **Load CSV…** loads a Trunk Recorder talkgroup CSV.
- **Paste from RadioReference…** reads a talkgroup table copied from RadioReference's web page.
  See [Pasting from RadioReference](#pasting-from-radioreference).
- **Download** saves the system's talkgroup file as a CSV (named after the file you loaded, or
  `<shortName>-talkgroups.csv`).
- **Clear** removes it.

The file isn't kept as a path: its text goes into the config, in the system's `talkgroupsCsv`,
with the original file name in `talkgroupsName`. To change it later, edit your copy and load it
again, or **Download**, edit, and load. Changes made while recording take effect at once (the log
says "Talkgroups updated").

When a file is loaded, chips under the field sum it up: how many talkgroups are **ignored** and
**encrypted**, and warnings for **no header row**, a missing **Mode** or **Description** column
(Trunk Recorder needs those, Trunk Recorder Pro doesn't), **unknown columns**, and **rows without
a number**.

### Format

Trunk Recorder Pro reads Trunk Recorder's talkgroup CSV, in either of its forms.

**With a header row** (the first column is `Decimal`): the columns can be in any order, and only
`Decimal` is required.

```csv
Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Priority
101,65,FD Disp,D,Fire Dispatch,Fire Dispatch,Fire,1
102,66,FD Tac 2,D,Fire Tactical 2,Fire-Tac,Fire,
203,cb,PD Tac,DE,Police Tactical,Law Tac,Police,
```

**Without a header** (Trunk Recorder's older form), the columns are fixed:
`Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority]`.

```csv
101,65,D,FD Disp,Fire Dispatch,Fire Dispatch,Fire
```

Either way:

- Fields are separated by commas, semicolons, tabs or `|`, whichever the first line uses (a
  spreadsheet set to a decimal-comma language saves with semicolons). A file loaded in Setup with
  anything but commas is rewritten with commas.
- Header names can be in any case. RadioReference's `DEC` and `Group` are read as `Decimal` and
  `Category`, and `AlphaTag` as `Alpha Tag`.
- Fields with a comma in them go in double quotes.
- Blank lines and lines starting with `#` are skipped, as is a spreadsheet's byte-order mark.
- A row whose `Decimal` isn't a number is skipped.

### Columns

| Column | Used for |
|---|---|
| `Decimal` | the talkgroup number (required) |
| `Hex` | nothing (it's the same number) |
| `Alpha Tag` | the talkgroup's short name: shown everywhere, `talkgroup_tag` in the call JSON |
| `Description` | `talkgroup_description` in the call JSON |
| `Tag` | the service type ("Fire Dispatch"): `talkgroup_group_tag` in the call JSON |
| `Category` (or `Group`) | the group it belongs to: `talkgroup_group` in the call JSON |
| `Mode` | see [Mode](#mode) |
| `Priority` | only `-1` matters: it means ignore, as in Trunk Recorder |
| `Ignore` | `x`, `yes`, `y`, `true`, `1` or `ignore`: never record it. Trunk Recorder Pro's own column |
| `Preferred Site` | multi-site: the short name of the site whose copy to keep |
| `Preferred NAC` | multi-site: the same as Trunk Recorder writes it (a NAC, or RFSS and site as `RRRRssss`, in decimal) |

The multi-site columns are explained in
[Several systems and sites](multi-site.md#calls-heard-on-several-sites).

If you're coming from Trunk Recorder: `Priority` doesn't decide which calls get a recorder when
they run short; there is no such ranking.

### Mode

The `Mode` column uses RadioReference's letters. Two things depend on it:

| Mode | Effect |
|---|---|
| `E`, `DE`, `TE` | encrypted: not recorded (unless `recordEncrypted` is on); the call is still followed for its unit IDs and aliases |
| anything starting with `A` (`A`, `AE`, `Ae`) | analog: the call is recorded as analog FM, whatever the control channel says |
| `D`, `T`, `M`, `De`, `Te`, … | nothing special |

`De` and `Te` (encrypted only sometimes) are recorded. Analog only makes sense on a SmartNet
system with analog channels; see [SmartNet](smartnet.md#analog-and-digital-voice).

## Choosing what to record

- **Ignore a talkgroup.** Put `x` in its `Ignore` column. Or, on the **Radio system** page, click
  **Ignore** next to the talkgroup; it becomes **Ignored ✕**, click again to record it again. That
  button edits the system's talkgroup file for you: it adds an `Ignore` column if there isn't one
  (and a header row to a file without one), and adds a row for a talkgroup that isn't in the file.
- **Only the talkgroups in the file.** Turn off **Talkgroups not in the talkgroup file** under
  **Call rules** on the **Recording** tab (`recordUnknown`, on by default), or for one system in its
  **Recording override**. This only has an effect once the system has a talkgroup file; with none,
  everything is recorded. A talkgroup that isn't in the file is still recorded while it's patched
  with one that is (a patch's supergroup usually isn't in anyone's file).
- **Encrypted talkgroups** aren't recorded; see the protocol pages ([P25](p25.md#encrypted-calls)).

The **Radio system** page's **What happened to calls** shows how each call went: **Recorded**,
**Followed only**, **Ignored**, **Encrypted**, **Not in the file**, **No recorder** or **Out of
band**.

## Pasting from RadioReference

RadioReference's talkgroup tables are free to view, no subscription needed. To use one:

1. Open your system's page in the
   [RadioReference database](https://www.radioreference.com/db/browse/) and find the talkgroup
   tables.
2. Select the tables, from a category heading down to the last row (several categories at once
   is fine), and copy.
3. On the system card, click **Paste from RadioReference…** and paste into the box. If the system
   has a site lock, its System ID and WACN are shown above the box, to check you're on the right
   RadioReference page.
4. Check the preview, then click **Use these** (or **Replace … with …** if the system already had
   talkgroups).

What it reads from the paste:

- Each row's number, mode, alpha tag, description and tag. The `HEX` column is skipped if it's
  there.
- A line on its own above a run of rows (the table's heading, such as "County Fire") becomes those
  talkgroups' `Category`.
- RadioReference's badges: `ENC` after a mode marks the talkgroup encrypted (`D` becomes `DE`,
  `T` becomes `TE`, anything else `E`); `ONLINE` after a number is skipped.
- A row without a mode gets `D`.

The result is stored as a normal talkgroup file named "RadioReference (pasted)", so **Download**
gives you a CSV you can edit and load again.

## Unit names

A system can have its own names for radio IDs ("Engine 12" for 1234). This is Trunk Recorder's
`unitTagsFile`. On the system card, **Unit names** → **Load CSV…** loads one; **Clear** removes it.
Like the talkgroup file, it's kept inside the config, under the system's `unitNames`.

The file has no header. Each line is a unit ID and its name:

```csv
# Fire department radios
1234,Engine 12
1235,Truck 4
/^7(\d{3})$/,Medic $1
```

- A plain number names only that radio.
- A pattern between slashes is a regular expression matched against the unit ID. The name can use
  its groups as `$1` or `\1`: the line above names radio 7042 "Medic 042".
- The first line that matches wins.
- Lines starting with `#` are skipped, and so are lines whose pattern isn't a valid regular
  expression.

Next to **Load CSV…**, a menu chooses which names come first, between this file and the talker
aliases radios send (below). It is Trunk Recorder's `unitTagsMode`:

| Menu | `mode` | A radio is named by |
|---|---|---|
| **These first, then aliases heard** | `user` (default) | this file; failing that, its talker alias |
| **Aliases heard first** | `ota` | its talker alias; failing that, this file |
| **Only these** | `user_only` | this file only |
| **No names** | `none` | nothing |

With no unit names file, a radio is named by its talker alias.

```json
"unitNames": { "csv": "1234,Engine 12\n/^7(\\d{3})$/,Medic $1\n", "name": "units.csv", "mode": "ota" }
```

The name chosen goes into each source's `tag` in the call JSON's `srcList`; the talker alias heard,
if any, goes into `tag_ota` whatever the mode. The **Calls** page shows names next to unit IDs.

Conventional systems take a unit names file too, on their own card.

## Talker aliases

Many P25 radios send their programmed name ("E12 CAPT") along with their unit ID while they talk:
a *talker alias*. Trunk Recorder Pro reads them on P25 voice channels, Phase 1 and Phase 2, in
Motorola's and Harris's formats, and learns them as it goes. There is nothing to turn on.

- Each new or changed alias is logged: `Unit 1234 is "E12 CAPT" (TG 2207)`.
- Aliases are kept per system in `<shortName>.units.csv`, in the folder the config file is in,
  and loaded on every start, so a radio keeps its name between runs.
- The file is in Trunk Recorder's `unitTagsOTA` format (no header;
  `unitID,alias,source,timestamp,WACN,SYS,talkgroup`; the newest line for a unit wins). You can
  copy a Trunk Recorder alias file into place under the system's short name to start with what it
  had learned.
- Encrypted calls are still followed for their aliases, because the alias is usually sent in the
  clear. When a system encrypts that too, the log says so once per talkgroup ("Talkgroup 2207:
  link control is encrypted, so its radios' talker aliases can't be read").

DMR and NXDN talker aliases aren't read.

## Common problems

**"no header row" chip.** The file is in Trunk Recorder's headerless form. It loads fine; the
columns are read in the fixed order. **Download** saves it with a header row added
(`Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category`, plus `Priority` when a row has one), and so
does ignoring a talkgroup from the **Radio system** page.

**"unknown column" chip.** A header isn't one of the names above (check the spelling). The column
is kept but not used.

**Talkgroups recorded that you didn't want.** Turn off **Talkgroups not in the talkgroup file**, or
mark them ignored.

**A unit name doesn't apply.** Check that the unit names menu isn't on **Aliases heard first** (an
alias wins there) or **No names**, and that no earlier line in the file matches the same ID.
