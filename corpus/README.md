# The regression corpus

`corpus.json` lists the recordings regression tests run on, each file by its SHA-256.
See [the plan](../research/iq-corpus.md).

```bash
cargo build --release -p trunk-pro
target/release/trunk-pro tool corpus run --out /tmp/after
```

The recordings aren't in git. `tool corpus` keeps them in `captures/.corpus`, and downloads any it
doesn't have from `iq.trunkrecorder.pro`. That site isn't up yet, so for now only a machine with the
reference recordings in `captures/` can run this, after `corpus/seed.py` puts them in the cache.

`seed.py` writes the scenarios of `captures/regress.sh` as SigMF capture folders, with their decoding
settings in the metadata, and adds them to the manifest. `tool corpus run` lays its output out as
`regress.sh` does, so `captures/compare.sh` compares the two.
