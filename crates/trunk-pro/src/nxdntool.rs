//! `trunk-pro tool nxdnscan <capture> --center Hz [--fs Hz] [--seconds N]
//! [--step Hz]` — every channel of a capture through both NXDN receivers
//! (2400 and 4800 baud) and the framer: the channels with NXDN on them, by
//! frames, rate, RAN and what they carry.
//!
//! `trunk-pro tool nxdn <capture> --center Hz --freq Hz [--nxdn 48|96]
//! [--frames] [--audio out.f32] [--variant sinc,…]` — one NXDN channel: its
//! layer 3 messages as JSON lines (`--frames`: every frame), then counts;
//! `--audio` vocodes its voice (8 kHz f32).
//!
//! `trunk-pro tool nxdnsynth <out.cu8> --center Hz [--kind conv|typeC|typeD]
//! [--nxdn 48|96] [--rate 2400000]` — a synthetic capture (rtl_sdr's cu8):
//! a conventional call at centre + 50 kHz; a Type-C control channel at
//! centre + 18.75 kHz granting group 3001 channel 12, whose call is at
//! centre + 68.75 kHz; or a Type-D site, repeaters at centre + 12.5 / 62.5
//! kHz, the call on the second. For checking a setup, or another decoder
//! (SDRTrunk, DSD-FME) against this one on the same IQ.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

use trunk_core::ambe::decode_vcw;
use trunk_core::dsp::c4fm::C4fm;
use trunk_core::dsp::{Channelizer, HeadId, Receiver};
use trunk_core::mbe::{self, FRAME_SAMPLES};
use trunk_core::nxdn::channel::{self, SacchAssembler};
use trunk_core::nxdn::frame::{Body, Frame, Framer, RfChannel};
use trunk_core::nxdn::layer3::{Context, Message};
use trunk_core::nxdn::Rate;

use crate::{die, Args};

fn capture(a: &Args, what: &str) -> (Vec<u8>, f64, f64) {
    let path = a.positional.get(1).unwrap_or_else(|| die(&format!("tool {what}: no capture")));
    let mut cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("fs", a.num("rate", 2_400_000.0));
    if let Some(s) = a.get("seconds").and_then(|v| v.parse::<f64>().ok()) {
        cap.truncate((s * fs) as usize * 2);
    }
    (cap, fs, a.num("center", 0.0))
}

/// Run the capture through the channelizer, calling `each` with every head's output.
fn channelize(cap: &[u8], chz: &mut Channelizer, mut each: impl FnMut(&Channelizer)) {
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if !ran {
            if used == 0 {
                break;
            }
            continue;
        }
        each(chz);
    }
}

pub fn run_scan(a: &Args) {
    let (cap, fs, center) = capture(a, "nxdnscan");
    let mut chz = Channelizer::new(fs, 24_000.0, 0.1);
    let rate = chz.output_rate();
    let span = trunk_core::trunk::usable_half_width(fs, a.num("guard", trunk_core::trunk::DEFAULT_GUARD_HZ));
    let step = a.num("step", 6250.0);
    let first = ((center - span) / step).ceil() as i64;
    let last = ((center + span) / step).floor() as i64;
    struct Rx {
        nxdn: Rate,
        head: HeadId,
        rx: C4fm,
        framer: Framer,
        lich: u64,
        rans: BTreeMap<u8, u64>,
        kinds: BTreeMap<&'static str, u64>,
    }
    struct Ch {
        hz: f64,
        rxs: Vec<Rx>,
    }
    let mut chans: Vec<Ch> = (first..=last)
        .map(|k| {
            let hz = k as f64 * step;
            let rxs = [Rate::N48, Rate::N96]
                .into_iter()
                .map(|n| {
                    let (head, _, _) = chz.add_head(hz - center, n.cutoff_hz(), 0.0);
                    let mut o = trunk_core::dsp::c4fm::C4fmOptions::nxdn(n.baud());
                    // A scan only needs syncs: no multi-symbol detection.
                    o.msd = None;
                    Rx { nxdn: n, head, rx: C4fm::with_options(rate, o), framer: Framer::new(), lich: 0, rans: BTreeMap::new(), kinds: BTreeMap::new() }
                })
                .collect();
            Ch { hz, rxs }
        })
        .collect();
    let t0 = Instant::now();
    let (mut syms, mut frames) = (Vec::new(), Vec::new());
    channelize(&cap, &mut chz, |chz| {
        for c in chans.iter_mut() {
            for r in c.rxs.iter_mut() {
                syms.clear();
                frames.clear();
                r.rx.push(chz.output(r.head).unwrap(), &mut syms);
                for s in &syms {
                    r.framer.push(s, &mut frames);
                }
                for f in &frames {
                    let Some((l, _)) = f.lich() else { continue };
                    r.lich += 1;
                    *r.kinds.entry(kind_name(l.rf(), l.body(), l.is_cac())).or_default() += 1;
                    let ran = if l.is_cac() { channel::cac(f).map(|c| c.0.ran) } else { channel::sacch(f).map(|s| s.sr.ran) };
                    if let Some(ran) = ran {
                        *r.rans.entry(ran).or_default() += 1;
                    }
                }
            }
        }
    });
    let secs = cap.len() as f64 / 2.0 / fs;
    eprintln!("{} channels × 2 rates, {secs:.1} s of capture in {:.1} s", chans.len(), t0.elapsed().as_secs_f64());
    let mut found: Vec<(&Ch, &Rx)> = chans.iter().flat_map(|c| c.rxs.iter().map(move |r| (c, r))).filter(|(_, r)| r.lich >= 6).collect();
    found.sort_by_key(|(_, r)| std::cmp::Reverse(r.lich));
    for (c, r) in found {
        let ran = r.rans.iter().max_by_key(|&(_, n)| n).map_or("?".into(), |(r, _)| r.to_string());
        let kinds: Vec<String> = r.kinds.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        println!("{:.5} MHz  {}  frames {:6}  ran {:>2}  {}", c.hz / 1e6, r.nxdn.name(), r.lich, ran, kinds.join(" "));
    }
}

fn kind_name(rf: RfChannel, body: Body, cac: bool) -> &'static str {
    if cac {
        return "control";
    }
    match (rf, body) {
        (_, Body::Voice { idle: true, .. }) => "idle",
        (RfChannel::Direct, Body::Voice { .. }) => "conv_voice",
        (_, Body::Voice { .. }) => "voice",
        (_, Body::Udch) => "data",
        (_, Body::Facch2) => "facch2",
    }
}

#[derive(Default)]
struct Counts {
    frames: u64,
    lich: u64,
    by_kind: BTreeMap<String, u64>,
    sacch: (u64, u64),
    facch1: (u64, u64),
    udch: (u64, u64),
    cac: (u64, u64),
    vcw: u64,
    vcw_errs: u64,
    vcw_clean: u64,
    vocoded: BTreeMap<String, u64>,
}

pub fn run(a: &Args) {
    let (cap, fs, center) = capture(a, "nxdn");
    let freq = a.num("freq", 0.0);
    let nxdn = Rate::parse(a.get("nxdn").unwrap_or("48")).unwrap_or_else(|| die("--nxdn 48|96"));
    let all = a.flag("frames");
    let mut chz = Channelizer::new(fs, 24_000.0, 0.1);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(freq - center, nxdn.cutoff_hz(), 0.0);
    // --variant sinc,nomsd,…: receiver settings (C4fmOptions::set).
    let mut opts = trunk_core::dsp::c4fm::C4fmOptions::nxdn(nxdn.baud());
    for v in a.get("variant").unwrap_or("").split(',').filter(|v| !v.is_empty()) {
        if !opts.set(v) {
            die(&format!("--variant: unknown setting {v}"));
        }
    }
    let mut rx = C4fm::with_options(rate, opts);
    let mut framer = Framer::new();
    let mut vocoder = mbe::Decoder::new(mbe::lcg(1), mbe::Profile::Enhanced);
    let mut audio_out = a.get("audio").map(|p| BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))));
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut c = Counts::default();
    let mut sf = SacchAssembler::default();
    let mut dfa = false;
    let (mut syms, mut frames) = (Vec::new(), Vec::<Frame>::new());
    let t0 = Instant::now();
    let mut last_msg = String::new();
    channelize(&cap, &mut chz, |chz| {
        syms.clear();
        frames.clear();
        rx.push(chz.output(head).unwrap(), &mut syms);
        for s in &syms {
            framer.push(s, &mut frames);
        }
        for f in &frames {
            c.frames += 1;
            let t = f.sample / rate;
            let lich = f.lich();
            let mut line = format!("{{\"t\":{t:.3},\"sync\":{}", f.sync_errs.map_or("null".into(), |e| e.to_string()));
            let mut msgs: Vec<(&str, Message, Vec<u8>)> = Vec::new();
            if let Some((l, inner)) = lich {
                c.lich += 1;
                let _ = write!(line, ",\"lich\":\"{:02x}\",\"lich_inner\":{inner}", l.raw);
                *c.by_kind.entry(format!("{:02x}", l.raw)).or_default() += 1;
                if l.is_cac() {
                    let d = channel::cac(f);
                    c.cac.1 += 1;
                    if let Some((sr, o, errs)) = d {
                        c.cac.0 += 1;
                        let _ = write!(line, ",\"cac\":{{\"ran\":{},\"structure\":{},\"errs\":{errs}}}", sr.ran, sr.structure);
                        let dual = sr.structure & 1 != 0;
                        let first = Message::parse(&o, Context::Control, dfa);
                        if let Message::SiteInfo { access, .. } = &first {
                            dfa = access.dfa;
                        }
                        msgs.push(("cac", first, o.to_vec()));
                        if dual {
                            msgs.push(("cac", Message::parse(&o[9..], Context::Control, dfa), (o[9..]).to_vec()));
                        }
                    }
                } else {
                    match l.body() {
                        Body::Voice { superframe, facch, .. } => {
                            c.sacch.1 += 1;
                            match channel::sacch(f) {
                                Some(s) => {
                                    c.sacch.0 += 1;
                                    let _ = write!(line, ",\"sacch\":{{\"ran\":{},\"structure\":{},\"errs\":{}}}", s.sr.ran, s.sr.structure, s.errs);
                                    if superframe {
                                        if let Some(o) = sf.push(&s) {
                                            msgs.push(("sacch", Message::parse(&o, Context::Traffic, dfa), (o).to_vec()));
                                        }
                                    }
                                }
                                None => sf.miss(),
                            }
                            for h in 0..2 {
                                if facch[h] {
                                    c.facch1.1 += 1;
                                    if let Some((o, _)) = channel::facch1(f, h) {
                                        c.facch1.0 += 1;
                                        msgs.push(("facch1", Message::parse(&o, Context::Traffic, dfa), (o).to_vec()));
                                    }
                                } else {
                                    for k in 0..2 {
                                        let (d, r) = f.voice_frame(2 * h + k);
                                        let v = decode_vcw(&d, Some(&r));
                                        c.vcw += 1;
                                        c.vcw_errs += v.errs as u64;
                                        c.vcw_clean += (v.errs <= 1) as u64;
                                        if let Some(w) = audio_out.as_mut() {
                                            let mut buf = [0f32; FRAME_SAMPLES];
                                            let k = vocoder.ambe(&v.bits, v.errs, &mut buf);
                                            *c.vocoded.entry(format!("{k:?}")).or_default() += 1;
                                            mbe::to_limited(&mut buf);
                                            for x in buf {
                                                w.write_all(&x.to_le_bytes()).unwrap();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Body::Udch | Body::Facch2 => {
                            c.udch.1 += 1;
                            if let Some((_, o, _)) = channel::udch(f) {
                                c.udch.0 += 1;
                                if l.body() == Body::Facch2 {
                                    msgs.push(("facch2", Message::parse(&o, Context::Traffic, dfa), (o).to_vec()));
                                }
                            }
                        }
                    }
                }
            }
            line.push('}');
            if all {
                writeln!(out, "{line}").unwrap();
            }
            for (ch, m, raw) in msgs {
                let mut s = format!("{m:?}");
                if let Message::PropForm { .. } = m {
                    s.push_str(&format!(" raw={}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>()));
                }
                if s != last_msg || all {
                    writeln!(out, "{{\"t\":{t:.3},\"ch\":\"{ch}\",\"msg\":\"{}\",\"detail\":{:?}}}", m.name(), s).unwrap();
                    last_msg = s;
                }
            }
        }
    });
    out.flush().unwrap();
    let secs = cap.len() as f64 / 2.0 / fs;
    let pct = |p: (u64, u64)| if p.1 == 0 { "-".to_string() } else { format!("{}/{} ({:.1} %)", p.0, p.1, 100.0 * p.0 as f64 / p.1 as f64) };
    eprintln!("{secs:.1} s of capture in {:.2} s; receiver separation {:?}", t0.elapsed().as_secs_f64(), rx.separation());
    eprintln!("frames {} (syncs {}{}), LICH valid {}", c.frames, framer.syncs, if framer.inverted { ", inverted" } else { "" }, c.lich);
    eprintln!("LICH values: {}", c.by_kind.iter().map(|(k, n)| format!("{k}:{n}")).collect::<Vec<_>>().join(" "));
    eprintln!("CAC {}  SACCH {}  FACCH1 {}  UDCH/FACCH2 {}", pct(c.cac), pct(c.sacch), pct(c.facch1), pct(c.udch));
    if c.vcw > 0 {
        eprintln!("voice codewords {}: ≤1 error {:.1} %, mean errors {:.2}", c.vcw, 100.0 * c.vcw_clean as f64 / c.vcw as f64, c.vcw_errs as f64 / c.vcw as f64);
        if !c.vocoded.is_empty() {
            eprintln!("vocoded: {}", c.vocoded.iter().map(|(k, n)| format!("{k}:{n}")).collect::<Vec<_>>().join(" "));
        }
    }
}

pub fn run_synth(a: &Args) {
    use trunk_core::nxdn::channel::{build as cb, Sr};
    use trunk_core::nxdn::layer3::{build as l3, CallHead, CALL_CONFERENCE};
    use trunk_core::nxdn::synth::{add_noise, cac_frame, dibits, modulate, voice_frame, Half, Tx};
    let path = a.positional.get(1).unwrap_or_else(|| die("tool nxdnsynth: no output file"));
    let center = a.num("center", 451_000_000.0);
    let fs = a.num("rate", 2_400_000.0);
    let nxdn = Rate::parse(a.get("nxdn").unwrap_or("48")).unwrap_or_else(|| die("--nxdn 48|96"));
    let kind = a.get("kind").unwrap_or("typeC");
    let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: if nxdn == Rate::N96 { 2 } else { 0 }, source: 77, destination: 3001 };
    let idle = l3::idle();
    let idle_frame = voice_frame(0x39, Sr { structure: 0, ran: 5 }, 0, &[Half::Facch1(idle.clone()), Half::Facch1(idle.clone())]);
    // (offset Hz, frames)
    let carriers: Vec<(f64, Vec<[u8; 192]>)> = match kind {
        "conv" => {
            let t = Tx { rate: nxdn, ran: 5, head, cipher: 0, superframes: 15, rf: 2, outbound: true, alias: None };
            vec![(50_000.0, t.frames())]
        }
        "typeC" => {
            let t = Tx { rate: nxdn, ran: 5, head, cipher: 0, superframes: 15, rf: 1, outbound: true, alias: None };
            let mut voice = vec![idle_frame; 4];
            voice.extend(t.frames());
            let sr = Sr { structure: 2, ran: 5 };
            let mut ctrl = vec![cac_frame(sr, &l3::site_info(0x123 << 12 | 0x045, 0x0200, 0, 0, [1, 0])), cac_frame(sr, &l3::vcall_assgn(false, &head, 4, 12))];
            while ctrl.len() < voice.len() + 4 {
                let m = if ctrl.len() % 8 == 0 { l3::site_info(0x123 << 12 | 0x045, 0x0200, 0, 0, [1, 0]) } else { l3::vcall_assgn(true, &head, 4, 12) };
                ctrl.push(cac_frame(sr, &m));
            }
            vec![(18_750.0, ctrl), (68_750.0, voice)]
        }
        "typeD" => {
            let tg: u16 = 3 << 11 | 101;
            let src: u16 = 3 << 11 | 1234;
            let d_frame = |lich: u8, structure: u8, data: u32, n: Option<usize>| {
                use trunk_core::nxdn::frame::{frame_dibits, Lich, BODY_SYMBOLS};
                let mut body = [0u8; BODY_SYMBOLS];
                body[..8].copy_from_slice(&Lich { raw: lich }.dibits());
                for (i, &b) in cb::scch(structure, false, data).iter().enumerate() {
                    let k = 16 + i;
                    body[k / 2] |= b << (1 - k % 2);
                }
                if let Some(n) = n {
                    for j in 0..4 {
                        let d = trunk_core::ambe::encode_vcw(&trunk_core::nxdn::synth::ambe(4 * n + j));
                        let at = (76 + 72 * j) / 2;
                        body[at..at + 36].copy_from_slice(&d);
                    }
                }
                frame_dibits(&body)
            };
            let idle_d = |k: usize| d_frame(0x7f, 3 - (k % 4) as u8, if k % 4 == 3 { cb::scch_id(0, 2046, false) } else { 0 }, None);
            let r1: Vec<_> = (0..70).map(idle_d).collect();
            let mut r2: Vec<_> = (0..4).map(idle_d).collect();
            for n in 0..60usize {
                let k = n % 4;
                let data = match k {
                    0 => cb::scch_info1(0, 0, 0, 0, 0),
                    1 | 3 => cb::scch_id(2, tg, true),
                    _ => cb::scch_id(0, src, false),
                };
                r2.push(d_frame(0x77, 3 - k as u8, data, Some(n)));
            }
            vec![(12_500.0, r1), (62_500.0, r2)]
        }
        k => die(&format!("--kind {k}: conv, typeC or typeD")),
    };
    let mut sum: Vec<num_complex::Complex32> = Vec::new();
    for (i, (off, frames)) in carriers.iter().enumerate() {
        let mut x = 7u32 + i as u32;
        let mut rnd = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect()
        };
        let mut d = rnd(nxdn.baud() as usize / 2);
        d.extend(dibits(frames));
        d.extend(rnd(nxdn.baud() as usize));
        let mut ph = 0.0;
        let iq = modulate(&d, None, nxdn, fs, *off, 0.25, &mut ph);
        if sum.len() < iq.len() {
            sum.resize(iq.len(), Default::default());
        }
        for (s, v) in sum.iter_mut().zip(&iq) {
            *s += v;
        }
    }
    let mut seed = 1;
    add_noise(&mut sum, 0.02, &mut seed);
    let mut w = BufWriter::new(File::create(path).unwrap_or_else(|e| die(&format!("{path}: {e}"))));
    for v in &sum {
        w.write_all(&[(v.re * 127.5 + 127.5).clamp(0.0, 255.0) as u8, (v.im * 127.5 + 127.5).clamp(0.0, 255.0) as u8]).unwrap();
    }
    eprintln!("{path}: {:.1} s at {fs} S/s, centre {center} Hz ({kind}, {})", sum.len() as f64 / fs, nxdn.name());
}
