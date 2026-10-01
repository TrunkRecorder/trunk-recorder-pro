# Vocoder Shootout

Decode the same P25 IMBE frames with several vocoders, score them, and build a
blind listening page. Because every decoder gets identical bits, any difference
you hear comes from the decoder, not the radio channel.

```
./shootout.py setup                       # once: builds the TR harness, Python env, DNSMOS model
./shootout.py run ~/TrunkRecorderPro/wmata/2026/10/1 -o wmata-oct1
open wmata-oct1/shootout.html
```

The calls need their vocoder frames saved: set `"captureFrames": true` under
`recording` in the trunk-pro config (or tick **Save vocoder frames**) and let
it record for a while first.

## Example calls

[examples/wmata-2026-10-01/](examples/wmata-2026-10-01/) holds 85 WMATA
calls (SmartNet with P25 Phase 1 voice, 490 and 496 MHz) recorded on
1 October 2026: each call's `.frames.jsonl` and its `.json` record, about
6 MB. They are nearly error-free, so differences between decoders are the
synthesis alone. Try:

```
./shootout.py run examples/wmata-2026-10-01 -o wmata-example
```

## Inputs

A file or a folder (searched recursively) of IMBE frames, in any of these forms:

| File | Contents |
|---|---|
| `*.frames.jsonl` | trunk-pro's vocoder frame capture. Turn on **Save vocoder frames** (`recording.captureFrames`) and every call gets one beside its WAV. The call's `.json` record next to it supplies the talkgroup name, description and frequency. |
| `*.hex` | One frame per line: 22 hex digits (the 88 bits u0..u7), optionally followed by E0 and ET error counts. |
| `*.imbe` | Raw 11-byte frames, u0..u7 packed MSB first. |

Frames in `.frames.jsonl` carry the FEC error counts, so decoders apply their
own repeat and mute rules exactly as they would live. The other two formats
count as error-free unless a `.hex` line gives the counts.

## Decoders

`./shootout.py decoders` lists them and whether each is ready.

| Key | Decoder |
|---|---|
| `trunk-pro` | trunk-pro's enhanced profile (`trunk-pro tool revoice`) |
| `trunk-pro-fixed` | trunk-pro's fixed-point profile, its default; sample-identical to `tr-fixed` (not run by default) |
| `tr-float` | Trunk Recorder's float decoder, `softVocoder: true`, current settings |
| `tr-fixed` | Trunk Recorder's fixed-point decoder (Pavel Yazev), `softVocoder: false` |
| `mbelib` | mbelib, as in DSD (trunk-pro's mbelib profile) |
| `blip25` | [blip25-mbe](https://github.com/openBLIP25/blip25-mbe), spec-faithful Rust decoder |
| `tr-air` | TR float with `aper_max=0.5`, high-band noise for over-voiced encoders |
| `tr-stock` | TR float with the September 2026 synthesis changes off (not run by default) |

Add one by adding an entry to [decoders.json](decoders.json). A decoder is a
command that reads `{in}` (a `.frames.jsonl`) and writes `{out}` as an 8 kHz
WAV or raw 16-bit samples. The TR harness takes any of the float decoder's
`VocoderParams` as `name=value` (see `software_imbe_decoder.h`), so variants
are one line each, for example:

```json
"tr-hf0": { "label": "TR float, no lift", "note": "…", "cmd": ["{harness}", "float", "{in}", "{out}", "hf_lift_db=0"], "output": "s16", "needs": ["harness"] }
```

Pick decoders for a run with `--decoders tr-float,tr-hf0`. The first decoder
listed is the reference that chooses each clip's 8-second excerpt.

## Outputs

| File | Contents |
|---|---|
| `shootout.html` | Self-contained listening page. The top calls by speech content appear as level-matched 8 s clips, AAC at 16 kHz the way OpenMHz gets them. Decoder names are hidden and shuffled until revealed; switching decoders keeps the playback position. Picks are remembered in the browser. |
| `report.md` | DNSMOS table, wins per decoder, voicing statistics, call list. |
| `scores.json` | Every score per call and decoder, for your own analysis. |
| `audio/<call>.<decoder>.wav` | Every full call through every decoder, unlevelled. |
| `DIR.zip` (beside DIR) | The page, report, scores and every WAV in one file to copy to another computer (`--no-zip` skips it). Unzip and open `shootout.html`; it needs nothing else. |

## Scores

DNSMOS is Microsoft's no-reference speech-quality predictor (1–5: signal,
background, overall). Each call is levelled, upsampled to 16 kHz and scored in
9-second windows. It was trained on noisy and processed speech, not vocoders,
so treat differences under about 0.1 as noise and let the listening page
settle close calls. **Wins** counts the calls on which a decoder scored
highest overall. A decoder that loses on every call by a small margin is a real
difference even when the averages look close.

The voicing statistics describe what the radios encoded, so they are the same
for every decoder. A high share of fully voiced frames, with every harmonic up
to 4 kHz sent as a pure tone, sounds buzzy or robotic on any decoder. WMATA's
490 MHz fleet measured 22–23 % on 1 October 2026; DC Fire's P25 system 11 %.

## Setup details

- `setup --tr-source DIR` points the harness at a Trunk Recorder checkout
  (default `~/Projects/Trunk Recorder/source`). It compiles
  `lib/op25_repeater/lib/software_imbe_decoder.cc` and the `imbe_vocoder`
  sources, which need Boost headers (`brew install boost`). Re-run setup after
  changing that tree.
- trunk-pro comes from `$TRUNK_PRO`, else `../../target/release/trunk-pro`,
  else `trunk-pro` on the PATH.
- Python packages go into `.venv/` here; `shootout.py` switches to it by itself.
- ffmpeg makes the page's AAC clips; without it the page carries WAV.
- `.venv/`, `.cache/`, `build/` and `shootout-*/` are local and ignored by git.
