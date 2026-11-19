//! Channel tapes as SigMF files, and replaying one through its decoder alone
//! (research/iq-corpus.md). What a decoder needs goes in the metadata:
//! `trp:channel` (how it was built) and annotations (what it was told when).
//! No talkgroups: which slots were listened to is enough.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use num_complex::Complex32;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha512};
use trunk_app::config::{Config, NameLists};
use trunk_core::dsp::AnalogTuning;
use trunk_core::p25::diversity::BankConfig;
use trunk_core::trunk::control::{self, ControlChannel};
use trunk_core::trunk::tape::{ControlTape, TapeMark, VoiceTape};
use trunk_core::trunk::voice::{self, TrackerOut, VoiceKind, VoiceParams, VoiceSpec};
use trunk_core::trunk::sites;

use crate::corpus::{envelope_of, frame_hashes, short_hash, CallSummary, Results};
use crate::toolkit::hex;

const COLLECTION: &str = "capture.sigmf-collection";

/// A decoder mark as its SigMF annotation.
fn annotation(m: &TapeMark) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or_default();
    v["core:label"] = "decoder".into();
    v
}

/// As `cf32_le`, what a head put out.
fn iq_bytes(iq: impl Iterator<Item = Complex32>) -> Vec<u8> {
    let mut out = Vec::new();
    for v in iq {
        out.extend_from_slice(&v.re.to_le_bytes());
        out.extend_from_slice(&v.im.to_le_bytes());
    }
    out
}

/// A folder holding one recording, as a capture's would.
fn write_folder(dir: &Path, global: Value, freq_hz: f64, annotations: Vec<Value>, data: &[u8]) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", dir.display());
    fs::create_dir_all(dir).map_err(fail)?;
    let mut g = json!({
        "core:datatype": "cf32_le",
        "core:version": "1.2.0",
        "core:recorder": format!("Trunk Recorder Pro {}", env!("CARGO_PKG_VERSION")),
        "core:sha512": hex(&Sha512::digest(data)),
        "core:collection": "capture",
        "core:extensions": [{ "name": "trp", "version": "0.1.0", "optional": true }],
        "trp:purpose": "healthy",
        "trp:app": { "version": env!("CARGO_PKG_VERSION") },
    });
    if let (Some(a), Some(b)) = (g.as_object_mut(), global.as_object()) {
        a.extend(b.clone());
    }
    let meta = json!({ "global": g, "captures": [{ "core:sample_start": 0, "core:global_index": 0, "core:frequency": freq_hz }], "annotations": annotations });
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    fs::write(dir.join("source-0.sigmf-data"), data).map_err(fail)?;
    fs::write(dir.join("source-0.sigmf-meta"), &text).map_err(fail)?;
    let collection = json!({ "collection": { "core:version": "1.2.0", "core:streams": [{ "name": "source-0", "hash": hex(&Sha512::digest(text.as_bytes())) }] } });
    fs::write(dir.join(COLLECTION), serde_json::to_string_pretty(&collection).map_err(|e| e.to_string())?).map_err(fail)
}

/// `extra`: what else its metadata says (why it was made).
pub fn write_voice(dir: &Path, t: &VoiceTape, extra: Value) -> Result<(), String> {
    let mut global = json!({
        "core:sample_rate": t.spec.rate,
        "core:description": format!("{} voice channel", t.spec.kind.name()),
        "trp:tier": "voice",
        "trp:channel": t,
    });
    merge(&mut global, extra);
    write_folder(dir, global, t.freq_hz, t.marks.iter().map(annotation).collect(), &iq_bytes(t.iq.iter().copied()))
}

/// `config`: the systems it's of, as a capture's `trp:config`, its sources aside.
pub fn write_control(dir: &Path, t: &ControlTape, freq_hz: f64, config: &Value, bandplan: &str, extra: Value) -> Result<(), String> {
    let mut config = config.clone();
    config["sources"] = json!([{ "type": "file", "path": "source-0.sigmf-data", "format": "cf32", "realtime": false, "centerHz": freq_hz, "rateHz": t.rate, "name": "radio-0" }]);
    let mut global = json!({
        "core:sample_rate": t.rate,
        "core:description": "Control channel",
        "trp:tier": "control",
        "trp:channel": t,
        "trp:config": config,
    });
    global["trp:channel"]["bandplan"] = bandplan.into();
    merge(&mut global, extra);
    write_folder(dir, global, freq_hz, vec![], &iq_bytes(t.iq.iter().copied()))
}

fn merge(a: &mut Value, b: Value) {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
}

fn read(dir: &Path) -> Result<(Value, Vec<Complex32>), String> {
    let path = dir.join("source-0.sigmf-meta");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let path = dir.join("source-0.sigmf-data");
    let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let iq = data.chunks_exact(8).map(|c| Complex32::new(f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect();
    Ok((meta, iq))
}

fn slot_hash(slot: usize) -> String {
    short_hash(format!("slot {slot}").as_bytes(), 8)
}

/// A voice tape through its decoder, as live: pushed as it was, told what it was when it was.
fn run_voice(meta: &Value, iq: &[Complex32], freq_hz: u64) -> Result<Results, String> {
    let VoiceTape { spec, first, block, .. } = VoiceTape::deserialize(&meta["global"]["trp:channel"]).map_err(|e| format!("This tape's trp:channel can't be read: {e}"))?;
    let marks = meta["annotations"].as_array().ok_or("No annotations")?.iter().filter_map(|a| TapeMark::deserialize(a).ok()).collect::<Vec<_>>();
    let mut d = voice::build(&spec);
    let mut out = Vec::new();
    let (mut pos, mut m) = (0, 0);
    while pos < iq.len() {
        while marks.get(m).is_some_and(|x| x.sample as usize <= pos) {
            d.set_params(&marks[m].params);
            // (Which talkgroup doesn't matter to a decoder alone: only whether it's listening.)
            d.listen(marks[m].slots.map(|on| on.then_some(0)));
            d.set_squelch(marks[m].squelch);
            m += 1;
        }
        let mut n = if pos == 0 && first > 0 { first } else if block > 0 { block } else { iq.len() };
        n = n.min(iq.len() - pos);
        if let Some(next) = marks.get(m) {
            n = n.min(next.sample as usize - pos);
        }
        d.push(&iq[pos..pos + n], &mut out);
        pos += n;
    }
    d.flush(&mut out);
    let mut frames: BTreeMap<usize, Vec<_>> = BTreeMap::new();
    let mut audio: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for v in out {
        match v.out {
            TrackerOut::Audio(_, f) => frames.entry(v.slot as usize).or_default().push(f),
            TrackerOut::AnalogAudio(a) => audio.entry(v.slot as usize).or_default().extend(a.iter().map(|s| *s as f64)),
            _ => {}
        }
    }
    let mut calls = Vec::new();
    for (slot, f) in frames {
        calls.push(CallSummary { c: slot_hash(slot), f: freq_hz, frames: f.len() as u64, bad: f.iter().filter(|x| x.bad()).count() as u64, errs: f.iter().map(|x| x.errs as u64).sum(), h: frame_hashes(&f), ..Default::default() });
    }
    for (slot, a) in audio {
        calls.push(CallSummary { c: slot_hash(slot), f: freq_hz, env: envelope_of(&a, 8000), ..Default::default() });
    }
    Ok(Results { calls, ..Default::default() })
}

/// A control tape through its protocol's decoder.
fn run_control(meta: &Value, iq: &[Complex32]) -> Result<Results, String> {
    let g = &meta["global"];
    let c = &g["trp:channel"];
    let cfg = Config::from_value(g["trp:config"].clone()).map_err(|e| format!("Its trp:config: {e}"))?;
    let engine = cfg.engine_config(0.0, &NameLists::new());
    let tape = ControlTape::deserialize(c).map_err(|e| format!("This tape's trp:channel can't be read: {e}"))?;
    let index = tape.site as usize;
    let (_, site) = sites(&engine.systems).nth(index).ok_or("Its site isn't in its trp:config")?;
    let rate = g["core:sample_rate"].as_f64().ok_or("No sample rate")?;
    let mut d: Box<dyn ControlChannel> = control::build(site, rate);
    d.restart(rate);
    if let Some(b) = c["bandplan"].as_str().filter(|b| !b.is_empty()) {
        d.load_bandplan(b);
    }
    let (block, carrier, t0) = (tape.block.max(1), tape.carrier, tape.t0);
    let mut steps = Vec::new();
    for (i, chunk) in iq.chunks(block).enumerate() {
        d.push(carrier, chunk, t0 + (i * block) as f64 / rate, rate, &mut steps);
    }
    d.flush(&mut steps);
    let mut r = Results::default();
    for m in steps.iter().flat_map(|s| s.msgs.iter()) {
        r.add_message(&format!("{} {}", m.kind.as_str(), m.meta), m.kind.as_str());
    }
    Ok(r)
}

/// What a channel tape's decoder makes of it.
pub fn run(dir: &Path) -> Result<Results, String> {
    let (meta, iq) = read(dir)?;
    let freq = meta["captures"][0]["core:frequency"].as_f64().unwrap_or(0.0) as u64;
    match meta["global"]["trp:tier"].as_str() {
        Some("voice") => run_voice(&meta, &iq, freq),
        Some("control") => run_control(&meta, &iq),
        other => Err(format!("{}: not a channel tape (tier {})", dir.display(), other.unwrap_or("none"))),
    }
}

/// How close two tapes of a channel are, dB of the signal over their
/// difference: what a channelizer change did to that channel's samples.
fn difference_db(a: &Path, b: &Path) -> Result<f64, String> {
    let ((_, x), (_, y)) = (read(a)?, read(b)?);
    let n = x.len().min(y.len());
    if n == 0 {
        return Err("A tape is empty".into());
    }
    let signal: f64 = x[..n].iter().map(|v| v.norm_sqr() as f64).sum();
    let diff: f64 = x[..n].iter().zip(&y[..n]).map(|(p, q)| (p - q).norm_sqr() as f64).sum();
    Ok(10.0 * (signal.max(1e-30) / diff.max(1e-30)).log10())
}

/// `tool channel <tape folder>`: what its decoder makes of it, as counts and hashes.
pub fn cli(a: &crate::Args) {
    let dir = a.positional.get(1).unwrap_or_else(|| crate::die("tool channel <tape folder>"));
    if let Some(other) = a.get("against") {
        let db = difference_db(Path::new(dir), Path::new(other)).unwrap_or_else(|e| crate::die(&e));
        println!("{db:.1} dB between them");
        return;
    }
    let r = run(Path::new(dir)).unwrap_or_else(|e| crate::die(&e));
    println!("{} control messages", r.messages.values().sum::<u64>());
    for c in &r.calls {
        println!("{}", serde_json::to_string(c).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_core::mbe::Profile;

    #[test]
    fn a_decoders_settings_read_back_the_same() {
        let spec = VoiceSpec { kind: VoiceKind::Tdma, rate: 37500.0, t0: 1.5, seed: 77, vocoder: Profile::Enhanced, bank: BankConfig { soft: false, ..Default::default() }, squelch: 0.25, subaudible: true, analog: AnalogTuning { data_mute: true, ..Default::default() }, ignore_hz: vec![(1.0, 2.0)], keys: None };
        let v = serde_json::to_value(&spec).unwrap();
        let back = VoiceSpec::deserialize(&v).unwrap();
        assert_eq!(format!("{spec:?}"), format!("{back:?}"));
        // As tapes were written before their types wrote themselves, so those still replay.
        let (b, a) = (&spec.bank, &spec.analog);
        let before = json!({
            "kind": "p25_phase2", "rate": 37500.0, "t0_s": 1.5, "seed": 77, "vocoder": "enhanced",
            "bank": { "cqpsk": b.cqpsk, "cqpsk_eq": b.cqpsk_eq, "c4fm": b.c4fm, "eq_taps": b.eq_taps, "eq_mu": b.eq_mu, "soft": false, "detect": b.detect, "flywheel": b.framer.flywheel, "flywheel_errs": b.framer.flywheel_errs, "nid_recover": b.framer.nid_recover, "nid_max_errs": b.framer.nid_max_errs },
            "squelch": 0.25, "subaudible": true,
            "analog": { "half_bw_hz": a.half_bw_hz, "deemph_tau_s": a.deemph_tau_s, "level_tau_s": a.level_tau_s, "low_cut_hz": a.low_cut_hz, "open_s": a.squelch.open_s, "hang_s": a.squelch.hang_s, "data_mute": true },
            "ignore_hz": [[1.0, 2.0]],
        });
        assert_eq!(v, before);
        for k in VoiceKind::ALL {
            assert_eq!(serde_json::from_value::<VoiceKind>(serde_json::to_value(k).unwrap()).unwrap(), k);
        }
        assert!(serde_json::from_value::<VoiceKind>(json!("p26")).unwrap_err().to_string().contains("`p26` isn't a voice kind"));
        let mark = TapeMark { sample: 4800, params: VoiceParams { tdma_key: Some((1, 2, 3)), nac: Some(0x293) }, slots: [true, false], squelch: 0.5 };
        let ann = annotation(&mark);
        assert_eq!(ann, json!({ "core:sample_start": 4800, "core:label": "decoder", "trp:voice_params": { "tdma_key": [1, 2, 3], "nac": 0x293 }, "trp:slots": [true, false], "trp:squelch": 0.5 }));
        assert_eq!(TapeMark::deserialize(&ann).unwrap(), mark);
    }

    #[test]
    fn marks_read_back_and_hold_no_talkgroup() {
        let m = TapeMark { sample: 9, params: VoiceParams { tdma_key: Some((1, 2, 3)), nac: Some(4) }, slots: [true, false], squelch: 0.5 };
        let v = annotation(&m);
        assert_eq!(TapeMark::deserialize(&v).unwrap(), m);
        assert!(!v.to_string().contains("talkgroup"));
    }
}
