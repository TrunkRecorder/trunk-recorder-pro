# The regression corpus

The recordings regression tests run on are listed at [trunkrecorder.pro/corpus](https://trunkrecorder.pro/corpus), each file by
its SHA-256. See [the plan](../research/iq-corpus.md).

```bash
cargo build --release -p trunk-pro
target/release/trunk-pro tool corpus run --out /tmp/after
```

The recordings aren't in git. `tool corpus` keeps them in `captures/.corpus`, and downloads any it
doesn't have from `iq.trunkrecorder.pro`.

`run` compares each capture with its expected output, prints what differs, and fails on a regression. It also writes
`<out>.report.md`, which GitHub shows on the run's page, and `<out>.report.json`.

| Result | Meaning | Fails CI |
|---|---|---|
| `match` | Decoded the same as expected | |
| `improved` | Everything expected, and more messages or good frames | |
| `changed` | Not clearly better or worse: a new call nobody expected, a call that runs longer, or other bits for as many good frames | |
| `regressed` | A message or call is missing, a call lost good frames or got over half a second shorter, or an analog call's loudness no longer matches | Yes |
| `known-failing` | A known bug, still failing | |
| `fixed` | Was known to fail, and now does better | |
| `no-baseline` | Nothing to compare with yet | |

For each capture that differs, the report gives control messages by type, then each call with its frequency, start and what
changed, the worst first. Expected output holds hashes, not what was said, so to see a message's text, replay the capture on
both builds and compare.

## Expected output

`expected/<id>/<version>.jsonl` is what TRP `<version>` decoded from capture `<id>`, as hashes and counts: one
file for each version whose output changed. `next.jsonl` is what's unreleased.

A change that moves results keeps them in the same pull request:

```bash
target/release/trunk-pro tool corpus run --out /tmp/after --bless next
```

The diff shows how much moved, and a release makes it that version's with
`trunk-pro tool corpus release <version>`.

## Adding a recording to the tests

Anyone can browse what's been contributed at [trunkrecorder.pro/corpus](https://trunkrecorder.pro/corpus). To add one, or take one
off, sign in there with GitHub: you need write access to this repo. A new test compares against what the release that checked it
decoded, until a pull request records its expected output here with `--bless`.

## Seeding

`seed.py` writes the scenarios of `captures/regress.sh` as SigMF capture folders, with their decoding
settings in the metadata, and adds them to a local manifest. `tool corpus run` lays its output out as
`regress.sh` does, so `captures/compare.sh` compares the two.
