-- Filter bank, last stage: FixedBank::output, then bank_wire::Packer.
--
-- Each FFT output (bit-reversed position i, bin k = bitrev(i)) is turned by
-- (-j)^(k q), q = (hop + 1) mod 4, cut to Q15 (rounded right 2 less the gain
-- shift, saturating) and kept in a frame RAM in bin order, two frames deep.
-- After a frame, the table's bins are read out of it and leave at once: a
-- block's packet streams out over its 48 hops, the issue 02 payload with
-- bladeRF's fifo_writer adding the header. The header leaves at the block's
-- start; a block that finds `pkt_ready` low there is dropped whole, and its
-- number skipped.
--
-- Each bin also sums I² + Q² of its output, saturating at 40 bits. The packet
-- ends with a slice of those sums, the next 64 bins' (24 at 42 bins), shifted
-- right 8: the host's view of the whole band. There's no quiet moment to read
-- them in, as the next frame's output follows at once, so each bin of the slice
-- is taken at the moment the block's last frame is added to it (and restarted
-- from zero), into a small RAM read out at the block's end.
--
-- The table: write entries and the count, then a new generation to
-- `commit_gen`. It takes effect at the next block's start.

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity fb_out is
    port (
        clk        : in  std_logic;
        rst        : in  std_logic;
        in_valid   : in  std_logic;
        in_re      : in  signed(17 downto 0);
        in_im      : in  signed(17 downto 0);
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
        dropped    : out unsigned(31 downto 0);
        saturated  : out unsigned(31 downto 0)
    );
end entity;

architecture rtl of fb_out is
    constant VERSION : natural := 2;
    constant HOPS    : natural := 48;

    type tbl_t is array (0 to 63) of unsigned(9 downto 0);
    signal pend_tbl, act_tbl : tbl_t := (others => (others => '0'));
    signal act_n   : unsigned(6 downto 0) := (others => '0');
    signal act_gen : unsigned(7 downto 0) := (others => '0');

    type frame_t is array (0 to 2047) of std_logic_vector(31 downto 0);
    signal frame : frame_t;

    -- Input side: turned in one clock (t_), cut and kept the next.
    signal pos  : unsigned(9 downto 0) := (others => '0');
    signal hop2 : unsigned(1 downto 0) := (others => '0');
    signal hb   : natural range 0 to HOPS - 1 := 0;
    signal fbuf : std_logic := '0';
    signal t_v, t_last : std_logic := '0';
    signal t_a, t_b : signed(18 downto 0);
    signal t_addr : unsigned(10 downto 0);
    signal t_hb : natural range 0 to HOPS - 1 := 0;
    signal t_buf : std_logic := '0';

    -- Read-out of a finished frame: header words at a block's start, then its bins.
    type sel_t is (IDLE, HDR0, HDR1, BINS, PHDR, PRD, PCLR);
    signal sel      : sel_t := IDLE;
    signal sel_go   : std_logic := '0';
    signal sel_buf  : std_logic := '0';
    signal sel_hb   : natural range 0 to HOPS - 1 := 0;
    signal sel_idx  : unsigned(6 downto 0) := (others => '0');
    signal sending  : std_logic := '0';
    signal block_no : unsigned(31 downto 0) := (others => '0');
    signal r_v : std_logic := '0';
    signal fr_q     : std_logic_vector(31 downto 0);
    signal drops, sats : unsigned(31 downto 0) := (others => '0');

    -- The power sums: one bin's read-modify-write every other clock at most.
    type sum_t is array (0 to 1023) of std_logic_vector(39 downto 0);
    shared variable sums : sum_t := (others => (others => '0'));
    signal p_v, p2_v : std_logic := '0';
    signal p_k, p2_k : unsigned(9 downto 0) := (others => '0');
    signal p_hb, p2_hb : natural range 0 to HOPS - 1 := 0;
    signal p_i, p_q : signed(15 downto 0) := (others => '0');
    signal pw : unsigned(31 downto 0) := (others => '0');
    signal pa_addr : unsigned(9 downto 0);
    signal pa_q, pa_d : std_logic_vector(39 downto 0) := (others => '0');
    signal pa_we : std_logic;
    -- The slice in the making, by its offset from `scan`.
    type slice_t is array (0 to 63) of std_logic_vector(31 downto 0);
    signal slice : slice_t;
    signal sl_q : std_logic_vector(31 downto 0) := (others => '0');
    signal sl_i : unsigned(5 downto 0) := (others => '0');
    signal take, sl_we : std_logic := '0';
    signal sl_at : unsigned(9 downto 0);
    signal sum_next : unsigned(39 downto 0);
    signal clearing : std_logic := '1';
    signal clr_addr : unsigned(9 downto 0) := (others => '0');
    signal scan : unsigned(9 downto 0) := (others => '0');
    signal p_left : unsigned(6 downto 0) := (others => '0');
    constant SUM_MAX : unsigned(39 downto 0) := (others => '1');

    -- Power values a packet of n bins carries (bank_wire::power_words).
    function power_words(n : unsigned(6 downto 0)) return unsigned is
    begin
        if n = 42 then
            return to_unsigned(24, 7);
        end if;
        return to_unsigned(64, 7);
    end function;

    function bitrev(x : unsigned(9 downto 0)) return unsigned is
        variable r : unsigned(9 downto 0);
    begin
        for b in 0 to 9 loop
            r(b) := x(9 - b);
        end loop;
        return r;
    end function;

    -- round_shift(x, 2 - g) then saturate to 16 bits.
    procedure cut(x : signed(18 downto 0); g : natural; variable y : out signed(15 downto 0); variable n : inout natural) is
        variable r : signed(26 downto 0);
    begin
        if g < 2 then
            r := shift_right(resize(x, 27) + to_signed(2 ** (1 - g), 27), 2 - g);
        else
            r := shift_left(resize(x, 27), g - 2);
        end if;
        if r > 32767 then
            y := to_signed(32767, 16); n := n + 1;
        elsif r < -32768 then
            y := to_signed(-32768, 16); n := n + 1;
        else
            y := resize(r, 16);
        end if;
    end procedure;
begin
    active_gen <= act_gen;
    dropped <= drops;
    saturated <= sats;

    -- The sum, its next value, and whether this bin's goes into the slice.
    process (pa_q, pw)
        variable v : unsigned(40 downto 0);
    begin
        v := resize(unsigned(pa_q), 41) + resize(pw, 41);
        if v > resize(SUM_MAX, 41) then
            sum_next <= SUM_MAX;
        else
            sum_next <= v(39 downto 0);
        end if;
    end process;
    sl_at <= p2_k - scan;
    take <= '1' when p2_v = '1' and p2_hb = HOPS - 1 and resize(sl_at, 11) < resize(power_words(act_n), 11) else '0';

    pa_addr <= clr_addr when clearing = '1' else p_k when p_v = '1' else p2_k;
    pa_we <= clearing or p2_v;
    pa_d <= (others => '0') when clearing = '1' or take = '1' else std_logic_vector(sum_next);
    sums_port : process (clk)
    begin
        if rising_edge(clk) then
            if pa_we = '1' then
                sums(to_integer(pa_addr)) := pa_d;
            end if;
            pa_q <= sums(to_integer(pa_addr));
        end if;
    end process;
    slice_port : process (clk)
    begin
        if rising_edge(clk) then
            if take = '1' then
                slice(to_integer(sl_at(5 downto 0))) <= std_logic_vector(sum_next(39 downto 8));
            end if;
            sl_q <= slice(to_integer(sl_i));
        end if;
    end process;

    process (clk)
        variable k : unsigned(9 downto 0);
        variable a, b, t : signed(18 downto 0);
        variable yi, yq : signed(15 downto 0);
        variable n : natural;
        variable q : natural range 0 to 3;
    begin
        if rising_edge(clk) then
            if tbl_we = '1' then
                pend_tbl(to_integer(tbl_addr)) <= tbl_bin;
            end if;

            -- Turn.
            t_v <= '0';
            if in_valid = '1' then
                k := bitrev(pos);
                q := to_integer((hop2 + 1) * k(1 downto 0)) mod 4;
                a := resize(in_re, 19);
                b := resize(in_im, 19);
                case q is
                    when 1 => t := a; a := b; b := -t;
                    when 2 => a := -a; b := -b;
                    when 3 => t := a; a := -b; b := t;
                    when others => null;
                end case;
                t_v <= '1';
                t_a <= a;
                t_b <= b;
                t_addr <= fbuf & k;
                t_buf <= fbuf;
                t_hb <= hb;
                if pos = 1023 then t_last <= '1'; else t_last <= '0'; end if;
                pos <= pos + 1;
                if pos = 1023 then
                    fbuf <= not fbuf;
                    hop2 <= hop2 + 1;
                    if hb = HOPS - 1 then hb <= 0; else hb <= hb + 1; end if;
                end if;
            end if;
            -- Cut, keep in bin order; a frame's last sample starts its read-out.
            sel_go <= '0';
            -- The sum: squares a clock after the cut, added a clock after that.
            p_v <= t_v;
            p2_v <= p_v;
            p2_k <= p_k;
            p2_hb <= p_hb;
            if p_v = '1' then
                pw <= unsigned(resize(p_i * p_i, 32)) + unsigned(resize(p_q * p_q, 32));
            end if;
            if clearing = '1' then
                clr_addr <= clr_addr + 1;
                if clr_addr = 1023 then clearing <= '0'; end if;
            end if;
            if t_v = '1' then
                n := 0;
                cut(t_a, to_integer(gain_shift), yi, n);
                cut(t_b, to_integer(gain_shift), yq, n);
                if n /= 0 then
                    sats <= sats + n;
                end if;
                frame(to_integer(t_addr)) <= std_logic_vector(yq) & std_logic_vector(yi);
                p_k <= t_addr(9 downto 0);
                p_hb <= t_hb;
                p_i <= yi;
                p_q <= yq;
                if t_last = '1' then
                    sel_go <= '1';
                    sel_buf <= t_buf;
                    sel_hb <= t_hb;
                end if;
            end if;

            -- Read out: one word a clock, the frame RAM's a clock behind.
            out_valid <= '0';
            out_sop <= '0';
            out_eop <= '0';
            r_v <= '0';
            case sel is
                when IDLE =>
                    if sel_go = '1' then
                        sel_idx <= (others => '0');
                        if sel_hb = 0 then
                            -- A block starts: the committed table takes over, and it's sent or dropped whole.
                            if commit_gen /= act_gen then
                                act_tbl <= pend_tbl;
                                act_n <= tbl_count;
                                act_gen <= commit_gen;
                            end if;
                            sending <= pkt_ready;
                            if pkt_ready = '0' then
                                drops <= drops + 1;
                            end if;
                            sel <= HDR0;
                        else
                            sel <= BINS;
                        end if;
                    end if;
                when HDR0 =>
                    out_valid <= sending;
                    out_sop <= sending;
                    out_data <= std_logic_vector(resize(act_n, 8)) & std_logic_vector(act_gen) & x"00" & std_logic_vector(to_unsigned(VERSION, 8));
                    sel <= HDR1;
                when HDR1 =>
                    out_valid <= sending;
                    out_data <= std_logic_vector(block_no);
                    block_no <= block_no + 1;
                    sel <= BINS;
                when BINS =>
                    if sel_idx = act_n then
                        if sel_hb = HOPS - 1 then sel <= PHDR; else sel <= IDLE; end if;
                    else
                        fr_q <= frame(to_integer(sel_buf & act_tbl(to_integer(sel_idx(5 downto 0)))));
                        r_v <= sending;
                        sel_idx <= sel_idx + 1;
                    end if;
                when PHDR =>
                    out_valid <= sending;
                    out_data <= std_logic_vector(resize(power_words(act_n), 16)) & std_logic_vector(resize(scan, 16));
                    p_left <= power_words(act_n);
                    sl_i <= (others => '0');
                    sel <= PRD;
                when PRD =>
                    -- The slice RAM reads sl_i this clock.
                    sel <= PCLR;
                when PCLR =>
                    out_valid <= sending;
                    out_data <= sl_q;
                    sl_i <= sl_i + 1;
                    p_left <= p_left - 1;
                    if p_left = 1 then
                        out_eop <= sending;
                        scan <= scan + resize(power_words(act_n), 10);
                        sel <= IDLE;
                    else
                        sel <= PRD;
                    end if;
            end case;
            if r_v = '1' then
                out_valid <= '1';
                out_data <= fr_q;
            end if;

            if rst = '1' then
                pos <= (others => '0');
                hop2 <= (others => '0');
                hb <= 0;
                fbuf <= '0';
                t_v <= '0';
                sel_go <= '0';
                sel <= IDLE;
                p_v <= '0';
                p2_v <= '0';
                scan <= (others => '0');
                clearing <= '1';
                clr_addr <= (others => '0');
                sending <= '0';
                r_v <= '0';
                block_no <= (others => '0');
                out_valid <= '0';
                out_sop <= '0';
                out_eop <= '0';
                act_gen <= (others => '0');
                act_n <= (others => '0');
                drops <= (others => '0');
                sats <= (others => '0');
            end if;
        end if;
    end process;
end architecture;
