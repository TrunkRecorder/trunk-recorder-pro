//! `trunk-pro tool snr <capture> --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet
//! [--seconds N] [--snr 40,20,16,14,12,10,8,6] [--variant base,…] [--cutoff Hz]`
//! (`--kind p2 --nac --sysid --wacn`: P25 Phase 2 voice; `--quality`: a real
//! channel as it is, per variant) —
//! weak-signal curves from a strong capture: one channel's IQ is cut out
//! once, then decoded again and again with noise added at each SNR (signal
//! power over the noise in 12.5 kHz). The noise is shaped by the same
//! channel filter the real noise would have come through, so the receivers
//! see what a weaker signal would give them.
//!
//! Counted per SNR, as a percentage of the noiseless run: its messages decoded
//! again, the same content within 0.2 s (dmr: CSBKs, LCs and AMBE codewords;
//! p25: TSBKs and IMBE codewords; smartnet: OSWs) — so a message or codeword
//! decoded wrong doesn't count, however sure its FEC was. Per receiver variant (see [`Variant`]).

use std::f64::consts::PI;
use std::fmt::Write as _;

use num_complex::Complex32;
use trunk_core::dmr::{self, Framer, SlotEvent};
use trunk_core::dsp::c4fm::{C4fm, C4fmOptions};
use trunk_core::dsp::{Channelizer, Receiver};
use trunk_core::p25::diversity::{best_imbe, best_tsbks, Bank, BankConfig};
use trunk_core::p25::frame::{LDU1, LDU2, TSDU};
use trunk_core::smartnet;

use crate::{die, Args};

/// Channel IQ of `freq` from a capture, through a head of `cutoff` Hz.
fn channel(a: &Args, cutoff: f64) -> (Vec<Complex32>, f64) {
    let path = a.positional.get(1).unwrap_or_else(|| die("tool snr: no capture"));
    let mut cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("rate", 2_400_000.0);
    if let Some(s) = a.get("seconds").and_then(|v| v.parse::<f64>().ok()) {
        cap.truncate((s * fs) as usize * 2);
    }
    let mut chz = Channelizer::new(fs, 24_000.0, 0.1);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(a.num("freq", 0.0) - a.num("center", 0.0), cutoff, 0.0);
    let mut iq = Vec::new();
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if ran {
            iq.extend_from_slice(chz.output(head).unwrap());
        }
        if used == 0 {
            break;
        }
    }
    (iq, rate)
}

/// Windowed-sinc low-pass taps (Blackman), cutoff `fc` at rate `fs`.
fn lowpass(fc: f64, fs: f64, n: usize) -> Vec<f32> {
    let m = (n - 1) as f64 / 2.0;
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m;
            let s = if x == 0.0 { 2.0 * fc / fs } else { (2.0 * PI * fc / fs * x).sin() / (PI * x) };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos() + 0.08 * (4.0 * PI * i as f64 / (n - 1) as f64).cos();
            s * w
        })
        .collect();
    // Unit passband gain: the shaped noise keeps the white noise's in-band density.
    let g: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v /= g;
    }
    h.into_iter().map(|v| v as f32).collect()
}

/// Mean power of the signal: over the 10 ms blocks within 50 ms of a
/// message the noiseless decode got (a channel can carry other things, or
/// nothing, the rest of the time).
fn signal_power(iq: &[Complex32], rate: f64, clean: &Count) -> f64 {
    let blk = (rate / 100.0) as usize;
    let mut on = vec![false; iq.len() / blk + 1];
    for (t, _) in clean.blocks.iter().chain(&clean.voice) {
        let c = (t * 100.0) as i64;
        for b in c - 5..=c + 5 {
            if b >= 0 && (b as usize) < on.len() {
                on[b as usize] = true;
            }
        }
    }
    let (mut sum, mut n) = (0.0f64, 0usize);
    for (i, c) in iq.chunks(blk).enumerate() {
        if on[i] {
            sum += c.iter().map(|v| v.norm_sqr() as f64).sum::<f64>();
            n += c.len();
        }
    }
    if n == 0 {
        die("tool snr: the noiseless channel decodes nothing");
    }
    sum / n as f64
}

/// `iq` plus shaped Gaussian noise at `snr_db` (in 12.5 kHz), seeded.
fn add_noise(iq: &[Complex32], rate: f64, s: f64, snr_db: f64, taps: &[f32], seed: u64) -> Vec<Complex32> {
    // Flat density N0 = S / (SNR · 12.5 kHz); per-sample variance N0 · rate.
    let var = s / 10f64.powf(snr_db / 10.0) / 12_500.0 * rate;
    let sd = (var / 2.0).sqrt() as f32;
    let mut x = seed | 1;
    let mut u = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    let n = iq.len() + taps.len();
    let white: Vec<Complex32> = (0..n)
        .map(|_| {
            let (a, b) = (u(), u());
            let r = (-2.0 * a.ln()).sqrt();
            Complex32::new((r * (2.0 * PI * b).cos()) as f32 * sd, (r * (2.0 * PI * b).sin()) as f32 * sd)
        })
        .collect();
    iq.iter()
        .enumerate()
        .map(|(i, &v)| {
            let mut acc = Complex32::default();
            for (k, &h) in taps.iter().enumerate() {
                acc += white[i + k] * h;
            }
            v + acc
        })
        .collect()
}

/// A receiver variant under test (`--variant`, comma separated).
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub c4fm: C4fmOptions,
    /// P25: the C4FM receiver alone (no CQPSK receivers).
    pub c4fm_only: bool,
    /// DMR: no soft combining of repeated blocks.
    pub no_combining: bool,
    pub fsk2: smartnet::Fsk2Options,
    /// DMR: sync errors accepted on the grid.
    pub grid_errs: Option<u32>,
    /// P25 Phase 2 (and CQPSK generally): the receiver's settings.
    pub cqpsk: trunk_core::dsp::cqpsk::Options,
}

/// A variant: its protocol's receiver (`base`) with `+`-joined changes.
fn variant(name: &str, kind: &str) -> Variant {
    let mut o = if kind == "dmr" { C4fmOptions::dmr() } else { C4fmOptions::default() };
    let mut c4fm_only = false;
    let mut no_combining = false;
    let mut fsk2 = smartnet::Fsk2Options::default();
    let mut grid_errs = None;
    // Phase 2 voice's receiver as the engine runs it.
    let mut cq = trunk_core::dsp::cqpsk::Options { baud: trunk_core::p25::phase2::SYMBOL_RATE, df_beta: 0.5, ..Default::default() };

    for part in name.split('+') {
        match part {
            "base" | "" => {}
            "c4fm-only" => c4fm_only = true,
            "nocombine" => no_combining = true,
            "coherent" => cq.coherent = true,
            "noamp" => cq.soft_amplitude = false,
            p if p.starts_with("df=") => cq.df_beta = p[3..].parse().unwrap_or(0.0),
            p if p.starts_with("eq=") => cq.eq_taps = p[3..].parse().unwrap_or(0),
            p if p.starts_with("mu=") => cq.eq_mu = p[3..].parse().unwrap_or(0.02),

            p if p.starts_with("grid=") => grid_errs = p[5..].parse().ok(),
            p => {
                if !o.set(p) && !fsk2.set(p) {
                    die(&format!("tool snr: unknown variant part \"{p}\""));
                }
            }
        }
    }
    Variant { name: name.into(), c4fm: o, c4fm_only, no_combining, fsk2, grid_errs, cqpsk: cq }
}

/// What a decode of the channel got: each message's time (s) and content,
/// and the voice codewords with ≤ 2 bit errors (time, bits).
#[derive(Clone, Debug, Default)]
struct Count {
    blocks: Vec<(f64, String)>,
    voice: Vec<(f64, String)>,
}

/// How many of `got` the clean run also has: same content, within 0.2 s.
fn matched(clean: &[(f64, String)], got: &[(f64, String)]) -> u64 {
    let mut by: std::collections::HashMap<&str, Vec<(f64, bool)>> = Default::default();
    for (t, c) in clean {
        by.entry(c.as_str()).or_default().push((*t, false));
    }
    let mut n = 0;
    for (t, c) in got {
        if let Some(v) = by.get_mut(c.as_str()) {
            if let Some(x) = v.iter_mut().find(|x| !x.1 && (x.0 - t).abs() < 0.2) {
                x.1 = true;
                n += 1;
            }
        }
    }
    n
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run_dmr(iq: &[Complex32], rate: f64, v: &Variant) -> Count {
    let mut rx = C4fm::with_options(rate, v.c4fm);
    let (mut syms, mut bursts, mut ev) = (Vec::new(), Vec::new(), Vec::new());

    let (mut f, mut ch) = (Framer::new(), dmr::Channel::default());
    if let Some(g) = v.grid_errs {
        f.grid_errs = g;
    }
    for s in ch.slots.iter_mut() {
        s.no_combining = v.no_combining;
    }
    let mut n = Count::default();
    for c in iq.chunks(4096) {
        syms.clear();
        bursts.clear();
        rx.push(c, &mut syms);
        for s in &syms {
            f.push(s, &mut bursts);
        }
        for b in &bursts {
            ev.clear();
            ch.burst(b, &mut ev);
            let t = b.sample / rate;
            for (s, e) in &ev {
                match e {
                    SlotEvent::Csbk { csbk, .. } => n.blocks.push((t, format!("{s}c{}", hex(&csbk.0)))),
                    SlotEvent::Lc { lc, .. } => n.blocks.push((t, format!("{s}l{}", hex(&lc.0)))),
                    SlotEvent::Voice { frame, .. } => n.voice.push((t, format!("{s}{}", hex(&frame.bits)))),
                    _ => {}
                }
            }
        }
    }
    n
}

fn run_p25(iq: &[Complex32], rate: f64, v: &Variant) -> Count {
    // The live receiver set (CQPSK, CQPSK/EQ, C4FM), or C4FM alone with `c4fm-only`.
    let cfg = if v.c4fm_only { BankConfig { cqpsk: false, cqpsk_eq: false, ..Default::default() } } else { BankConfig::default() };
    let mut bank = Bank::with_c4fm(rate, cfg, v.c4fm);
    let mut groups = Vec::new();
    let mut n = Count::default();
    for c in iq.chunks(4096) {
        groups.clear();
        bank.push(c, &mut groups);
        for g in &groups {
            let t = g[0].sample / rate;
            if g[0].nid.duid == TSDU {
                n.blocks.extend(best_tsbks(g).0.iter().map(|b| (t, hex(b))));
            } else if g.iter().any(|f| f.nid.duid == LDU1 || f.nid.duid == LDU2) {
                for (k, p) in best_imbe(g).iter().enumerate() {
                    n.voice.push((t + k as f64 * 0.02, format!("{:x?}", p.u)));
                }
            }
        }
    }
    n
}

/// P25 Phase 2 TDMA: H-DQPSK receiver → slot framer → tracker (descrambled
/// with `key`: NAC, System ID, WACN) → AMBE codewords of both slots.
fn run_p2(iq: &[Complex32], rate: f64, v: &Variant, key: (u32, u32, u32)) -> Count {
    use trunk_core::dsp::cqpsk::Cqpsk;
    use trunk_core::p25::phase2;
    use trunk_core::trunk::tdma::TdmaTracker;
    use trunk_core::trunk::voice::TrackerOut;
    let mut rx = Cqpsk::new(rate, v.cqpsk);
    let mut fr = phase2::Framer::new();
    let mut tr = TdmaTracker::new(1);
    tr.set_key(key.0, key.1, key.2);
    let (mut syms, mut pkts, mut out) = (Vec::new(), Vec::new(), Vec::new());
    let mut n = Count::default();
    for c in iq.chunks(4096) {
        syms.clear();
        pkts.clear();
        rx.push(c, &mut syms);
        for s in &syms {
            fr.push(s, &mut pkts);
        }
        for p in &pkts {
            out.clear();
            let t = p.sample / rate;
            tr.packet(p, t, &mut out);
            for (slot, o) in out.drain(..) {
                if let TrackerOut::Audio(_, f) = o {
                    n.voice.push((t, format!("{slot}{}", hex(&f.bits))));
                }
            }
        }
    }
    n
}

fn run_smartnet(iq: &[Complex32], rate: f64, v: &Variant) -> Count {
    let mut rx = smartnet::Fsk2::with_options(rate, v.fsk2);
    let mut fr = smartnet::Framer::new();
    let (mut bits, mut out) = (Vec::new(), Vec::new());
    let mut n = Count::default();
    for c in iq.chunks(4096) {
        bits.clear();
        rx.push(c, &mut bits);
        for b in &bits {
            out.clear();
            fr.push(b.soft, &mut out);
            for o in &out {
                if let smartnet::Word::Osw(w, _) = o {
                    n.blocks.push((b.sample / rate, format!("{:04x}{}{:03x}", w.addr, w.grp as u8, w.cmd)));
                }
            }
        }
    }
    n
}

pub fn run(a: &Args) {
    if a.flag("separation") {
        return separation(a);
    }
    if a.flag("quality") {
        return quality(a);
    }
    let kind = a.get("kind").unwrap_or("dmr");
    let cutoff = a.num(
        "cutoff",
        match kind {
            "dmr" => dmr::CHANNEL_CUTOFF_HZ,
            "smartnet" => smartnet::CHANNEL_CUTOFF_HZ,
            _ => 7000.0,
        },
    );
    let snrs: Vec<f64> = a.get("snr").unwrap_or("40,20,16,14,12,10,8,6").split(',').filter_map(|s| s.parse().ok()).collect();
    let variants: Vec<Variant> = a.get("variant").unwrap_or("base").split(',').map(|n| variant(n, kind)).collect();
    let (iq, rate) = channel(a, cutoff);
    let taps = lowpass(cutoff, rate, 63);
    let hexarg = |k: &str| a.get(k).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    let key = (hexarg("nac"), hexarg("sysid"), hexarg("wacn"));
    let run = |iq: &[Complex32], v: &Variant| match kind {
        "dmr" => run_dmr(iq, rate, v),
        "p25" => run_p25(iq, rate, v),
        "smartnet" => run_smartnet(iq, rate, v),
        "p2" => run_p2(iq, rate, v, key),
        k => die(&format!("tool snr: unknown kind {k}")),
    };
    let clean: Vec<Count> = variants.iter().map(|v| run(&iq, v)).collect();
    let s = signal_power(&iq, rate, &clean[0]);
    let mut head = format!("{:>6}", "SNR dB");
    for (v, c) in variants.iter().zip(&clean) {
        let _ = write!(head, "  {:>22}", format!("{} ({}/{})", v.name, c.blocks.len(), c.voice.len()));
    }
    println!("{} s of {kind} at {:.0} Hz; clean counts (blocks/voice) in the header", iq.len() as f64 / rate, a.num("freq", 0.0));
    println!("{head}");
    let seeds: Vec<u64> = (0..a.num("trials", 1.0) as u64).map(|k| 0x9e37_79b9_7f4a_7c15 ^ k.wrapping_mul(0x1234_5678_9abc)).collect();
    for &snr in &snrs {
        let mut line = format!("{snr:>6.1}");
        let noisy: Vec<Vec<Complex32>> = seeds.iter().map(|&sd| add_noise(&iq, rate, s, snr, &taps, sd)).collect();
        for (v, c) in variants.iter().zip(&clean) {
            let (mut b, mut vo) = (0u64, 0u64);
            for x in &noisy {
                let n = run(x, v);
                b += matched(&c.blocks, &n.blocks);
                vo += matched(&c.voice, &n.voice);
            }
            let t = seeds.len() as f64;
            let pct = |x: u64, of: usize| if of == 0 { "-".to_string() } else { format!("{:.1}", 100.0 * x as f64 / t / of as f64) };
            let _ = write!(line, "  {:>22}", format!("{} / {}", pct(b, c.blocks.len()), pct(vo, c.voice.len())));
        }
        println!("{line}");
    }
}

/// `tool snr … --kind p2 --quality`: a real channel as it is, per variant —
/// the share of AMBE codewords with ≤ 1 bit error, and MAC PDUs failing.
pub fn quality(a: &Args) {
    use trunk_core::dsp::cqpsk::Cqpsk;
    use trunk_core::p25::phase2;
    use trunk_core::trunk::tdma::TdmaTracker;
    use trunk_core::trunk::voice::TrackerOut;
    let (iq, rate) = channel(a, a.num("cutoff", 7000.0));
    let hexarg = |k: &str| a.get(k).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    for name in a.get("variant").unwrap_or("base").split(',') {
        let v = variant(name, "p2");
        let mut rx = Cqpsk::new(rate, v.cqpsk);
        let mut fr = phase2::Framer::new();
        let mut tr = TdmaTracker::new(1);
        tr.set_key(hexarg("nac"), hexarg("sysid"), hexarg("wacn"));
        let (mut syms, mut pkts, mut out) = (Vec::new(), Vec::new(), Vec::new());
        let (mut n, mut clean, mut errs) = (0u64, 0u64, 0u64);
        for c in iq.chunks(4096) {
            syms.clear();
            pkts.clear();
            rx.push(c, &mut syms);
            for s in &syms {
                fr.push(s, &mut pkts);
            }
            for p in &pkts {
                out.clear();
                tr.packet(p, p.sample / rate, &mut out);
                for (_, o) in out.drain(..) {
                    if let TrackerOut::Audio(_, f) = o {
                        n += 1;
                        clean += (f.errs <= 1) as u64;
                        errs += f.errs as u64;
                    }
                }
            }
        }
        println!("{name:>16}: {n} codewords, {:.1} % with ≤ 1 error, {:.2} errors each; {} MAC PDUs", 100.0 * clean as f64 / n.max(1) as f64, errs as f64 / n.max(1) as f64, tr.mac_pdus);
    }
}

/// `tool snr … --separation`: the C4FM receiver's level separation at each SNR (for the MSD gate).
pub fn separation(a: &Args) {
    let (iq, rate) = channel(a, a.num("cutoff", dmr::CHANNEL_CUTOFF_HZ));
    let kind = a.get("kind").unwrap_or("dmr");
    let clean = run_dmr(&iq, rate, &variant("base", "dmr"));
    let s = signal_power(&iq, rate, &clean);
    let taps = lowpass(a.num("cutoff", dmr::CHANNEL_CUTOFF_HZ), rate, 63);
    for snr in [30.0, 20.0, 15.0, 12.0, 10.0, 8.0, 6.0, 4.0, 2.0, 0.0, -5.0, -20.0] {
        let x = add_noise(&iq, rate, s, snr, &taps, 7);
        let o = if kind == "dmr" { C4fmOptions::dmr() } else { C4fmOptions::default() };
        let mut rx = C4fm::with_options(rate, C4fmOptions { msd: None, ..o });
        let mut sep = Vec::new();
        let mut out = Vec::new();
        for c in x.chunks(4800) {
            rx.push(c, &mut out);
            if rx.separation().is_finite() {
                sep.push(rx.separation());
            }
        }
        sep.sort_by(f32::total_cmp);
        println!("{snr:6.1} dB: separation median {:.2} (10 % {:.2}, 90 % {:.2})", sep[sep.len() / 2], sep[sep.len() / 10], sep[sep.len() * 9 / 10]);
    }
}
