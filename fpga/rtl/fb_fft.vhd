-- 1024-point radix-2² decimation-in-frequency FFT, streaming (FixedBank::fft).
-- Natural order in, bit-reversed order out, frames back to back. Stage s
-- (group N = 1024 / 4^s): butterflies of span N/2 and N/4, halving after
-- butterfly b when bit b of SHIFTS is set, then the twiddles (none at N = 4).
-- A frame's last sample leaves once 1023 more have gone in.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fb_fft is
    generic (SHIFTS : natural := 2#1110111011#);
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

architecture rtl of fb_fft is
    type sig_t is array (0 to 15) of signed(17 downto 0);
    signal re, im : sig_t;
    signal v : std_logic_vector(0 to 15);

    function bit_of(b : natural) return natural is
    begin
        return (SHIFTS / 2 ** b) mod 2;
    end function;
begin
    v(0) <= in_valid;
    re(0) <= in_re;
    im(0) <= in_im;

    stages : for s in 0 to 4 generate
        constant N : positive := 1024 / 4 ** s;
        -- Signals 3s, 3s+1, 3s+2 → 3s+3: after BF2I, BF2II, twiddle.
    begin
        bf1 : entity work.fb_bf2
            generic map (L => N / 2, MINUS_J => false, SHIFT => bit_of(2 * s))
            port map (clk, rst, v(3 * s), re(3 * s), im(3 * s), v(3 * s + 1), re(3 * s + 1), im(3 * s + 1));
        bf2 : entity work.fb_bf2
            generic map (L => N / 4, MINUS_J => true, SHIFT => bit_of(2 * s + 1))
            port map (clk, rst, v(3 * s + 1), re(3 * s + 1), im(3 * s + 1), v(3 * s + 2), re(3 * s + 2), im(3 * s + 2));
        twiddled : if N > 4 generate
            tw : entity work.fb_twiddle
                generic map (N => N)
                port map (clk, rst, v(3 * s + 2), re(3 * s + 2), im(3 * s + 2), v(3 * s + 3), re(3 * s + 3), im(3 * s + 3));
        else generate
            v(3 * s + 3) <= v(3 * s + 2);
            re(3 * s + 3) <= re(3 * s + 2);
            im(3 * s + 3) <= im(3 * s + 2);
        end generate;
    end generate;

    out_valid <= v(15);
    out_re <= re(15);
    out_im <= im(15);
end architecture;
