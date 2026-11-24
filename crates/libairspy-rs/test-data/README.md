# Golden test vectors

Reference input/output pairs for the IQ converter ports, generated
from the **original C implementation** so the Rust converters can be
verified bit-for-bit (int16) and within documented tolerance (float32).

## Provenance and licensing

The generating source is airspyone_host at commit
`bd15be38e91ebaa3e0bebb1e320255bde4ccf059` — the last revision on which
`iqconverter_float.[ch]` / `iqconverter_int16.[ch]` carry their
original **MIT license** (Copyright (C) 2014, Youssef Touil). Upstream
"Copyright Update" commits on June 10, 2025 relicensed those files
restrictively (the first of them already flips iqconverter_int16.c, so
the reference must predate them all); this project ports from, and
generates vectors with, the all-MIT revision only. Its code bodies are
identical to the relicensed files apart from a FreeBSD build-flag
ifdef.

These vector files are program output, committed as fixtures.

**State-zeroing note:** upstream's `iqconverter_int16` create/reset
leaves parts of its state uninitialized (the reset memsets use
`sizeof(int16_t)` for the `int32_t` FIR queue and a quarter-sized
delay-line clear), so raw C output depends on allocator leftovers. The
harness therefore zeroes the int16 converter's full state after
create, making the vectors capture the algorithm rather than the
heap; the float converter's reset is correct and needs no such step.

## Format (`iq/`)

Per scenario (`impulse`, `dc`, `tone`, `noise`), three files, one value
per line, 6144 lines each (three sequential 2048-sample process calls,
concatenated, so filter state persistence across calls is captured):

- `<scenario>.input.txt` — raw 12-bit ADC words (`0..4095`, decimal)
- `<scenario>.int16.txt` — the in-place buffer after each
  `iqconverter_int16_process` call; inputs were
  `(word - 2048) << SAMPLE_SHIFT` (decimal `i16`)
- `<scenario>.float.txt` — same for `iqconverter_float_process` with
  inputs `(word - 2048) * SAMPLE_SCALE`; each line is the exact
  IEEE-754 bit pattern as 8 lowercase hex digits

Scenarios: `impulse` = mid-scale with one full-scale spike at index
100; `dc` = constant 3072 (+0.5 FS); `tone` = fs/16 sine at half
amplitude; `noise` = fixed-seed LCG (`s = s*1664525 + 1013904223`,
seed `0x12345678`, top 12 bits).

## Regeneration

1. Fetch the four converter files at the MIT commit above plus
   `filters.h` into a scratch directory.
2. Build the generator (kept out of the repository by design; see the
   PR that introduced these vectors for its source):
   `gcc -O2 -I <scratch> gen_vectors.c iqconverter_float.c
   iqconverter_int16.c -lm`
3. Run from the repository root; it rewrites `test-data/iq/`. The
   harness must fully zero the int16 converter state after create
   (see the state-zeroing note above).

The `tone` inputs are produced with `sin(3)`/libm, so regeneration on a
different platform could alter *input* words slightly — the committed
input files are the contract; outputs must always be regenerated
together with inputs by the C reference.
