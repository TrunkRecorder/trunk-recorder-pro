-- fb_core against the model's golden vectors (fpga/sim/vectors), bit for bit.
-- MODE: "plain"; "switch" (a second table committed during block 0);
-- "ready" (pkt_ready low as block 0 starts, so it's dropped whole);
-- "restart" (reset 5000 samples in, then the input again from the start);
-- "gain" (the output gain shift set to 2).

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use std.env.all;

entity tb_fb_core is
    generic (
        VECTORS : string := "../vectors";
        MODE    : string := "plain";
        -- Through fb_radio, as in the radio: samples on a 16 MHz clock.
        BRIDGE  : boolean := false
    );
end entity;

architecture sim of tb_fb_core is
    signal clk, rx : std_logic := '0';
    signal check_clk : std_logic;
    signal rst : std_logic := '1';
    signal ready, in_valid, tbl_we, pkt_ready : std_logic := '0';
    signal in_i, in_q : signed(11 downto 0) := (others => '0');
    signal tbl_addr : unsigned(5 downto 0) := (others => '0');
    signal tbl_bin : unsigned(9 downto 0) := (others => '0');
    signal tbl_count : unsigned(6 downto 0) := (others => '0');
    signal commit_gen, active_gen : unsigned(7 downto 0) := (others => '0');
    signal out_valid, out_sop, out_eop : std_logic;
    signal out_data : std_logic_vector(31 downto 0);
    signal fold_sat, out_sat, dropped : unsigned(31 downto 0);
    signal checked : natural := 0;
    signal want_packets : natural := 2;
    signal gain_shift : unsigned(2 downto 0);
    signal adc_peak : unsigned(11 downto 0) := (others => '0');
    signal adc_clips : unsigned(31 downto 0) := (others => '0');

    type char_file is file of character;
    type int_array is array (natural range <>) of natural;
    constant TABLE1 : int_array := (3, 40, 41, 924, 1023, 0, 512);
    constant TABLE2 : int_array := (512, 7, 1023, 600);

    function expected_file return string is
    begin
        if MODE = "switch" then
            return VECTORS & "/expected_switch.pkt";
        elsif MODE = "gain" then
            return VECTORS & "/expected_gain.pkt";
        end if;
        return VECTORS & "/expected.pkt";
    end function;
begin
    gain_shift <= "010" when MODE = "gain" else "000";
    clk <= not clk after 6.25 ns;
    rx <= not rx after 31.25 ns;
    check_clk <= rx when BRIDGE else clk;

    plain : if not BRIDGE generate
        dut : entity work.fb_core
            port map (clk, rst, ready, in_valid, in_i, in_q, gain_shift, tbl_we, tbl_addr, tbl_bin, tbl_count, commit_gen,
                      active_gen, pkt_ready, out_valid, out_sop, out_eop, out_data, fold_sat, out_sat, dropped);
    end generate;
    bridged : if BRIDGE generate
        ready <= '1';
        out_sat <= (others => '0');
        dut : entity work.fb_radio
            port map (clk => clk, rst => rst, enable => '1', gain_shift => gain_shift, tbl_we => tbl_we, tbl_addr => tbl_addr,
                      tbl_bin => tbl_bin, tbl_count => tbl_count, commit_gen => commit_gen, active_gen => active_gen,
                      saturated => fold_sat, dropped => dropped, adc_peak => adc_peak, adc_clips => adc_clips, peak_read => '0', rx_clk => rx, run => not rst, sample_valid => in_valid,
                      sample_i => in_i, sample_q => in_q, pkt_ready => pkt_ready, out_valid => out_valid,
                      out_sop => out_sop, out_eop => out_eop, out_data => out_data);
    end generate;

    stimulus : process
        file f : char_file;
        variable c0, c1, c2, c3 : character;
        variable n : natural;
        procedure write_table(t : int_array; gen : natural) is
        begin
            for i in t'range loop
                tbl_we <= '1';
                tbl_addr <= to_unsigned(i, 6);
                tbl_bin <= to_unsigned(t(i), 10);
                wait until rising_edge(clk);
            end loop;
            tbl_we <= '0';
            tbl_count <= to_unsigned(t'length, 7);
            commit_gen <= to_unsigned(gen, 8);
            wait until rising_edge(clk);
        end procedure;
        -- One sample every 10 clocks: 8 MS/s at 80 MHz.
        procedure sample(i, q : integer) is
        begin
            if BRIDGE then
                in_i <= to_signed(i, 12);
                in_q <= to_signed(q, 12);
                in_valid <= '1';
                wait until rising_edge(rx);
                in_valid <= '0';
                wait until rising_edge(rx);
                return;
            end if;
            in_i <= to_signed(i, 12);
            in_q <= to_signed(q, 12);
            in_valid <= '1';
            wait until rising_edge(clk);
            in_valid <= '0';
            for k in 1 to 9 loop
                wait until rising_edge(clk);
            end loop;
        end procedure;
        impure function s16 return integer is
            variable lo, hi : character;
            variable v : integer;
        begin
            read(f, lo);
            read(f, hi);
            v := character'pos(lo) + 256 * character'pos(hi);
            if v >= 32768 then
                v := v - 65536;
            end if;
            return v;
        end function;
        procedure feed(limit : natural) is
            variable i, q : integer;
        begin
            file_open(f, VECTORS & "/input.sc16", read_mode);
            n := 0;
            while not endfile(f) and n < limit loop
                i := s16;
                q := s16;
                if MODE = "switch" and n = 1000 then
                    write_table(TABLE2, 2);
                end if;
                if MODE = "ready" and n = 5000 then
                    pkt_ready <= '1';
                end if;
                sample(i, q);
                n := n + 1;
            end loop;
            file_close(f);
        end procedure;
    begin
        pkt_ready <= '0' when MODE = "ready" else '1';
        if MODE = "ready" then
            want_packets <= 1;
        end if;
        wait for 100 ns;
        wait until rising_edge(clk);
        rst <= '0';
        -- (Bridged, `ready` isn't brought out: the ring clears after reset, in 51 us.)
        if BRIDGE then wait for 60 us; else wait until ready = '1'; end if;
        wait until rising_edge(clk);
        write_table(TABLE1, 1);
        if MODE = "restart" then
            feed(5000);
            rst <= '1';
            wait until rising_edge(clk);
            rst <= '0';
            wait until rising_edge(clk);
            wait until ready = '1';
            wait until rising_edge(clk);
        end if;
        feed(natural'high);
        -- Zeros after: the last frame leaves the FFT as the next one goes in.
        while checked < want_packets loop
            sample(0, 0);
        end loop;
        for k in 1 to 5000 loop
            wait until rising_edge(clk);
        end loop;
        assert checked = want_packets report "packets: " & integer'image(checked) severity failure;
        assert fold_sat = 2 report "fold saturations: " & integer'image(to_integer(fold_sat)) & ", want 2" severity failure;
        if MODE = "ready" then
            assert dropped = 1 report "dropped: " & integer'image(to_integer(dropped)) severity failure;
        end if;
        if BRIDGE then
            -- Input sample 5000 holds the extreme codes, 2047 and -2048.
            assert adc_peak = 2048 report "ADC peak: " & integer'image(to_integer(adc_peak)) severity failure;
            assert adc_clips >= 1 report "ADC clips: " & integer'image(to_integer(adc_clips)) severity failure;
        end if;
        report MODE & ": " & integer'image(checked) & " packets match the model" severity note;
        finish;
    end process;

    check : process
        file f : char_file;
        variable c : character;
        variable len, word, k, pkt : natural := 0;
        variable want : std_logic_vector(31 downto 0);
        impure function byte return natural is
        begin
            read(f, c);
            return character'pos(c);
        end function;
        procedure next_header is
        begin
            -- (Two statements: the order of calls within an expression isn't defined.)
            len := byte;
            len := len + 256 * byte;
            for i in 1 to 14 loop
                k := byte;
            end loop;
        end procedure;
        procedure open_expected is
        begin
            file_open(f, expected_file, read_mode);
            pkt := 0;
            if MODE = "ready" then
                -- Block 0 never left: skip its packet.
                next_header;
                for i in 1 to 4 * len loop
                    k := byte;
                end loop;
                pkt := 1;
            end if;
        end procedure;
    begin
        open_expected;
        loop
            wait until rising_edge(check_clk) and (out_valid = '1' or rst = '1');
            if rst = '1' then
                -- A reset ends any packet half sent (on the radio, fifo_writer resets with the stream): start again.
                file_close(f);
                open_expected;
                checked <= 0;
                next;
            end if;
            -- (Words from blocks after the last expected are the extra zeros' doing.)
            if checked >= want_packets then
                next;
            end if;
            if out_sop = '1' then
                next_header;
                word := 0;
            end if;
            want := (others => '0');
            for b in 0 to 3 loop
                want(8 * b + 7 downto 8 * b) := std_logic_vector(to_unsigned(byte, 8));
            end loop;
            assert out_data = want
                report "packet " & integer'image(pkt) & " word " & integer'image(word) & ": got " & to_hstring(out_data) & ", want " & to_hstring(want)
                severity failure;
            word := word + 1;
            if out_eop = '1' then
                assert word = len report "packet " & integer'image(pkt) & ": " & integer'image(word) & " words, want " & integer'image(len) severity failure;
                pkt := pkt + 1;
                checked <= checked + 1;
            end if;
        end loop;
    end process;
end architecture;
