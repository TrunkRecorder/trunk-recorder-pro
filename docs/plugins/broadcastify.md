# Broadcastify Calls

This page covers the Broadcastify Calls plugin: what it sends, its settings, and how to set it up.

[Broadcastify Calls](https://www.broadcastify.com/calls/) collects recorded calls from many
contributors' recorders ("nodes") and plays them on Broadcastify. The plugin uploads each call
your recorder records to it. It does what Trunk Recorder's built-in Broadcastify uploader does.

| | |
|---|---|
| Plugin id | `broadcastify` |
| Hears | recorded calls |
| Audio | M4A (required) |
| Source | [github.com/TrunkRecorder/trunk-plugin-broadcastify](https://github.com/TrunkRecorder/trunk-plugin-broadcastify) |

## What it needs

- **A Broadcastify Calls node**, with an API key, and a system ID for each system you upload.
  You get these from Broadcastify when it approves your node; see
  [broadcastify.com/calls](https://www.broadcastify.com/calls/) for how to apply.
- **An M4A encoder.** Broadcastify takes AAC audio, not WAV. macOS has one built in. On Linux and
  Windows, install [ffmpeg](https://ffmpeg.org) (on Debian and Raspberry Pi OS:
  `sudo apt install ffmpeg`). Without one, the plugin won't start, and says
  *Broadcastify needs calls as M4A, and there's no M4A encoder on this computer.* See
  [M4A audio](README.md#m4a-audio).

## Setting it up

1. On **Plugins** > **Install & settings**, find **Broadcastify Calls** under **Find plugins** and
   press **Install** (or run `trunk-pro plugin install broadcastify`).
2. In **Setup** > **Plugins**, turn **Broadcastify Calls** on. Leave **Upload server** as it is
   unless Broadcastify tells you otherwise.
3. On the card of each system you upload, under **Plugins** > **Broadcastify Calls**, fill in the
   **API key** and the **System ID** Broadcastify gave you.
4. If Broadcastify should only get some talkgroups, list them in **Only these talkgroups**, or
   list the ones to leave out in **Not these talkgroups** (see [Talkgroup lists](#talkgroup-lists)).
5. Start recording, or if you're recording already, the plugin restarts with the new settings. Its
   log says what it uploads:

   ```
   [broadcastify] uploading dcfd as system 1234 (key …a7), talkgroups: …
   ```

Watch **Plugins** > **Health**: the Broadcastify Calls card counts calls sent, skipped and failed,
and shows the service as **up** once uploads go through.

## Settings

For the whole recorder (Setup > **Plugins**, or `plugins.broadcastify.settings` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `server` | | `https://api.broadcastify.com/call-upload` | string (URL) | **Upload server.** Leave it as it is, unless Broadcastify tells you to change it. |
| `talkerAliases` | | `true` | bool | **Send talker aliases.** When the first radio on a call sent its name over the air, send that too. |
| `skipCertificateCheck` | | `false` | bool | **Skip certificate checks.** Upload even when Broadcastify's certificate has expired. Leave it off unless uploads fail with a certificate error; the log warns while it's on. |

For each system (the system's card, or its `plugins.broadcastify` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `apiKey` | ✓ | | string | **API key.** Your node's upload key. A system without one isn't uploaded. |
| `systemId` | ✓ | | integer | **System ID.** The system's number on Broadcastify Calls. Needed whenever the system has an API key. |
| `talkgroupAllow` | | (all) | list | **Only these talkgroups.** Upload these and no others. |
| `talkgroupDeny` | | (none) | list | **Not these talkgroups.** Never upload these. |

```json
{
  "plugins": { "broadcastify": { "enabled": true } },
  "systems": [
    {
      "shortName": "dcfd",
      "controlChannelsHz": [857987500],
      "plugins": { "broadcastify": { "apiKey": "your-broadcastify-api-key", "systemId": 1234, "talkgroupDeny": ["99*"] } }
    }
  ]
}
```

The plugin won't start if no system has an API key, or if a system has a key but no system ID
(*Add dcfd's Broadcastify system ID (or clear its API key).*).

### Talkgroup lists

Each entry is a talkgroup number, or a pattern where `*` stands for any digits and `?` for one:
`507*` is every talkgroup starting with 507, and `12???` every five-digit one starting with 12. In
the form, separate entries with commas. When both lists are set, a talkgroup has to be in the
first and not in the second.

## What it sends

For each recorded call of a system with an API key, the plugin first posts the call's details to
the upload server: its call JSON (talkgroup, frequency, times, the radios heard), its length, the
system ID and API key, and the first radio's talker alias when **Send talker aliases** is on.
Broadcastify answers with where to send the audio, and the plugin then uploads the call's M4A
there.

Broadcastify judges how far behind a node is from each call's end time. A call ends a few seconds
after its last transmission, so (as Trunk Recorder does) the times sent are those of the call's
audio played back to back, ending when the call was saved. The files on disk keep the real times.

**Encrypted calls are never sent**, and neither are calls on talkgroups your lists leave out.
Both are marked skipped.

## When an upload fails

- **Broadcastify can't be reached, has a server error, or gives an answer the plugin doesn't
  recognise.** The call is tried again after 10 seconds, 1 minute, 5 minutes and 15 minutes; after
  that it's marked failed. While calls are waiting, the plugin shows **Needs attention**. As in
  Trunk Recorder, this includes Broadcastify rejecting the API key, so a wrong key shows up as
  retries and then failures: check the log's last error.
- **Recording stops with calls still waiting.** They're saved in
  `plugin-data/broadcastify/queue.jsonl` and uploaded when recording starts again.
- **Broadcastify answers `SKIPPED`.** The call is marked skipped.
- **Broadcastify answers `REJECTED`**, or the server refuses the request outright (an HTTP 4xx
  error other than 408 or 429). The call is marked failed and not tried again.

Whether a failed call's files are kept follows your **Files** settings: see
[What happens to the files](README.md#what-happens-to-the-files).

## Coming from Trunk Recorder

**Import Trunk Recorder config…** brings over:

| Trunk Recorder | Here |
|---|---|
| `broadcastifyCallsServer` | **Upload server** (`server`) |
| `broadcastifySslVerifyDisable` | **Skip certificate checks** (`skipCertificateCheck`) |
| A system's `broadcastifyApiKey` | That system's **API key** (`apiKey`) |
| A system's `broadcastifySystemId` | That system's **System ID** (`systemId`) |

The import also brings over `broadcastifyOTA` (as `talkerAliases`) and each system's talkgroup
lists: `broadcastifyAllow` / `broadcastifyDeny`, or the older `broadcastifyWhitelist` /
`broadcastifyBlacklist` and `talkgroupWhitelist` / `talkgroupBlacklist`, become `talkgroupAllow` /
`talkgroupDeny`. The plugin also understands Trunk Recorder's names if
you paste them into `config.json`: `broadcastifyCallsServer`, `broadcastifyOTA` and
`broadcastifySslVerifyDisable` in its settings, and `broadcastifyApiKey`, `broadcastifySystemId`,
`broadcastifyAllow` / `broadcastifyDeny` (or the older `…Whitelist` / `…Blacklist`) in a
system's.
