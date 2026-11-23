-- The twiddle multiply after a radix-2² stage of group size N (FixedBank::fft).
--
-- Sample p of each N group, in quarter b = p / (N/4) at i = p mod (N/4), is
-- multiplied by twiddle (i * q * (1024/N)) mod 1024, q being 0, 2, 1, 3 for
-- quarters 0..3, rounded right 17 and saturated to 18 bits. i = 0 passes
-- untouched. (Quarter 0 still multiplies, by 131071/131072: the model does.)
-- Two clocks of latency. Only every (1024/N)th twiddle is used: the ROM holds those N.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use work.fb_rom_pkg.all;

entity fb_twiddle is
    generic (N : positive);
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

architecture rtl of fb_twiddle is
    type tw_rom_t is array (0 to N - 1) of signed(17 downto 0);
    function rom(re : boolean) return tw_rom_t is
        variable r : tw_rom_t;
    begin
        for m in r'range loop
            if re then r(m) := to_signed(TW_RE(m * (1024 / N)), 18); else r(m) := to_signed(TW_IM(m * (1024 / N)), 18); end if;
        end loop;
        return r;
    end function;
    constant WRE : tw_rom_t := rom(true);
    constant WIM : tw_rom_t := rom(false);
    type q_t is array (0 to 3) of natural;
    constant QS : q_t := (0, 2, 1, 3);

    signal pos : natural range 0 to N - 1 := 0;
    signal v1, pass1 : std_logic := '0';
    signal a1, b1, wr1, wi1 : signed(17 downto 0) := (others => '0');

    function cut(x : signed(36 downto 0)) return signed is
        variable r : signed(36 downto 0) := shift_right(x + to_signed(2 ** 16, 37), 17);
    begin
        if r > 131071 then
            return to_signed(131071, 18);
        elsif r < -131072 then
            return to_signed(-131072, 18);
        end if;
        return resize(r, 18);
    end function;
begin
    process (clk)
        variable i, k : natural;
    begin
        if rising_edge(clk) then
            v1 <= '0';
            out_valid <= '0';
            if in_valid = '1' then
                i := pos mod (N / 4);
                k := (i * QS(pos / (N / 4))) mod N;
                a1 <= in_re;
                b1 <= in_im;
                wr1 <= WRE(k);
                wi1 <= WIM(k);
                if i = 0 then pass1 <= '1'; else pass1 <= '0'; end if;
                v1 <= '1';
                pos <= (pos + 1) mod N;
            end if;
            if v1 = '1' then
                if pass1 = '1' then
                    out_re <= a1;
                    out_im <= b1;
                else
                    out_re <= cut(resize(a1 * wr1, 37) - resize(b1 * wi1, 37));
                    out_im <= cut(resize(a1 * wi1, 37) + resize(b1 * wr1, 37));
                end if;
                out_valid <= '1';
            end if;
            if rst = '1' then
                pos <= 0;
                v1 <= '0';
                out_valid <= '0';
            end if;
        end if;
    end process;
end architecture;
