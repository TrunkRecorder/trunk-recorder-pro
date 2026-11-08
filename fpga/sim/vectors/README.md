# Filter bank golden vectors

Made by `golden_vectors_match_the_model` in `crates/trunk-core/src/dsp/bank_wire.rs`, which fails if the model
stops matching these files. Rewrite them with:

```bash
BANK_GOLDEN_WRITE=1 cargo test --release -p trunk-core dsp::bank_wire::tests::golden
```

The RTL must reproduce the packets' payloads bit for bit (bladeRF's `fifo_writer` makes the header, with its own
timestamp) from `input.sc16`, starting with every register and delay line at zero.

| File | Contents |
|---|---|
| `input.sc16` | 24576 samples (two blocks), SC16 Q11: I then Q, each a little-endian `i16` in −2048…2047 |
| `coefficients.hex` | The ROM: 4096 prototype coefficients, oldest sample first, 18-bit two's complement, one per line |
| `expected.pkt` | Two channel packets (blocks 0 and 1), each with the 16-byte header. The format is in `bank_wire.rs` |
| `expected_switch.pkt` | The same input, with table `512, 7, 1023, 600` committed as generation 2 during block 0, so block 1 uses it |
| `expected_gain.pkt` | As `expected.pkt`, with an output gain shift of 2 |
| `../../rtl/fb_rom_pkg.vhd` | The RTL's coefficient and twiddle ROMs and its fold shift, as a VHDL package |

Design: `Design::default()` in `filter_bank.rs` (4096 taps, Kaiser β 7, 18-bit coefficients, data and twiddles,
FFT scaling `0b1110111011`).

Bin table, generation 1: `3, 40, 41, 924, 1023, 0, 512`. That covers a bin with a tone at its centre (3), a tone
midway between two bins (40, 41), a quarter-bin offset (924), a weak tone (1023), DC (0) and the Nyquist bin (512).

The input is tones near full scale, a pseudo-random floor, and one sample at the extreme codes (sample 5000). Two
values saturate in the fold, so the vectors also check clamping.
