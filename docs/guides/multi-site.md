# Several systems and sites

This page explains how Trunk Recorder Pro records several trunked systems at once, or several
sites of one system: how they share your radios, how a site lock keeps each one on the right
control channel, and how a call heard on more than one site is saved once. Every key is listed
in the [trunked systems reference](../configuration/systems.md#multi-site).

## Several systems

One recorder can follow any number of trunked systems, in any mix of P25, SmartNet, DMR and
NXDN, along with conventional channels. Each gets its own card under **Setup** → **Systems**,
with its own:

- **Short name**: its recordings folder, and the name its learned band plan (`<shortName>.bandplan`)
  and talker aliases (`<shortName>.units.csv`) are kept under. Two systems can't share one
  ("Two systems are named "…" — each needs its own short name").
- control channels, modulation, talkgroup file, unit names and site lock;
- **Recording override**: its own call rules, where they differ from the **Recording** tab's;
- plugin settings.

**Record** on the card switches a system off without removing it.

The systems share your sources and the pool of recorders. Each control channel runs on whichever
source covers it, and each call on whichever source covers its voice channel, so one wideband
radio can serve several systems, or several radios one system. A source set to **Auto** centres
itself over a system that the sources before it don't cover. On each system card, chips say
which source carries its control channel and how many of its voice channels are covered. See
[Radios](radios.md).

## Several sites of one system

A large P25 or SmartNet system has several *sites*, each with its own control channel and voice
channels, covering a different area. To record more than one site, add each **as its own system**
(as in Trunk Recorder), with its own short name and control channels, and lock each to its site.

The quickest way: set up one site with **Find my system**, then add its neighbours. A P25 control
channel announces its neighbouring sites, and the recorder offers them in two places:

- in **Find my system**, under **Neighbouring sites** in its checklist, each with an **Add**
  button;
- while recording, in the **Neighbouring sites** panel in a system's detail on the **Decode** page
  ("announced, not recorded"), each with an **Add** button. The new site records from the next
  Start, if a source covers it.

A site added this way gets its control channel, a site lock (WACN, System ID, RFSS and site), and
the modulation and talkgroup file of a site of the same system you already have.

When two configured sites belong to the same system, the system card shows a chip **multi-site
with …** naming the others.

## Site lock

A site lock tells a system to follow only a control channel that announces a particular identity.
Without one, a system follows whatever it decodes on the frequencies you gave it, which goes wrong
when a neighbouring site, or another system, is on or near one of them.

It's under **Site lock** at the bottom of the system card. Fill in only what you need; an empty
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
  page marks the system red with the same reason.
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

On a multi-site system, a call is usually granted on every site that has radios on its talkgroup.
Recording each site, you'd get the same call several times. By default it's saved once.

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
there, add a **Preferred Site** column to the talkgroup file with that site's short name. Trunk
Recorder's **Preferred NAC** column also works: the site's NAC, or its RFSS and site as `RRRRssss`
(RFSS 1 site 23 is `10023`), in decimal. The preferred site's copy is kept as long as it has at
least 90% of the best copy's clean audio; below that, the best copy wins anyway.

**On the dashboard.** An active call heard on other sites shows **also on …** under its talkgroup
on the **Calls** page ("Same call on other sites; best copy saved"). Live listening plays one copy
at a time and stays with it until it goes quiet.

**Turning it off.** **Save a call heard on several sites once** on the **Recording** tab
(`recording.dropDuplicateCalls`, on by default). Off, every site's copy is saved, each in its own
site's folder.

## Which sites count as one system

Sites are grouped by what their control channels announce:

- **P25**: the same WACN and System ID.
- **SmartNet**: the same System ID.
- **NXDN Type-C**: the same system code.

Two system entries following the *same* site (the same site number, or the same control channel)
are never treated as copies of each other.

**Site group** on the system card (`siteGroup`) overrides this. Systems with the same group name
are one system, whatever they announce. Use it:

- for **DMR** sites and **NXDN Type-D** sites, which announce no system identity, so they're never
  grouped otherwise;
- for two P25 systems linked by **ISSI**, which carry each other's talkgroups under different
  System IDs;
- to keep a site **out** of its system's group: give it a name of its own.

```json
{ "shortName": "capmax-north", "type": "dmr", "siteGroup": "capmax", "controlChannelsHz": [452175000] }
```

Importing a Trunk Recorder config turns its `multiSiteSystemName` into site groups.

The **multi-site with …** chip in Setup groups sites the way the recorder does: by site group;
otherwise by the site lock (P25 WACN and System ID, SmartNet System ID, NXDN Type-C system code);
otherwise, while recording, by what the control channels announce. Two entries following the same
site are never grouped. Hover over the chip to see why the sites are grouped.

## Common problems

**A site keeps saying its control channel is another system's.** The site lock doesn't match.
Check which field the message names; if you edited the config by hand, check decimal versus hex.

**The same call is saved twice.** The two sites aren't in one group: they're DMR or Type-D sites
without a **Site group**, ISSI-linked systems with different System IDs, or **Save a call heard on
several sites once** is off. Or the grants came more than 3 seconds apart.

**Calls from one site never get kept.** Its copies decode less cleanly than another site's. Add a
**Preferred Site** for the talkgroups you want from it, or improve its reception.

**A neighbouring site was added but records nothing.** No source covers its control channel. Its
card shows **no control channel inside a source**.
