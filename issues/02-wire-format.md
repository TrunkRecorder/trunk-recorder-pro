# 02: Wire format

**Size:** S. **Needs:** 01.

**Status (2026-11-08):** done. The bin limit is 42, not 64: a packet must fit one USB 3 DMA transaction (2048
DWORDs, 4 of them bladeRF's header), and 2 + 48 × 42 = 2018. Found reading `fx3_gpif.vhd` after the stock builds. The format lives in `crates/trunk-core/src/dsp/bank_wire.rs`, and vectors are in
`fpga/sim/vectors/`. The header's length counts payload DWORDs only, and the host reads packets through libbladeRF's async interface (issue 04's status says why). Power packets are
encoded and parsed, but the model doesn't make them until issue 08.

## Goal

Fix the contract between FPGA and host in one place, so both sides can be built and tested separately.

## Proposed format

Each packet uses libbladeRF's `BLADERF_FORMAT_PACKET_META` header (length, flags, core ID, 64-bit timestamp,
meta flags), followed by this little-endian payload:

| DWORD | Contents |
|---|---|
| 0 | Version (8 bits), kind (8), table generation (8), bin count N (8) |
| 1 | Block counter (32) |
| 2… | Kind 0, channels: 48 hops × N bins, each I (16) and Q (16), in table order |
| 2… | Kind 1, power: 1024 unsigned power values, sent every 16 blocks |

Rules:

- The timestamp counts input samples. A jump in the block counter or timestamp means lost blocks.
- `N = 0` is valid, so the host's clock keeps advancing with no bins enabled.
- A new bin table takes effect at a block boundary, and the generation says which table a packet used.
- A saturation count needs a home too: put it in a status register (issue 04) unless the design shows a better
  place.

**Packet lengths:** libbladeRF warns that lengths of `n*256-4` DWORDs jam a sync buffer. A channel packet has
48N + 2 payload DWORDs and a power packet has 1026, so neither can hit that value, with or without the 4 header
DWORDs. Keep a test for this.

## Registers (over Wishbone)

| Address | Register |
|---|---|
| 0x00 | Read: core version and build ID |
| 0x04 | Control: enable, output gain shift (see below) |
| 0x08 | Pending bin count |
| 0x0C | Commit: write a generation to apply the pending table at the next block |
| 0x10 | Read: saturation count |
| 0x14 | Read: blocks dropped because the USB side wasn't ready |
| 0x18 | Read: RX clock in Hz / 256 |
| 0x400–0x4FC | Pending table: 64 bin indexes |

The output gain shift stays adjustable on purpose: real signal levels will need tuning that a model can't
predict.

## Work

1. Add `crates/trunk-core/src/dsp/bank_wire.rs` with a pure parser and encoder. Keep it free of I/O, so the
   browser build still compiles. Its module doc is the format's single source of truth.
2. Add a writer to the issue 01 model, which turns a capture into a packet file.
3. Generate golden vectors: a short input with its expected packets, for the RTL testbench in issue 05.

## Done when

- [x] Parser and encoder round-trip, and malformed, truncated and wrong-version packets return errors without
      panicking.
- [x] A test confirms no valid N gives a banned length.
- [x] Golden vectors are checked in, small enough to review.
