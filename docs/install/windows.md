# Windows

This page covers installing Trunk Recorder Pro on Windows, getting the RTL-SDR driver in place,
and where it keeps its files. It runs on 64-bit Windows 10 and 11 on Intel/AMD processors.

## Install the RTL-SDR driver (once)

Windows doesn't let programs talk to an RTL-SDR directly until it has the WinUSB driver. Every
RTL-SDR program needs this, so if SDR# or SDR++ already works with your dongle, skip this step.

1. Plug in the dongle.
2. Download and run [Zadig](https://zadig.akeo.ie).
3. In **Options**, tick **List All Devices**.
4. Pick **Bulk-In, Interface (Interface 0)** from the list. (Don't pick a device you don't
   recognise: Zadig will replace the driver of whatever you choose.)
5. Make sure the driver on the right is **WinUSB** and click **Replace Driver** (or
   **Install Driver**).

Do this once for each dongle, and again if you plug a dongle into a different USB port and it
isn't found.

## Install Trunk Recorder Pro

1. Download `trunk-pro-<version>-windows-x86_64.zip` from the
   [releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases).
2. Unzip it somewhere permanent, such as `C:\Program Files\Trunk Recorder Pro\` or a folder in
   your Documents. There is no installer: the folder holds `trunk-pro.exe` and the license files.
3. Double-click `trunk-pro.exe`.

Windows SmartScreen may say it protected your PC from an unrecognised app. Click **More info**,
then **Run anyway**.

## Running it

`trunk-pro.exe` opens a console window and your browser at `http://localhost:8080`. The console
window is the program: the log scrolls by in it, and closing it quits Trunk Recorder Pro. To stop
it, use **Quit** in the top bar of the interface, press Ctrl-C in the console, or close the window.
Each of these stops recording first, so calls in progress are saved. Windows also lets it save
them when you sign out or shut down. Windows gives a closing program about 5 seconds, which is
plenty for a normal stop.

Closing the browser tab doesn't stop recording. Double-clicking `trunk-pro.exe` while it is
already running opens the running one in your browser.

Now go to [Getting started](../getting-started.md) to set up your system.

### From a command prompt

`trunk-pro.exe` takes the same commands and options as on every other platform. From the folder you
unzipped it into:

```bat
trunk-pro.exe --help
trunk-pro.exe devices
trunk-pro.exe --no-open --start
```

`--no-open` starts it without opening a browser and `--start` begins recording at once.
[Command line](../command-line.md) lists every option.

### Recording unattended

Turn on **Start recording when the app starts** in **Setup → Recording**, so that recording resumes
whenever `trunk-pro.exe` is started. Trunk Recorder Pro doesn't install a Windows service or a
startup entry; to start it at logon, put a shortcut to `trunk-pro.exe` in your Startup folder
(press Win+R and type `shell:startup`). Set Windows not to sleep while it records.

## Where things are kept

| What | Where |
|---|---|
| Settings (`config.json`) | `%APPDATA%\trunk-pro\` (usually `C:\Users\<you>\AppData\Roaming\trunk-pro\`) |
| Band plans, talker aliases, plugins, statistics | The same folder, beside `config.json` |
| Log files (when **To files** is on in **Setup → Recording → Log**) | `logs\` in the same folder |
| Recordings | `%USERPROFILE%\TrunkRecorderPro\` (change it in **Setup → Recording → Recordings folder**) |

Paste `%APPDATA%\trunk-pro` into the File Explorer address bar to open the settings folder.

## M4A files

Plugins that upload compressed audio (OpenMHz, Broadcastify and others) need M4A files, which
Trunk Recorder Pro makes with `ffmpeg`. Install it and make sure `ffmpeg.exe` is on your `PATH`
(for example `winget install ffmpeg`, then open a new console). Without it, calls are saved as WAV
only and plugins are told so.

## Other radios

USRPs (with Ettus's UHD installer), Airspys (`airspy.dll` next to `trunk-pro.exe`) and SoapySDR
devices (PothosSDR) work too. See [Other radios](other-radios.md).

## Uninstalling

Delete the folder you unzipped. Your settings and recordings are kept; delete
`%APPDATA%\trunk-pro\` and `%USERPROFILE%\TrunkRecorderPro\` too if you want them gone.

## Problems

- **The dongle isn't listed, or says it can't be opened.** Check the WinUSB driver (above), and
  close any other SDR program: only one program can use a dongle at a time. Then click
  **Refresh** next to the dongle in **Setup → Radios**.
- **"Port 8080 is in use by another program."** Start it from a command prompt on another port:
  `trunk-pro.exe --port 8081`.
- **The console window flashes and disappears.** Run `trunk-pro.exe` from a command prompt to read
  the error. The usual reason is a `config.json` that isn't valid JSON after a hand edit.

More in [Troubleshooting](../troubleshooting.md).
