# Upload script

This page covers the Upload script plugin: what your script is given, its settings, how failures
and retries work, and how to bring a Trunk Recorder `uploadScript` over.

The Upload script plugin runs a script or program of yours on each call the recorder records: to
copy it to a server, upload it somewhere no plugin does, transcribe it, or anything else. It does
what Trunk Recorder's `uploadScript` setting does, and existing scripts work unchanged.

| | |
|---|---|
| Plugin id | `upload-script` |
| Hears | recorded calls |
| Audio | asks for M4A, so the third path has a file when there's an encoder |
| Source | [github.com/TrunkRecorder/trunk-plugin-upload-script](https://github.com/TrunkRecorder/trunk-plugin-upload-script) |

## Setting it up

1. Make your script executable (`chmod +x ~/bin/upload-call.sh`), and try it by hand on a call's
   files first.
2. On **Plugins** > **Install & settings**, find **Upload script** under **Find plugins** and press
   **Install** (or run `trunk-pro plugin install upload-script`).
3. In **Setup** > **Plugins**, turn **Upload script** on and fill in **Script** with the script's
   full path and any arguments of your own.
4. To run a different script for one system, put it in that system's card under **Plugins** >
   **Upload script**.
5. Start recording, or if you're recording already, the plugin restarts with the new settings. Its
   log says what it runs for each system: `dcfd: running /home/pi/bin/upload-call.sh`.

To try it on calls already recorded, without waiting for new ones:

```bash
trunk-pro plugin run upload-script ~/TrunkRecorderPro/dcfd --limit 3
```

## Settings

For the whole recorder (Setup > **Plugins**, or `plugins.upload-script.settings` in
`config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `script` | | | string | **Script.** What to run for each call: the full path of a script or program, then any arguments of your own. Systems with a script of their own run that instead. |
| `timeoutSeconds` | | `300` | integer, 1–3600 | **Time limit (seconds).** A run that takes longer is stopped, and counts as failed. |

For each system (the system's card, or its `plugins.upload-script` in `config.json`):

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `script` | | (the script above) | string | **Script.** This system's own script, run instead of the one above. |

```json
{
  "plugins": {
    "upload-script": { "enabled": true, "settings": { "script": "/home/pi/bin/upload-call.sh", "timeoutSeconds": 300 } }
  },
  "systems": [
    {
      "shortName": "wmata",
      "plugins": { "upload-script": { "script": "/home/pi/bin/upload-wmata.sh --dest 'wmata archive'" } }
    }
  ]
}
```

A system runs its own script if it has one, else the script for the whole recorder; a system with
neither is skipped. The plugin won't start if no system has a script to run (*Set the script to
run.*).

### Writing the Script setting

- The first word is the program, the rest its arguments, split the way a shell would: quote
  arguments with spaces (`/home/me/up.sh 'my server'` or `"my server"`), and `\` escapes the next
  character.
- A script needs its **full path**. `~/` at the start works. A relative path (`./upload.sh`) is
  refused, because the plugin doesn't run in the folder Trunk Recorder did.
- A program on the PATH can be named on its own (`rclone`, `python3`). The plugin also looks in
  `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` and `/bin`, since an app started from the
  desktop doesn't get your shell's PATH.
- The plugin checks the script when it starts: one that isn't there, or isn't executable, stops it
  with a message saying so (*make it executable (chmod +x)*).

## What the script is given

After its own arguments, three paths, in the same order as Trunk Recorder's `uploadScript`:

1. the call's WAV
2. its JSON (the call JSON, in Trunk Recorder's format: see [Recordings](../guides/recordings.md))
3. its M4A

The script runs with the recordings folder as its working folder, with no input, and with the
recorder's own environment; no extra environment variables are set. That environment is the
app's, so if the recorder was started from the desktop, commands *inside* your script may not be
on its PATH: use full paths there, or set `PATH` at the top of the script.

Some things to know about those paths:

- **The M4A is there only when the recorder has an M4A encoder** (built into macOS; install ffmpeg
  elsewhere: see [M4A audio](README.md#m4a-audio)). Without one, the third path is where it would
  be, and there's no file there.
- **The WAV may not be there either.** When there's an M4A encoder and the call's audio isn't kept
  (**Keep the audio after uploading** off), the recorder doesn't write the WAV at all for plugins
  that take M4A, and this one does. If your script needs the WAV, leave that setting on, or have
  the script use the M4A.
- **With a RAM spool**, files that are only kept for the plugins are in the spool folder, not the
  recordings folder. Use the paths you're given rather than building your own.
- **The files may be deleted after your script finishes.** Once every plugin that takes recorded
  calls has reported on a call, the recorder applies your **Files** settings: with **Keep the audio
  after uploading** or **Keep the call JSON after uploading** off, those files go then. A script
  that wants to keep a copy should copy it. See
  [What happens to the files](README.md#what-happens-to-the-files).

Scripts run one call at a time on each of two threads, so two calls can be running at once.

## When it fails

- **Exit status 0** means the script did its job. What it printed goes to the plugin's debug log,
  which isn't shown in the interface.
- **Any other status** marks the call failed, with the last line the script printed (on stderr if
  it printed anything there, else on stdout). Everything it printed goes to the plugin's log.
- **Exit status 75** asks for the call to be tried again later: after 10 seconds, 1 minute,
  5 minutes and 15 minutes, then it's marked failed. While calls are waiting, the plugin shows
  **Needs attention**. Calls still waiting when recording stops are saved in
  `plugin-data/upload-script/queue.jsonl` and run again when recording starts. (Trunk Recorder
  never retries.)
- **A run over the time limit** is stopped (on Linux and macOS, along with anything it started),
  and the call is marked failed (*took longer than 300 s, so it was stopped*).

A failed run keeps the call's files if **Keep everything when an upload fails** is on.

## Coming from Trunk Recorder

**Import Trunk Recorder config…** puts each system's `uploadScript` into that system's **Script**.
Then:

- Change a relative path (`./encode-upload.sh`) to the script's full path, or the plugin won't
  start.
- The arguments are the same, in the same order: your own, then the WAV, the JSON and the M4A.
- The script now runs with the recordings folder as its working folder, not Trunk Recorder's
  folder.
- Trunk Recorder's `audioArchive`, `callLog` and `archiveFilesOnFailure` are the **Files**
  settings in Setup's **Recording** tab, and work the same way: the files are deleted (or kept)
  once the script has run. A script that deleted the files itself can keep doing so.
