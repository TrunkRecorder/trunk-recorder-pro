#!/bin/sh
# Adds the p25chan revision to a bladeRF checkout (35174c3):
#   fpga/bladerf/apply.sh <bladeRF>
#   cd <bladeRF>/hdl/quartus && ./build_bladerf.sh -b bladeRF-micro -r p25chan -s A4
set -eu
here=$(cd "$(dirname "$0")" && pwd)
p="$1/hdl/fpga/platforms/bladerf-micro"
cp "$p/vhdl/bladerf-hosted.vhd" "$p/vhdl/bladerf-p25chan.vhd"
patch -s "$p/vhdl/bladerf-p25chan.vhd" "$here/p25chan-top.patch"
cp "$here"/../rtl/fb_*.vhd "$p/vhdl/"
cp "$here/p25chan.sdc" "$p/constraints/"
{
    sed -e 's/bladerf-hosted\.vhd/bladerf-p25chan.vhd/' -e '/rx_packet_generator\.vhd/d' "$p/bladerf-hosted.qip"
    for f in fb_rom_pkg fb_fold fb_bf2 fb_twiddle fb_fft fb_out fb_core fb_radio fb_regs; do
        echo "set_global_assignment -name VHDL_FILE [file normalize [file join \$here vhdl/$f.vhd]]"
    done
    echo 'set_global_assignment -name SDC_FILE [file normalize [file join $here constraints/p25chan.sdc]]'
} > "$p/bladerf-p25chan.qip"
grep -q p25chan "$p/build/platform.conf" ||
    sed -i 's/^PLATFORM_REVISIONS=("hosted"/PLATFORM_REVISIONS=("hosted" "p25chan"/' "$p/build/platform.conf"
grep -q 'make_revision p25chan' "$p/build/bladerf.tcl" ||
    sed -i 's/^make_revision hosted$/make_revision hosted\nmake_revision p25chan/' "$p/build/bladerf.tcl"
