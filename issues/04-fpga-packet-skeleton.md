# 04: FPGA skeleton: test packets and registers

**Size:** M. **Needs:** 00, 02.

**Status (2026-11-08):** done. `fpga/bladerf/apply.sh` adds the `p25chan` revision to a
bladeRF checkout. It's the `hosted` top level (not WLAN: hosted already has the packet path and Wishbone wiring) with
TX removed, `fb_regs` on the Wishbone bus and `fb_testgen` making the packets. `fpga/sim/tb_fb_testgen.vhd` checks both,
and `trunk-pro tool bank-test <serial> --image <rbf>` is the test tool. N is at most 42 (issue 02). Results are in the
research note.

What issue 07's receiver has to do, all found on the radios:
- **Load the image, close, and open again.** Setting the sample rate in the session that loaded it times out in the
  RFIC's calibration.
- **Use the async interface** (`bladerf_init_stream`, `bladerf_stream`). `bladerf_sync_rx` in libbladeRF 2.5 treats a
  buffer as 512-DWORD messages with their own headers, so it damages these packets and leaves the timestamp 0.
- **Start `bladerf_stream`, then enable RX.** `bladerf_stream` sets the packet format as it starts. Enabled before it,
  the image sends but no transfer completes.
- **Write the registers after RX is on.** Starting RX clears control, count and generation.
- **Reassemble packets from USB transfers.** A 42-bin packet arrives as 2048, 2048, 2048 and 1944 bytes; the header's
  length says where it ends.

## Goal

Prove the whole path from fabric to host on real hardware, using a counter in place of DSP. This finds the
integration problems before the filter bank exists.

## Work

1. Create `fpga/` in this repo, with the core and a patch against bladeRF `35174c3`.
2. Add a `p25chan` revision:
   - Add it to `PLATFORM_REVISIONS` in `build/platform.conf`.
   - Add `bladerf-p25chan.qip` and `vhdl/bladerf-p25chan.vhd`, copied from the hosted files (issue status above), not WLAN with its modem and its
     AGC mux removed.
   - Leave `rx.vhd` and `fifo_writer.vhd` alone. Packet support is already there.
3. Add a generator driving `rx_packet_control` that emits valid issue 02 packets with counting samples, at the
   real block rate, on `rx_clock`.
4. Add a Wishbone slave with the issue 02 registers. Hosted leaves this bus unterminated, so drive
   `wbm_wb_clk_i` and `wbm_wb_rst_i` here, and answer `wbm_wb_ack_i`.
5. Write a small test tool that loads the image, sets `PACKET_META`, reads packets and checks the counter,
   timestamps, generation and registers. Use the `bladeRF-cli` scripting or a standalone Rust test binary,
   whichever is shorter.

## Check these assumptions

- Selecting `PACKET_META` is enough to enable packet mode, through `nios_gpio.o.packet_en`.
- The wideband path still works on this image with `SC16_Q11_META`.
- libbladeRF accepts the image: `BLADERF_CAP_FPGA_PACKET_META` and the compatibility table depend on the
  reported FPGA version, so keep it.
- The firmware has `BLADERF_CAP_FW_SHORT_PACKET`.
- `bladerf_wishbone_master_write` reaches the slave, and how long a write takes. This decides how quickly a new
  bin can be enabled.
- The RX clock can carry 2 M DWORD/s. It's inferred at about 16 MHz at 8 MSPS, so measure it.
- `sync_rx` keeps up with about 650 packets per second, without excess CPU.

## Done when

- [x] A 10 minute run at N = 42 on both boards has no counter errors or overruns.
- [x] Register reads and writes work, and a table commit lands on a block boundary with the right generation.
- [x] Wideband streaming on the same image matches stock.
- [x] Measured Wishbone latency, packet CPU and RX clock are recorded in `research/fpga-channelizer.md`.
- [x] ~~A power cycle returns the board to its normal image.~~ Not tested and not needed: images only ever go into RAM.
