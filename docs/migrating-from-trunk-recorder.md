# Migrating from Trunk Recorder

If you already run Trunk Recorder, you can bring its `config.json` across in a few clicks. This page
explains how to run the import, what it converts, what it leaves behind and why, and the ideas that
work differently here.

## Running the import

1. Open the import, either way:
   - On first run, the setup guide's welcome screen has **I have a Trunk Recorder config**.
   - Any time later: **Setup** → **Systems** → **Import Trunk Recorder config…**.
2. **Choose config.** In the desktop app you browse the recorder's computer: open the folder
   Trunk Recorder runs from, and when it holds a `config.json`, press **Use it** (or click any
   `.json` file). **Or upload a config.json** picks one from the computer your browser is on
   instead. In the browser version, press **Choose config.json…**.
3. **Review.** "Here's what comes over" lists the radios, systems, conventional channels, the
   recordings folder and any uploaders, with anything that needs attention marked:
   - When the desktop app reads `config.json` from disk, it also reads each system's
     `talkgroupsFile` and `channelFile`, beside the config (as Trunk Recorder, run from its folder,
     finds them) or by file name in that folder. Any file it couldn't find, every `unitTagsFile`,
     and all files when you uploaded the config instead, show as **Not found** with
     **Choose file…** next to them. Pick each one, or skip it and add it later in Setup.
   - A dongle that isn't plugged in or isn't free can be swapped for one that is.
   - **Good to know** lists what changed meaning on the way.
4. Press **Import**. This **replaces your current systems and radios**. Anything left to finish
   (a site lock, a squelch to check, a missing file, a plugin to install, a control channel no radio
   reaches) is listed on the Setup page and highlighted where it needs doing.

Nothing is written to your Trunk Recorder folder; its files are only read.

## What converts

The importer reads Trunk Recorder's keys and writes this app's. A key not listed here is not read.

### Sources

| Trunk Recorder | Trunk Recorder Pro |
|---|---|
| `driver: "osmosdr"`, `device: "rtl=…"` | `rtlsdr` source. A serial becomes `serial`; a device index (`rtl=0`) becomes `""`, the first free dongle. A `rate` over 3.2 MSPS becomes 2.4 MSPS |
| `device` containing `airspy` | `airspy` source. The serial from `airspy=0x…`; `bias=1` → `biasTee`. `lnaGain`, `mixGain`, `ifGain` / `vgaGain` (or `gainSettings` LNA / MIX / IF) → manual mode steps; otherwise `gain` → the linearity step (0–21). A rate the Airspy can't do becomes 6 MSPS |
| `device: "hackrf=…"`, `bladerf`, `airspyhf`, `lime` / `limesdr`, `sdrplay`, `plutosdr`, `redpitaya`, `xtrx`, or `soapy=…` | `soapy` source with matching `args` (`driver=hackrf`, plus `serial=…` when the device string had one; `soapy=` arguments kept as they are). Stage gains (`lnaGain`, `vgaGain`, `ampGain`, `ifGain`, `bbGain`, `mixGain`, `tiaGain`, `pgaGain`, `vga1Gain`, `vga2Gain`, `gainSettings`) → `gains`. AGC is on if `agc` was, or if no gain was given |
| `driver: "usrp"` | `usrp` source; `device` → `args`, `antenna` → `antenna` |
| Any other `driver` | Skipped, with a note |
| `center` | `centerHz` |
| `rate` | `rateHz` |
| `gain` | `gainDb` |
| `agc` | `agc` |
| `ppm` | `ppm` (rounded to a whole number for an RTL-SDR) |
| `error` (Hz) | Converted to `ppm` at the source's center frequency |
| `autoTune` | `autoTune` |

USRP, Airspy and SoapySDR sources need their driver installed on this computer; the review says so.

### Trunked systems

| Trunk Recorder | Trunk Recorder Pro |
|---|---|
| `type: "p25"`, `"smartnet"`, `"dmr"` | A trunked system of that type, one per entry, as in Trunk Recorder |
| `shortName` | `shortName` (made unique if two systems share one) |
| `control_channels` | `controlChannelsHz` |
| `modulation: "qpsk"` / `"fsk4"` | `modulation` the same; anything else, or no `modulation`, becomes `"auto"` |
| `talkgroupsFile` | `talkgroupsCsv` (the file's contents) and `talkgroupsName`. Semicolon, tab or bar delimiters become commas, header names are tidied |
| `unitTagsFile`, `unitTagsMode` | `unitNames.csv` (contents), `unitNames.name`, `unitNames.mode` |
| `bandplan` | `bandplan` (default `800_standard`) |
| `bandplanBase`, `bandplanSpacing`, `bandplanHigh` (MHz) | `bandplanBaseHz`, `bandplanSpacingHz`, `bandplanHighHz` (Hz) |
| `bandplanOffset` | `bandplanOffset` |
| Top-level `defaultMode: "analog"` | `defaultMode: "analog"` on every SmartNet system (it only affects SmartNet) |
| DMR `lcnTable` | `lcnTableHz` |
| DMR `channels` | `dmrChannelsHz` |
| `multiSite: true` with `multiSiteSystemName` | `siteGroup` = that name. With no name, the site is grouped from what its control channel announces |
| `multiSite` off, when other systems have it on | A `siteGroup` of its own (its short name), so every call there is kept |
| `siteId` | A to-do: set a **Site lock** for that site |

### Conventional systems

| Trunk Recorder | Trunk Recorder Pro |
|---|---|
| `type: "conventional"`, `"conventionalP25"`, `"conventionalDMR"` | A conventional system each, with its own short name and folder. Channels get mode `fm`, `p25` or `dmr` by type |
| `channels` (frequencies) | `channels`, one row per frequency |
| `channelFile` | Its rows, copied into `channels`. With no `Mode` column, each row takes its system's mode. The file isn't linked; you can link one later in Setup |
| `enabled: false` | `enabled: false` |
| `squelch` | Not converted. A to-do asks you to check the squelch (see [below](#squelch-is-relative)) |

### Call rules

These are per system in Trunk Recorder. Each becomes the system's own rule here; a rule with the same
value on every system becomes the global one on the **Recording** tab instead.

| Trunk Recorder | Trunk Recorder Pro |
|---|---|
| `recordUnknown` | `recordUnknown` |
| `compressWav` | `compressWav` |
| `audioArchive` | `audioArchive` |
| `callLog` | `callLog` |
| `filenameFormat` (system's, or the top-level one for systems without their own) | `filenameFormat` |
| `minDuration` | `minCallS` |
| `maxDuration` | `maxCallS` |
| `minTransmissionDuration` | `minTransmissionS` |
| `digitalLevels` (default 1) | `digitalLevelDb`: 20·log10(value), so 2 → +6 dB |
| `analogLevels` (default 8) | `analogLevelDb`: 20·log10(value / 8), so 16 → +6 dB |
| `captureDir` (top level) | `recording.captureDir` |
| `callTimeout` | `recording.callTimeoutS` |
| `recordUUVCalls` | `recording.recordUnitToUnit` |
| `archiveFilesOnFailure` | `recording.archiveFilesOnFailure` |

### Log

| Trunk Recorder | Trunk Recorder Pro |
|---|---|
| `logLevel` | `log.level` |
| `consoleLog` | `log.console` |
| `logFile` | `log.file` |
| `logDir` | `log.dir`. A relative folder is now taken from the config file's folder, not from where the app was started |
| `syslogFriendly` | `log.syslogFriendly` |
| `logColor` | `log.color` |
| `frequencyFormat` | `log.frequencyFormat` |
| `statusAsString` | `log.statusAsString` |
| `controlWarnRate` | `log.controlWarnRatePerS` |
| `talkgroupDisplayFormat` (a system's; the first one found) | `log.talkgroupDisplayFormat` |

### Uploads and streaming

Trunk Recorder's built-in uploaders and its plugins become this app's plugins (see
[plugins](plugins/README.md)). Their settings come across; a plugin that is already installed is
turned on, and one that isn't is left as a to-do to install.

| Trunk Recorder | Plugin and its settings |
|---|---|
| A system's `apiKey`, `openmhzSystemId`; top-level `uploadServer` | `openmhz`: per system `apiKey`, `systemName`; `server` |
| A system's `broadcastifyApiKey`, `broadcastifySystemId`; `broadcastifyCallsServer`, `broadcastifySslVerifyDisable`, `broadcastifyOTA` | `broadcastify`: per system `apiKey`, `systemId`; `server`, `skipCertificateCheck`, `talkerAliases` |
| A system's `broadcastifyAllow` / `broadcastifyDeny` (or `…Whitelist` / `…Blacklist`, `talkgroupWhitelist` / `talkgroupBlacklist`) | `broadcastify`: per system `talkgroupAllow` / `talkgroupDeny` |
| A system's `uploadScript` | `upload-script`: per system `script` |
| The rdio-scanner plugin (`server`, each system's `apiKey`, `systemId`, `talkgroupAllow`, `talkgroupDeny`) | `rdioscanner` |
| The openmhz or broadcastify plugin entry | `openmhz` / `broadcastify`, as above |
| The simplestream plugin's `streams` | `simplestream`: `streams`, with `address`, `port` and `useTCP` turned into a `url` |
| A plugin entry with `enabled: false` | Skipped |
| Any other plugin | Listed under "Other plugins: no equivalent yet" |

The browser version has no plugins, so there these settings aren't kept.

## What doesn't convert, and why

- **Squelch.** Trunk Recorder's is an absolute level; here it is dB above the measured noise floor, so
  the number doesn't carry over. Conventional systems start at the default, 8 dB.
- **`siteId`.** Becomes a site lock to fill in, since a site lock here can check more than the site
  number (NAC, WACN, System ID, RFSS, site).
- **Recorder counts** (`digitalRecorders`, `analogRecorders` and the like) and
  `signalDetectorThreshold`. There are no per-source recorders here; see below.
- **A conventional system's `talkgroupsFile`.** Name the channels in the channel rows instead.
- **`conversationMode: false`** (a file per transmission). Each call is one file here; the import
  notes it.
- **Unsupported system types** (anything but the six above) are skipped, with a note.
- Anything else Trunk Recorder has that isn't in the tables above (`ver`, `tempDir`, `instanceId`,
  `hideEncrypted`, analog filter settings and so on) isn't read.

## Defaults that differ

When a key is missing from the Trunk Recorder config, the import leaves this app's default, which is
not always Trunk Recorder's:

| Setting | Trunk Recorder's default | Here |
|---|---|---|
| `compressWav` | on | off (`compressWav: false`): turn on **Also save an M4A** if you want `.m4a` files kept |
| `archiveFilesOnFailure` | off | on |
| `multiSite` / duplicate calls | off: every copy saved | on (`dropDuplicateCalls: true`): a call heard on several sites is saved once. The import notes this; turn off **Save a call heard on several sites once** to keep every copy |
| `modulation` | `qpsk` | `auto` (both receivers) |
| `captureDir` | the folder it was started from | `~/TrunkRecorderPro` |
| `frequencyFormat` | `exp` | `mhz` |

## Concepts that work differently

### Squelch is relative

Squelch here is **dB above the measured noise floor**, not an absolute level. The recorder measures
the noise floor near each channel all the time, so the same setting works whatever the gain or
antenna. Only conventional channels have one (`squelchDb`, default 8, per system or per channel).
Trunked systems need none: calls start from the control channel's grants. See the
[conventional guide](guides/conventional.md).

### No recorder counts

Trunk Recorder gives each source a number of digital and analog recorders. Here every source's
spectrum is split into channels as needed, and the only limit is `recording.maxRecorders`, the
number of trunked calls recorded at once across all systems (default 32). There is nothing to
balance between sources.

### Conventional channels are found in the spectrum you already receive

A conventional channel doesn't take a recorder of its own. Each channel's frequency is watched in the
spectrum the sources already cover, and a call opens when the signal rises above the squelch. So
conventional channels can share a dongle with a trunked system. Several users of one frequency can
be split by CTCSS/DCS tone, NAC, colour code or RAN ([tones guide](guides/tones.md)).

### Plugins are separate programs

Trunk Recorder's plugins are shared libraries built against it. Here a plugin is a separate program
the recorder starts and talks to, installed from the **Plugins** page (or `trunk-pro plugin install`)
without building anything. OpenMHz, Broadcastify Calls, Rdio Scanner, simplestream and upload-script
plugins are published. See [plugins](plugins/README.md).

### Files and settings live in the app's folder

The config is in the app's config folder ([where](configuration/README.md#where-the-file-is)), not
the folder you run it from, and keys are camelCase with frequencies in Hz. Talkgroup and unit files
are stored **inside** the config (`talkgroupsCsv`, `unitNames.csv`), so editing the original CSV
afterwards changes nothing: load it again in Setup. A conventional system can stay linked to a
channel CSV, though (`channelFile`), re-read at every Start.

### What stays compatible

- **Talkgroup, unit tag and channel CSV files** read as they are, including RadioReference's
  exports. See [systems.md](configuration/systems.md#talkgroup-file) and
  [conventional.md](configuration/conventional.md#csv-format).
- **Call JSON** uses Trunk Recorder's field names (`talkgroup`, `start_time`, `freq`, `srcList`, `freqList`
  and so on), so tools that read Trunk Recorder's JSON read these. See the
  [recordings guide](guides/recordings.md).
- **Folders and file names** are Trunk Recorder's by default
  (`<shortName>/<year>/<month>/<day>/<talkgroup>-<epoch>_<freq>.wav`), and `filenameFormat` takes
  Trunk Recorder's tokens. See [recording.md](configuration/recording.md#file-names).
- **Unit tags** support Trunk Recorder's regular-expression form and `unitTagsMode` values.
- **Log options** keep Trunk Recorder's names, levels and file naming.
