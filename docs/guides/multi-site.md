# Several systems and sites

This page explains how Trunk Recorder Pro records several trunked systems at once, or several
sites of one system: how they share your radios, how a site lock keeps each one on the right
control channel, and how a call heard on more than one site is saved once. Every key is listed
in the [trunked systems reference](../configuration/systems.md#multi-site).

## Several systems

One recorder can follow any number of trunked systems, in any mix of P25, SmartNet, DMR and
NXDN, along with conventional channels. Each gets its own card under **Setup** → **Systems**,
with its own:

- **Short name**: its recordings folder, and the name its talker aliases (`<shortName>.units.csv`)
  are kept under. Two systems can't share one ("Two systems are named "…" — each needs its own
  short name").
- talkgroup file and unit names;
- **Sites**: each a control channel to follow, with its modulation and site lock;
- **Recording override**: its own call rules, where they differ from the **Recording** tab's;
- plugin settings.

**Record** on the card switches a system off without removing it.

The systems share your sources and the pool of recorders. Each control channel runs on whichever
source covers it, and each call on whichever source covers its voice channel, so one wideband
radio can serve several systems, or several radios one system. A source set to **Auto** centres
itself over a site that the sources before it don't cover. On each site's card, chips say which
source carries its control channel and how many of its voice channels are covered. See
[Radios](radios.md).

## Several sites of one system

A large P25 or SmartNet system has several *sites*, each with its own control channel and voice
channels, covering a different area. To record more than one, add each as a site of the same
system: on its card, under **Sites**, **Add site**, then give the new site its control channels and
lock it to its site. The system's talkgroup file, unit names, call rules and plugin settings apply
to every site.

Each site is a folded card with its name, a **Record** switch, its control channels and the chips
saying whether a source covers them. Open it for its modulation, its protocol's channel fields and
its **Site lock**. A site's **Site name** is how the talkgroup file's **Preferred Site** names it,
and what calls and the log say for `site`; left empty, it's the system's short name.

The quickest way: set up one site with **Find my system**, then add its neighbours. A P25 control
channel announces its neighbouring sites, and the recorder offers them in two places:

- in **Find my system**, under **Neighbouring sites** in its checklist, each with an **Add**
  button;
- while recording, in the **Neighbouring sites** panel in a system's detail on the **Decode** page
  ("announced, not recorded"), each with an **Add** button. The new site records from the next
  Start, if a source covers it.

A site added this way becomes a site of its system (a new system, if it isn't set up), with its
control channel, a site lock (WACN, System ID, RFSS and site) and the modulation of a site you
already have. **Find my system** can also add a control channel it found **as a site of** a system,
or put it in place of one of a system's sites.

**Make them one system.** When two systems set up apart announce the same system on the air (P25:
the same WACN and System ID; SmartNet: System ID; NXDN Type-C: system code), each card says so,
with **Make them one system**. The other system's sites move to this one, which keeps its own
talkgroup file and settings; the other's recordings stay on disk.

## Site lock

A site lock tells a site to follow only a control channel that announces a particular identity.
Without one, a site follows whatever it decodes on the frequencies you gave it, which goes wrong
when a neighbouring site, or another system, is on or near one of them.

It's under **Site lock** at the bottom of each site's card. Fill in only what you need; an empty
field matches anything. Which fields there are depends on what the protocol announces:

| Type | Fields |
|---|---|
| P25 | **NAC**, **WACN**, **System ID** (hex), **RFSS**, **Site** (decimal) |
| SmartNet | **System ID** (hex), **Site** (decimal; announced by some VHF/UHF systems) |
| NXDN Type-C | **System ID** (hex), **Site** (decimal) |
| DMR, NXDN Type-D | none: colour code and RAN keep their sites apart |

What happens:

- A control channel that announces something different isn't followed. Its grants are ignored,
  the recorder moves on to the next control channel in the system's list, and the log says why
  ("Control channel 857.98750 MHz is not this system: NAC 2A3 (expected 293)"). The **Decode**
  page marks the site red with the same reason.
- Grants wait until every locked field has been heard. The site number comes in a broadcast every
  few seconds, so the first grants after starting may be held. A call still going is granted again.
- **Clear the lock** removes it.

### Hex in Setup, decimal in the config file

Setup shows and takes NAC, WACN and System ID in hex, the way RadioReference and the dashboard
show them. In the config file, `expect` holds plain JSON numbers, which are decimal:

```json
"expect": { "nac": 1091, "wacn": 781824, "sysId": 1093, "rfss": 1, "site": 3 }
```

is NAC 443, WACN BEE00, System ID 445, site 1-3. If you edit the file by hand, convert first; a hex
value typed as decimal is a different number, and the system will refuse its own control channel.
(`trunk-pro replay --system` takes them in hex.)

## Calls heard on several sites

On a system with several sites, a call is usually granted on every site that has radios on its
talkgroup. Recording each site, you'd get the same call several times. By default it's saved once.

**How it works.** Calls on two sites of the same system are copies of each other when they're on
the same talkgroup and granted within 3 seconds of each other. Every copy is recorded. When the
last one ends, the best copy is saved and the others are dropped. The log says which: "TG 101 FD
Disp was heard on east and west: saved east's copy".

Recording every copy, rather than only the first (which is what Trunk Recorder does), means a site
that fades mid-call, or whose voice channel no source covers, doesn't cost you the call.

**The best copy** is the one with the most cleanly decoded audio: voice frames that weren't lost,
repeated or muted. A tie goes to the one with fewer corrected bit errors, then to the one that
started first. If the best copy isn't kept (shorter than its site's **Shortest call, s**, say), the
next best is tried.

**Preferring a site.** To keep a talkgroup's calls from a particular site when it's heard well
there, add a **Preferred Site** column to the talkgroup file with that site's name. Trunk
Recorder's **Preferred NAC** column also works: the site's NAC, or its RFSS and site as `RRRRssss`
(RFSS 1 site 23 is `10023`), in decimal. The preferred site's copy is kept as long as it has at
least 90% of the best copy's clean audio; below that, the best copy wins anyway.

**On the dashboard.** An active call heard on other sites shows **also on …** under its talkgroup
on the **Calls** page ("Same call on other sites; best copy saved"). Live listening plays one copy
at a time and stays with it until it goes quiet.

**Turning it off.** **Save a call heard on several sites once** on the **Recording** tab
(`recording.dropDuplicateCalls`, on by default). Off, every site's copy is saved in the system's
folder, each with its `site` in its call JSON.

## Which sites count as one system

The sites listed under one system, and only those. Nothing is joined while recording from what the
control channels announce, so list together:

- **DMR** sites and **NXDN Type-D** sites, which announce no system identity;
- two P25 systems linked by **ISSI**, which carry each other's talkgroups under different System
  IDs;
- and keep a site **out** of a system by making it a system of its own.

```json
{ "shortName": "capmax", "type": "dmr",
  "sites": [{ "name": "north", "controlChannelsHz": [452175000] },
            { "name": "south", "controlChannelsHz": [452525000] }] }
```

Importing a Trunk Recorder config makes the systems with `multiSite` on and one
`multiSiteSystemName` one system, with a site each. A config from an older version, whose systems
were each one site, is rewritten on load: those with the same site group, or site locks naming one
system, become one system, each a site named as it was (the old file is kept as
`config.json.before-sites`). Systems that were joined only from the air stay apart until you
**Make them one system**.

## Common problems

**A site keeps saying its control channel is another system's.** The site lock doesn't match.
Check which field the message names; if you edited the config by hand, check decimal versus hex.

**The same call is saved twice.** The two sites aren't sites of one system (set up apart: **Make
them one system**, or move one under **Sites**), or **Save a call heard on several sites once** is
off. Or the grants came more than 3 seconds apart.

**Calls from one site never get kept.** Its copies decode less cleanly than another site's. Add a
**Preferred Site** for the talkgroups you want from it, or improve its reception.

**A neighbouring site was added but records nothing.** No source covers its control channel. Its
site card shows **no control channel inside a source**.
