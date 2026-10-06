# Rdio Scanner

This page covers the Rdio Scanner plugin: what it sends, its settings, and how to set it up.

[Rdio Scanner](https://github.com/chuot/rdio-scanner) is a web scanner you run yourself: it takes
calls from recorders and plays them in a browser, like a scanner. The plugin uploads each call your
recorder records to your Rdio Scanner server. It does what Trunk Recorder's Rdio Scanner uploader
plugin does.

| | |
|---|---|
| Plugin id | `rdioscanner` |
| Hears | recorded calls |
| Audio | M4A when there's an encoder, WAV otherwise |
| Source | [github.com/TrunkRecorder/trunk-plugin-rdioscanner](https://github.com/TrunkRecorder/trunk-plugin-rdioscanner) |

## What it needs

- **An Rdio Scanner server**, with an API key (from the API keys section of its administration
  page) and a system set up in it for each system you upload. Note each system's ID there.
- **Optionally, an M4A encoder.** Calls go as M4A when there's one (about a tenth the size), and as
  WAV when there isn't. macOS has one built in; on Linux and Windows, install
  [ffmpeg](https://ffmpeg.org). See [M4A audio](README.md#m4a-audio).

## Setting it up

1. On **Plugins** > **Install & settings**, find **Rdio Scanner** under **Find plugins** and press
   **Install** (or run `trunk-pro plugin install rdioscanner`).
2. In **Setup** > **Plugins**, turn **Rdio Scanner** on and fill in **Server**: the address you
   open Rdio Scanner with in a browser, like `http://192.168.1.20:3000`.
3. On the card of each system you upload, under **Plugins** > **Rdio Scanner**, fill in the
   **API key** and the system's **System ID** in Rdio Scanner.
4. Optionally limit the talkgroups with **Only these talkgroups** or **Not these talkgroups**.
5. Start recording, or if you're recording already, the plugin restarts with the new settings. Its
   log lists what it uploads (`uploading dcfd as system 1`), and **Plugins** > **Health** shows the
   server as **Rdio Scanner at 192.168.1.20:3000**, up or down.

## Settings

For the whole recorder (Setup > **Plugins**, or `plugins.rdioscanner.settings` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `server` | ✓ | | string (URL) | **Server.** Your Rdio Scanner's web address, the one you open it with. Calls go to `<server>/api/call-upload`. |

For each system (the system's card, or its `plugins.rdioscanner` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `apiKey` | ✓ | | string | **API key.** A key from Rdio Scanner's administration page. A system without one isn't uploaded. |
| `systemId` | ✓ | | integer | **System ID.** The system's ID in Rdio Scanner. Needed whenever the system has an API key. |
| `talkgroupAllow` | | (all) | list | **Only these talkgroups.** Upload these and no others. |
| `talkgroupDeny` | | (none) | list | **Not these talkgroups.** Never upload these. |

```json
{
  "plugins": { "rdioscanner": { "enabled": true, "settings": { "server": "http://192.168.1.20:3000" } } },
  "systems": [
    {
      "shortName": "dcfd",
      "controlChannelsHz": [857987500],
      "plugins": { "rdioscanner": { "apiKey": "your-rdio-scanner-api-key", "systemId": 1 } }
    }
  ]
}
```

Talkgroup list entries are numbers or patterns: `*` stands for any digits and `?` for one, so
`507*` is every talkgroup starting with 507. In the form, separate them with commas. When both
lists are set, a talkgroup has to be in the first and not in the second.

The plugin won't start without a server, without at least one system with an API key, or with a
system that has a key but no system ID.

## What it sends

For each recorded call of a system with an API key, the plugin sends the call's audio and the
fields Trunk Recorder sends:

- talkgroup, and its names: your talkgroup file's alpha tag as Rdio Scanner's label, its group tag
  as Rdio Scanner's tag, its description as the name, and its group
- frequency, and each frequency used during the call with its error and spike counts
- start time
- the radios heard, with their names (from your unit names, or else the talker alias heard over
  the air)
- the talkgroups patched with it, when it was patched
- the system's ID, and its short name as the system label

**Encrypted calls are never sent**, and neither are calls on talkgroups your lists leave out. Both
are marked skipped.

## When an upload fails

- **The server can't be reached, or has a server error.** The call is tried again after 10 seconds,
  1 minute, 5 minutes and 15 minutes; after that it's marked failed. While calls are waiting, the
  plugin shows **Needs attention**.
- **Recording stops with calls still waiting.** They're saved in
  `plugin-data/rdioscanner/queue.jsonl` and uploaded when recording starts again.
- **The server refuses the call**: a wrong API key (*Rdio Scanner refused the API key*), missing
  details, or another HTTP 4xx error other than 408 or 429. The call is marked failed and not tried
  again.

Whether a failed call's files are kept follows your **Files** settings: see
[What happens to the files](README.md#what-happens-to-the-files).

## Coming from Trunk Recorder

**Import Trunk Recorder config…** brings over Trunk Recorder's `rdioscanner` plugin entry: its
`server`, and each system's `apiKey`, `systemId`, `talkgroupAllow` and `talkgroupDeny` (matched
by short name).
