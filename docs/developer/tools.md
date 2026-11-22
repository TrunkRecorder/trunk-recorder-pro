# Developer tools

The `trunk-pro tool …` commands for testing and measuring the decoders: synthetic captures, the
CPU cost of a radio's read path, the regression corpus, checking a submitted capture, channel
tapes and weak-signal curves. The diagnostic tools an operator uses on a capture (`cc`, `voice`,
`p2`, `smartnet`, `dmr`, `nxdn`, `revoice`, `sdr`, `tones`) are in
[Command line](../command-line.md#tool). How these fit into testing a change is in
[Testing](testing.md).

Like the others, they print JSON lines on stdout with a summary on stderr, and where a tool takes
`--rate`, `--fs` is accepted as well.

| Tool | What it does |
|---|---|
| [`tool p25synth`](#tool-p25synth) | Make a synthetic P25 Phase 1 capture, encrypted with a key of yours or clear |
| [`tool dmrsynth`](#tool-dmrsynth) | Make a synthetic DMR capture |
| [`tool nxdnsynth`](#tool-nxdnsynth) | Make a synthetic NXDN capture |
| [`tool perf`](#tool-perf) | One radio's read path over a capture, for counting its CPU cost |
| [`tool corpus`](#tool-corpus) | The recordings regression tests run on: add, download and replay them |
| [`tool submission`](#tool-submission) | Check that a capture is what it says it is: its hash and size, and that each radio's recording is IQ |
| [`tool channel`](#tool-channel) | Run one channel tape through its decoder alone |
| [`tool snr`](#tool-snr) | Weak-signal curves: decode a strong channel again with noise added |

## Synthetic captures

### tool p25synth

```bash
trunk-pro tool p25synth <out.cu8|out.cf32> [--center 851000000] [--freq centre+100000]
                        [--rate 2400000] [--keys keys.json] [--mi HEX] [--voice call.frames.jsonl]
                        [--superframes 10] [--wav out.wav] [--nac 659] [--tg 1201]
```

Writes one P25 Phase 1 transmission (a header, then 180 ms voice frames, then a terminator) on
`--freq`. With `--keys`, the voice is encrypted with the first key in that
[keys file](../configuration/systems.md#encryption-keys-keys), under its algorithm and key ID, with
`--mi` (16 hex digits) as the first message indicator. Without it the transmission is clear.
`--voice` takes the voice from a call saved with **Save vocoder frames** (`.sdr` or
`.frames.jsonl`); otherwise the frames are made up and sound like noise. `--wav` also writes the
transmission as 48 kHz discriminator audio, the input DSD-style decoders take.

Use it to check decryption without an encrypted system on the air:

```bash
trunk-pro tool p25synth test.cu8 --keys keys.json --voice call.frames.jsonl
trunk-pro replay test.cu8 --center 851000000 --rate 2400000 --p25 851100000 --keys keys.json
```

The call should be saved with `"decrypted": 1` and sound like the call `--voice` came from.
Replay it with a different key and it's noise.

### tool dmrsynth

```bash
trunk-pro tool dmrsynth <out.cu8> [--center 464000000] [--freq centre+100000] [--rate 2400000]
                        [--superframes 15] [--tg 101] [--source 1234] [--cc 1]
```

Writes a synthetic DMR repeater on `--freq`: idle bursts, a voice call on slot 1 (made-up voice
frames), idle again. `replay <out.cu8> --center 464000000 --rate 2400000 --dmr 464100000` records
the call.

### tool nxdnsynth

```bash
trunk-pro tool nxdnsynth <out.cu8> [--center 451000000] [--kind conv|typeC|typeD] [--nxdn 48|96]
                         [--rate 2400000]
```

Writes a synthetic capture, handy for checking a setup when there's no NXDN on the air near you:

- `conv`: a conventional call at centre + 50 kHz
- `typeC` (the default): a control channel at centre + 18.75 kHz granting group 3001 on
  channel 12, whose call is at centre + 68.75 kHz
- `typeD`: repeaters at centre + 12.5 and + 62.5 kHz, the call on the second

## Measuring and testing

### tool perf

```bash
trunk-pro tool perf rtl|hackrf|airspy <capture.cu8> [--rate 2400000] [--passes 1]
```

Runs one radio's read path over a capture, on one thread: the bytes in the blocks that radio's
driver hands over, through the driver's own per-block code (for an RTL-SDR or HackRF, the check for
a stuck stream) and out of the queue its decoding thread reads. Nothing else runs, so counting the
process's instructions gives that path's cost; `--passes N` goes over the capture N times. It's what
`perf/gate.py` measures; see [CPU cost](README.md#cpu-cost).

### tool corpus

```
trunk-pro tool corpus add <capture folder> --manifest file --id ID --protocol P [--tier wideband]
trunk-pro tool corpus fetch [--only a,b] [--tier t]
trunk-pro tool corpus run --out dir [--only a,b] [--tier t] [--against next|version] [--bless next|version]
trunk-pro tool corpus release <version>
```

The recordings regression tests run on, listed at [trunkrecorder.pro/corpus](https://trunkrecorder.pro/corpus) by SHA-256. `add` lists a SigMF
capture in a local manifest and puts its files in the cache. `fetch` downloads what the cache lacks, resuming an interrupted
download and keeping a file only if it matches its hash. `run` replays each capture into `<out>/<id>`, with
what it printed in `<out>/<id>.stdout` and `.stderr`, compares each with its expected output, writes
`<out>.report.json`, and fails if any replay failed or regressed. `--bless` keeps what it got as the expected
output of the next release or of a version. `release` makes the unreleased expected output that version's.

| Option | Default | What it does |
|---|---|---|
| `--manifest url-or-file` | `trunkrecorder.pro`'s list | The list of captures |
| `--expected dir` | `corpus/expected` | Where expected output is kept |
| `--cache dir` | `TRP_CORPUS_CACHE`, or `captures/.corpus` | Where the files are kept |
| `--base url` | `https://iq.trunkrecorder.pro/blobs/` | Where files are downloaded from, by hash |
| `--api url` | `https://trunkrecorder.pro/corpus/api/v1` | The corpus API: the capture list, and the expected output of other versions |
| `--only a,b` | all | Just these capture ids |
| `--tier t` | all | Just this tier (`wideband`, `control`, `voice`) |
| `--against v` | `next`, else the newest at or below this build | The expected output to compare with |

### tool submission

```
trunk-pro tool submission <folder> --out dir
```

Checks that a capture is what it says it is: its hash and size, and that each radio's recording is IQ from a radio (not
random or compressed bytes, whose spectrum is flat, nor text or other bytes that sit to one side of zero). It counts
clipping, lost samples and what decodes, but a capture that decodes nothing still passes: that's what a new decoder, or
a failure being debugged, needs. It writes `checks.json` and a `waterfall-N.bin` for each radio to `dir`, as counts and
levels, and fails if the capture doesn't check out. For a wideband capture, `sites` sets each site's control channel
blocks as live decoded them beside the replay's over the same part of it (`live_window`; none for an older capture,
when the replay's are over all of it): `live_good`, `live_bad`, `replay_good`, `replay_bad`, and their totals.

### tool channel

```
trunk-pro tool channel <tape folder> [--against <other tape folder>]
```

Runs a channel tape (`replay --tapes`) through its decoder alone and prints counts and hashes of what it heard.
`--against` says how close two tapes of the channel are, in dB: what a channelizer change did to it.

### tool snr

```bash
trunk-pro tool snr <capture.cu8> --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet|p2
                   [--seconds N] [--snr 40,20,16,14,12,10,8,6] [--variant base,…] [--cutoff Hz]
                   [--trials 1] [--nac --sysid --wacn] [--repeat] [--kinds]
```

Takes one strong channel from a capture and decodes it again and again with noise added at each
SNR, counting how much of what the clean run decoded still comes through. It's how receiver
changes are compared. `--kind p2` needs `--nac`, `--sysid` and `--wacn` (hex). `--quality`
(with `--kind p2`) reports a real channel as it is, and `--separation` the C4FM receiver's level
separation at each SNR. `--repeat` (with `--kind p25`) scores the vocoder's frame-repeat rules,
on soft- and hard-decision error counts: wrong frames played, good frames repeated, and how far
the audio is from the clean run's. `--kinds` (with `--kind p25`) counts the voice channel's
signalling per kind of word (link control, encryption sync, TDULC) rather than together.
