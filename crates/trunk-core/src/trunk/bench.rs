//! What each decoder costs (`cargo test --release -p trunk-core
//! trunk::bench -- --ignored --nocapture`): every voice decoder and control
//! channel as the engine builds them, at a 2.4 MS/s source's channel rate
//! (37.5 kS/s), in the engine's blocks, on a clean synthetic signal, a weak
//! one and noise — behind an RTL-SDR's channel filter, as the channelizer
//! gives them. ns per channel sample, and what was decoded.

use std::f64::consts::PI;
use std::time::Instant;

use num_complex::Complex32;

use super::control::{self, Protocol};
use super::voice::{self, TrackerOut, VoiceKind, VoiceSpec};
use super::{SmartnetConfig, SystemConfig};
use crate::dsp::filters::{blackman_sinc, rrc_taps};
use crate::p25::diversity::BankConfig;

const RATE: f64 = 37_500.0;
const BLOCK: usize = 192;
const SECS: f64 = 20.0;

fn noise(iq: &mut [Complex32], sigma: f32, seed: u64) {
    crate::nxdn::synth::add_noise(iq, sigma, &mut { seed });
}

/// Behind the channelizer's head: `cutoff` Hz, an RTL-SDR's ~3.2 kHz transition.
fn channel(iq: &[Complex32], cutoff: f64) -> Vec<Complex32> {
    let len = ((5.5 / (3200.0 / RATE)).ceil() as usize) | 1;
    let h = blackman_sinc(len, cutoff / RATE);
    let sum: f64 = h.iter().sum();
    let h: Vec<f32> = h.iter().map(|v| (v / sum) as f32).collect();
    let half = len / 2;
    (0..iq.len())
        .map(|i| {
            let mut acc = Complex32::default();
            for (k, &t) in h.iter().enumerate() {
                if let Some(x) = (i + k).checked_sub(half).and_then(|j| iq.get(j)) {
                    acc += x * t;
                }
            }
            acc
        })
        .collect()
}

/// Repeat `d` to `secs` of symbols at `baud`.
fn fill(d: &[u8], baud: f64) -> Vec<u8> {
    let mut out = Vec::new();
    while (out.len() as f64) < SECS * baud {
        out.extend_from_slice(d);
    }
    out
}

/// π/4-DQPSK at `baud`, root-raised-cosine (α 0.2): P25 CQPSK (4800) and Phase 2 H-DQPSK (6000).
fn dqpsk(dibits: &[u8], baud: f64) -> Vec<Complex32> {
    let sps = RATE / baud;
    let taps = rrc_taps(0.2, sps, (8.0 * sps).round() as usize | 1);
    let n = (dibits.len() as f64 * sps) as usize;
    let mut ph = 0.0f64;
    let mut x = vec![Complex32::default(); n + taps.len()];
    for (k, &d) in dibits.iter().enumerate() {
        ph += PI / 4.0 * match d {
            0b01 => 3.0,
            0b00 => 1.0,
            0b10 => -1.0,
            _ => -3.0,
        };
        let at = k as f64 * sps;
        let (i0, frac) = (at.floor() as usize, at - at.floor());
        // (Nearest sample: fine for a cost measurement.)
        let i = i0 + (frac >= 0.5) as usize;
        if i < n {
            x[i] += Complex32::from_polar(1.0, ph as f32);
        }
    }
    let half = taps.len() / 2;
    (0..n)
        .map(|i| {
            let mut acc = Complex32::default();
            for (k, &t) in taps.iter().enumerate() {
                if let Some(v) = (i + k).checked_sub(half).and_then(|j| x.get(j)) {
                    acc += v * t;
                }
            }
            acc * 0.3
        })
        .collect()
}

fn p25_frames() -> Vec<u8> {
    use crate::p25::synth::{dibits, imbe, Tx};
    dibits(&Tx { nac: 0x293, talkgroup: 1201, source: 1_234_567, cipher: None, voice: (0..18 * 4).map(imbe).collect() }.frames())
}

fn p25_c4fm() -> Vec<Complex32> {
    let mut ph = 0.0;
    crate::p25::synth::modulate(&fill(&p25_frames(), 4800.0), RATE, 0.0, 0.3, &mut ph)
}

fn p25_cqpsk() -> Vec<Complex32> {
    dqpsk(&fill(&p25_frames(), 4800.0), 4800.0)
}

fn p2_hdqpsk() -> Vec<Complex32> {
    use crate::p25::synth::{ambe, Tx2};
    let tx = Tx2 { nac: 0x293, sys_id: 0x445, wacn: 0xbee00, cipher: None, voice: (0..18 * 4).map(ambe).collect() };
    let d: Vec<u8> = tx.packets().iter().flat_map(|(p, _)| p.dibits).collect();
    dqpsk(&fill(&d, 6000.0), 6000.0)
}

fn dmr() -> Vec<Complex32> {
    use crate::dmr::synth::{dibits, modulate, Tx};
    let tx = Tx { cc: 1, slot: 0, talkgroup: 101, source: 1234, superframes: 8, idle_before: 2, idle_after: 2, csbks: Vec::new(), mobile: false };
    let mut ph = 0.0;
    modulate(&fill(&dibits(&tx), 4800.0), RATE, 0.0, 0.3, &mut ph)
}

fn nxdn48() -> Vec<Complex32> {
    use crate::nxdn::layer3::{CallHead, CALL_CONFERENCE};
    use crate::nxdn::synth::{dibits, modulate, Tx};
    use crate::nxdn::Rate;
    let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 77, destination: 3001 };
    let mut ph = 0.0;
    modulate(&fill(&dibits(&Tx::new(Rate::N48, 5, head, 2, 15).frames()), 2400.0), None, Rate::N48, RATE, 0.0, 0.3, &mut ph)
}

/// SmartNet: 2FSK OSWs at 3600 baud, ±2400 Hz.
fn smartnet() -> Vec<Complex32> {
    use crate::smartnet::osw::{encode, Osw};
    let bits: Vec<u8> = (0..400u32).flat_map(|k| encode(Osw { addr: (0x1000 + k) as u16, grp: k % 3 == 0, cmd: (k % 0x300) as u16 })).collect();
    let bits = fill(&bits, 3600.0);
    let n = (bits.len() as f64 / 3600.0 * RATE) as usize;
    let mut ph = 0.0f64;
    (0..n)
        .map(|i| {
            let b = bits[((i as f64 / RATE * 3600.0) as usize).min(bits.len() - 1)];
            ph += 2.0 * PI * if b == 1 { 2400.0 } else { -2400.0 } / RATE;
            Complex32::from_polar(0.3, ph as f32)
        })
        .collect()
}

/// Analog FM voice: a 1 kHz tone, 2.5 kHz deviation.
fn analog() -> Vec<Complex32> {
    let mut ph = 0.0f64;
    (0..(SECS * RATE) as usize)
        .map(|i| {
            ph += 2.0 * PI * 2500.0 * (2.0 * PI * 1000.0 * i as f64 / RATE).sin() / RATE;
            Complex32::from_polar(0.3, ph as f32)
        })
        .collect()
}

/// The clean signal, a weak one (noise `weak_db` below it across the
/// channel) and noise alone, each behind the channel filter.
fn inputs(clean: Vec<Complex32>, cutoff: f64, weak_db: f64) -> [(&'static str, Vec<Complex32>); 3] {
    let p: f64 = clean.iter().map(|x| x.norm_sqr() as f64).sum::<f64>() / clean.len() as f64;
    let sigma = (p / 10f64.powf(weak_db / 10.0) / 2.0).sqrt() as f32;
    let mut c = clean.clone();
    noise(&mut c, 0.002, 1);
    let mut w = clean.clone();
    noise(&mut w, sigma, 2);
    let mut q = vec![Complex32::default(); clean.len()];
    noise(&mut q, sigma, 3);
    [("clean", channel(&c, cutoff)), ("weak", channel(&w, cutoff)), ("noise", channel(&q, cutoff))]
}

fn voice_cost(kind: VoiceKind, bank: BankConfig, iq: &[Complex32]) -> (f64, usize) {
    let mut v = voice::build(&VoiceSpec { kind, rate: RATE, t0: 0.0, seed: 1, vocoder: crate::mbe::Profile::Fixed, bank, squelch: 1e-4, subaudible: false, analog: Default::default(), ignore_hz: Vec::new(), keys: None });
    v.listen([Some(1201), Some(1201)]);
    let mut out = Vec::new();
    let mut audio = 0;
    let t = Instant::now();
    for b in iq.chunks(BLOCK) {
        out.clear();
        v.push(b, &mut out);
        audio += out.iter().filter(|o| matches!(o.out, TrackerOut::Audio(..) | TrackerOut::AnalogAudio(_))).count();
    }
    (t.elapsed().as_nanos() as f64 / iq.len() as f64, audio)
}

fn control_cost(sys: &SystemConfig, iq: &[Complex32]) -> (f64, usize) {
    let mut cc = control::build(sys, RATE);
    cc.restart(RATE);
    let mut steps = Vec::new();
    let t = Instant::now();
    for (k, b) in iq.chunks(BLOCK).enumerate() {
        cc.push(0, b, k as f64 * BLOCK as f64 / RATE, RATE, &mut steps);
    }
    let ns = t.elapsed().as_nanos() as f64 / iq.len() as f64;
    let (good, _) = cc.counts();
    (ns, good as usize)
}

#[test]
#[ignore]
fn decoder_costs() {
    let p25_auto = BankConfig::default();
    let p25_qpsk = BankConfig { c4fm: false, ..Default::default() };
    let p25_fsk4 = BankConfig { cqpsk: false, cqpsk_eq: false, ..Default::default() };
    println!("ns per channel sample at {RATE} S/s (×37 500 = ns per second: 1 % of a core = 267 ns/sample)");
    println!("{:34} {:>16} {:>16} {:>16}", "voice", "clean", "weak", "noise");
    let voice_cases: Vec<(&str, VoiceKind, BankConfig, Vec<Complex32>, f64, f64)> = vec![
        ("P25 P1 C4FM, bank auto", VoiceKind::Fdma, p25_auto, p25_c4fm(), 7000.0, 6.0),
        ("P25 P1 C4FM, bank fsk4", VoiceKind::Fdma, p25_fsk4, p25_c4fm(), 7000.0, 6.0),
        ("P25 P1 CQPSK, bank auto", VoiceKind::Fdma, p25_auto, p25_cqpsk(), 7000.0, 6.0),
        ("P25 P1 CQPSK, bank qpsk", VoiceKind::Fdma, p25_qpsk, p25_cqpsk(), 7000.0, 6.0),
        ("P25 P2 H-DQPSK", VoiceKind::Tdma, p25_auto, p2_hdqpsk(), 7000.0, 6.0),
        ("DMR", VoiceKind::Dmr, p25_auto, dmr(), crate::dmr::CHANNEL_CUTOFF_HZ, 6.0),
        ("NXDN48", VoiceKind::Nxdn(crate::nxdn::Rate::N48), p25_auto, nxdn48(), 3400.0, 3.0),
        ("analog FM", VoiceKind::Analog, p25_auto, analog(), 7000.0, 6.0),
    ];
    for (name, kind, bank, clean, cutoff, weak_db) in voice_cases {
        let r: Vec<String> = inputs(clean, cutoff, weak_db).iter().map(|(_, iq)| voice_cost(kind, bank, iq)).map(|(ns, n)| format!("{ns:6.0} ({n:5})")).collect();
        println!("{name:34} {}", r.iter().map(|s| format!("{s:>16}")).collect::<Vec<_>>().join(" "));
    }
    println!("{:34} {:>16} {:>16} {:>16}", "control (good blocks)", "clean", "weak", "noise");
    let sys = |protocol: Protocol, bank: BankConfig| SystemConfig { control_channels: vec![851e6], protocol, bank, ..Default::default() };
    let bandplan = crate::smartnet::Bandplan::from_config("400_custom", 459_000_000.0, 10_000.0, 380, 461_000_000.0).unwrap();
    let control_cases: Vec<(&str, SystemConfig, Vec<Complex32>, f64, f64)> = vec![
        ("P25 C4FM, bank auto", sys(Protocol::P25, p25_auto), p25_c4fm(), 7000.0, 6.0),
        ("P25 CQPSK, bank auto", sys(Protocol::P25, p25_auto), p25_cqpsk(), 7000.0, 6.0),
        ("P25 CQPSK, bank qpsk", sys(Protocol::P25, p25_qpsk), p25_cqpsk(), 7000.0, 6.0),
        ("SmartNet", sys(Protocol::SmartNet(SmartnetConfig { bandplan, analog_default: false }), p25_auto), smartnet(), crate::smartnet::CHANNEL_CUTOFF_HZ, 6.0),
        ("DMR (one carrier)", sys(Protocol::Dmr(Default::default()), p25_auto), dmr(), crate::dmr::CHANNEL_CUTOFF_HZ, 6.0),
        (
            "NXDN48 Type-D (one carrier)",
            sys(Protocol::Nxdn(crate::nxdn::trunking::NxdnConfig { kind: crate::nxdn::trunking::Kind::TypeD, ..Default::default() }), p25_auto),
            nxdn48(),
            3400.0,
            3.0,
        ),
    ];
    for (name, sys, clean, cutoff, weak_db) in control_cases {
        let r: Vec<String> = inputs(clean, cutoff, weak_db).iter().map(|(_, iq)| control_cost(&sys, iq)).map(|(ns, n)| format!("{ns:6.0} ({n:5})")).collect();
        println!("{name:34} {}", r.iter().map(|s| format!("{s:>16}")).collect::<Vec<_>>().join(" "));
    }
}

/// The 4FSK receivers alone, by setting: where their time goes.
#[test]
#[ignore]
fn c4fm_parts() {
    use crate::dsp::c4fm::{C4fm, C4fmOptions};
    use crate::dsp::Receiver;
    let run = |iq: &[Complex32], o: C4fmOptions| {
        let mut rx = C4fm::with_options(RATE, o);
        let mut out = Vec::new();
        let t = Instant::now();
        for b in iq.chunks(BLOCK) {
            out.clear();
            rx.push(b, &mut out);
        }
        (t.elapsed().as_nanos() as f64 / iq.len() as f64, rx.separation())
    };
    for (name, base, clean, cutoff, weak) in [("P25", C4fmOptions::default(), p25_c4fm(), 7000.0, 6.0), ("DMR", C4fmOptions::dmr(), dmr(), crate::dmr::CHANNEL_CUTOFF_HZ, 6.0)] {
        let ins = inputs(clean, cutoff, weak);
        for v in ["", "nomsd", "nomsd,box"] {
            let mut o = base;
            for s in v.split(',').filter(|s| !s.is_empty()) {
                assert!(o.set(s));
            }
            let r: Vec<String> = ins.iter().map(|(n, iq)| {
                let (ns, sep) = run(iq, o);
                format!("{n} {ns:5.0} ns (sep {sep:4.1})")
            }).collect();
            println!("{name} {v:10} {}", r.join("   "));
        }
    }
}

/// A long run of P25's 4FSK receiver on a weak signal, to profile (`sample`).
#[test]
#[ignore]
fn c4fm_profile() {
    use crate::dsp::c4fm::{C4fm, C4fmOptions};
    use crate::dsp::Receiver;
    let [_, (_, weak), _] = inputs(p25_c4fm(), 7000.0, 6.0);
    let mut rx = C4fm::with_options(RATE, C4fmOptions::default());
    let mut out = Vec::new();
    let t = Instant::now();
    while t.elapsed().as_secs() < 12 {
        for b in weak.chunks(BLOCK) {
            out.clear();
            rx.push(b, &mut out);
        }
    }
}
