//! What one watched NXDN carrier costs (`cargo test --release -p trunk-core
//! nxdn::bench -- --ignored --nocapture`): the receiver and framer at a
//! 2.4 MS/s source's channel rate, in the engine's blocks, on noise and on a
//! signal, by receiver setting.

use std::time::Instant;

use num_complex::Complex32;

use super::frame::Framer;
use super::layer3::{CallHead, CALL_CONFERENCE};
use super::synth::{add_noise, dibits, modulate, Tx};
use super::Rate;
use crate::dsp::c4fm::{C4fm, C4fmOptions};
use crate::dsp::Receiver;

const RATE: f64 = 37_500.0;
const BLOCK: usize = 192;

fn signal(secs: f64) -> Vec<Complex32> {
    let head = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 77, destination: 3001 };
    let one = dibits(&Tx::new(Rate::N48, 5, head, 2, 15).frames());
    let mut d = Vec::new();
    while (d.len() as f64) < secs * 2400.0 {
        d.extend_from_slice(&one);
    }
    let mut ph = 0.0;
    let mut iq = modulate(&d, None, Rate::N48, RATE, 0.0, 0.25, &mut ph);
    add_noise(&mut iq, 0.01, &mut 3);
    iq
}

fn noise(secs: f64) -> Vec<Complex32> {
    let mut iq = vec![Complex32::default(); (secs * RATE) as usize];
    add_noise(&mut iq, 0.01, &mut 5);
    iq
}

/// ns per sample, and frames found.
fn run(iq: &[Complex32], opts: C4fmOptions) -> (f64, usize) {
    let mut rx = C4fm::with_options(RATE, opts);
    let mut framer = Framer::new();
    let (mut syms, mut frames, mut found) = (Vec::new(), Vec::new(), 0);
    let t = Instant::now();
    for b in iq.chunks(BLOCK) {
        syms.clear();
        rx.push(b, &mut syms);
        frames.clear();
        for s in &syms {
            framer.push(s, &mut frames);
        }
        found += frames.len();
    }
    (t.elapsed().as_nanos() as f64 / iq.len() as f64, found)
}

#[test]
#[ignore]
fn carrier_cost() {
    let (sig, quiet) = (signal(30.0), noise(30.0));
    for (name, sets) in [("nxdn48 (as now)", ""), ("no MSD", "nomsd"), ("boxcar, no MSD", "nomsd,box"), ("no MSD, block 480", "nomsd,block=480")] {
        let mut o = C4fmOptions::nxdn(2400.0);
        for s in sets.split(',').filter(|s| !s.is_empty()) {
            assert!(o.set(s), "{s}");
        }
        let (ns_sig, f_sig) = run(&sig, o);
        let (ns_noise, f_noise) = run(&quiet, o);
        println!("{name:22} signal {ns_sig:6.0} ns/sample ({f_sig} frames)   noise {ns_noise:6.0} ns/sample ({f_noise} frames)");
    }
}

/// SACCH blocks decoded (CRC good) from `iq` at `rate`.
fn sacch_ok(iq: &[Complex32], rate: f64, opts: C4fmOptions) -> usize {
    let mut rx = C4fm::with_options(rate, opts);
    let mut framer = Framer::new();
    let (mut syms, mut frames, mut ok) = (Vec::new(), Vec::new(), 0);
    for b in iq.chunks(BLOCK) {
        syms.clear();
        rx.push(b, &mut syms);
        frames.clear();
        for s in &syms {
            framer.push(s, &mut frames);
        }
        ok += frames.iter().filter(|f| super::channel::sacch(f).is_some()).count();
    }
    ok
}

/// Sensitivity, before (`minsps=0`) and after: SACCH blocks decoded as noise
/// is added, on the synthetic signal and (NXDN_IQ=file.cu8, 48 kS/s) a real one.
#[test]
#[ignore]
fn sensitivity() {
    let variants: Vec<(&str, C4fmOptions)> = ["minsps=0", ""]
        .iter()
        .map(|v| {
            let mut o = C4fmOptions::nxdn(2400.0);
            if !v.is_empty() {
                assert!(o.set(v));
            }
            (if v.is_empty() { "decimated" } else { "as before" }, o)
        })
        .collect();
    let mut cases: Vec<(String, Vec<Complex32>, f64)> = vec![("synthetic 37.5 kS/s".into(), signal(60.0), RATE)];
    if let Ok(path) = std::env::var("NXDN_IQ") {
        let b = std::fs::read(&path).unwrap();
        let iq: Vec<Complex32> = b.chunks_exact(2).map(|p| Complex32::new((p[0] as f32 - 127.5) / 127.5, (p[1] as f32 - 127.5) / 127.5)).collect();
        cases.push((format!("{path} 48 kS/s"), iq, 48_000.0));
    }
    for (name, clean, rate) in cases {
        let p: f64 = clean.iter().map(|x| x.norm_sqr() as f64).sum::<f64>() / clean.len() as f64;
        println!("{name}: SACCH blocks decoded by added noise (dB below the signal's power)");
        for snr in [20.0, 6.0, 3.0, 1.0, 0.0, -1.0, -2.0, -3.0, -4.0, -6.0] {
            // (Noise over the whole channel bandwidth; three seeds.)
            let mut sum = vec![0; variants.len()];
            for seed in [11, 12, 13] {
                let mut iq = clean.clone();
                add_noise(&mut iq, (p / 10f64.powf(snr / 10.0) / 2.0).sqrt() as f32, &mut { seed });
                let iq = channel_filter(&iq, rate);
                for (k, (_, o)) in variants.iter().enumerate() {
                    sum[k] += sacch_ok(&iq, rate, *o);
                }
            }
            let counts: Vec<String> = variants.iter().zip(&sum).map(|((n, _), c)| format!("{n} {c:5}")).collect();
            println!("  {snr:4} dB   {}", counts.join("   "));
        }
    }
}

/// The channelizer's head for NXDN48 as an RTL-SDR's would be: a 3.4 kHz
/// cutoff with its ~3.2 kHz transition (4097 taps at 2.4 MS/s).
fn channel_filter(iq: &[Complex32], rate: f64) -> Vec<Complex32> {
    let len = ((5.5 / (3200.0 / rate)).ceil() as usize) | 1;
    let h = crate::dsp::filters::blackman_sinc(len, 3400.0 / rate);
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

/// The receiver's separation on pure noise against a signal near the decoding threshold.
#[test]
#[ignore]
fn separation_noise_vs_weak() {
    let clean = signal(20.0);
    let p: f64 = clean.iter().map(|x| x.norm_sqr() as f64).sum::<f64>() / clean.len() as f64;
    let sep = |iq: &[Complex32]| {
        let mut rx = C4fm::with_options(RATE, C4fmOptions::nxdn(2400.0));
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for (i, b) in iq.chunks(BLOCK).enumerate() {
            out.clear();
            rx.push(b, &mut out);
            if i > 400 && i % 40 == 0 {
                seen.push(rx.separation());
            }
        }
        seen.sort_by(f32::total_cmp);
        let n = seen.len();
        (seen[0], seen[n / 2], seen[n - 1])
    };
    for scale in [0.001f32, 0.01, 0.1] {
        let mut q = vec![Complex32::default(); clean.len()];
        add_noise(&mut q, scale, &mut 9);
        println!("noise σ {scale}: separation min/median/max {:?}", sep(&channel_filter(&q, RATE)));
    }
    for snr in [0.0, -3.0, -4.0, -6.0, -8.0] {
        let mut iq = clean.clone();
        add_noise(&mut iq, (p / 10f64.powf(snr / 10.0) / 2.0).sqrt() as f32, &mut 7);
        println!("signal at {snr} dB: separation min/median/max {:?}", sep(&channel_filter(&iq, RATE)));
    }
}
