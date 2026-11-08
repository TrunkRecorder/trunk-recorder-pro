-- The FPGA filter bank, on one clock (80 MHz in the radio): SC16 Q11 samples
-- in, issue 02 packet payloads out. The model it matches bit for bit is
-- FixedBank + bank_wire::Packer (crates/trunk-core/src/dsp/).

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fb_core is
    port (
        clk        : in  std_logic;
        rst        : in  std_logic;
        -- Low while reset clears the sample ring.
        ready      : out std_logic;
        in_valid   : in  std_logic;
        in_i       : in  signed(11 downto 0);
        in_q       : in  signed(11 downto 0);
        gain_shift : in  unsigned(2 downto 0);
        tbl_we     : in  std_logic;
        tbl_addr   : in  unsigned(5 downto 0);
        tbl_bin    : in  unsigned(9 downto 0);
        tbl_count  : in  unsigned(6 downto 0);
        commit_gen : in  unsigned(7 downto 0);
        active_gen : out unsigned(7 downto 0);
        pkt_ready  : in  std_logic;
        out_valid  : out std_logic;
        out_sop    : out std_logic;
        out_eop    : out std_logic;
        out_data   : out std_logic_vector(31 downto 0);
        fold_sat   : out unsigned(31 downto 0);
        out_sat    : out unsigned(31 downto 0);
        dropped    : out unsigned(31 downto 0)
    );
end entity;

architecture rtl of fb_core is
    signal f_v, x_v : std_logic;
    signal f_re, f_im, x_re, x_im : signed(17 downto 0);
begin
    fold : entity work.fb_fold
        port map (clk => clk, rst => rst, ready => ready, in_valid => in_valid, in_i => in_i, in_q => in_q,
                  out_valid => f_v, out_re => f_re, out_im => f_im, saturated => fold_sat);
    fft : entity work.fb_fft
        port map (clk => clk, rst => rst, in_valid => f_v, in_re => f_re, in_im => f_im,
                  out_valid => x_v, out_re => x_re, out_im => x_im);
    output : entity work.fb_out
        port map (clk => clk, rst => rst, in_valid => x_v, in_re => x_re, in_im => x_im, gain_shift => gain_shift,
                  tbl_we => tbl_we, tbl_addr => tbl_addr, tbl_bin => tbl_bin, tbl_count => tbl_count,
                  commit_gen => commit_gen, active_gen => active_gen, pkt_ready => pkt_ready,
                  out_valid => out_valid, out_sop => out_sop, out_eop => out_eop, out_data => out_data,
                  dropped => dropped, saturated => out_sat);
end architecture;
