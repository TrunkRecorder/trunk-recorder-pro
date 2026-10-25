# Adding a system from the directory

The [systems directory](https://trunkrecorder.pro/directory/) holds systems Trunk Recorder Pro users
share. Any system there can be added to your recorder: its sites and channels, its FCC licenses and
its named talkgroups (the rest your recorder hears for itself, soon enough).

On the system's page in the directory, **Add to Trunk Recorder Pro**:

- **Open in Trunk Recorder Pro for the web**: opens the browser version
  (trunkrecorder.pro/app) with the system ready to add.
- **Open in Trunk Recorder Pro**: the desktop app, at the address you open it at (`http://localhost:8080`
  by default; the directory remembers what you type).
- **Download for Trunk Recorder Pro**: one file (`….trp-system.json`). In Trunk Recorder Pro,
  **Setup → Systems → Import…**, and choose it.

Either way, Trunk Recorder Pro shows what it will add before adding it:

- which of its sites to add (all, at first);
- its short name (its folder for calls), one none of your systems has;
- if you already have the system (the same directory record, the same P25 identity, or a control
  channel in common), **Update** it instead: it gets the directory link, and the sites, licenses and
  talkgroups it lacks. Nothing of yours is replaced: not its name, not a talkgroup you named, not a
  license you typed.

The system keeps its link to the directory (`directory` and each site's `directoryId`), so if you
[share it](../configuration/systems.md#sharing-with-the-directory), your shares add to the same record.
Afterwards, Setup's to-do list says if no radio covers its frequencies yet.

The file is the directory's `GET /directory/api/v1/systems/{id}/config` (`format: "trp-system"`, `v: 1`):
the system as Trunk Recorder Pro's config has it, its talkgroup file (`talkgroupsCsv`), and where it came
from (`source`).
