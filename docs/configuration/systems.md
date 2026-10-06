# Trunked Systems

Each entry in `systems` is one trunked system, or one **site** of a multi-site system. This page lists
the keys for P25, SmartNet, DMR and NXDN systems, the talkgroup file format, unit names and the site
lock. For how to set each kind up, see the guides: [P25](../guides/p25.md),
[SmartNet](../guides/smartnet.md), [DMR](../guides/dmr.md), [NXDN](../guides/nxdn.md),
[multi-site](../guides/multi-site.md) and [talkgroups and units](../guides/talkgroups-and-units.md).

```json
"systems": [
  { "shortName": "dcfd", "type": "p25", "controlChannelsHz": [857987500, 858987500],
    "talkgroupsCsv": "Decimal,Alpha Tag,Mode\n1201,FD Disp,D\n", "expect": { "nac": 1091 } },
  { "shortName": "wmata", "type": "smartnet", "controlChannelsHz": [489087500],
    "bandplan": "400_custom", "bandplanBaseHz": 489012500, "bandplanSpacingHz": 12500,
    "bandplanOffset": 380, "bandplanHighHz": 490000000 }
]
```

A trunked system shares a handful of voice frequencies among many talkgroups. A **control channel**
continuously announces which talkgroup is being given (granted) which frequency, and the recorder
follows those grants. A system is recorded only when it is `enabled` **and** has at least one control
channel. Each site you record from its own control channel is its own entry, with its own
`shortName` and folder, as in Trunk Recorder.

## Keys for every trunked system

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `shortName` | | `"sys1"` | string | The system's identity: its recordings folder, the `short_name` in call JSON, its learned band plan and talker alias files, and how plugins know it. Every system, trunked or conventional, enabled or not, needs its own. Keep it to letters, digits, `-` and `_` |
| `name` | | `""` | string | What people call it ("County Public Safety"), for display |
| `type` | | `"p25"` | string | `"p25"`, `"smartnet"`, `"dmr"` or `"nxdn"` (any case). Anything else is treated as P25 |
| `enabled` | | `true` | bool | `false` keeps it in the config without recording it |
| `controlChannelsHz` | | `[]` | array of numbers | P25 and SmartNet: the control channel and its alternates, Hz. The recorder hunts through them, moving on when one goes quiet for 5 s. DMR and NXDN: every frequency listed is watched at once (NXDN Type-D: the repeaters) |
| `talkgroupsCsv` | | `""` | string | The talkgroup file's **contents**, not a path. See [Talkgroup file](#talkgroup-file) |
| `talkgroupsName` | | `""` | string | The name of the file the talkgroups came from, for display |
| `unitNames` | | empty | object | Names for the system's radios. See [Unit names](#unit-names) |
| `expect` | | `{}` | object | Site lock: only follow a control channel with this identity. See [Site lock](#site-lock-expect) |
| `voiceChannelsHz` | | `[]` | array of numbers | Voice channels Find my system heard. Used only to place Auto sources so they cover them too |
| `siteGroup` | | `""` | string | Groups sites whose calls are the same calls. See [Multi-site](#multi-site) |
| `recording` | | `{}` | object | This system's own call rules; a rule left out follows the global `recording`. See [recording.md](recording.md#per-system-rules) |
| `plugins` | | `{}` | object | This system's plugin settings, by plugin id. See [plugins.md](plugins.md#per-system-settings) |

## P25 and SmartNet

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `modulation` | | `"auto"` | string | `"auto"`: run both receivers and keep the better result for each frame. `"qpsk"`: CQPSK / LSM simulcast only. `"fsk4"`: C4FM only. Any other value is `"auto"`. Auto costs more CPU; set the one your system uses if you know it |

P25 Phase 2 voice needs no setting: a Phase 2 grant is followed as Phase 2.

## SmartNet

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bandplan` | | `""` (= `800_standard`) | string | `"800_standard"`, `"800_reband"`, `"800_splinter"`, `"900"` or `"400_custom"` (VHF, UHF and OBT systems). Trunk Recorder's spellings `800_domestic`, `800_rebanded`, `800_domestic_splinter` and `obt` work too, as does any name starting `400` |
| `bandplanBaseHz` | with `400_custom` | | number | Frequency of channel number `bandplanOffset`, Hz |
| `bandplanSpacingHz` | with `400_custom` | | number | Channel spacing, Hz |
| `bandplanOffset` | | `0` | integer | The first channel number |
| `bandplanHighHz` | with `400_custom` | | number | Top of the band, Hz. Must be above `bandplanBaseHz` |
| `defaultMode` | | `""` (digital) | string | `"analog"`: a talkgroup never seen granted before is recorded as analog FM. Otherwise as P25 digital |

Find my system fills in the four `400_custom` numbers for SmartNet OBT systems.

## DMR

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `dmrChannelsHz` | | `[]` | array of numbers | Voice frequencies to watch besides the control channels, Hz (Trunk Recorder's `channels`; for Capacity Plus, every repeater of the site). Each must be inside a source |
| `lcnTableHz` | | `{}` | object | Logical channel number → frequency, Hz: `{ "101": 452275000 }` (Trunk Recorder's `lcnTable`). Wins over channels learned from the air; channels left out are learned |
| `colorCode` | | `null` | integer or null | Only this colour code, 0–15. `null`: the control channel's |

DMR sites announce no system identity, so `expect` is ignored; use `colorCode`, and give the sites of
one system the same `siteGroup`.

## NXDN

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `nxdnType` | | `""` (= `"typeC"`) | string | `"typeC"`: a control channel assigns calls to channels (Kenwood NEXEDGE, Icom IDAS Type-C). `"typeD"`: IDAS distributed trunking with no control channel; list every repeater in `controlChannelsHz` |
| `nxdnRate` | | `""` (= `"nxdn48"`) | string | The control channel's rate (Type-D: the repeaters'): `"nxdn48"` (4800 bps, 6.25 kHz) or `"nxdn96"` (9600 bps, 12.5 kHz). Voice channels go at the rate each grant names |
| `nxdnChannelsHz` | | `[]` | array of numbers | Voice frequencies to watch besides the control channels, Hz. A grant to a channel number not in `lcnTableHz` is learned when its call comes up on one of them. Each must be inside a source |
| `lcnTableHz` | | `{}` | object | Channel number → frequency, Hz: `{ "12": 451118750 }` (RadioReference lists them as LCNs for many systems). Wins over channels learned from the air. Not needed when the site uses Direct Frequency Assignment. Type-D: repeater number → frequency |
| `ran` | | `null` | integer or null | Only this RAN, 0–63. `null`: the control channel's |

An NXDN Type-C grant names a channel number. Its frequency comes from `lcnTableHz`; from the grant
itself when the site uses Direct Frequency Assignment; or is learned when the granted group's call
comes up on a frequency in `nxdnChannelsHz`. Until then the grant is logged as "frequency not known
yet", and the dashboard lists the channel numbers still unknown. Learned tables are kept in
`<shortName>.bandplan` beside the config.

## Site lock (`expect`)

A control channel announcing a different identity than the lock is not followed: its grants are
ignored and the recorder moves on to the next control channel. Until every locked field has been
heard, grants wait. Use it to keep one site of a multi-site system from wandering onto a neighbour on
a nearby frequency.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `nac` | | (any) | integer | P25 NAC |
| `wacn` | | (any) | integer | P25 WACN |
| `sysId` | | (any) | integer | P25 or SmartNet System ID; NXDN Type-C system code |
| `rfss` | | (any) | integer | P25 RFSS |
| `site` | | (any) | integer | P25 site; SmartNet site (when the system sends it); NXDN Type-C site code |

Values are plain **decimal** numbers in the file, although the interface (under **Site lock**) shows
NAC, WACN and System ID in hex. NAC `0x443` is `"nac": 1091`.

Only the fields a protocol announces are compared:

- **P25**: all five.
- **SmartNet**: `sysId` and `site`. Lock `site` only if the system sends it (OBT systems do),
  otherwise grants wait for it forever.
- **NXDN Type-C**: `sysId` and `site`, from the site's SITE_INFO.
- **DMR and NXDN Type-D**: none; use `colorCode` or `ran` instead.

## Multi-site

A call on a multi-site system is usually granted on several sites at once. Each copy is recorded;
when the last one ends, only the best copy is saved (the one decoded most cleanly, or the talkgroup's
Preferred Site). Turn that off with `recording.dropDuplicateCalls` to keep every copy.

Sites are grouped into one system by what their control channels announce: P25 by WACN and System ID,
SmartNet by System ID, NXDN Type-C by system code. Two entries following the *same* site are never
grouped.

A non-empty `siteGroup` replaces that grouping:

- Systems with the same `siteGroup` are grouped.
- A `siteGroup` used by only one system keeps that system on its own.

Give DMR and NXDN Type-D sites a group name, since they announce no system identity. Give systems
linked by ISSI one too. To prefer a site for a talkgroup, use the talkgroup file's `Preferred Site`
or `Preferred NAC` column.

## Talkgroup file

`talkgroupsCsv` holds a Trunk Recorder talkgroup file, as text. RadioReference's talkgroup CSV export
reads as is. The interface loads it from a file or a RadioReference paste; by hand, put the file's
contents in the string with each line ending in `\n`.

```csv
Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category,Priority
1201,4b1,D,FD Disp,Fire Dispatch,Fire Dispatch,DC Fire,1
1202,4b2,DE,FD Tac,Fire Tac (encrypted),Fire-Tac,DC Fire,2
```

It comes in two forms:

- **With a header row**, if the first cell is `Decimal` (or RadioReference's `DEC`). These columns
  are recognised, in any order and any case: `Decimal`, `Hex`, `Mode`, `Alpha Tag` (or `AlphaTag`),
  `Description`, `Tag`, `Category` (or `Group`), `Priority`, `Preferred NAC`, `Preferred Site`,
  `Ignore`. Others are ignored.
- **Without a header**, the columns are, in order:
  `Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority]`.

Parsing:

- The delimiter is guessed from the first line: commas, semicolons, tabs or bars. Fields can be
  quoted (`"Fire, Dispatch"`).
- Blank lines and lines starting with `#` are skipped. A byte-order mark is ignored.
- A row whose `Decimal` isn't a whole number is skipped.

| Column | Meaning |
|---|---|
| `Decimal` | The talkgroup number |
| `Mode` | `E`, `TE` or `DE` mean encrypted: such talkgroups aren't recorded unless `recordEncrypted` is on. Other letters (`D`, `A`, `T`…) are informational |
| `Alpha Tag`, `Description`, `Tag`, `Category` | Names, written to the call JSON and shown in the interface |
| `Priority` | A negative priority means never record (as in Trunk Recorder). Default 1 |
| `Ignore` | `true`, `yes`, `y`, `1`, `x` or `ignore` (any case): never record |
| `Preferred Site` | Multi-site: a site's `shortName`. Its copy of a call is kept when it has at least 90 % of the best copy's clean audio |
| `Preferred NAC` | The same, by the site's NAC, or by RFSS and site written `RRRRssss` (RFSS 1, site 23: `10023`) |

A talkgroup not in the file is still recorded unless `recordUnknown` is off; see
[recording.md](recording.md#recording-keys). Changes to the talkgroup file apply at once, even while
recording.

## Unit names

`unitNames` holds Trunk Recorder's unit tags file, as text, and how its names combine with talker
aliases heard over the air. Each trunked and conventional system has its own.

```json
"unitNames": {
  "csv": "1201234,Engine 12\n/^14(\\d{2})$/,Medic $1\n",
  "name": "dcfd-units.csv",
  "mode": "user"
}
```

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `csv` | | `""` | string | The file's **contents**: `unit,name` lines with no header, `#` for comments. A unit written between slashes (`/^14(\d{2})$/`) is a regular expression; `$1` or `\1` in the name inserts its groups. The first match wins |
| `name` | | `""` | string | The file it came from, for display |
| `mode` | | `""` (= `"user"`) | string | `"user"`: your names first, then talker aliases heard. `"ota"`: aliases heard first. `"user_only"`: only your names. `"none"`: no names |

Talker aliases heard over the air are kept in `<shortName>.units.csv` beside the config file
([Files kept beside the config](README.md#files-kept-beside-the-config)).
