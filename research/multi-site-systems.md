# Multi-site: a system with sites

October 2026. Branch `multisite-systems` (worktree `../trunk-recorder-lite-sites`).

Today each configured system is one site. Sites of one radio system are separate
systems, linked while running (P25 by WACN + System ID heard on the air, SmartNet
by System ID, NXDN Type-C by system code, or a `siteGroup` name). Everything else
is keyed by the site's short name: the talkgroup file, unit names, recording
rules, plugin settings and delays, stats, the radio registry, the Listen page's
choices. A saved call carries whichever site's copy won, and the winner changes
from call to call. Each feature that doesn't separately learn "these names are
one system" breaks for multi-site, as a sweep on 2026-10-17 found (Ignore,
plugin delays, per-site settings, counts, aliases, first-seen, plugin events, the
API docs).

The fix is to change the structure, not to patch each feature: a **system** is
the radio system, and a **site** is one control channel it follows.

## The model

| On the **system** | On each **site** |
|---|---|
| `shortName`, `name`, `type`, `enabled` | `name` (unique in its system), `enabled` |
| talkgroups (`talkgroupsCsv`, `talkgroupsName`), `unitNames` | `controlChannelsHz`, `voiceChannelsHz`, `modulation` |
| `recording` overrides, `keys`, `plugins`, `pluginDelays` | `expect` (the site lock) |
| `defaultMode` (SmartNet), `nxdnType` | band plan (SmartNet), `lcnTableHz`, `dmrChannelsHz`, `colorCode`, `nxdnRate`, `nxdnChannelsHz`, `ran` |

```jsonc
{ "shortName": "dcfd", "type": "p25", "talkgroupsCsv": "…", "plugins": { … },
  "sites": [
    { "name": "dcfd", "controlChannelsHz": [851012500], "expect": { "rfss": 1, "site": 1 } },
    { "name": "dcfd-south", "controlChannelsHz": [852337500], "expect": { "rfss": 1, "site": 2 } } ] }
```

- Every system has `sites` (at least one). A one-site system's site defaults to
  the system's short name; more sites default to `<short>-2`, `-3`… A site's
  name is how the talkgroup file's **Preferred Site** names it, how its band
  plan file and its control channel stats are named, and what calls and plugins
  say for `site`.
- **Duplicates are dropped only among a system's sites.** No grouping from the
  air at run time: `siteGroup`, `SiteKey::group` and `ControlChannel::site_group`
  go. DMR, NXDN Type-D and ISSI-linked systems are just sites listed under one
  system. Two separately configured systems announcing the same WACN + System ID
  are no longer merged while running; Setup offers to make them one.
- Conventional systems are unchanged (no sites).

## Migrating

`config.json` (trunk-app `load`, and the web's `normalizeSystem`) is rewritten on
load:

- A system without `sites` gets one, from its own RF fields; its name is the
  system's short name. Nothing else changes.
- Systems that were linked by config — the same `siteGroup`, or site locks with
  the same type, WACN and System ID — become one system. Its short name is the
  `siteGroup` name if there was one, else the first site's short name; each site
  keeps its old short name as its site name (so Preferred Site columns, band plan
  files, recordings folders and the first site's stats keep working). System
  settings come from the first site; a site whose talkgroup file, unit names,
  recording rules, plugin settings, delays or keys differ gets a note saying
  which were dropped.
- Systems linked only from the air can't be seen at load. Setup shows them
  ("dcfd and dcfd2 announce the same system") with **Make them one system**.

On disk:

- Recordings: `<system>/<Y>/<M>/<D>/`. Older calls are still found under each
  site's name (a migrated site's name is its old short name).
- Band plans are per site: `<system>.bandplan` for a site named as its system,
  else `<system>.<site>.bandplan`; a site that was a system of its own reads
  its old `<site>.bandplan` once.
- Talker aliases and the radio registry are per system; a site's old files (by
  its old short name) are read into its system's on load.
- Stats: series per system stay `sys/<system>/…`; control channel series are per
  site, `sys/<system>/cc/…` for a site named as its system (the old key, history
  kept) and `sys/<system>/site/<site>/cc/…` for the others.

## Removing the upgrade

The upgrade is for a while only (there is no promise of compatibility yet).
All of it is in `crates/trunk-app/src/migrate.rs` and `web/src/migrate.ts`,
with their tests (in `migrate.rs` and `web/src/migrate.test.ts`). To drop
it, delete those, `pub mod migrate;` in trunk-app's lib.rs, and the lines
the compilers then point at:

- `Config::from_value` (config.rs): the `migrate::config` call (`notes`
  stays empty);
- trunk-pro `serve` (main.rs): the `migrate::save` block;
- `Session::new` / `Session::load_units` (session.rs): the
  `old_bandplan` / `old_names` lookups;
- `older_calls` (runtime.rs): `old_names`;
- `storedConfig` (config.ts): `migrateSystems(…)` → `.map(normalizeSystem)`;
- `readSelection` (listen.ts): `migrateSelection` (names as they are).

And the docs' "Upgrading" notes (configuration/systems.md, guides/multi-site.md).

## Engine (trunk-core)

- The engine's config stays a flat list, now of sites: `SystemConfig` became
  `SiteConfig` (with `site`, its name), `EngineConfig.systems` became
  `sites`, and sites with one short name are one system, numbered where its
  first site is (`system_numbers`). Each site carries its system's policy
  (talkgroups, call and save rules, unit tags, keys); the app writes the same
  into each. One `Trunk` per site, with `site` and `system`; the alias book
  and wrong keys are per system.
- `Call` gets `site: u16` beside `system: u16`. (Renamed first, so the compiler
  finds every use, then each decided.) Radio-side keys use the site: channels,
  voice params, pending tracker output, `SysHost`, band plans.
- Events: `ControlChannel`, `Message` and control-channel `Note`s carry the site
  (and its system); `Audio`, `UnitAlias`, `UnitPosition` the system.
- `MultiSite`: members are `(CallId, site)`; twins are calls of the same system
  on another site (and of the same kind: unit-to-unit or not), from the first
  grant, without waiting for the site's identity. Preferred site matches the
  site name.
- `set_talkgroups` / `set_keys` / `unit_alias` act on every site of a system.
- Status: `SiteStatus { site, system, site name, … }` per running site, as
  `SystemStatus` is today.

## Recorder (trunk-app, trunk-pro, trunk-web)

- Config: `System` + `Site`, validation (unique site names in a system; per-site
  source coverage), `engine_config`, migration and its notes, `call_systems`,
  `recording_of` / `short_name_of` by system.
- Call JSON: `short_name` is the system; new `site` (its name); `{site}` in
  `filenameFormat`; `{sys_num}` is the system's number.
- Status: `systems[]` per system — `index`, `shortName`, `activeCalls`,
  `recording`, `callsConcluded`, `patches` — with `sites[]`: `index`, `name`,
  `controlChannelHz`, `identity`, `good`, `bad`, `modulation`, `mismatch`,
  `adjacent`, `dmr`, `nxdn`. Calls: `system` / `systemName` the system, `site`
  its site, `alsoOn` other sites.
- Live audio: the frame's `u16` is the system; `listen.system` a system's short
  name (the `sites` list on frames from PR #48 goes).
- Plugins (SDK 0.4.0, API 1): `system` / `index` mean the system; `CallInfo`,
  `ConcludedCall`, `UnitEvent`, `AudioChunk` get `site` (name); `Status.systems`
  stays one entry per site, with `site_name`. One `callStart` per call (its
  first copy) and one `callEnd` (the copy kept).
- CLI: `--system name:Hz…` stays one-site; `--system dcfd/south:Hz…` adds a site
  to system `dcfd`.

## Web

- `protocol.ts` / `config.ts`: `System` + `Site`, `normalizeSystem` migration,
  Trunk Recorder import (`multiSite` + `multiSiteSystemName` make one system
  with sites; talkgroup files folded, conflicts noted).
- Setup: one card per system. System groups: basics, talkgroups and units,
  overrides. A **Sites** group lists site sub-cards (name, Record, control
  channels and coverage, collapsed); open, a site shows modulation, the
  protocol's channel fields and its site lock. **Add site**; neighbours the
  control channel announces add as sites of their system; the survey adds a
  control channel as a new system or a site of one.
- Dashboard: one Overview card per system with a row per site; Decode per
  system with a section per site; RF markers per site; Radio, Live, Listen per
  system. The stitching code goes: `multiSiteGroup`, `siteSiblings`,
  `multiSitePartners`, `bySite`, `MultiSiteChip`, `siteChoices`, Listen's
  `sitesNow` / `regrouped`.
- Docs: configuration/systems.md, guides/multi-site.md, the API (llms.txt,
  schema, client.js, examples), migrating-from-trunk-recorder.md and the
  architecture pages.

## Steps

- [x] 1. Engine: systems with sites, `Call::site`, multisite by system, status per site, tests
- [x] 2. trunk-app: config + migration, engine config, session (names, status, call JSON, `site`), stats keys, aliases / band plans by system or site, older calls
- [x] 3. trunk-pro + SDK + trunk-web: protocol, plugins, server, CLI, browser maps
- [x] 4. Web: protocol, config + migration + TR import, Setup, dashboard, Listen / Live, tests, schema
- [x] 5. Docs, changelog

Checked end to end on 2026-10-17: an older config (two systems joined by a
`siteGroup`, sources the DCFD captures) was migrated on start (backup kept,
notes logged), recorded as one system of two sites (calls in `dcfd/`, call
JSON `site`), and Setup, Overview, Decode, Live and Listen showed it so.

## Left for later

- The radio registry (`radio/<name>.json`) of a site that was a system of
  its own isn't merged into its system's: it stays under its old name (as a
  removed system's does).
- Each site's own plugin identity (an uploader that wants one per site), if
  one turns out to need it.
- Trunkamp (the store's third-party interface): check it reads nothing that
  moved under `sites[]` (`controlChannelHz`, `identity`). Trunk City and
  Trunk-tris read only what stayed the system's (`shortName`, `name`, `type`,
  `enabled`, `talkgroupsCsv`, status `index` → `shortName`).

## Decisions

- No back-compat shim in the protocol (none promised yet); API_VERSION stays 1.
- No run-time grouping from the air (a Setup suggestion instead).
- Old per-site data is found by keeping each migrated site's old short name as
  its site name, rather than moving files.
