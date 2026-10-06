# macOS

This page covers installing Trunk Recorder Pro on a Mac, running it, and where it keeps its files.
It runs on macOS 11 (Big Sur) or newer, on Apple silicon and Intel Macs.

## Install the app

1. Download `trunk-pro-<version>-macos.dmg` from the
   [releases page](https://github.com/TrunkRecorder/trunk-recorder-pro/releases).
2. Open it and drag **Trunk Recorder Pro** to **Applications**.
3. Open **Trunk Recorder Pro** from Applications.

If the release isn't signed with an Apple Developer ID, macOS blocks the first launch and says it
can't check the app. Open it once, then go to **System Settings → Privacy & Security**, scroll
down to the message about Trunk Recorder Pro and click **Open Anyway**. You only have to do this
once per version.

RTL-SDR dongles need no driver on macOS. Plug one in before or after you start the app.

## Running it

The app has no Dock icon and no menu bar. It runs in the background and opens its interface in
your default browser at `http://localhost:8080`. From there:

- If you close the browser tab, the app keeps running and recording. Open **Trunk Recorder Pro**
  again and it brings up the running one in your browser instead of starting a second copy.
- To stop it, click **Quit** in the top bar of the interface. Calls in progress are saved before
  it exits.

Now go to [Getting started](../getting-started.md) to set up your system.

### Recording unattended

To have a Mac record by itself after a restart:

1. Turn on **Start recording when the app starts** in **Setup → Recording**. Without it the app
   starts but waits for you to press **Start**.
2. Add **Trunk Recorder Pro** to **System Settings → General → Login Items** so it starts when you
   log in.
3. Stop the Mac from sleeping while it records (in **System Settings → Energy**, or **Battery** on
   a laptop). A sleeping Mac stops reading the dongle.

## The command-line version

The program inside the app is the full command-line tool. You can run it from Terminal:

```bash
"/Applications/Trunk Recorder Pro.app/Contents/MacOS/trunk-pro" --help
```

If you would rather not use the app (on a Mac used as a server, say), download
`trunk-pro-<version>-macos-universal.tar.gz` instead. It holds the same `trunk-pro` binary on its
own:

```bash
tar xzf trunk-pro-<version>-macos-universal.tar.gz
sudo cp trunk-pro-<version>-macos-universal/trunk-pro /usr/local/bin/
trunk-pro                  # opens http://localhost:8080
```

Run from Terminal, the log is printed in the window, and Ctrl-C stops it (calls in progress are
saved first). `trunk-pro --no-open` starts it without opening a browser, `--start` begins recording
at once, and `--bind 0.0.0.0` makes the interface reachable from other machines (there is no
login, so only do that on a network you trust). [Command line](../command-line.md) lists every
option.

## Where things are kept

| What | Where |
|---|---|
| Settings (`config.json`) | `~/Library/Application Support/trunk-pro/` |
| Band plans, talker aliases, plugins, statistics | The same folder, beside `config.json` |
| Log files (when **To files** is on in **Setup → Recording → Log**) | `logs/` in the same folder |
| Recordings | `~/TrunkRecorderPro/` (change it in **Setup → Recording → Recordings folder**) |

`~/Library` is hidden in Finder; use **Go → Go to Folder...** and paste the path.

## M4A files

Plugins that upload compressed audio (OpenMHz, Broadcastify and others) need M4A files. On macOS
Trunk Recorder Pro makes them with `afconvert`, which is built into macOS, so there is nothing to
install. If `ffmpeg` is installed (for example with `brew install ffmpeg`), it is used instead.

## Other radios

USRPs, Airspys and SoapySDR devices work once their drivers are installed with Homebrew
(`brew install uhd`, `brew install airspy`, `brew install soapysdr`). See
[Other radios](other-radios.md).

## Uninstalling

Quit the app, then drag **Trunk Recorder Pro** from Applications to the Trash. Your settings and
recordings stay where they are; delete `~/Library/Application Support/trunk-pro/` and
`~/TrunkRecorderPro/` if you want them gone too.

## Problems

- **The dongle isn't listed.** Quit any other SDR program (SDR++, GQRX, CubicSDR, `rtl_tcp`):
  only one program can use a dongle at a time. Then click **Refresh** next to the dongle in
  **Setup → Radios**.
- **"Port 8080 is in use by another program."** Something else is using the port. Start with
  another one from Terminal: `trunk-pro --port 8081`.
- **The app starts and immediately shows an alert.** The alert says why it couldn't start; the
  usual reason is a `config.json` that isn't valid JSON after a hand edit.

More in [Troubleshooting](../troubleshooting.md).
