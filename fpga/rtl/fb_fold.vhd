-- Filter bank, stage 1: the polyphase fold (FixedBank::run_frame's first loop).
--
-- Every 256 input samples, each of the 1024 FFT inputs j is
--     sum over r = 0..3 of x[oldest + j + 1024 r] * COEF(j + 1024 r),
-- rounded right by FOLD_SHIFT and saturated to 18 bits, oldest being the
-- sample 4095 before the latest. Two taps a clock (r = 0 and 2, then 1 and 3)
-- from one 4096-sample ring, a true dual-port RAM. A sample waits in a
-- one-word buffer until port A is free (at most two clocks), and the fold
-- pauses a clock for it: about 2310 clocks a hop of 2560 at 80 MHz and
-- 8 MS/s. A sample arriving during a fold overwrites the oldest, which the
-- fold has read by then: it reads the oldest quarter at least twice as fast
-- as samples can come (one every 4 clocks at most). The coefficients are
-- symmetric, so the ROM holds half. Outputs come at most every other clock,
-- as the FFT needs.
--
-- Reset clears the ring (4096 clocks, `ready` low) so a restarted stream
-- starts from zeros, as the model does.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use work.fb_rom_pkg.all;

entity fb_fold is
    port (
        clk      : in  std_logic;
        rst      : in  std_logic;
        ready    : out std_logic;
        in_valid : in  std_logic;
        in_i     : in  signed(11 downto 0);
        in_q     : in  signed(11 downto 0);
        out_valid: out std_logic;
        out_re   : out signed(17 downto 0);
        out_im   : out signed(17 downto 0);
        -- Fold results clamped since reset.
        saturated: out unsigned(31 downto 0)
    );
end entity;

architecture rtl of fb_fold is
    type ring_t is array (0 to 4095) of std_logic_vector(23 downto 0);
    shared variable ring : ring_t := (others => (others => '0'));
    type coef_rom_t is array (0 to 2047) of signed(17 downto 0);
    function to_rom return coef_rom_t is
        variable r : coef_rom_t;
    begin
        for i in r'range loop
            r(i) := to_signed(COEF(i), 18);
        end loop;
        return r;
    end function;
    constant ROM : coef_rom_t := to_rom;
    -- Coefficient i lives at min(i, 4095 - i).
    function half(i : unsigned(11 downto 0)) return unsigned is
    begin
        if i(11) = '1' then
            return resize(4095 - i, 11);
        end if;
        return resize(i, 11);
    end function;

    signal clearing : std_logic := '1';
    signal clr_addr : unsigned(11 downto 0) := (others => '0');
    signal wptr     : unsigned(11 downto 0) := (others => '0');
    signal wpend    : std_logic := '0';
    signal waddr    : unsigned(11 downto 0) := (others => '0');
    signal wdata    : std_logic_vector(23 downto 0) := (others => '0');
    signal since    : unsigned(7 downto 0) := (others => '0');
    signal pending  : std_logic := '0';
    signal pend_base: unsigned(11 downto 0) := (others => '0');

    signal running  : std_logic := '0';
    signal base     : unsigned(11 downto 0) := (others => '0');
    signal j        : unsigned(9 downto 0) := (others => '0');
    signal phase    : std_logic := '0';

    -- Pipeline: addresses (s0) -> RAM data and coefficients (s1) -> products (s2) -> sum (s3).
    signal s0_v, s0_ph, s1_v, s1_ph, s2_v, s2_ph : std_logic := '0';
    signal addr_a, addr_b : unsigned(11 downto 0) := (others => '0');
    signal caddr_a, caddr_b : unsigned(10 downto 0) := (others => '0');
    signal s1_a, s1_b : std_logic_vector(23 downto 0) := (others => '0');
    signal pa_addr : unsigned(11 downto 0);
    signal pa_we : std_logic;
    signal pa_d : std_logic_vector(23 downto 0);
    signal s1_ca, s1_cb : signed(17 downto 0) := (others => '0');
    signal p_ar, p_ai, p_br, p_bi : signed(29 downto 0) := (others => '0');
    signal acc_r, acc_i : signed(31 downto 0) := (others => '0');
    signal sat_count : unsigned(31 downto 0) := (others => '0');

    -- round_shift then saturate to 18 bits; counts a clamp.
    procedure cut(x : signed(31 downto 0); signal y : out signed(17 downto 0); n : inout natural) is
        variable r : signed(32 downto 0);
    begin
        r := shift_right(resize(x, 33) + to_signed(2 ** (FOLD_SHIFT - 1), 33), FOLD_SHIFT);
        if r > 131071 then
            y <= to_signed(131071, 18); n := n + 1;
        elsif r < -131072 then
            y <= to_signed(-131072, 18); n := n + 1;
        else
            y <= resize(r, 18);
        end if;
    end procedure;
begin
    ready <= not clearing;
    saturated <= sat_count;

    -- The ring's two ports, as Quartus's true dual-port template. Port A:
    -- the clear sweep, else the fold's read issued last clock, else a waiting sample.
    pa_addr <= clr_addr when clearing = '1' else addr_a when s0_v = '1' else waddr;
    pa_we <= clearing or (wpend and not s0_v);
    pa_d <= (others => '0') when clearing = '1' else wdata;
    port_a : process (clk)
    begin
        if rising_edge(clk) then
            if pa_we = '1' then
                ring(to_integer(pa_addr)) := pa_d;
            end if;
            s1_a <= ring(to_integer(pa_addr));
        end if;
    end process;
    port_b : process (clk)
    begin
        if rising_edge(clk) then
            s1_b <= ring(to_integer(addr_b));
        end if;
    end process;

    process (clk)
        variable sum_r, sum_i : signed(31 downto 0);
        variable n : natural;
    begin
        if rising_edge(clk) then
            out_valid <= '0';
            if clearing = '1' then
                clr_addr <= clr_addr + 1;
                if clr_addr = 4095 then
                    clearing <= '0';
                end if;
            else
                if wpend = '1' and s0_v = '0' then
                    wpend <= '0';
                end if;
                if in_valid = '1' then
                    wpend <= '1';
                    waddr <= wptr;
                    wdata <= std_logic_vector(in_q) & std_logic_vector(in_i);
                    wptr <= wptr + 1;
                    since <= since + 1;
                    if since = 255 then
                        -- The oldest of the 4096 the hop folds: 4095 before this one.
                        pending <= '1';
                        pend_base <= wptr - 4095;
                    end if;
                end if;
            end if;

            -- s0: walk j, two taps a clock. A waiting sample that port A can't
            -- take this clock (it's reading) gets the next one: pause one clock.
            s0_v <= '0';
            if clearing = '1' or (wpend = '1' and s0_v = '1') then
                null;
            elsif running = '0' and pending = '1' then
                running <= '1';
                pending <= '0';
                base <= pend_base;
                j <= (others => '0');
                phase <= '0';
            elsif running = '1' then
                s0_v <= '1';
                s0_ph <= phase;
                if phase = '0' then
                    addr_a <= base + resize(j, 12);
                    addr_b <= base + resize(j, 12) + 2048;
                    caddr_a <= half(resize(j, 12));
                    caddr_b <= half(resize(j, 12) + 2048);
                else
                    addr_a <= base + resize(j, 12) + 1024;
                    addr_b <= base + resize(j, 12) + 3072;
                    caddr_a <= half(resize(j, 12) + 1024);
                    caddr_b <= half(resize(j, 12) + 3072);
                    j <= j + 1;
                    if j = 1023 then
                        running <= '0';
                    end if;
                end if;
                phase <= not phase;
            end if;

            -- s1: the ring's reads land (port processes); the coefficients here.
            s1_v <= s0_v;
            s1_ph <= s0_ph;
            s1_ca <= ROM(to_integer(caddr_a));
            s1_cb <= ROM(to_integer(caddr_b));

            -- s2: four multipliers.
            s2_v <= s1_v;
            s2_ph <= s1_ph;
            p_ar <= signed(s1_a(11 downto 0)) * s1_ca;
            p_ai <= signed(s1_a(23 downto 12)) * s1_ca;
            p_br <= signed(s1_b(11 downto 0)) * s1_cb;
            p_bi <= signed(s1_b(23 downto 12)) * s1_cb;

            -- s3: accumulate a j's four taps, then round and saturate.
            if s2_v = '1' then
                sum_r := resize(p_ar, 32) + resize(p_br, 32);
                sum_i := resize(p_ai, 32) + resize(p_bi, 32);
                if s2_ph = '0' then
                    acc_r <= sum_r;
                    acc_i <= sum_i;
                else
                    n := 0;
                    cut(acc_r + sum_r, out_re, n);
                    cut(acc_i + sum_i, out_im, n);
                    sat_count <= sat_count + n;
                    out_valid <= '1';
                end if;
            end if;

            if rst = '1' then
                clearing <= '1';
                clr_addr <= (others => '0');
                wptr <= (others => '0');
                wpend <= '0';
                since <= (others => '0');
                pending <= '0';
                running <= '0';
                s0_v <= '0';
                s1_v <= '0';
                s2_v <= '0';
                out_valid <= '0';
                sat_count <= (others => '0');
            end if;
        end if;
    end process;
end architecture;
