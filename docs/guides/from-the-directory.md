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

## A licensed system no one has shared yet

The directory's map can also show systems that are licensed but that no one has shared (**Licensed, not
shared yet**). Each has a system page like any other, from its FCC license: its sites are the license's
base and repeater stations, with the frequencies licensed at each, and its licenses are those
frequencies. It has no talkgroups or history, since no one has recorded it yet.

**Add to Trunk Recorder Pro** works the same way, but the system comes known only by its licenses: a
license doesn't say which frequency is the control channel, or the system's identity. So instead of
adding it, Trunk Recorder Pro looks for it with [Find my system](find-my-system.md):

- **Start the survey** opens Setup → Systems and scans the bands its licensed frequencies are in.
- The signals it finds on those frequencies are marked as the system's, and listed first.
- Once the radio's frequency error is known, if the survey is listening to another system's control
  channel, it switches to the strongest one on the system's licenses.
- Add the system as usual. It gets its licenses on its frequencies, and the licensee's name. Then
  **Share it**, so the system shows on the directory's map for others.

If you already have a system with its call sign, the import offers to update that one with the licenses
it lacks instead.

The file is the same `trp-system` bundle as a shared system's (`GET /directory/api/v1/systems/fcc_<call
sign>/config`), with no sites (`system.sites` is empty) and its licensed frequencies in `system.fcc`.
