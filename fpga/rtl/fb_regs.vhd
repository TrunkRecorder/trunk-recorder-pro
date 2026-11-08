-- The filter bank's registers (issue 02), a slave on bladeRF's Wishbone master.
--
-- That master presents word addresses (its Avalon side is in WORDS), holds the
-- address and data for the one clock `cyc` is high, and takes one ack the
-- clock after. Word address, with the host's byte address in brackets:
--   0 (0x00)  R   core id (8) version (8) build (16)
--   1 (0x04)  RW  bit 0 enable, bits 6:4 output gain shift
--   2 (0x08)  RW  pending bin count, held at MAX_BINS: a longer packet can't
--                 fit one USB transaction and would jam the stream
--   3 (0x0C)  W   commit: this generation takes the pending table at the next
--                 block.  R: the generation in use
--   4 (0x10)  R   filter bank saturations
--   5 (0x14)  R   blocks dropped because the USB side wasn't ready
--   6 (0x18)  R   rx_clock / 256, counted over SYS_HZ clocks
--   7 (0x1C)  R   largest |I| or |Q| from the ADC since this was last read (read clears it)
--   8 (0x20)  R   samples at the ADC's rails (|I| or |Q| of 2028 or more)
--   0x100..0x13F (0x400..0x4FC)  RW  pending table
-- All on the Nios clock.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fb_regs is
    generic (
        BUILD   : natural := 0;
        SYS_HZ  : positive := 80_000_000
    );
    port (
        clk        : in  std_logic;
        rst        : in  std_logic;
        wb_cyc     : in  std_logic;
        wb_we      : in  std_logic;
        wb_adr     : in  std_logic_vector(31 downto 0);
        wb_wdata   : in  std_logic_vector(31 downto 0);
        wb_rdata   : out std_logic_vector(31 downto 0);
        wb_ack     : out std_logic;
        enable     : out std_logic;
        gain_shift : out unsigned(2 downto 0);
        tbl_we     : out std_logic;
        tbl_addr   : out unsigned(5 downto 0);
        tbl_bin    : out unsigned(9 downto 0);
        tbl_count  : out unsigned(6 downto 0);
        commit_gen : out unsigned(7 downto 0);
        active_gen : in  unsigned(7 downto 0);
        saturated  : in  unsigned(31 downto 0);
        dropped    : in  unsigned(31 downto 0);
        -- rx_clock / 256, from the other domain: every rising edge is counted.
        rx_tick    : in  std_logic;
        adc_peak   : in  unsigned(11 downto 0);
        adc_clips  : in  unsigned(31 downto 0);
        peak_read  : out std_logic
    );
end entity;

architecture rtl of fb_regs is
    constant CORE_ID  : natural := 16#25#;
    constant VERSION  : natural := 2;
    constant MAX_BINS : natural := 42;

    type mem_t is array (0 to 63) of std_logic_vector(9 downto 0);
    signal mem : mem_t := (others => (others => '0'));
    attribute ramstyle : string;
    attribute ramstyle of mem : signal is "MLAB";
    signal mem_q : std_logic_vector(9 downto 0) := (others => '0');

    signal ctl_en : std_logic := '0';
    signal gain   : unsigned(2 downto 0) := (others => '0');
    signal count  : unsigned(6 downto 0) := (others => '0');
    signal gen    : unsigned(7 downto 0) := (others => '0');
    signal rdata  : std_logic_vector(31 downto 0) := (others => '0');
    signal ack    : std_logic := '0';
    signal is_tbl : std_logic := '0';

    signal tick_sync : std_logic_vector(2 downto 0) := (others => '0');
    attribute altera_attribute : string;
    attribute altera_attribute of tick_sync : signal is "-name SYNCHRONIZER_IDENTIFICATION FORCED_IF_ASYNCHRONOUS";
    signal ticks, rate : unsigned(31 downto 0) := (others => '0');
    signal window : natural range 0 to SYS_HZ - 1 := 0;
begin
    enable     <= ctl_en;
    gain_shift <= gain;
    tbl_count  <= count;
    commit_gen <= gen;
    wb_ack     <= ack;
    peak_read  <= '1' when wb_cyc = '1' and wb_we = '0' and wb_adr(8) = '0' and wb_adr(3 downto 0) = "0111" else '0';
    wb_rdata   <= x"00000" & "00" & mem_q when is_tbl = '1' else rdata;

    table : process (clk)
    begin
        if rising_edge(clk) then
            if wb_cyc = '1' and wb_we = '1' and wb_adr(8) = '1' then
                mem(to_integer(unsigned(wb_adr(5 downto 0)))) <= wb_wdata(9 downto 0);
            end if;
            if wb_cyc = '1' then
                mem_q <= mem(to_integer(unsigned(wb_adr(5 downto 0))));
            end if;
        end if;
    end process;

    tbl_we   <= wb_cyc and wb_we and wb_adr(8);
    tbl_addr <= unsigned(wb_adr(5 downto 0));
    tbl_bin  <= unsigned(wb_wdata(9 downto 0));

    process (clk)
        variable v : unsigned(31 downto 0);
    begin
        if rising_edge(clk) then
            ack <= wb_cyc;
            if wb_cyc = '1' then
                is_tbl <= wb_adr(8);
                rdata <= (others => '0');
                case to_integer(unsigned(wb_adr(3 downto 0))) is
                    when 0 => rdata <= std_logic_vector(to_unsigned(CORE_ID, 8) & to_unsigned(VERSION, 8) & to_unsigned(BUILD, 16));
                    when 1 => rdata(0) <= ctl_en; rdata(6 downto 4) <= std_logic_vector(gain);
                    when 2 => rdata(6 downto 0) <= std_logic_vector(count);
                    when 3 => rdata(7 downto 0) <= std_logic_vector(active_gen);
                    when 4 => rdata <= std_logic_vector(saturated);
                    when 5 => rdata <= std_logic_vector(dropped);
                    when 6 => rdata <= std_logic_vector(rate);
                    when 7 => rdata(11 downto 0) <= std_logic_vector(adc_peak);
                    when 8 => rdata <= std_logic_vector(adc_clips);
                    when others => null;
                end case;
                if wb_we = '1' and wb_adr(8) = '0' then
                    v := unsigned(wb_wdata);
                    case to_integer(unsigned(wb_adr(3 downto 0))) is
                        when 1 => ctl_en <= v(0); gain <= v(6 downto 4);
                        when 2 =>
                            if v > MAX_BINS then count <= to_unsigned(MAX_BINS, 7); else count <= v(6 downto 0); end if;
                        when 3 => gen <= v(7 downto 0);
                        when others => null;
                    end case;
                end if;
            end if;

            -- The rx_clock measurement.
            tick_sync <= tick_sync(1 downto 0) & rx_tick;
            if window = SYS_HZ - 1 then
                window <= 0;
                rate <= ticks;
                ticks <= (others => '0');
            else
                window <= window + 1;
                if tick_sync(2 downto 1) = "01" then
                    ticks <= ticks + 1;
                end if;
            end if;

            if rst = '1' then
                ctl_en <= '0';
                gain <= (others => '0');
                count <= (others => '0');
                gen <= (others => '0');
                ack <= '0';
                window <= 0;
                ticks <= (others => '0');
            end if;
        end if;
    end process;
end architecture;
