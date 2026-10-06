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

## When changes apply

Changing any plugin setting, recorder-wide or per system, or `recording.m4a`, restarts the running
plugins at once with the new settings, even while recording. The OpenMHz, Broadcastify, Rdio Scanner
and upload-script plugins save calls still waiting in `plugin-data/<id>/queue.jsonl`, so a restart
doesn't lose them.

## Files

| Where | What |
|---|---|
| `plugins/<id>/<id>` (`<id>.exe` on Windows) | The installed executable |
| `plugin-data/<id>/` | Its own data and working folder |
| `plugin-registry.json` | The plugin store's cached index |

All beside the config file; see [Files kept beside the config](README.md#files-kept-beside-the-config).
