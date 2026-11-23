#!/bin/sh
# Runs the filter bank's testbenches in GHDL. Build files go to $FB_SIM_DIR (default: a temp dir).
# fb_radio uses Quartus's dual-clock FIFO, whose simulation model comes with Quartus.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
rtl="$here/../rtl"
sim_lib=${QUARTUS_ROOTDIR:-$HOME/intelFPGA_lite/23.1std/quartus}/eda/sim_lib
work=${FB_SIM_DIR:-$(mktemp -d)}
mkdir -p "$work"
cd "$work"
G="ghdl"
O="--std=08 -frelaxed -fsynopsys"
$G -a $O --work=altera_mf "$sim_lib/altera_mf_components.vhd" "$sim_lib/altera_mf.vhd"
for f in fb_rom_pkg fb_fold fb_bf2 fb_twiddle fb_fft fb_out fb_core fb_radio; do
    $G -a $O "$rtl/$f.vhd"
done
$G -a $O "$here/tb_fb_core.vhd"
$G -e $O tb_fb_core
for mode in ${*:-plain switch ready restart gain}; do
    $G -r $O tb_fb_core -gVECTORS="$here/vectors" -gMODE="$mode" --assert-level=error
done
for mode in plain ready; do
    $G -r $O tb_fb_core -gVECTORS="$here/vectors" -gMODE="$mode" -gBRIDGE=true --assert-level=error --ieee-asserts=disable
done
