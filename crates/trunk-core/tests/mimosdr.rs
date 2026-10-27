//! Our `.sdr` files through MimoSDR's own player (sdr-decode): a call must
//! play there as it does here. The vocoders differ, so the test is the
//! loudness envelope (each 20 ms frame's RMS) tracking ours: frames out of
//! order, or bits misread, and it doesn't.
//!
//! `TRP_SDR_DIR=<dir> cargo test -p trunk-core --test mimosdr -- --nocapture`
//! also checks every `.sdr` under `dir` (a `captures/regress.sh` output)
//! against the WAV recorded beside it.

use trunk_core::mbe::{self, Kind, FRAME_SAMPLES};
use trunk_core::trunk::frames::{Codec, VoiceFrame};
use trunk_core::trunk::sdr::{call_sdr, sdr_frames, SdrInfo, SdrMode};

fn mimosdr(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let call = sdr_decode::Call::parse(bytes).map_err(|e| format!("{e:?}"))?;
    let opts = sdr_decode::Options { normalize: false, ..Default::default() };
    let r = call.render(&sdr_decode::Keys::none(), &opts).map_err(|e| format!("{e:?}"))?;
    Ok(r.pcm.iter().map(|&s| s as f32).collect())
}

/// Our vocoder over the file's frames, as `tool revoice` plays them.
fn ours(bytes: &[u8]) -> Vec<f32> {
    let mut dec = mbe::Decoder::new(mbe::lcg(1), mbe::Profile::Fixed);
    let mut out = Vec::new();
    for f in sdr_frames(bytes) {
        let mut buf = [0f32; FRAME_SAMPLES];
        match f.codec {
            Codec::Imbe => dec.imbe(&f.bits[..].try_into().unwrap(), f.e0, if f.erased { 0 } else { f.errs }, f.erased, &mut buf),
            Codec::Ambe => dec.ambe(&f.bits[..].try_into().unwrap(), f.errs, &mut buf),
        };
        out.extend_from_slice(&buf);
    }
    out
}

/// Pearson r of the two signals' per-frame RMS, over the shorter one.
fn envelope_r(a: &[f32], b: &[f32]) -> f64 {
    let env = |x: &[f32]| -> Vec<f64> {
        x.chunks_exact(FRAME_SAMPLES).map(|c| (c.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt()).collect()
    };
    let (ea, eb) = (env(a), env(b));
    let n = ea.len().min(eb.len());
    let (ea, eb) = (&ea[..n], &eb[..n]);
    let mean = |x: &[f64]| x.iter().sum::<f64>() / n as f64;
    let (ma, mb) = (mean(ea), mean(eb));
    let cov: f64 = ea.iter().zip(eb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = ea.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = eb.iter().map(|y| (y - mb).powi(2)).sum();
    cov / (va * vb).sqrt()
}

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
    *seed >> 8
}

/// Random frames: every frame a new pitch and gain, so the envelope jumps
/// frame to frame and only a reader in step with ours can follow it.
fn frames(codec: Codec, n: usize) -> Vec<VoiceFrame> {
    let mut seed = 17;
    let nbits = if codec == Codec::Imbe { 88 } else { 49 };
    (0..n)
        .map(|_| VoiceFrame {
            codec,
            bits: (0..nbits).map(|_| (lcg(&mut seed) & 1) as u8).collect(),
            e0: 0,
            errs: 0,
            erased: false,
            kind: Kind::Voice,
            tone: None,
        })
        .collect()
}

fn info() -> SdrInfo {
    SdrInfo { start_us: 1_790_000_000_000_000, duration_ms: 8_000, hz: 851_012_500, nac: 0x293, tg: 101, first_src: 1234, src: 1234, slot: 1, ..Default::default() }
}

#[test]
fn our_calls_play_in_mimosdr() {
    for (mode, codec) in [(SdrMode::P25Fdma, Codec::Imbe), (SdrMode::P25Tdma, Codec::Ambe), (SdrMode::Dmr, Codec::Ambe)] {
        let bytes = call_sdr(mode, &frames(codec, 360), &info()).unwrap();
        let (theirs, ours) = (mimosdr(&bytes).unwrap(), ours(&bytes));
        assert_eq!(theirs.len(), ours.len(), "{mode:?}: length");
        let r = envelope_r(&theirs, &ours);
        assert!(r > 0.8, "{mode:?}: MimoSDR's envelope r = {r:.2} against ours");
    }
}

/// Every `.sdr` under `TRP_SDR_DIR` renders in MimoSDR, and per mode its
/// envelope tracks the WAV we recorded (the median call's r: single calls
/// differ where the two vocoders' concealment and muting do).
#[test]
fn recorded_calls_play_in_mimosdr() {
    let Ok(dir) = std::env::var("TRP_SDR_DIR") else { return };
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(&dir)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "sdr") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no .sdr files under {dir}");
    let mut by_mode = std::collections::BTreeMap::<&str, Vec<f64>>::new();
    let mut failed = Vec::new();
    for p in &files {
        let bytes = std::fs::read(p).unwrap();
        let kind = sdr_decode::Call::parse(&bytes).map(|c| c.kind().name()).unwrap_or("?");
        let theirs = match mimosdr(&bytes) {
            Ok(t) => t,
            Err(e) => {
                failed.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        let Ok(wav) = std::fs::read(p.with_extension("wav")) else { continue };
        let (rate, ours) = trunk_core::wav::decode(&wav).unwrap();
        assert_eq!(rate, 8000, "{}", p.display());
        let r = envelope_r(&theirs, &ours);
        println!("{kind:9} r={r:5.2} {}", p.display());
        by_mode.entry(kind).or_default().push(if r.is_nan() { 0.0 } else { r });
    }
    assert!(failed.is_empty(), "MimoSDR couldn't render:\n{}", failed.join("\n"));
    for (kind, mut rs) in by_mode {
        rs.sort_by(f64::total_cmp);
        let median = rs[rs.len() / 2];
        println!("{kind}: {} calls, median r {median:.2}", rs.len());
        assert!(median > 0.8, "{kind}: median envelope r {median:.2} over {} calls", rs.len());
    }
}
