-- The filter bank in the radio: fb_core on the Nios clock (80 MHz), between
-- the radio's sample clock and bladeRF's fifo_writer on rx_clock.
--
-- Samples cross in a 16-word dual-clock FIFO and reach the core at most every
-- 4 clocks, as fb_fold needs. Packet words cross back in a 256-word one: the
-- core sends a hop's words at 80 MHz and rx_clock takes them at 16, so it has
-- to hold a hop's worth (44 words at 42 bins).
--
-- `run` (rx_clock): the stream is on, in packet mode. It clears both FIFOs when
-- low, and its fall resets the core, so a new stream starts from block 0 and a
-- clear ring. A block is sent only if `enable` is set.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

library altera_mf;
use altera_mf.altera_mf_components.all;

entity fb_radio is
    port (
        clk          : in  std_logic;
        rst          : in  std_logic;
        enable       : in  std_logic;
        gain_shift   : in  unsigned(2 downto 0);
        tbl_we       : in  std_logic;
        tbl_addr     : in  unsigned(5 downto 0);
        tbl_bin      : in  unsigned(9 downto 0);
        tbl_count    : in  unsigned(6 downto 0);
        commit_gen   : in  unsigned(7 downto 0);
        active_gen   : out unsigned(7 downto 0);
        saturated    : out unsigned(31 downto 0);
        dropped      : out unsigned(31 downto 0);

        rx_clk       : in  std_logic;
        run          : in  std_logic;
        sample_valid : in  std_logic;
        sample_i     : in  signed(11 downto 0);
        sample_q     : in  signed(11 downto 0);
        pkt_ready    : in  std_logic;
        out_valid    : out std_logic;
        out_sop      : out std_logic;
        out_eop      : out std_logic;
        out_data     : out std_logic_vector(31 downto 0)
    );
end entity;

architecture rtl of fb_radio is
    signal clr : std_logic;

    -- Into the core.
    signal in_full, in_empty, in_pop : std_logic;
    signal in_q : std_logic_vector(23 downto 0);
    signal pace : unsigned(1 downto 0) := (others => '0');
    signal core_v : std_logic := '0';
    signal core_i, core_q : signed(11 downto 0) := (others => '0');

    signal run_sync, ready_sync : std_logic_vector(2 downto 0) := (others => '0');
    attribute altera_attribute : string;
    attribute altera_attribute of run_sync   : signal is "-name SYNCHRONIZER_IDENTIFICATION FORCED_IF_ASYNCHRONOUS";
    attribute altera_attribute of ready_sync : signal is "-name SYNCHRONIZER_IDENTIFICATION FORCED_IF_ASYNCHRONOUS";
    signal core_rst : std_logic := '1';

    -- Out of the core.
    signal c_v, c_sop, c_eop : std_logic;
    signal c_data : std_logic_vector(31 downto 0);
    signal fold_sat, out_sat : unsigned(31 downto 0);
    signal out_full, out_empty : std_logic;
    signal out_q : std_logic_vector(33 downto 0);
begin
    clr <= not run;

    samples : dcfifo
        generic map (
            lpm_width => 24, lpm_widthu => 4, lpm_numwords => 16, lpm_showahead => "ON",
            overflow_checking => "ON", underflow_checking => "ON", use_eab => "OFF",
            read_aclr_synch => "ON", write_aclr_synch => "ON",
            intended_device_family => "Cyclone V"
        )
        port map (
            aclr => clr,
            wrclk => rx_clk, wrreq => sample_valid and not in_full, wrfull => in_full,
            data => std_logic_vector(sample_q) & std_logic_vector(sample_i),
            rdclk => clk, rdreq => in_pop, rdempty => in_empty, q => in_q
        );

    in_pop <= '1' when in_empty = '0' and pace = 0 else '0';

    process (clk)
    begin
        if rising_edge(clk) then
            core_v <= in_pop;
            core_i <= signed(in_q(11 downto 0));
            core_q <= signed(in_q(23 downto 12));
            if in_pop = '1' then
                pace <= to_unsigned(3, 2);
            elsif pace /= 0 then
                pace <= pace - 1;
            end if;

            run_sync <= run_sync(1 downto 0) & run;
            ready_sync <= ready_sync(1 downto 0) & pkt_ready;
            core_rst <= rst or (run_sync(2) and not run_sync(1));
        end if;
    end process;

    core : entity work.fb_core
        port map (
            clk => clk, rst => core_rst, ready => open,
            in_valid => core_v, in_i => core_i, in_q => core_q, gain_shift => gain_shift,
            tbl_we => tbl_we, tbl_addr => tbl_addr, tbl_bin => tbl_bin, tbl_count => tbl_count,
            commit_gen => commit_gen, active_gen => active_gen,
            pkt_ready => ready_sync(2) and enable,
            out_valid => c_v, out_sop => c_sop, out_eop => c_eop, out_data => c_data,
            fold_sat => fold_sat, out_sat => out_sat, dropped => dropped
        );
    saturated <= fold_sat + out_sat;

    packets : dcfifo
        generic map (
            lpm_width => 34, lpm_widthu => 8, lpm_numwords => 256, lpm_showahead => "ON",
            overflow_checking => "ON", underflow_checking => "ON", use_eab => "ON",
            read_aclr_synch => "ON", write_aclr_synch => "ON",
            intended_device_family => "Cyclone V"
        )
        port map (
            aclr => clr,
            wrclk => clk, wrreq => c_v and not out_full, wrfull => out_full,
            data => c_sop & c_eop & c_data,
            rdclk => rx_clk, rdreq => not out_empty, rdempty => out_empty, q => out_q
        );

    process (rx_clk)
    begin
        if rising_edge(rx_clk) then
            out_valid <= not out_empty;
            out_sop <= out_q(33) and not out_empty;
            out_eop <= out_q(32) and not out_empty;
            out_data <= out_q(31 downto 0);
        end if;
    end process;
end architecture;
