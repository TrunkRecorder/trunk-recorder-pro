# OpenMHz

This page covers the OpenMHz plugin: what it sends, its settings, and how to set it up, from an
OpenMHz account to calls showing up on the site.

[OpenMHz](https://openmhz.com) is a free website, run by the author of Trunk Recorder, where
people share and listen to recordings of radio systems. The OpenMHz plugin uploads each call your
recorder records to an OpenMHz system. It does what Trunk Recorder's built-in OpenMHz uploader
does, and sends the same thing.

| | |
|---|---|
| Plugin id | `openmhz` |
| Hears | recorded calls |
| Audio | M4A (required) |
| Source | [github.com/TrunkRecorder/trunk-plugin-openmhz](https://github.com/TrunkRecorder/trunk-plugin-openmhz) |

## What it needs

- **An OpenMHz system** for each system you upload, and its API key (below).
- **An M4A encoder.** OpenMHz takes M4A, not WAV. macOS has one built in. On Linux and Windows,
  install [ffmpeg](https://ffmpeg.org) (on Debian and Raspberry Pi OS: `sudo apt install ffmpeg`).
  Without one, the plugin won't start, and says
  *OpenMHz needs calls as M4A, and there's no M4A encoder on this computer.* See
  [M4A audio](README.md#m4a-audio).

## Setting it up

### 1. Make an OpenMHz account and system

1. Register for an account at [account.openmhz.com](https://account.openmhz.com/), and confirm
   your email address.
2. Log in at [admin.openmhz.com](https://admin.openmhz.com) and add a system. Pick its
   **Short Name** with care: it's how OpenMHz knows the system, it's part of the system's web
   address, and it's what the plugin uploads to.
3. Import your talkgroups on the system's Talkgroups tab. OpenMHz takes a CSV file with
   `Decimal`, `Alpha Tag` and `Description` columns; the talkgroup file you use with the recorder
   has those. If you check **Ignore Unknown Talkgroups** on the system, OpenMHz drops calls on
   talkgroups that aren't in that list.
4. Open the system's Config tab and copy its **API Key**.

OpenMHz's own pages explain the rest of the system's options; the
[Trunk Recorder OpenMHz guide](https://trunkrecorder.com/docs/OpenMHz) walks through them with
screenshots.

### 2. Install the plugin

On **Plugins** > **Install & settings**, find **OpenMHz** under **Find plugins** and press
**Install** (or run `trunk-pro plugin install openmhz`).

### 3. Give it your key

1. In **Setup** > **Plugins**, turn **OpenMHz** on. Leave **Upload server** as it is.
2. Go to the card of the system you're uploading (the chip under the plugin's card takes you
   there) and, under **Plugins** > **OpenMHz**, paste the **API key**.
3. If the system's short name in the recorder isn't the same as its Short Name on OpenMHz, put the
   OpenMHz one in **Name on OpenMHz**. (Or rename the system here to match.)
4. Repeat for any other system you upload. Systems without an API key aren't uploaded.

### 4. Check it

Start recording (or, if you're already recording, the plugin restarts with the new settings). The
plugin's log says what it's uploading, with the last two characters of each key so you can tell
them apart:

```
[openmhz] uploading dcfd as dcfd (key …3f)
```

On **Plugins** > **Health**, the OpenMHz card counts calls sent and failed, and shows OpenMHz as
**up** once uploads go through. A new system can take a while to appear in OpenMHz's public list.

To try it on calls you've already recorded before you turn it on:

```bash
trunk-pro plugin run openmhz ~/TrunkRecorderPro/dcfd --limit 3
```

That uses your real settings, so the calls really are uploaded.

## Settings

For the whole recorder (Setup > **Plugins**, or `plugins.openmhz.settings` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `server` | | `https://api.openmhz.com` | string (URL) | **Upload server.** Where to upload. Leave it as it is unless you run your own OpenMHz server. |

For each system (the system's card, or its `plugins.openmhz` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `apiKey` | ✓ | | string | **API key.** The system's upload key from OpenMHz. Needed for each system you upload; a system without one isn't uploaded. |
| `systemName` | | the system's short name | string | **Name on OpenMHz.** The system's Short Name on OpenMHz, when it differs from its short name here. |

```json
{
  "plugins": { "openmhz": { "enabled": true } },
  "systems": [
    {
      "shortName": "dcfd",
      "controlChannelsHz": [857987500],
      "plugins": { "openmhz": { "apiKey": "your-openmhz-api-key" } }
    }
  ]
}
```

The plugin won't start without at least one system with an API key
(*Add your OpenMHz API key to the systems you want to upload.*).

## What it sends

For each recorded call of a system with an API key, the plugin posts to
`<server>/<name on OpenMHz>/upload`:

- the call's M4A audio
- talkgroup, frequency, start and stop time, length, emergency flag
- error and spike counts
- the radios heard on the call, with their positions in the audio and their names (from your
  unit names, or else the talker alias heard over the air)
- the talkgroups patched with it, when it was patched

Nothing else, and nowhere else.

## When an upload fails

The plugin keeps a queue and works it off on two threads, so a slow upload doesn't hold up the
next call.

- **OpenMHz can't be reached, or has a server error.** The call is tried again after 10 seconds,
  1 minute, 5 minutes and 15 minutes; after that it's marked failed
  (`gave up after 5 tries: …`). While calls are waiting, the plugin shows **Needs attention** with
  how many are waiting to retry.
- **Recording stops with calls still waiting.** They're saved in the plugin's data folder
  (`plugin-data/openmhz/queue.jsonl`) and uploaded when recording starts again.
- **OpenMHz refuses the call.** That isn't retried: the call is marked failed and the log says
  why. *OpenMHz refused the API key for dcfd* means the key is wrong; *OpenMHz has no system named
  dcfd* means the name doesn't match a Short Name on OpenMHz (set **Name on OpenMHz**).
- **The talkgroup isn't on the OpenMHz system** (with **Ignore Unknown Talkgroups** checked there).
  The call is marked skipped, not failed.
- **A system without an API key.** Its calls are marked skipped.

Whether a failed call's files are kept follows your **Files** settings: see
[What happens to the files](README.md#what-happens-to-the-files).

## Coming from Trunk Recorder

**Import Trunk Recorder config…** brings your OpenMHz settings over:

| Trunk Recorder | Here |
|---|---|
| `uploadServer` (top level) | **Upload server** (`server`) |
| A system's `apiKey` | That system's **API key** (`apiKey`) |
| A system's `openmhzSystemId` | That system's **Name on OpenMHz** (`systemName`) |
| `shortName` | Used as the OpenMHz name when **Name on OpenMHz** is empty, as before |

The plugin also accepts Trunk Recorder's key names (`uploadServer`, `openmhzSystemId`) if you
paste them into `config.json`.
