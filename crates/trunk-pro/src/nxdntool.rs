//! `trunk-pro tool nxdnscan <capture> --center Hz [--fs Hz] [--seconds N]
//! [--step Hz]` — every channel of a capture through both NXDN receivers
//! (2400 and 4800 baud) and the framer: the channels with NXDN on them, by
//! frames, rate, RAN and what they carry.
//!
//! `trunk-pro tool nxdn <capture> --center Hz --freq Hz [--nxdn 48|96]
//! [--frames] [--audio out.f32]` — one NXDN channel: its layer 3 messages
//! as JSON lines (`--frames`: every frame), then counts; `--audio` vocodes
//! its voice (8 kHz f32).

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
    let mut rx = nxdn.receiver(rate);
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
            let mut msgs: Vec<(&str, Message)> = Vec::new();
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
                        msgs.push(("cac", first));
                        if dual {
                            msgs.push(("cac", Message::parse(&o[9..], Context::Control, dfa)));
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
                                            msgs.push(("sacch", Message::parse(&o, Context::Traffic, dfa)));
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
                                        msgs.push(("facch1", Message::parse(&o, Context::Traffic, dfa)));
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
                                    msgs.push(("facch2", Message::parse(&o, Context::Traffic, dfa)));
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
            for (ch, m) in msgs {
                let s = format!("{m:?}");
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
