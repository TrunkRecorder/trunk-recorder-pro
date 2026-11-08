# 05: Filter bank RTL in simulation

**Size:** L. **Needs:** 01, 02. **Can run alongside:** 03.

**Status (2026-11-08):** the core is written and matches the model bit for bit. `fpga/rtl/fb_*.vhd`,
`fpga/sim/run.sh` (GHDL, four cases) and `fpga/syn/core_alone.tcl` (trial compile) are in. Open:
- **Clock-crossing FIFOs:** these go in with the integration (issue 06), using Nuand's dcfifo wrappers.
- **Per-block testbenches:** skipped. One core-level test against the golden vectors catches any block's error,
  but doesn't say which block.

Changes from the plan:
- **Packets stream:** a block's packet goes out over its 48 hops instead of from a block buffer, saving 20 RAM
  blocks. The header leaves at the block's start, which is also where `packet_ready` decides whether the block is
  sent or dropped whole.
- **Bin limit 42:** see issue 02.
- **The xA4 needs RAM back:** stock uses 274 of its 308 blocks, and the core needs 36. See the research note.

## Goal

Build the filter bank in VHDL and prove it matches the fixed-point model bit for bit, in simulation.

## Work

Build and test each block alone, in this order:

1. **Input.** Async FIFO from `rx_clock` to the 80 MHz `sys_clock`, plus ADC clip and peak counters.
2. **Polyphase filter.** Shared multipliers, delay line and coefficients in M10K, with coefficients from the
   issue 01 script.
3. **FFT.** 1024-point streaming, with the issue 01 scaling. Check the licence and the Lite edition first: use
   Quartus's FFT IP only if both are acceptable and it matches the model exactly. Otherwise write a radix-2²
   pipeline.
4. **Rotation.** `j^(k·t)` per bin, without a multiplier.
5. **Bin select.** Pick up to 42 bins (issue 02's limit), with the table switched at block boundaries.
6. **Packetizer.** Replace the issue 04 counter, including `pkt_sop`, `pkt_eop` and `packet_ready`. Decide what
   happens when `packet_ready` is low: drop a whole block, never part of one, so the host sees a clean gap.
7. **Output.** Async FIFO back to `rx_clock`.
8. **Power accumulators.** Leave this block out until issue 08.

Nuand's `fir_filter.vhd` is one multiplier per tap with fixed coefficients, so it doesn't fit here. `fifo_writer`
and the async FIFOs can be reused as they are.

## Done when

- [ ] Each block has a testbench (one core-level testbench so far), run by a single script with GHDL or the Questa that ships with Quartus.
- [x] The full core reproduces the issue 02 golden vectors exactly, for every bit.
- [x] Tests cover a table change mid-stream, `packet_ready` going low, and full-scale input without wraparound.
- [x] Reset and enable leave no stale state: a restarted stream gives the same output.
- [x] A trial Quartus synthesis of the core alone gives its resource use and a rough Fmax for the C8 speed grade.

## Risk

The 80 MHz clock may not suit the design. The FFT needs 32 MS/s and the filter about 203 M real MAC/s, which
leaves margin for sharing multipliers, but C8 is the slowest grade. Add registers before raising the clock.
