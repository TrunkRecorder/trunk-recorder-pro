# photounit's custom Trunk Recorder Pro

This branch (`custom`) is upstream Trunk Recorder Pro plus everything built for
rdio, rdioa and multi that isn't upstream (yet). `main` stays a plain copy of
upstream, so pull requests can still be made from their own branches.

Each build kit gets a tag, `custom-vNN`, so every version stays here even after
the branch moves on to a newer upstream release.

## What's in it

**Current: custom-v39, on upstream v0.5.0 (a5c43761)**

The changes are the commits on top of upstream (`git log v0.5.0..custom`):

| # | Change | Status |
|---|---|---|
| 1 | EDACS: the data burst is found at every channel rate, from the key-up's first moments | custom |
| 2 | SoapySDR Find keeps what tells a radio's entries apart (an RSPduo's tuner mode) | custom |
| 3 | Build tag in `--version`, the log and the page | kit only |
| 4 | Multi-site: one grant carried to several sites is one transmission | custom |
| 5 | SoapySDR: AGC on/off and gain stages apply while recording | custom |
| 6 | Trunked analog calls: the silent tail after the last speech is cut | custom |
| 7, 8 | Multi-site: copies on the air together are one transmission | custom |
| 9 | Rebase helpers for 0.3.5 (changelog entries, a test helper) | custom |
| 10 | Tail trim: with one talker, speech is near the call's loud speech | custom |
| 11 | SoapySDR live gain fitted to 0.3.8's gain model | custom |
| 12 | Setup while recording: Next Start / Live marks; Find lists what it found | custom |
| 13 | Tones: a tone's edge windows no longer throw the whole tone away (page decoding) | custom, PR planned |
| 14 | Plugin page: the settings card above the lists when settings come first | custom |

Also sent upstream separately: `fix-temp-auxtin` (Nuvoton AUXTIN temperature fix).
Already merged upstream: EDACS, bladeRF, live gain, Airspy IQ rate.

## Folders

| Folder | What |
|---|---|
| `custom/kit/` | The build kit's `build.sh` and `UPGRADE.md` (install steps for the boxes) |
| `custom/plugins/page-alerts/` | Page alerts plugin (two-tone paging, Rdio Scanner upload, voice alerts, unknown-tone saving) |
| `custom/plugins/health/` | Health check plugin (ntfy alerts) |
| `custom/tools/trp-health` | The command-line health check script |

## Building from this branch

```sh
git clone -b custom https://github.com/photounit/trunk-recorder-pro.git
cd trunk-recorder-pro
# the same steps build.sh runs after "git am": web UI, then cargo build --release
```

The build kit's patches are these commits: `git format-patch v0.5.0..custom -- . ':!custom'`
gives the same patch files.

Plugins: `cd custom/plugins/page-alerts && bash build.sh` (likewise `health`).
After a rebuild, press **Reinstall** on the plugin in TRP's Plugins page.
