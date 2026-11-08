# The RX sample clock and the Nios clock are asynchronous: everything between them goes
# through fb_radio's dual-clock FIFOs and synchronizers, or is a still, slow register.
set rx_clock {U_nios_system|axi_ad9361_0|i_dev_if|i_rx|i_altlvds_rx|auto_generated|pll_sclk~PLL_OUTPUT_COUNTER|divclk}
set sys_clock {U_system_pll|altera_pll_i|general[0].gpll~PLL_OUTPUT_COUNTER|divclk}
set_false_path -from [get_clocks $rx_clock] -to [get_clocks $sys_clock]
set_false_path -from [get_clocks $sys_clock] -to [get_clocks $rx_clock]
