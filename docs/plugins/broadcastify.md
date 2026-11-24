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
4. If Broadcastify should only get some talkgroups, choose **Only some** under **Talkgroups** and
   list them, or **All but some** and list the ones to leave out (see
   [Choosing talkgroups](../configuration/plugins.md#choosing-talkgroups)).
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
| `healthReports` | | `true` | bool | **Send health reports.** Every 5 minutes, tell Broadcastify how well this recorder hears each system it uploads ([Health reports](#health-reports)). Leave it on: without them Broadcastify can't compare your node with others, so it may get a lower priority or be left out, and can't tell you about problems with it ([Turning them off](#turning-them-off)). (Plugin 0.2.0 and later.) |
| `healthServer` | | (beside `server`) | string (URL) | **Health report server.** Leave it empty: reports go to `…/call-health` beside the upload server. |

For each system (the system's card, or its `plugins.broadcastify` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `apiKey` | ✓ | | string | **API key.** Your node's upload key. A system without one isn't uploaded. |
| `systemId` | ✓ | | integer | **System ID.** The system's number on Broadcastify Calls. Needed whenever the system has an API key. |
| `talkgroupMode` | | `"all"` | `"all"`, `"only"` or `"allBut"` | **Talkgroups.** Every talkgroup, only some, or all but some. See [Choosing talkgroups](../configuration/plugins.md#choosing-talkgroups). |
| `talkgroupAllow` | | (none) | list | **Only these.** With `"only"`: upload these and no others. |
| `talkgroupDeny` | | (none) | list | **Except these.** With `"only"` or `"allBut"`: never upload these. |

```json
{
  "plugins": { "broadcastify": { "enabled": true } },
  "systems": [
    {
      "shortName": "dcfd",
      "sites": [{ "controlChannelsHz": [857987500] }],
      "plugins": { "broadcastify": { "apiKey": "your-broadcastify-api-key", "systemId": 1234, "talkgroupMode": "allBut", "talkgroupDeny": ["99*"] } }
    }
  ]
}
```

The plugin won't start if no system has an API key, or if a system has a key but no system ID
(*Add dcfd's Broadcastify system ID (or clear its API key).*).

A site that Broadcastify Calls knows as a system of its own gets its own **API key** and **System
ID** in its row under **Plugins** > **Broadcastify Calls** on the system's card (`perSite`, by the
site's id: `"perSite": { "8": { "apiKey": "…", "systemId": 77 } }`); a site needs both (*Add the
Broadcastify system ID and API key of dcfd's site south.*). A call goes to every site that heard it
and has a system of its own there; one only sites without a system of their own heard goes to the
system's. A site turned off for the plugin (**Sites** chips) is left out. See
[Settings for each site](../configuration/plugins.md#settings-for-each-site).

Each site needs its own Broadcastify system ID. If one system ID covers several sites, the plugin
warns you (plugin 0.2.1 and later), in its status and its log: Broadcastify Calls records each call
as coming from the site the system ID is registered to, so the other sites' calls would be labeled
as the wrong site. More system IDs can be applied for at
[broadcastify.com/apply/calls](https://www.broadcastify.com/apply/calls), with your existing API key.
Merging two systems in Setup (**Make them one system**) keeps this: the merged system's API key
and system ID go to the sites that move, so each keeps uploading as its own ID.

A system can also set **Upload server**, **Send talker aliases** and **Skip certificate checks**
again, in **Broadcastify Calls shared settings** on its card (`override` in its
`plugins.broadcastify` entry). Each server has its own queue and Health status.

Nor will it start with a talkgroup list entry that isn't a talkgroup, a range or a pattern
(*dcfd's talkgroups: "10O1" isn't a talkgroup…*). Ranges (`1000-1099`) and the menu are plugin 0.1.7
and later.

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

## Health reports

Every 5 minutes, at a random point of its own, each node (a Broadcastify system ID) sends
Broadcastify a [node health report](../api/node-health.md): how many seconds the recorder was
decoding the system's control channel, the control messages it decoded and lost, the calls the
system announced and why any weren't recorded (no radio covering the frequency, every recorder
busy), and the voice frames it lost, along with its radios' dropped samples and clipping. When
several nodes upload one system, Broadcastify can take its calls from the one hearing it best, and
can tell a node that's up but has no calls from one that's down.

A site with a system ID of its own is reported to that ID alone. Sites left on one system ID are one
report (Broadcastify takes one report per system ID each interval); two recorder systems on one ID
are reported for the first. A system or site the plugin is turned off for is neither uploaded nor
reported, nor is one with no API key and system ID.

Reports go on a thread of their own, so uploads never wait on them. A report that fails is carried
by the next. When the server doesn't take them (not found, or an answer that isn't a report's), the
plugin says so once and tries again less and less often, up to once an hour.

### What a report contains

Counts of what the recorder heard, not scores:

- **For each system or site:** the identity its control channel broadcasts (WACN, System ID, RFSS,
  site, NAC) and the control channel's frequency; the seconds spent decoding it, the messages
  decoded and lost, its signal-to-noise ratio and phase error; the calls announced, how many were
  recorded and why any weren't; the voice frames received and lost.
- **For each conventional channel:** its frequency and the name you gave it, the time it was heard
  and open, its levels, and its calls.
- **For each radio (SDR) those use:** the name you gave it, its kind (`airspy`, `hackrf`, ...),
  seconds of samples dropped, clipping and frequency error.
- **About the recorder:** its name and version, how long it has been recording, and how far the
  computer's clock is from the radio system's.
- **Who sends it:** the node's system ID and API key, as every upload does. Broadcastify doesn't
  keep the key with the report.

Nothing about people: no talkgroups, radio IDs, talker aliases or audio.

### Turning them off

Untick **Send health reports** (`healthReports: false`). Uploads carry on as before, but
Broadcastify relies on these reports to look after your node, and without them:

- **Your node can't be compared with others.** When several nodes upload the same system,
  Broadcastify chooses whose calls to take by how well each one hears it. A node that doesn't
  report can't be compared, so it may be given a lower priority or left out.
- **Broadcastify can't tell you about problems.** It can't warn you of an outage, a lost control
  channel, rising voice loss or other trends on your node, and it can't tell a node that's quiet
  from one that's down.
- **Support can't see how your node has been doing** when you ask for help with it.

We recommend leaving them on.

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

Whether a failed call's files are kept follows your **Archive** settings (**Keep when a plugin
fails**): see
[What happens to the files](README.md#what-happens-to-the-files).

## Coming from Trunk Recorder

**Setup → Systems → Import…** brings over:

| Trunk Recorder | Here |
|---|---|
| `broadcastifyCallsServer` | **Upload server** (`server`) |
| `broadcastifySslVerifyDisable` | **Skip certificate checks** (`skipCertificateCheck`) |
| A system's `broadcastifyApiKey` | That system's **API key** (`apiKey`) |
| A system's `broadcastifySystemId` | That system's **System ID** (`systemId`) |

The import also brings over `broadcastifyOTA` (as `talkerAliases`) and each system's talkgroup
lists: `broadcastifyAllow` / `broadcastifyDeny`, or the older `broadcastifyWhitelist` /
`broadcastifyBlacklist` and `talkgroupWhitelist` / `talkgroupBlacklist`, become `talkgroupAllow` /
`talkgroupDeny`, with the **Talkgroups** menu set to match. The plugin also understands Trunk Recorder's names if
you paste them into `config.json`: `broadcastifyCallsServer`, `broadcastifyOTA` and
`broadcastifySslVerifyDisable` in its settings, and `broadcastifyApiKey` and
`broadcastifySystemId` in a system's.

## Getting help

For anything about your Broadcastify Calls node (its system IDs and API key, its status on
Broadcastify's Manage page, calls Broadcastify rejects or doesn't show), email Broadcastify at
[support@broadcastify.com](mailto:support@broadcastify.com). Say which node it is (its system ID)
and what the plugin's log says, with the last error if there is one. Don't send your API key.

For the recorder itself (radios, decoding, Setup), see [Troubleshooting](../troubleshooting.md).
