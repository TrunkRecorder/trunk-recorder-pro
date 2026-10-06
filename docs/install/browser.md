# In the browser

This page covers the browser version of Trunk Recorder Pro: what it is, how to open or host it,
how to get calls out of it, and what it can't do compared with the desktop app.

## What it is

The browser version is the same decoding engine as the desktop app, compiled to WebAssembly and
running in a background worker inside a browser tab. It reaches an RTL-SDR directly over
**WebUSB** and keeps recorded calls in the browser's own private storage (the Origin Private File
System, OPFS). Nothing is installed and nothing leaves your computer. The interface is the same as
the desktop app's.

It's a quick way to try Trunk Recorder Pro on a system, or to record from a computer where you
can't install anything. For anything you want to leave running, use the desktop app.

## Which browsers

WebUSB is only in Chromium-based browsers:

- **Google Chrome**, **Microsoft Edge**, **Opera** or **Brave** on Windows, macOS, Linux or
  ChromeOS;
- **Chrome on Android**, with the dongle on a USB-OTG adapter.

Safari, Firefox and every browser on iPhone and iPad lack WebUSB. In those the page says "This
browser can't open Trunk Recorder Pro" instead of starting.

WebUSB also only works on secure pages: `https://` addresses, or `http://localhost`. On a plain
`http://` address from another machine the page says "This page needs a secure connection".

## Opening it

The simplest way is the hosted copy at [trunkrecorder.pro/app](https://trunkrecorder.pro/app/),
which is the browser build from the latest release.

Then:

1. Plug in the dongle.
2. Set up a system (or use **Find my system**), as in [Getting started](../getting-started.md).
3. In **Setup → Radios**, press **Connect...** on the dongle source and choose the dongle in the
   browser's list. The browser asks for each dongle once; after that it is remembered for the
   site.
4. Press **Start**.

### Getting the dongle to show up

The browser has to be allowed to open the dongle, the same as any SDR program:

- **Windows:** install the WinUSB driver for "Bulk-In, Interface (Interface 0)" with Zadig, as
  described in [Windows](windows.md#install-the-rtl-sdr-driver-once).
- **Linux:** add a udev rule that gives your user access to the dongle (USB `0bda:2838` and `0bda:2832`); the one in
  the [Linux](linux.md#dongle-access-udev) package works. Unlike the desktop app, the browser can't
  detach the kernel's DVB TV driver, so unload it first:
  `sudo rmmod dvb_usb_rtl28xxu` (or blacklist it so it never loads).
- **macOS:** nothing to do. Quit any other SDR program that holds the dongle.

Only dongles with an **R820T or R828D** tuner work in the browser (nearly all of them; the RTL-SDR
Blog V3 and V4 included). Older E4000, FC0012, FC0013 and FC2580 dongles can only be used by the
desktop app, through SoapySDR.

## Hosting it yourself

The browser version is a folder of static files that runs from any web server, in any folder. Get
`trunk-pro-<version>-browser.zip` from the
[releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases), unzip it, and serve
the folder. To try it on your own computer:

```bash
unzip trunk-pro-<version>-browser.zip
cd trunk-pro-<version>-browser
python3 -m http.server 8000
```

and open `http://localhost:8000`. It won't run from a `file://` address (double-clicking
`index.html`): browsers don't allow WebUSB or module workers there. On a server other people reach,
serve it over HTTPS.

(If you build from source, `npm run build:web` in `web/` produces the same folder as `web/dist-web`;
see [Building from source](README.md#building-from-source).)

## Your calls

Calls are stored inside the browser, per site, in Trunk Recorder's layout
(`<system>/<year>/<month>/<day>/<talkgroup>-<time>_<frequency>.wav` and `.json`). You can play them
on the **Calls** page. At the bottom of that page:

| Button | What it does |
|---|---|
| **Export to folder...** | Copies every call into a folder you pick, in the same layout. Only in browsers that let pages write to folders (desktop Chrome and Edge). |
| **Download zip** | Downloads every call as one zip file. |
| **Delete all** | Deletes every call stored in this browser (only while stopped). |
| **Keep** | Asks the browser to protect the calls from being cleared when the disk gets full. Shown until the browser agrees. |

The same line shows how much space the calls take and how much the browser allows.

Export regularly. The calls live only in this browser's storage for this site: clearing the
browser's site data deletes them, and a browser that runs short of space may clear them unless
you pressed **Keep**. Your settings are kept in the browser's site storage too, so clearing site
data resets them.

## Keeping it recording

The recorder runs only while its tab is open. While it records, the page holds a screen wake lock
so a phone or tablet doesn't sleep (sleeping would freeze the tab). That only works while the tab
is in front: switch tabs or apps on a phone and the browser may pause it.

## What it can't do

Compared with the desktop app, the browser version:

- drives **RTL-SDRs only** (R820T / R828D). There are no USRP, Airspy or SoapySDR sources;
- has **no plugins**, so nothing is uploaded (no OpenMHz, Broadcastify and so on) and there is no
  **Plugins** page;
- makes **no M4A files**, keeps no log files, and has no RAM spool;
- can't **start recording by itself**: you press **Start** each time you open it;
- has no **Platform** page;
- can't link a conventional system to a channel file on disk; channels are entered in the page;
- has no **Quit** button: close the tab to stop it.

Several dongles do work, and so do **Find my system**, the guard band profiler, live listening and
replaying a capture file you pick from your computer. For several dongles or long unattended
runs, the desktop app is sturdier.
