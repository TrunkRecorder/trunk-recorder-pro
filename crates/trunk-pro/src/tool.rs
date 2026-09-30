//! `trunk-pro tool cc|voice|frames` — one channel's decode as JSON lines, in
//! the format of research/native-bench's C++ `p25tool`, so the comparison and
//! ground-truth scripts there run against this implementation unchanged.
//! `tool p2` — a Phase 2 TDMA channel: every framed slot (dibits, burst type,
//! AMBE codewords, MAC PDU) given the scrambler seed `--nac --sysid --wacn`;
//! `--audio out.f32 --slot 0|1` vocodes that slot's voice codewords.
//!
//! Receivers: `--demod cqpsk|c4fm|auto` (auto: CQPSK + C4FM), `--diversity 1`
//! (CQPSK + CQPSK/EQ + C4FM, `--eq 9 --mu 0.02`). Decoding: `--trellis
//! greedy|viterbi`, `--soft none|amp`, `--softfec 0|1`, `--flywheel 0|1`,
//! `--nidrecover 0|1`. Output: `--audio out.f32` (voice), `--iq out.cf32`.
//!
//! `tool smartnet <capture> --center Hz --rate Hz --cc Hz [--bandplan 400_custom
//! --bp-base Hz --bp-spacing Hz --bp-offset N --bp-high Hz] [--osw]` — a
//! SmartNet control channel: its messages (and with `--osw` every OSW) as
//! JSON lines, then OSW counts and the measured carrier offset.
//!
//! `tool revoice <call.frames.jsonl> <out.wav> [--profile enhanced|mbelib]
//! [--seed 1]` — vocode a call's frame capture (the recording setting
//! "Save vocoder frames") again, e.g. with the other vocoder profile.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

use trunk_core::dsp::Channelizer;
use trunk_core::mbe::{self, FRAME_SAMPLES};
use trunk_core::p25::diversity::{best_es, best_frame, best_imbe, best_lc, best_tsbks, Bank, BankConfig, Group};
use trunk_core::p25::frame::{duid_name, FramerOptions, HDU, LDU1, LDU2, TDULC, TSDU};
use trunk_core::p25::tsbk::{decode_tsdu, tsbk_opcode, Trellis};
use trunk_core::p25::voice::{decode_hdu, decode_ldu1_lc, decode_ldu2_es, decode_tdulc, imbe_params_to_bits, ldu_imbe, EncryptionSync, ImbeParams, LinkControl};

use crate::{die, Args};

pub fn run(a: &Args) {
    let mode = a.positional.first().map(String::as_str).unwrap_or_else(|| die("tool cc|voice|frames|p2|revoice <capture> …"));
    if mode == "p2" {
        return run_p2(a);
    }
    if mode == "dmr" {
        return crate::dmrtool::run(a);
    }
    if mode == "dmrscan" {
        return crate::dmrtool::run_scan(a);
    }
    if mode == "revoice" {
        return run_revoice(a);
    }
    if mode == "smartnet" {
        return run_smartnet(a);
    }
    let path = a.positional.get(1).unwrap_or_else(|| die("tool: no capture"));
    let cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("fs", a.num("rate", 2_400_000.0));
    let center = a.num("center", 0.0);
    let freq = a.num(if mode == "cc" { "cc" } else { "freq" }, 0.0);

    let demod = a.get("demod").unwrap_or("auto");
    let div = a.flag("diversity");
    let cfg = BankConfig {
        cqpsk: demod != "c4fm",
        cqpsk_eq: div && demod != "c4fm",
        c4fm: demod != "cqpsk",
        eq_taps: a.num("eq", 9.0) as usize,
        eq_mu: a.num("mu", 0.02) as f32,
        framer: FramerOptions { flywheel: a.get("flywheel") != Some("0"), nid_recover: a.get("nidrecover") != Some("0"), ..Default::default() },
        soft: a.get("soft") != Some("none"),
    };
    let greedy = a.get("trellis") == Some("greedy");
    let soft_fec = a.get("softfec") != Some("0");

    let mut chz = Channelizer::new(fs, 24_000.0, 1.0);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(freq - center, 7000.0, 0.0);
    let mut bank = Bank::new(rate, cfg);
    let mut iq_out = a.get("iq").map(|p| BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))));
    let mut audio_out = a.get("audio").map(|p| BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))));
    let mut vocoder = mbe::Decoder::new(mbe::lcg(1), if a.get("profile") == Some("mbelib") { mbe::Profile::Mbelib } else { mbe::Profile::Enhanced });
    if !soft_fec {
        vocoder.hard_fec();
    }
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let mut st = Stats::default();
    let mut groups = Vec::new();
    let t0 = Instant::now();
    let mut off = 0;
    let mut handle = |groups: &mut Vec<Group>, st: &mut Stats, out: &mut BufWriter<_>| {
        for g in groups.drain(..) {
            let f = best_frame(&g);
            *st.frames.entry(duid_name(f.nid.duid)).or_default() += 1;
            match mode {
                "cc" if f.nid.duid == TSDU => {
                    let (good, bad) = if greedy {
                        let r = decode_tsdu(f, Trellis::Greedy);
                        (r.good, r.bad)
                    } else {
                        best_tsbks(&g)
                    };
                    st.bad += bad;
                    for t in good {
                        st.good += 1;
                        let hex: String = t.iter().map(|b| format!("{b:02x}")).collect();
                        let _ = writeln!(out, "{{\"t\":{:.3},\"nac\":{},\"op\":{},\"hex\":\"{hex}\"}}", f.sample / rate, f.nid.nac, tsbk_opcode(&t));
                    }
                }
                "voice" => {
                    let chosen = if f.nid.duid == LDU1 || f.nid.duid == LDU2 {
                        Some(if soft_fec { best_imbe(&g) } else { ldu_imbe(f, false) })
                    } else {
                        None
                    };
                    let (lc, es) = if div { (best_lc(&g), best_es(&g)) } else { (decode_ldu1_lc(&f.raw), decode_ldu2_es(&f.raw)) };
                    let line = voice_line(f, chosen.as_ref(), lc, es, st, &mut vocoder, audio_out.as_mut());
                    let _ = writeln!(out, "{line}");
                }
                _ => {}
            }
        }
    };
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if ran {
            let iq = chz.output(head).unwrap();
            if let Some(w) = iq_out.as_mut() {
                for c in iq {
                    let _ = w.write_all(&c.re.to_le_bytes());
                    let _ = w.write_all(&c.im.to_le_bytes());
                }
            }
            bank.push(iq, &mut groups);
            handle(&mut groups, &mut st, &mut out);
        }
        if used == 0 {
            break;
        }
    }
    bank.flush(&mut groups);
    handle(&mut groups, &mut st, &mut out);
    let cpu = t0.elapsed().as_secs_f64();
    let _ = out.flush();

    let air = cap.len() as f64 / 2.0 / fs;
    let mut s = format!("{{\"mode\":\"{mode}\",\"airS\":{air:.2},\"pctCore\":{:.3},\"demod\":\"{demod}\",\"framesPerRx\":{:?},\"good\":{},\"bad\":{},\"frames\":{{", 100.0 * cpu / air, bank.frames_per_rx(), st.good, st.bad);
    let parts: Vec<String> = st.frames.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    s.push_str(&parts.join(","));
    s.push('}');
    if mode == "voice" {
        let _ = write!(s, ",\"codewords\":{},\"meanFecErrs\":{:.3},\"repeatWorthy\":{}", st.codewords, st.fec_errs as f64 / st.codewords.max(1) as f64, st.repeat_worthy);
        if audio_out.is_some() {
            let _ = write!(s, ",\"vocoder\":{{\"voice\":{},\"repeat\":{},\"muted\":{}}}", st.kinds[0], st.kinds[1], st.kinds[2]);
        }
    }
    s.push('}');
    eprintln!("{s}");
}

#[derive(Default)]
struct Stats {
    frames: BTreeMap<&'static str, u64>,
    good: usize,
    bad: usize,
    codewords: u64,
    fec_errs: u64,
    repeat_worthy: u64,
    kinds: [u64; 3],
}

fn lc_json(key: &str, lc: Option<LinkControl>) -> String {
    let n = |v: Option<u32>| v.map_or(-1, |x| x as i64);
    match lc {
        None => format!(",\"{key}\":null"),
        Some(lc) => format!(
            ",\"{key}\":{{\"lco\":{},\"mfid\":{},\"raw\":\"{}\",\"prot\":{},\"svc\":{},\"tgid\":{},\"target\":{},\"src\":{}}}",
            lc.lco,
            lc.mfid,
            lc.raw.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            lc.protected as u8,
            lc.svc_opts.map_or(-1, |v| v as i64),
            n(lc.tgid),
            n(lc.target),
            n(lc.source)
        ),
    }
}

fn voice_line(
    f: &trunk_core::p25::Frame,
    chosen: Option<&[ImbeParams; 9]>,
    lc: Option<LinkControl>,
    es: Option<EncryptionSync>,
    st: &mut Stats,
    vocoder: &mut mbe::Decoder,
    audio: Option<&mut BufWriter<File>>,
) -> String {
    let mut s = format!("{{\"t\":{:.3},\"duid\":{},\"nac\":{},\"complete\":{},\"nbits\":{},\"raw\":\"", f.symbol as f64 / 4800.0, f.nid.duid, f.nid.nac, f.complete as u8, f.raw.len());
    for c in f.raw.chunks(4) {
        let v = (0..4).fold(0u8, |v, j| (v << 1) | c.get(j).copied().unwrap_or(0));
        let _ = write!(s, "{v:x}");
    }
    s.push('"');
    let mut audio = audio;
    match f.nid.duid {
        LDU1 | LDU2 => {
            s.push_str(",\"imbe\":[");
            for (k, p) in chosen.unwrap().iter().enumerate() {
                st.codewords += 1;
                st.fec_errs += p.errs as u64;
                if p.e0 >= 3 || p.errs >= 10 {
                    st.repeat_worthy += 1;
                }
                if let Some(w) = audio.as_mut() {
                    let mut buf = [0f32; FRAME_SAMPLES];
                    let kind = vocoder.imbe(&imbe_params_to_bits(&p.u), p.e0, p.errs, false, &mut buf);
                    st.kinds[kind as usize] += 1;
                    mbe::to_unit(&mut buf);
                    for v in buf {
                        let _ = w.write_all(&v.to_le_bytes());
                    }
                }
                let u = &p.u;
                let _ = write!(
                    s,
                    "{}{{\"u\":[{},{},{},{},{},{},{},{}],\"errs\":{},\"e0\":{},\"c\":{:.3},\"mr\":{:.3}}}",
                    if k > 0 { "," } else { "" },
                    u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7],
                    p.errs,
                    p.e0,
                    p.cost,
                    p.mean_rel
                );
            }
            s.push(']');
            if f.nid.duid == LDU1 {
                s.push_str(&lc_json("lc", lc));
            } else {
                match es {
                    Some(e) => {
                        let _ = write!(s, ",\"es\":{{\"algid\":{},\"keyid\":{}}}", e.algid, e.keyid);
                    }
                    None => s.push_str(",\"es\":null"),
                }
            }
        }
        HDU => match decode_hdu(&f.raw) {
            Some(h) => {
                let _ = write!(s, ",\"hdu\":{{\"algid\":{},\"keyid\":{},\"mfid\":{},\"tgid\":{}}}", h.es.algid, h.es.keyid, h.mfid, h.tgid);
            }
            None => s.push_str(",\"hdu\":null"),
        },
        TDULC => s.push_str(&lc_json("tdulc", decode_tdulc(&f.raw))),
        _ => {}
    }
    s.push('}');
    s
}

fn num_any(a: &Args, key: &str) -> u32 {
    a.get(key).map_or(0, |v| {
        let v = v.trim();
        if let Some(h) = v.strip_prefix("0x") { u32::from_str_radix(h, 16) } else { v.parse() }.unwrap_or_else(|_| die(&format!("--{key}: not a number")))
    })
}

/// `tool revoice`: a frame capture back through the vocoder.
fn run_revoice(a: &Args) {
    use trunk_core::trunk::frames::hex_bits;
    let path = a.positional.get(1).unwrap_or_else(|| die("tool revoice <call.frames.jsonl> <out.wav>"));
    let out = a.positional.get(2).unwrap_or_else(|| die("tool revoice: no output .wav"));
    let mbelib = a.get("profile") == Some("mbelib");
    let profile = if mbelib { mbe::Profile::Mbelib } else { mbe::Profile::Enhanced };
    let mut dec = mbe::Decoder::new(mbe::lcg(a.num("seed", 1.0) as u32), profile);
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let (mut audio, mut kinds, mut differ) = (Vec::new(), BTreeMap::<String, u32>::new(), 0u32);
    fn not_frame<T>(path: &str, n: usize) -> T {
        die(&format!("{path}:{}: not a frame record", n + 1))
    }
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let f: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|_| not_frame(path, n));
        let int = |k: &str| f[k].as_u64().unwrap_or_else(|| not_frame(path, n)) as u32;
        let codec = f["codec"].as_str().unwrap_or_else(|| not_frame(path, n));
        let hex = f["bits"].as_str().unwrap_or_else(|| not_frame(path, n));
        let mut buf = [0f32; FRAME_SAMPLES];
        let kind = match codec {
            "imbe" => {
                let bits: [u8; 88] = hex_bits(hex, 88).and_then(|b| b.try_into().ok()).unwrap_or_else(|| not_frame(path, n));
                let erased = f["erased"].as_bool().unwrap_or(false);
                dec.imbe(&bits, int("e0"), if erased { 0 } else { int("errs") }, erased, &mut buf)
            }
            "ambe" => {
                let bits: [u8; 49] = hex_bits(hex, 49).and_then(|b| b.try_into().ok()).unwrap_or_else(|| not_frame(path, n));
                dec.ambe(&bits, int("errs"), &mut buf)
            }
            _ => not_frame(path, n),
        };
        let name = format!("{kind:?}").to_lowercase();
        differ += (f["out"].as_str() != Some(name.as_str())) as u32;
        *kinds.entry(name).or_default() += 1;
        if mbelib {
            mbe::to_unit(&mut buf);
        } else {
            mbe::to_limited(&mut buf);
        }
        audio.extend_from_slice(&buf);
    }
    std::fs::write(out, trunk_core::wav::encode(&audio, mbe::SAMPLE_RATE)).unwrap_or_else(|e| die(&format!("{out}: {e}")));
    eprintln!("{} frames ({:.2} s) → {out}: {kinds:?}; {differ} decoded differently from the recording", audio.len() / FRAME_SAMPLES, audio.len() as f64 / mbe::SAMPLE_RATE as f64);
}

fn run_p2(a: &Args) {
    use trunk_core::dsp::cqpsk::{self, Cqpsk};
    use trunk_core::dsp::Receiver;
    use trunk_core::p25::phase2::{self, *};

    let path = a.positional.get(1).unwrap_or_else(|| die("tool p2: no capture"));
    let cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("fs", a.num("rate", 2_400_000.0));
    let (center, freq) = (a.num("center", 0.0), a.num("freq", 0.0));
    let mask = a.get("nac").map(|_| xor_mask(num_any(a, "nac"), num_any(a, "sysid"), num_any(a, "wacn")));
    let soft = a.get("soft") != Some("none");
    let want_slot = a.num("slot", 0.0) as usize;
    let mut chz = Channelizer::new(fs, 24_000.0, 1.0);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(freq - center, 7000.0, 0.0);
    let mut rx = Cqpsk::new(rate, cqpsk::Options { baud: phase2::SYMBOL_RATE, eq_taps: a.num("eq", 0.0) as usize, ..Default::default() });
    let mut framer = phase2::Framer::default();
    let mut vocoder = mbe::Decoder::new(mbe::lcg(1), mbe::Profile::Enhanced);
    let mut audio_out = a.get("audio").map(|p| BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))));
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let (mut syms, mut pkts) = (Vec::new(), Vec::new());
    let mut by_type: BTreeMap<i8, u64> = BTreeMap::new();
    let (mut vcw, mut vcw_errs, mut vcw_clean, mut macs, mut mac_fail) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let t0 = Instant::now();
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if ran {
            syms.clear();
            pkts.clear();
            rx.push(chz.output(head).unwrap(), &mut syms);
            for s in &syms {
                framer.push(s, &mut pkts);
            }
            for p in &pkts {
                let burst = &p.dibits[10..];
                let kind = duid_decode(burst);
                *by_type.entry(kind).or_default() += 1;
                let mut line = format!("{{\"t\":{:.4},\"slot\":{},\"type\":{kind},\"dibits\":\"", p.sample / rate, p.slot);
                for d in p.dibits {
                    line.push((b'0' + d) as char);
                }
                line.push('"');
                let scrambled = matches!(kind, BURST_4V | BURST_2V | BURST_SACCH_S | BURST_LCCH_S | BURST_FACCH_S);
                let mut x = [0u8; BURST_DIBITS];
                x.copy_from_slice(&burst[..BURST_DIBITS]);
                if scrambled {
                    if let Some(m) = &mask {
                        for (i, v) in x.iter_mut().enumerate() {
                            *v ^= m[p.slot * SLOT_DIBITS + i];
                        }
                    }
                }
                if kind == BURST_4V || kind == BURST_2V {
                    line.push_str(",\"vcw\":[");
                    let at: &[usize] = if kind == BURST_4V { &[11, 48, 96, 133] } else { &[11, 48] };
                    for (k, &o) in at.iter().enumerate() {
                        let rel = &p.rel[2 * (10 + o)..2 * (10 + o) + 72];
                        let f = decode_vcw(&x[o..o + 36], soft.then_some(rel));
                        vcw += 1;
                        vcw_errs += f.errs as u64;
                        vcw_clean += (f.errs <= 1) as u64;
                        let bits: String = f.bits.iter().map(|&b| (b'0' + b) as char).collect();
                        let _ = write!(line, "{}{{\"bits\":\"{bits}\",\"errs\":{}}}", if k > 0 { "," } else { "" }, f.errs);
                        if SLOT_CHANNEL[p.slot] == want_slot {
                            if let Some(w) = audio_out.as_mut() {
                                let mut buf = [0f32; FRAME_SAMPLES];
                                vocoder.ambe(&f.bits, f.errs, &mut buf);
                                mbe::to_unit(&mut buf);
                                for v in buf {
                                    let _ = w.write_all(&v.to_le_bytes());
                                }
                            }
                        }
                    }
                    line.push(']');
                } else if matches!(kind, BURST_SACCH_S | BURST_SACCH_U | BURST_FACCH_S | BURST_FACCH_U) {
                    match decode_acch(&x, kind == BURST_FACCH_S || kind == BURST_FACCH_U) {
                        Some(pdu) => {
                            macs += 1;
                            let hex: String = pdu.bytes.iter().map(|b| format!("{b:02x}")).collect();
                            let _ = write!(line, ",\"mac\":{{\"op\":{},\"offset\":{},\"hex\":\"{hex}\"}}", pdu.opcode, pdu.offset);
                            if pdu.opcode == MAC_PTT {
                                let m = parse_mac_ptt(&pdu.bytes);
                                let _ = write!(line, ",\"ptt\":{{\"algid\":{},\"keyid\":{},\"src\":{},\"tg\":{}}}", m.algid, m.keyid, m.source, m.group);
                            }
                        }
                        None => {
                            mac_fail += 1;
                            line.push_str(",\"mac\":null");
                        }
                    }
                }
                line.push('}');
                let _ = writeln!(out, "{line}");
            }
        }
        if used == 0 {
            break;
        }
    }
    let cpu = t0.elapsed().as_secs_f64();
    let _ = out.flush();
    let air = cap.len() as f64 / 2.0 / fs;
    let types: Vec<String> = by_type.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    eprintln!(
        "{{\"mode\":\"p2\",\"airS\":{air:.2},\"pctCore\":{:.3},\"symbols\":{},\"syncs\":{},\"packets\":{},\"types\":{{{}}},\"vcw\":{vcw},\"meanErrs\":{:.3},\"cleanFrac\":{:.4},\"mac\":{macs},\"macFail\":{mac_fail}}}",
        100.0 * cpu / air,
        rx.symbols,
        rx.syncs,
        framer.packets,
        types.join(","),
        vcw_errs as f64 / vcw.max(1) as f64,
        vcw_clean as f64 / vcw.max(1) as f64,
    );
}

/// A SmartNet band plan from `--bandplan` and the `--bp-*` options.
pub fn smartnet_bandplan(a: &Args, name: &str) -> trunk_core::smartnet::Bandplan {
    trunk_core::smartnet::Bandplan::from_config(
        name,
        a.num("bp-base", 0.0),
        a.num("bp-spacing", 0.0),
        a.num("bp-offset", 0.0) as u16,
        a.num("bp-high", 0.0),
    )
    .unwrap_or_else(|e| die(&e))
}

fn run_smartnet(a: &Args) {
    use trunk_core::smartnet::{self, FramerOut, Fsk2, Framer, Parser};
    let path = a.positional.get(1).unwrap_or_else(|| die("tool smartnet: no capture"));
    let cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("rate", 2_400_000.0);
    let mut chz = Channelizer::new(fs, 24_000.0, 1.0);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(a.num("cc", 0.0) - a.num("center", 0.0), smartnet::CHANNEL_CUTOFF_HZ, 0.0);
    let mut rx = Fsk2::new(rate);
    let mut framer = Framer::default();
    let mut parser = Parser::new(smartnet_bandplan(a, a.get("bandplan").unwrap_or("800_standard")));
    let show_osw = a.flag("osw");
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let (mut bits, mut fout, mut msgs) = (Vec::new(), Vec::new(), Vec::new());
    let mut n_bits = 0u64;
    let t0 = Instant::now();
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if ran {
            bits.clear();
            rx.push(chz.output(head).unwrap_or(&[]), &mut bits);
            for b in &bits {
                n_bits += 1;
                fout.clear();
                framer.push(b.soft, &mut fout);
                let t = b.sample / rate;
                for o in &fout {
                    match *o {
                        FramerOut::Osw(osw, _) => {
                            if show_osw {
                                let f = parser.bandplan().rx_hz(osw.cmd).map_or("null".into(), |f| f.to_string());
                                let _ = writeln!(out, "{{\"t\":{t:.4},\"osw\":{{\"addr\":{},\"grp\":{},\"cmd\":\"{:03x}\",\"rx_hz\":{f}}}}}", osw.addr, osw.grp, osw.cmd);
                            }
                            parser.osw(osw, t, &mut msgs);
                        }
                        FramerOut::Bad(_) => {
                            if show_osw {
                                let _ = writeln!(out, "{{\"t\":{t:.4},\"bad\":true}}");
                            }
                            parser.bad(t, &mut msgs);
                        }
                    }
                }
                for m in msgs.drain(..) {
                    let _ = writeln!(
                        out,
                        "{{\"t\":{t:.4},\"kind\":\"{}\",\"tg\":{},\"freq\":{},\"src\":{},\"analog\":{},\"enc\":{},\"meta\":{:?}}}",
                        m.kind.as_str(),
                        m.talkgroup,
                        m.freq_hz,
                        m.source,
                        m.analog,
                        m.encrypted,
                        m.meta
                    );
                }
            }
        }
        if used == 0 {
            break;
        }
    }
    let _ = out.flush();
    let air = cap.len() as f64 / 2.0 / fs;
    eprintln!(
        "{air:.1} s of air in {:.2} s: {} bits, {} good OSWs ({:.1}/s of {:.1}/s), {} lost; offset {:+.0} Hz, deviation ±{:.0} Hz; system {}",
        t0.elapsed().as_secs_f64(),
        n_bits,
        framer.good,
        framer.good as f64 / air,
        smartnet::SYMBOL_RATE / smartnet::osw::FRAME_BITS as f64,
        framer.bad,
        rx.offset_hz(),
        rx.deviation_hz(),
        parser.sys_id.map_or("?".into(), |s| format!("{s:04x}")),
    );
}
