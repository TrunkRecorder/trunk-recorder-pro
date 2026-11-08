# Trial synthesis of the filter bank core alone (issue 05): resources and Fmax.
#   cd <empty dir> && quartus_sh -t <repo>/fpga/syn/core_alone.tcl [5CEBA4F23C8]
# Every port but the clock is a virtual pin: the core alone has more ports than the chip has pins.
set here [file dirname [file normalize [info script]]]
set part [expr {$argc > 0 ? [lindex $argv 0] : "5CEBA4F23C8"}]
project_new fb_core -overwrite
set_global_assignment -name FAMILY "Cyclone V"
set_global_assignment -name DEVICE $part
set_global_assignment -name TOP_LEVEL_ENTITY fb_core
set_global_assignment -name VHDL_INPUT_VERSION VHDL_2008
foreach f {fb_rom_pkg fb_fold fb_bf2 fb_twiddle fb_fft fb_out fb_core} {
    set_global_assignment -name VHDL_FILE $here/../rtl/$f.vhd
}
set_global_assignment -name SDC_FILE $here/core_alone.sdc
set_instance_assignment -name VIRTUAL_PIN ON -to *
set_instance_assignment -name VIRTUAL_PIN OFF -to clk
load_package flow
execute_flow -compile
project_close
