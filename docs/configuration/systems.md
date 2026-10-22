# Trunked Systems

Each entry in `systems` is one trunked system: its talkgroups, radios and rules, and its `sites`,
each a control channel to follow. This page lists the keys for P25, SmartNet, DMR and NXDN systems
and their sites, the talkgroup file format, unit names and the site lock. For how to set each kind
up, see the guides: [P25](../guides/p25.md), [SmartNet](../guides/smartnet.md),
[DMR](../guides/dmr.md), [NXDN](../guides/nxdn.md), [multi-site](../guides/multi-site.md) and
[talkgroups and units](../guides/talkgroups-and-units.md).

```json
"systems": [
  { "shortName": "dcfd", "type": "p25",
    "sites": [{ "controlChannelsHz": [857987500, 858987500], "expect": { "nac": 1091 } }] },
  { "shortName": "wmata", "type": "smartnet",
    "sites": [{ "controlChannelsHz": [489087500], "bandplan": "400_custom", "bandplanBaseHz": 489012500,
                "bandplanSpacingHz": 12500, "bandplanOffset": 380, "bandplanHighHz": 490000000 }] }
]
```

A trunked system shares a handful of voice frequencies among many talkgroups. A **control channel**
continuously announces which talkgroup is being given (granted) which frequency, and the recorder
follows those grants. A system is recorded only when it is `enabled` **and** has an enabled site
with at least one control channel. A system with several sites, each with its own control channel,
is still one entry: one short name, one folder, one talkgroup file. (Trunk Recorder makes each site
a system of its own.)

## Keys for every trunked system

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `shortName` | | `"sys1"` | string | The system's identity: its recordings folder, the `short_name` in call JSON, its talker alias file, and how plugins know it. Every system, trunked or conventional, enabled or not, needs its own. Keep it to letters, digits, `-` and `_` |
| `name` | | `""` | string | What people call it ("County Public Safety"), for display |
| `fcc` | | `{}` | object | Optional: who is licensed on each of its frequencies. See [FCC licenses](#fcc-licenses) |
| `share` | | `false` | bool | Share it with the community directory while it records. See [Sharing with the directory](#sharing-with-the-directory) |
| `directory` | | none | object | Set by sharing: the directory's record of it, `{ id, rev, sharedAt }` (its ID, its version when last shared, when, Unix ms). Shares land on that record |
| `type` | | `"p25"` | string | `"p25"`, `"smartnet"`, `"dmr"`, `"nxdn"` or `"edacs"` (any case). Anything else is refused when the config is read |
| `enabled` | | `true` | bool | `false` keeps it in the config without recording it |
| `sites` | | `[]` | array | Where it is recorded from: one entry per site. See [Keys for each site](#keys-for-each-site) |
| `unitNames` | | empty | object | Names for the system's radios. See [Unit names](#unit-names) |
| `keys` | | `""` | string | P25: the encryption keys it decrypts with, a key source's name (desktop app only; not in the interface). See [Encryption keys](#encryption-keys-keys) |
| `recording` | | `{}` | object | This system's own call rules; a rule left out follows the global `recording`. See [recording.md](recording.md#per-system-rules) |
| `plugins` | | `{}` | object | This system's plugin settings, by plugin id. See [plugins.md](plugins.md#per-system-settings) |
| `pluginDelays` | | `[]` | array | How long plugins wait for this system's calls (all of them, some talkgroups', some plugins'). See [plugins.md](plugins.md#delays) |

## FCC licenses

`fcc` (a system's, or a [conventional system's](conventional.md)) says who is licensed on each of
its frequencies, keyed by frequency in Hz (on the 6.25 kHz channel raster, as a string). A system
can hold several licenses, and a conventional system can mix licensees, so each frequency has its
own. It's optional and for reference; plugins are told it.

```json
"fcc": {
  "857987500": { "callSign": "WPXT459", "name": "DISTRICT OF COLUMBIA", "from": "lookup" },
  "859087500": { "callSign": "KNJA391", "from": "air" }
}
```

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `callSign` | ✓ | | string | The license's call sign |
| `name` | | `""` | string | Who holds it, as the FCC has it |
| `from` | | `"user"` | string | `"user"` (typed in; never replaced), `"air"` (heard: a Motorola base station ID) or `"lookup"` (Identify) |

The recorder fills it as it hears Motorola P25 base station IDs: a control channel's, for each
channel it names, and a voice channel's for itself. A call sign heard replaces a lookup's (keeping
its name when it's the same call sign), never yours. **Identify** on the system's card looks up
licensees near you, and names call signs heard on the air (sending only the call signs). See
[P25](../guides/p25.md#fcc-call-signs-motorola-base-station-id).

The call sign that names the system (plugins' `fcc_call_sign`) is its control channel's, else the
one on the most of its frequencies.

## Sharing with the directory

With `share` on (**Share channel and talkgroup config with the community** in the system's Basics), the
recorder shares the system with the community directory at
[trunkrecorder.pro/directory](https://trunkrecorder.pro/directory/) while it records. Only P25, SmartNet,
DMR and NXDN systems can be shared for now.

What it sends is what the radio heard of the system:
- each site that's decoded: its identity, control channels, modulation, band plan, and the neighbours it
  announces;
- the voice channels granted there, with how many grants;
- the talkgroups granted: their numbers, how many grants, at which sites, whether encrypted;
- call signs heard on the air (the `fcc` entries `from` the air).

It doesn't send talkgroup names, descriptions, tags or groups, anything typed or looked up, radio IDs,
aliases, audio, keys, or a location. Each share carries an anonymous ID for the install (kept in the data
folder as `directory-recorder-id`; the browser build keeps its own).

It first shares after the system has been decoded for 5 minutes, then again when it hears a talkgroup or
voice channel it hadn't (at most once an hour), and otherwise once a day. A failed share is tried again
later, waiting longer each time (up to 6 hours). Setup shows when it last shared, or why it couldn't, with
a link to the system's page in the directory. The directory's answer is kept as `directory` and each site's
`directoryId`, so later shares, and other recorders hearing other sites, build the same record.

## Keys for each site

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `name` | | the system's `shortName` | string | The site's name, unique in its system: what the talkgroup file's `Preferred Site` names, the `site` in call JSON and the log. Leave it out for a system of one site |
| `enabled` | | `true` | bool | `false` keeps the site without recording from it (**Record** on its card) |
| `directoryId` | | `""` | string | Set by [sharing](#sharing-with-the-directory): the directory's ID for the site |
| `controlChannelsHz` | | `[]` | array of numbers | P25 and SmartNet: the control channel and its alternates, Hz. The recorder hunts through them, moving on when one goes quiet for 5 s. DMR and NXDN: every frequency listed is watched at once (NXDN Type-D: the repeaters) |
| `expect` | | `{}` | object | Site lock: only follow a control channel with this identity. See [Site lock](#site-lock-expect) |
| `voiceChannelsHz` | | `[]` | array of numbers | Voice channels Find my system heard. Used only to place Auto sources so they cover them too |

The protocol keys below are a site's too, except `defaultMode` (SmartNet) and `nxdnType` (NXDN),
which are the system's.

## P25 and SmartNet

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `modulation` | | `"auto"` | string | `"auto"`: detect it — both receivers run until one clearly decodes better (a few seconds), then only that one, on each channel; detected again when the control channel moves or after 10 s without a frame. `"qpsk"`: CQPSK / LSM simulcast only. `"fsk4"`: C4FM only (any case; anything else is refused when the config is read). The log says what was detected; set the one your system uses if you know it |

P25 Phase 2 voice needs no setting: a Phase 2 grant is followed as Phase 2.

## SmartNet

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bandplan` | | `""` (= `800_standard`) | string | `"800_standard"`, `"800_reband"`, `"800_splinter"`, `"900"` or `"400_custom"` (VHF, UHF and OBT systems). Trunk Recorder's spellings `800_domestic`, `800_rebanded`, `800_domestic_splinter` and `obt` work too, as does any name starting `400` |
| `bandplanBaseHz` | with `400_custom` | | number | Frequency of channel number `bandplanOffset`, Hz |
| `bandplanSpacingHz` | with `400_custom` | | number | Channel spacing, Hz |
| `bandplanOffset` | | `0` | integer | The first channel number |
| `bandplanHighHz` | with `400_custom` | | number | Top of the band, Hz. Must be above `bandplanBaseHz` |
| `defaultMode` | | `"digital"` | string | `"analog"`: a talkgroup never seen granted before is recorded as analog FM. `"digital"` (or `""`): as P25 digital. Any other value is refused |

Find my system fills in the four `400_custom` numbers for SmartNet OBT systems.

## DMR

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `dmrChannelsHz` | | `[]` | array of numbers | Voice frequencies to watch besides the control channels, Hz (Trunk Recorder's `channels`; for Capacity Plus, every repeater of the site). Each must be inside a source |
| `lcnTableHz` | | `{}` | object | Logical channel number → frequency, Hz: `{ "101": 452275000 }` (Trunk Recorder's `lcnTable`). Wins over channels learned from the air; channels left out are learned |
| `colorCode` | | `null` | integer or null | Only this colour code, 0–15. `null`: the control channel's |

DMR sites announce no system identity, so `expect` is ignored; use `colorCode`, and list the sites
of one system under it.

## NXDN

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `nxdnType` | | `"typeC"` | string | `"typeC"`: a control channel assigns calls to channels (Kenwood NEXEDGE, Icom IDAS Type-C). `"typeD"`: IDAS distributed trunking with no control channel; list every repeater in `controlChannelsHz` |
| `nxdnRate` | | `"nxdn48"` | string | The control channel's rate (Type-D: the repeaters'): `"nxdn48"` (`"48"`, `"4800"`; 4800 bps, 6.25 kHz) or `"nxdn96"` (`"96"`, `"9600"`; 9600 bps, 12.5 kHz). Voice channels go at the rate each grant names |
| `nxdnChannelsHz` | | `[]` | array of numbers | Voice frequencies to watch besides the control channels, Hz. A grant to a channel number not in `lcnTableHz` is learned when its call comes up on one of them. Each must be inside a source |
| `lcnTableHz` | | `{}` | object | Channel number → frequency, Hz: `{ "12": 451118750 }` (RadioReference lists them as LCNs for many systems). Wins over channels learned from the air. Not needed when the site uses Direct Frequency Assignment. Type-D: repeater number → frequency |
| `ran` | | `null` | integer or null | Only this RAN, 0–63. `null`: the control channel's |

An NXDN Type-C grant names a channel number. Its frequency comes from `lcnTableHz`; from the grant
itself when the site uses Direct Frequency Assignment; or is learned when the granted group's call
comes up on a frequency in `nxdnChannelsHz`. Until then the grant is logged as "frequency not known
yet", and the dashboard lists the channel numbers still unknown. Learned tables are kept per site
beside the config: `<shortName>.bandplan`, or `<shortName>.<site>.bandplan` for a site with a name
of its own.

## EDACS

EDACS Standard and Networked sites (GE / Ericsson / Harris), with analog FM and ProVoice voice.
Extended Addressing (EA) sites aren't decoded yet.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `lcnTableHz` | ✓ | `{}` | object | The site's channel list: LCN (1–25) → frequency, Hz, `{ "1": 856187500, "3": 857712500 }`. RadioReference lists each site's channels with their LCNs |
| `esk` | | `false` | boolean | The system scrambles its control channel with ESK 0xA0 (some Harris systems): its messages don't decode without it |

The control channel grants each call a channel number (LCN) and says whether its voice is analog
or ProVoice; `lcnTableHz` turns the number into a frequency. Every frequency of the site (its
`controlChannelsHz` and `lcnTableHz`) is watched at once, so the control channel is found wherever
the site has moved it, and the list is checked against the air: when a call is assigned an LCN,
the carrier that keys just then is that channel. Once three calls agree, the LCN is that frequency,
whatever the list says, and the log says so ("LCN 4 is 860.1125 MHz on the air, not 857.0375 MHz
as the channel list has it"); what was learned is kept beside the config (`<shortName>.<site>.bandplan`)
for the next start. Only listed frequencies are watched: a call on an LCN whose frequency is in
neither list is logged once and not recorded, so add the frequency to `lcnTableHz`. A grant to the frequency the control channel is on is never
followed, whichever LCN it names. A site ID that disagrees with the list's numbers corrects them only
once three in a row agree. The site lock (`expect`) compares the site number the control channel
announces (`site`).

On an analog call, the 9600 baud signalling burst at the start of each transmission (on the voice
channel) is left out of the audio.

Groups are AFS numbers, written in decimal in the talkgroup file (RadioReference's DEC column:
AFS 03-091 is 457); radios are logical IDs (LIDs). A talkgroup marked `DE` (digital, encrypted) is
skipped as encrypted. ProVoice is decoded with the same vocoder as P25 Phase 1 (IMBE); the call JSON
says `"audio_type": "digital"`, analog calls `"analog"`.

## Site lock (`expect`)

A control channel announcing a different identity than the lock is not followed: its grants are
ignored and the recorder moves on to the next control channel. Until every locked field has been
heard, grants wait. Use it to keep a site from wandering onto a neighbouring site, or another
system, on a nearby frequency. Each site has its own.

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

A system with several sites usually hears a call granted on more than one of them at once. Each
copy is recorded; when the last one ends, only the best copy is saved (the one decoded most
cleanly, or the talkgroup's Preferred Site). Turn that off with `recording.dropDuplicateCalls` to
keep every copy.

Only the sites listed under one system are copies of each other. Two systems are never joined while
recording, whatever their control channels announce; when two systems announce the same system on
the air, Setup offers **Make them one system**. To prefer a site for a talkgroup, use the talkgroup
file's `Preferred Site` or `Preferred NAC` column.

```json
{ "shortName": "capmax", "type": "dmr",
  "sites": [{ "name": "north", "controlChannelsHz": [452175000] },
            { "name": "south", "controlChannelsHz": [452525000] }] }
```

An older config, whose systems were each one site, is rewritten on load (the old file is kept as
`config.json.before-sites`). Each old system becomes a site named as it was. Those with the same
`siteGroup`, or with site locks naming one system (the same type and WACN and System ID; SmartNet:
System ID; NXDN Type-C: system code), become one system, with the first one's short name and
settings; the log says what wasn't kept. Systems that were joined only by what they announced on
the air stay apart until you make them one in Setup.

## Encryption keys (`keys`)

**Experimental.** If you are authorized to monitor an encrypted P25 system and hold its keys, the
recorder can decrypt its voice, P25 Phase 1 and Phase 2. DMR and NXDN calls stay encrypted.

The keys live in a file of their own, never in the config, which the interface reads and edits. The
config's top-level `keys` names each such file once, as a **key source**, and each system that
decrypts names its source in its own `keys` (systems on one county's radios share one):

```json
{
  "keys": { "county": { "file": "county.json" } },
  "systems": [{ "shortName": "fire", "keys": "county", ... }, { "shortName": "pd", "keys": "county", ... }]
}
```

| Key source key | Description |
|---|---|
| `file` | A keys file (below) or a [KFDtool key container](#keys-in-a-kfdtool-container), absolute or relative to the config file's folder. Which it is, is read from the file |
| `passwordFile` | A container's password, in a file of its own. See [Keys in a KFDtool container](#keys-in-a-kfdtool-container) |

A config from before key sources, whose systems named their keys file in `keys`, is read as one source
per file, named after it.

A keys file:

```json
{ "keys": [
    { "algId": "0xAA", "keyId": "0x1B50", "key": "1234567890" },
    { "algId": "0x84", "keyId": "0x0002", "key": "<64 hex digits>" },
    { "algId": "0x84", "key": "<64 hex digits>", "talkgroups": [1201, 1202] }
]}
```

| Key | Description |
|---|---|
| `algId` | The algorithm: `0xAA` ADP, `0x81` DES-OFB, `0x83` 3DES, `0x89` AES-128, `0x84` AES-256. Hex in a string, or a number |
| `keyId` | The key ID the radios send. Hex in a string, or a number |
| `key` | The key in hex, exactly as long as the algorithm needs: ADP 10 digits, DES 16, AES-128 32, 3DES 48, AES-256 64. Spaces are allowed. A shorter key is an error, never padded |
| `talkgroups` | Optional. Use this key on these talkgroups whatever key ID the air names (or only under its `keyId`, if it gives one). Tried before the keys matched by key ID |

- The file is read when recording starts. After editing it, stop and start recording. A file that
  can't be read, or a key that is wrong, stops the start with a message saying which key and what to
  fix. The key itself is never shown, logged or sent to plugins.
- Make the file readable only by you (`chmod 600 keys.json`). The log warns when other users can
  read it.
- A system with keys records its encrypted P25 calls, even with `recordEncrypted` off, because the
  key ID only arrives with the voice. A call whose key isn't in the file is then left out, as an
  encrypted call always is. One that was decrypted is saved with `"decrypted": 1` in its JSON and
  can be played in Listen.
- A call joined late, or one whose encryption sync was lost to a fade, is decrypted from the next
  sync on, and the voice just before that sync too: at most the frames lost with the sync are
  missing.
- The talkgroup page on the Radio dashboard lists the ciphers heard (algorithm and key ID), which
  tells you which keys a talkgroup needs.

### Keys in a KFDtool container

A key source's `file` can be a KFDtool key container (`.ekc`), the file KFDtool exports, instead of a
JSON file. It is encrypted with its own password, which goes in a separate file named by `passwordFile`:

```json
"keys": { "county": { "file": "county.ekc", "passwordFile": "/run/secrets/county-password" } }
```

- The password file holds the password on its first line. On Unix it must not be readable by
  others (`chmod 600`), except under `/run/secrets`, where the container runtime sets the modes;
  otherwise the start is refused. Kept apart from the container (a Docker or systemd secret), it
  means the keys are never in the clear on disk.
- Every voice key in the container is used by key ID on any talkgroup. Key-encryption keys, and
  algorithms that can't be decrypted (DES-XL), are listed in the log and left out; the rest still
  load. A container with the wrong password, or a damaged one, stops the start.
- `passwordFile` with a JSON keys file is an error.

#### Unlocking a container in the interface

A container with no `passwordFile` doesn't stop recording from starting: its systems run without
keys, and **Setup → Recording → Key containers** asks for its password. **Unlock** decrypts it in
memory, and its systems decrypt from then on, calls in progress too (from their next encryption
sync). **Lock** forgets the keys and they stop at once. The password is never saved or logged.

- The keys last until Lock or until the app stops. After a restart, a crash or an update, the
  container is locked again until someone types the password, and its encrypted calls are left
  out as any encrypted call is. Use `passwordFile` for a recorder that must decrypt unattended.
- Anyone who can open the interface can unlock and lock it. The interface is plain HTTP, so a
  password typed on another computer crosses the network unencrypted; by default the interface
  is only reachable from the recorder's own computer (`server.bind`).

## Talkgroup file

A system's talkgroups are a Trunk Recorder talkgroup file, kept beside the config file as
`<shortName>.talkgroups.csv` (not in the config). RadioReference's talkgroup CSV export reads as is.
Setup loads it from a file or a RadioReference paste and saves it there, and renames it when the
system is renamed. By hand, put the file in the config's folder under that name. A conventional
system has its own file the same way, which names the talkgroups its P25, DMR and NXDN channels hear.
A config from an older version, which kept the file inside it as `talkgroupsCsv`, is upgraded when
it's read: the talkgroups are moved out into these files.

```csv
Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category,Priority
1201,4b1,D,FD Disp,Fire Dispatch,Fire Dispatch,DC Fire,1
1202,4b2,DE,FD Tac,Fire Tac (encrypted),Fire-Tac,DC Fire,2
```

It comes in two forms:

- **With a header row**, if the first cell is `Decimal` (or RadioReference's `DEC`). These columns
  are recognised, in any order and any case: `Decimal`, `Hex`, `Mode`, `Alpha Tag` (or `AlphaTag`),
  `Description`, `Tag`, `Category` (or `Group`), `Priority`, `Preferred NAC`, `Preferred Site`,
  `Ignore`, `Send To`. Others are ignored.
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
| `Preferred Site` | Multi-site: a site's `name` (a site with none: the system's `shortName`). Its copy of a call is kept when it has at least 90 % of the best copy's clean audio |
| `Preferred NAC` | The same, by the site's NAC, or by RFSS and site written `RRRRssss` (RFSS 1, site 23: `10023`) |
| `Send To` | The plugins its calls go to, separated by `;` (`rdioscanner`, `openmhz;rdioscanner`; `Rdio Scanner` works too). Empty: every plugin. `none`: no plugin. See [Send To](plugins.md#send-to-which-plugins-get-which-calls) |

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
| `mode` | | `"user"` | string | `"user"`: your names first, then talker aliases heard. `"ota"`: aliases heard first. `"user_only"`: only your names. `"none"`: no names. Any other value is refused |

Talker aliases heard over the air are kept in `<shortName>.units.csv` beside the config file
([Files kept beside the config](README.md#files-kept-beside-the-config)).
