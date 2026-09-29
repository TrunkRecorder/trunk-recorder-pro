//! `trunk-lite tool cc|voice|frames` — one channel's decode as JSON lines, in
//! the format of research/native-bench's C++ `p25tool`, so the comparison and
//! ground-truth scripts there run against this implementation unchanged.
//!
//! Receivers: `--demod cqpsk|c4fm|auto` (auto: CQPSK + C4FM), `--diversity 1`
//! (CQPSK + CQPSK/EQ + C4FM, `--eq 9 --mu 0.02`). Decoding: `--trellis
//! greedy|viterbi`, `--soft none|amp`, `--softfec 0|1`, `--flywheel 0|1`,
//! `--nidrecover 0|1`. Output: `--audio out.f32` (voice), `--iq out.cf32`.

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
    let mode = a.positional.first().map(String::as_str).unwrap_or_else(|| die("tool cc|voice|frames <capture> …"));
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
            ",\"{key}\":{{\"lco\":{},\"prot\":{},\"svc\":{},\"tgid\":{},\"target\":{},\"src\":{}}}",
            lc.lco,
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
