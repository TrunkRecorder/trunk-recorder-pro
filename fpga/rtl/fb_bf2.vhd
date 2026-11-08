-- One radix-2 butterfly stage of the FFT, single-path delay feedback.
--
-- Over each group of 2L samples: the first L go into the delay line (and the
-- previous group's differences come out); with each of the second L, the
-- butterfly (a, b) -> (a + b, a - b) of the sample L earlier and this one:
-- the sum comes out, the difference goes into the line. So samples leave in
-- the model's in-place order, L later. MINUS_J: b is first turned by -j in the
-- second half of every 4L group (radix-2²'s BF2II). SHIFT: halve, rounding.
-- Each result is saturated to 18 bits, as FixedBank::butterfly.
--
-- `in_valid` must not be high two clocks running: the line is a RAM read a
-- clock ahead.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fb_bf2 is
    generic (
        L       : positive;
        MINUS_J : boolean;
        SHIFT   : natural range 0 to 1
    );
    port (
        clk       : in  std_logic;
        rst       : in  std_logic;
        in_valid  : in  std_logic;
        in_re     : in  signed(17 downto 0);
        in_im     : in  signed(17 downto 0);
        out_valid : out std_logic;
        out_re    : out signed(17 downto 0);
        out_im    : out signed(17 downto 0)
    );
end entity;

architecture rtl of fb_bf2 is
    type line_t is array (0 to L - 1) of std_logic_vector(35 downto 0);
    signal mem   : line_t := (others => (others => '0'));
    -- A whole RAM block for a short line wastes it: logic, then MLABs, then M10K.
    function style return string is
    begin
        if L <= 16 then return "logic"; elsif L <= 64 then return "MLAB"; end if;
        return "M10K";
    end function;
    attribute ramstyle : string;
    attribute ramstyle of mem : signal is style;
    signal ptr   : natural range 0 to L - 1 := 0;
    signal rd    : std_logic_vector(35 downto 0) := (others => '0');
    -- Position in the 4L group, and whether the first L have gone through.
    signal pos   : unsigned(31 downto 0) := (others => '0');
    signal primed: std_logic := '0';

    function rnd_sat(x : signed(19 downto 0)) return signed is
        variable r : signed(19 downto 0);
    begin
        if SHIFT = 1 then
            r := shift_right(x + 1, 1);
        else
            r := x;
        end if;
        if r > 131071 then
            return to_signed(131071, 18);
        elsif r < -131072 then
            return to_signed(-131072, 18);
        end if;
        return resize(r, 18);
    end function;
begin
    process (clk)
        variable ar, ai, br, bi : signed(19 downto 0);
        variable p : natural;
        variable wr, wi : signed(17 downto 0);
    begin
        if rising_edge(clk) then
            out_valid <= '0';
            rd <= mem(ptr);
            if in_valid = '1' then
                p := to_integer(pos(31 downto 0)) mod (4 * L);
                if (p mod (2 * L)) < L then
                    -- First half: out goes the stored difference, in goes x.
                    out_re <= signed(rd(17 downto 0));
                    out_im <= signed(rd(35 downto 18));
                    out_valid <= primed;
                    mem(ptr) <= std_logic_vector(in_im) & std_logic_vector(in_re);
                else
                    ar := resize(signed(rd(17 downto 0)), 20);
                    ai := resize(signed(rd(35 downto 18)), 20);
                    if MINUS_J and p >= 2 * L then
                        br := resize(in_im, 20);
                        bi := -resize(in_re, 20);
                    else
                        br := resize(in_re, 20);
                        bi := resize(in_im, 20);
                    end if;
                    out_re <= rnd_sat(ar + br);
                    out_im <= rnd_sat(ai + bi);
                    out_valid <= '1';
                    wr := rnd_sat(ar - br);
                    wi := rnd_sat(ai - bi);
                    mem(ptr) <= std_logic_vector(wi) & std_logic_vector(wr);
                    primed <= '1';
                end if;
                ptr <= (ptr + 1) mod L;
                pos <= pos + 1;
            end if;
            if rst = '1' then
                ptr <= 0;
                pos <= (others => '0');
                primed <= '0';
                out_valid <= '0';
            end if;
        end if;
    end process;
end architecture;
