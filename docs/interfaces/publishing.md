# Publishing an interface

The interface store lists interfaces from the
[TrunkRecorder/interfaces](https://github.com/TrunkRecorder/interfaces) registry, as the plugin
store lists [plugins](../plugins/writing-plugins.md). Each listing pins one release: its version,
the commit it was built from, and the SHA-256 of its archive. The recorder installs only an
archive that matches, so what users get is what was reviewed.

Your interface lives in your own GitHub repository. The registry only points to its releases.

## The folder

An interface is a folder of web files with no server side:

```text
index.html          the page the interface opens on
interface.json      what it is
screenshots/        pictures of it, for the store (optional)
app.js, style.css…  whatever else it uses (any layout; subfolders are fine)
```

It's served at the root of a port of its own, with the recorder's API beside it. Refer to your
own files with relative paths (`./app.js`) and to the recorder with absolute ones
(`/api/client.js`, `/api/ws`, `/calls/…`). Don't put files under `api/` or `calls/`: those paths
are the recorder's. Hidden files (names starting with `.`) aren't served.

Don't hard-code a host or port: `new TrunkClient()` connects to the page's own. Keep it
self-contained. Load nothing from a CDN at run time, because a recorder may have no internet.

### `interface.json`

```json
{
  "id": "trunkamp",
  "name": "Trunkamp",
  "version": "1.0.0",
  "description": "A Winamp-style player: live audio and recorded calls, with an equalizer.",
  "api": 1,
  "access": "listen",
  "license": "MIT",
  "homepage": "https://example.com/trunkamp",
  "repository": "https://github.com/someone/trunkamp",
  "screenshots": [
    { "path": "screenshots/player.png", "caption": "The player, with the playlist" },
    { "path": "screenshots/phone.png", "caption": "At phone width" }
  ]
}
```

| Key | Required | Description |
|---|:---:|---|
| `id` | ✓ | Lowercase letters, digits and `-`, up to 64 characters. It names the folder it's installed in and its entry in the config |
| `name` | ✓ | What it's called on the Interfaces page |
| `version` | ✓ | Semantic version (`1.0.0`, `1.1.0-beta.1`); updates are offered when the store's is newer. Its release's tag is `v<version>` |
| `description` | ✓ for the store | One sentence |
| `api` | ✓ | The interface API it's written for: `1`. A recorder speaking an older one won't install it (its `hello.api` and `GET /api/version` say which it speaks) |
| `access` | | `"listen"` (the default) or `"control"`: what it needs. See below |
| `license` | ✓ for the store | An SPDX identifier (`MIT`), shown on its card |
| `homepage`, `repository` | | Web addresses (`https://…`), shown on its card |
| `screenshots` | | Up to 6 pictures of it, each `{ "path", "caption"? }`: a `.png`, `.jpg` (`.jpeg`) or `.webp` of at most 2 MB in its folder, each file name different. The store shows them before it's installed, and the Interfaces page on its card; the first one leads, so make it the one that says the most. About 1280 × 800 reads well |

There's no author field: the store shows the GitHub account the repository is in ("by
someone" for `github.com/someone/trunkamp`).

**Ask for listen access** unless the interface must change the recorder. Listen lets it see
everything the recorder shows and play every call; what it can't do is in
[the API guide](../api/README.md#access). An interface that asks for control is reviewed more
closely, and users are warned before installing it.

## A release

A release has these files:

- `<id>-<version>.zip`: one folder, `<id>-<version>/`, holding `interface.json`, `index.html`
  and the rest, screenshots included.
- Each screenshot again, on its own, as `<id>-<version>-<file name>`
  (`trunkamp-1.0.0-player.png`), so the store can show it without the zip. File names must be
  different from each other.
- `SHA256SUMS`: every file's checksum, as `sha256sum` writes it.

The [interface template](https://github.com/TrunkRecorder/trunk-interface-template)'s release
workflow makes them when you push a tag `v<version>`, and attests each file's build provenance;
the registry checks the zip's. Start from the template, or copy its `.github/workflows/release.yml`.

A release made this way can be installed before it's listed, with **Install from GitHub…** or
`trunk-pro interface install <owner>/<repo>`: that checks the zip against the release's own
`SHA256SUMS` (it arrived intact, but nobody reviewed it) and gives it listen access only.

## Listing it

1. Release it (above).
2. Fork the registry and add your entry:

   ```sh
   ./add-release https://github.com/you/trunkamp v1.0.0
   ./check interfaces/trunkamp.json
   ```

   `add-release` writes `interfaces/<id>.json` from the release and rebuilds `index.json`;
   `check` downloads the zip and checks it.
3. Open a pull request. A maintainer reviews it.

To release an update, do the same with the new tag. Your entry keeps its tier.

## An entry

```json
{
  "id": "trunkamp",
  "name": "Trunkamp",
  "description": "A Winamp-style player: live audio and recorded calls, with an equalizer.",
  "repository": "https://github.com/someone/trunkamp",
  "license": "MIT",
  "tier": "community",
  "version": "1.0.0",
  "api": 1,
  "access": "listen",
  "tag": "v1.0.0",
  "commit": "…40 hex…",
  "asset": {
    "url": "https://github.com/someone/trunkamp/releases/download/v1.0.0/trunkamp-1.0.0.zip",
    "sha256": "…64 hex…"
  },
  "screenshots": [
    {
      "url": "https://github.com/someone/trunkamp/releases/download/v1.0.0/trunkamp-1.0.0-player.png",
      "sha256": "…64 hex…",
      "caption": "The player, with the playlist"
    }
  ]
}
```

`screenshots` is in `interface.json`'s order, with its captions; each file's checksum must match
the same picture in the zip. The recorder fetches a screenshot itself, checks it, and keeps it,
so the page showing the store never reaches GitHub.

`name`, `description`, `api`, `access`, `license` and `homepage` are what its `interface.json`
says. `tier` is `official` for interfaces maintained with the recorder (in the TrunkRecorder
organization) and `community` for everyone else's.

## Interfaces written for an older recorder

Recorder 0.2.0 changed the protocol without changing `api` (still `1`), so an interface written for
an earlier recorder may need a new version. The API guide has the messages; what moved:

- **`setConfig` names the version it edits**: `{ config, version }`, with the `version` the last
  `config` message carried. If the config changed elsewhere meanwhile, the edit is refused. Every
  `setConfig` is answered to its sender with `configSaved { version, error? }`.
- **Talkgroups are not in the config.** Each system's talkgroup file arrives as a `talkgroups { system,
  csv }` message (when you connect, and when one changes), and an interface with control access changes
  one with `setTalkgroups { system, csv }`. `talkgroupsCsv` and `talkgroupsName` are gone from the
  config.
- **`hello` comes last.** A new connection is sent `state`, `config`, `talkgroups`, `units`, `heard`,
  `history`, `radios`, `survey`, `monitorEvent`s and `host` first, as it would be sent any later
  change, then `hello`, which means "caught up" and carries only `{ version, platform, api,
  configPath?, surveyBands }` and `capabilities` (what the recorder can do beyond recording:
  `service`, `drivers`, `plugins`, `interfaces`, `host`, `keys`). Wait for it before treating
  something missing as missing.
- **Systems are numbered from 0**, conventional ones too (there is no `CONVENTIONAL` number), and
  `status.systemNames` lists every system's short name by that number.
- **`status` is slimmer**: a site's neighbours, DMR and NXDN channels and a system's patches come in
  `decodeDetail` (topic `decode:<shortName>`).
- **One store for both kinds**: `store { kind }`, `install { kind, … }` and `installProgress { kind, … }`,
  with `kind` `plugin` or `interface`, replace the separate plugin and interface messages.
- **A site's identity is one shape** everywhere, and a field not heard yet is left out rather than
  `null`: test with `!= null`.

Since 0.2.0 (still `api` 1), what an interface may notice:

- **Conventional systems are in `status.systems`** too, after the trunked ones, with `sites: []`
  (and `conventionalOpen`): a table of sites should still give them a row.
- **A conventional channel's code** is `code` (the CTCSS/DCS code, NAC, colour code or RAN),
  `slot` and `matchTalkgroup`; its old `tone` is gone. A digital channel with no `talkgroup` files
  its calls under the talkgroup the air names.
- **`CallRecord.nac` is text**, in hex as everywhere else (`"293"`; `""` for none), not a number.
- **EDACS** is a system `type` and `system_type`, beside P25, SmartNet, DMR and NXDN.
- **A call may be one transmission**: a system with `recording.conversationMode` false makes each
  transmission a call. Copies of one transmission saved from a system's sites share
  `CallRecord.transmission_id`.
- **A shared system's `share.state`** can be `needsCallSign`.
- **New, nothing removed**: `pluginAction` / `pluginActionResult` (by `id`), `licenseQuery` /
  `callSignQuery`, `radioQuery`'s `tones`, more `monitorEvent` kinds, `host.platform.clock` and the
  RAM spool's `removed` / `missing`.

The JavaScript client does most of this for you (`rec.configVersion`, `rec.talkgroupFiles`,
`rec.setTalkgroups()`, `rec.capabilities`, `systemName(status, system)`, `rec.talkgroups()`), so an
interface that uses it and none of the above directly needs no change. Say in your README which
recorder version it is made for.

## What review looks at

The checks prove where the files came from. A maintainer reviews the rest:

- **The source is public**, and its license allows it to be listed.
- **It does what it says**, and sends nothing anywhere the user didn't ask for. In particular, no
  analytics and no calls to outside services the description doesn't mention.
- **It loads nothing from elsewhere** at run time: no CDN scripts, fonts or styles.
- **It asks for the access it needs**: listen, unless it really has to change the recorder.
- **It works at phone width** and in the browsers people use.
- **Its screenshots show it**, as it is in this version, with nothing private in them (no real
  unit names or talkgroups you shouldn't share).
- **The id is fitting**, and doesn't impersonate another interface or service.
