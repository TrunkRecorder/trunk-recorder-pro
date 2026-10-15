# Plugins

Plugins are separate programs the recorder runs while it records: uploaders, streamers, scripts. The
config says which run and with what settings, recorder-wide under the top-level `plugins` and per
system under each system's `plugins`. This page lists those keys. For installing, updating and
setting up plugins, see [using plugins](../plugins/README.md) and each plugin's own page.

```json
{
  "plugins": {
    "openmhz": { "enabled": true, "settings": { "server": "https://api.openmhz.com" } }
  },
  "systems": [
    { "shortName": "dcfd", "type": "p25", "controlChannelsHz": [857987500],
      "plugins": { "openmhz": { "apiKey": "...", "systemName": "dcfd" } } }
  ]
}
```

Plugins are installed from the **Plugins** page, or with `trunk-pro plugin install <id>`, into
`plugins/<id>/` beside the config file. Installing one doesn't turn it on; the config does. The
Plugins page and Setup's **Plugins** tab edit these keys for you, with a form built from each plugin's
own description of its settings. Plugins are a desktop feature; the browser version has none.

## Recorder-wide settings

The top-level `plugins` object is keyed by plugin id. Each entry:

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `enabled` | | `false` | bool | Run it while recording |
| `settings` | | `null` | any | Its settings for the whole recorder, as the plugin's own schema describes them (the `config` schema in its manifest; `trunk-pro plugin describe <executable>` prints it) |
| `path` | | `""` | string | Run this executable instead of the installed one: a build of your own, while developing a plugin. Use an absolute path |
| `shareDecrypted` | | `false` | bool | Give it calls decrypted with your keys (their audio and files). See [Decrypted calls](../plugins/README.md#decrypted-calls) |

A plugin with no entry is off. Unknown keys in an entry are dropped on save.

## Per-system settings

Each trunked or conventional system's `plugins` object is keyed by plugin id too. What goes inside is
whatever that plugin's per-system schema (`system_config` in its manifest) asks for: typically an API
key and the system's name or ID on the service.

```json
{ "shortName": "wmata", "type": "smartnet",
  "plugins": { "openmhz": { "apiKey": "...", "systemName": "wmata" },
               "broadcastify": { "apiKey": "...", "systemId": 1234 } } }
```

Plugins know each system by its `shortName`. Renaming a system in Setup carries its plugin settings
with it; renaming it by hand in the file is fine too, since the settings live inside the system.

### Overriding the recorder-wide settings

`override` in a system's settings for a plugin changes any of the plugin's recorder-wide `settings`
for that system alone, such as sending its calls to another server. Settings it leaves out are the
recorder-wide ones. A required setting, such as Rdio Scanner's server, can be left empty
recorder-wide when every system that uses the plugin overrides it.

```json
{ "shortName": "dcfd", "type": "p25",
  "plugins": { "rdioscanner": { "apiKey": "...", "systemId": 3,
                                "override": { "server": "https://rdio.example.org" } } } }
```

In Setup, each plugin on a system's card has a *settings for this system* section for this. Keys
the plugin has no setting for are ignored, with a warning in its log. An uploader keeps a separate
queue for each server it sends to, so a server that's down doesn't hold up uploads to the others.

### Several copies of a plugin

A list in place of one object runs the plugin more than once for the system, to upload to two
places, say. Each copy has its own settings, and its own `override`:

```json
"plugins": { "rdioscanner": [
  { "apiKey": "...", "systemId": 3 },
  { "apiKey": "...", "systemId": 1, "override": { "server": "https://club.example.org" } }
] }
```

The second copy (and any after it) runs as a process of its own, `rdioscanner#2`, with its own data
folder (`plugin-data/rdioscanner#2/`), for the systems that have a second copy. In Setup, **Add
plugin** on a system's card adds one.

## Delays

Plugins can be given calls late: a while after they end, for talkgroups that are sensitive, or for a
service that asks for a delay. Delays are rules in a `pluginDelays` list, at the top of the config
(every system) and in any trunked or conventional system (that system). Each rule picks calls by its
keys, and a key left out picks everything:

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `plugins` | | every plugin | array | Plugin ids. One copy of a plugin is `id#2`, `id#3`… |
| `talkgroups` | | every talkgroup | array | Numbers, ranges (`"1000-1099"`) and patterns (`"507*"`, `"12?45"`), as strings or numbers |
| `delayS` | | `0` | number | How long, in seconds. `0` is no delay, to let something through that a broader rule delays |

```json
{
  "pluginDelays": [{ "plugins": ["broadcastify"], "delayS": 120 }],
  "systems": [
    { "shortName": "dcfd", "type": "p25", "controlChannelsHz": [857987500],
      "pluginDelays": [
        { "talkgroups": ["1000-1099"], "delayS": 1800 },
        { "talkgroups": ["1000-1099"], "plugins": ["rdioscanner"], "delayS": 0 },
        { "plugins": ["broadcastify"], "delayS": 0 }
      ] }
  ]
}
```

When several rules pick the same call for a plugin:

1. A rule that **narrows** another replaces it. A rule narrows another when it picks only calls the
   other picks: a system's rule narrows a top-level one, a rule naming talkgroups narrows one that
   doesn't, and a rule naming plugins narrows one for every plugin. (Talkgroups are compared as
   written: `["1001"]` is inside `["1001", "1002"]`, but not inside `["1000-1099"]`.)
2. Of the rules left, the **longest** delay wins.

So a sensitive system's or talkgroup's delay is never cut short by a plugin's own delay, while a
system can still change one plugin's delay for itself. With the example above, Broadcastify gets
dcfd's talkgroups 1000–1099 after 30 minutes and dcfd's other calls at once (its own `0` narrows
the top-level 2 minutes), and other systems' calls after 2 minutes. Rdio Scanner gets everything at
once. Every other plugin gets 1000–1099 after 30 minutes.

What a delay holds back, for each plugin it applies to:

- **Finished calls** wait in `plugin-hold/` beside the config, one file each, so a restart doesn't
  lose them. When their time comes, they go to the plugin if it's running and recording has started.
  A call waiting for a plugin that's no longer running (turned off, removed), or no longer allowed
  decrypted calls, is dropped, and the plugin's log says so.
- **Live events** about the call (it starting and ending, a unit's activity on its talkgroup) are
  delayed the same, in memory only.
- **Live audio** of a delayed call isn't streamed to that plugin at all.

A call's files stay until every plugin it was held for has had it: the plugin's hour to report on a
call counts from when it's given the call. With a RAM spool, held calls wait there; the spool's
sweep for forgotten files waits the longest delay longer.

In Setup, a plugin's card on the **Plugins** tab sets its delay on every system, and **Plugin delays
on every system** below the cards holds the other top-level rules. A system's card has **Plugin
delays** under Overrides for its own, with what they come to for each plugin. The Plugins page shows
how many calls are waiting for each plugin.

## When changes apply

A change to the delays applies at once, without restarting the plugins, to the calls already
waiting too: shortening one lets them go sooner.

Changing any other plugin setting, recorder-wide or per system, or `recording.m4a`, restarts the running
plugins at once with the new settings, even while recording. The OpenMHz, Broadcastify, Rdio Scanner
and upload-script plugins save calls still waiting in `plugin-data/<id>/queue.jsonl`, so a restart
doesn't lose them.

## Files

| Where | What |
|---|---|
| `plugins/<id>/<id>` (`<id>.exe` on Windows) | The installed executable |
| `plugin-data/<id>/` | Its own data and working folder |
| `plugin-hold/` | Finished calls waiting out a [delay](#delays) |
| `plugin-registry.json` | The plugin store's cached index |

All beside the config file; see [Files kept beside the config](README.md#files-kept-beside-the-config).
