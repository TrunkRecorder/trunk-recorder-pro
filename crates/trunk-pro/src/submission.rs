//! `trunk-pro tool submission <folder> --out dir`: is a contributed capture
//! what it says it is (research/iq-corpus.md)? Its checks and waterfalls go in
//! `dir`, as counts and levels: the service publishes those, not what's decoded.
//! Exits non-zero if it fails: the service deletes those.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha512};
use trunk_app::config::SampleFormat;

use crate::iq_capture::{self, Recording};
use crate::toolkit::hex;
use crate::{corpus, die, tape_file, waterfall, Args};

/// How much of a recording is at the rails, percent.
fn clipping_pct(r: &Recording) -> Result<f64, String> {
    let bytes = fs::read(&r.data).map_err(|e| format!("{}: {e}", r.data.display()))?;
    let (at, total) = match r.format {
        SampleFormat::Cu8 => (bytes.iter().filter(|b| matches!(**b, 0 | 255)).count(), bytes.len()),
        SampleFormat::Cs16 => (bytes.chunks_exact(2).filter(|c| matches!(i16::from_le_bytes([c[0], c[1]]), i16::MIN | i16::MAX)).count(), bytes.len() / 2),
        SampleFormat::Cf32 => (bytes.chunks_exact(4).filter(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs() >= 1.0).count(), bytes.len() / 4),
    };
    Ok(if total == 0 { 0.0 } else { at as f64 / total as f64 * 100.0 })
}

fn hash_ok(meta_path: &Path, data: &Path) -> Result<bool, String> {
    let meta: Value = serde_json::from_str(&fs::read_to_string(meta_path).map_err(|e| format!("{}: {e}", meta_path.display()))?).map_err(|e| format!("{}: {e}", meta_path.display()))?;
    let Some(want) = meta["global"]["core:sha512"].as_str() else { return Ok(true) };
    let bytes = fs::read(data).map_err(|e| format!("{}: {e}", data.display()))?;
    Ok(want.eq_ignore_ascii_case(&hex(&Sha512::digest(&bytes))))
}

/// What a wideband capture decodes to.
fn decode_wideband(folder: &Path, out: &Path) -> Result<corpus::Results, String> {
    let id = "submission";
    let calls = out.join(id);
    fs::create_dir_all(&calls).map_err(|e| format!("{}: {e}", calls.display()))?;
    let stdout = fs::File::create(out.join(format!("{id}.stdout"))).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = Command::new(exe).arg("replay").arg(folder).arg("--out").arg(&calls).args(["--capture-frames", "--messages"]).stdout(stdout).stderr(Stdio::null()).status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("It wouldn't replay.".into());
    }
    let r = corpus::summarize(out, id)?;
    let _ = fs::remove_dir_all(&calls);
    let _ = fs::remove_file(out.join(format!("{id}.stdout")));
    Ok(r)
}

/// Check the capture in `folder`, leaving the results in `out`. `Err`: why it isn't one.
fn check(folder: &Path, out: &Path) -> Result<Value, String> {
    let name = folder.to_str().ok_or("Its path isn't text")?;
    let opened = iq_capture::open(name)?;
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let (mut radios, mut seconds) = (Vec::new(), 0.0f64);
    for (i, r) in opened.recordings.iter().enumerate() {
        let len = fs::metadata(&r.data).map_err(|e| format!("{}: {e}", r.data.display()))?.len();
        let bps = r.format.bytes_per_sample() as u64;
        if len == 0 || len % bps != 0 {
            return Err(format!("Recording {i}'s size doesn't fit its sample type."));
        }
        if !hash_ok(&r.data.with_extension("sigmf-meta"), &r.data)? {
            return Err(format!("Recording {i} doesn't match its hash."));
        }
        seconds = seconds.max((len / bps) as f64 / r.rate_hz);
        fs::write(out.join(format!("waterfall-{i}.bin")), waterfall::of_recording(r)?.to_bytes()).map_err(|e| e.to_string())?;
        radios.push(json!({ "center_hz": r.center_hz, "rate_hz": r.rate_hz, "clipping_pct": clipping_pct(r)?, "lost_samples": r.gaps.iter().map(|g| g.1).sum::<u64>() }));
    }
    let meta: Value = serde_json::from_str(&fs::read_to_string(opened.recordings[0].data.with_extension("sigmf-meta")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let tier = meta["global"]["trp:tier"].as_str().unwrap_or("wideband").to_string();
    let decoded = if tier == "wideband" {
        if opened.config.is_none() {
            return Err("It doesn't say what systems it's of.".into());
        }
        decode_wideband(folder, out)?
    } else {
        tape_file::run(folder)?
    };
    let (messages, calls): (u64, u64) = (decoded.messages.values().sum(), decoded.calls.len() as u64);
    // (In case it becomes a test: hashes and counts, nothing decoded.)
    fs::write(out.join("expected.jsonl"), corpus::expected_text("submission", env!("CARGO_PKG_VERSION"), &decoded)).map_err(|e| e.to_string())?;
    if messages == 0 && calls == 0 {
        return Err("Nothing could be decoded from it.".into());
    }
    let live = &meta["global"]["trp:live"];
    let live_good: u64 = live["sites"].as_array().map_or(0, |a| a.iter().filter_map(|s| s["messages_good"].as_u64()).sum());
    Ok(json!({
        "ok": true,
        "tier": tier,
        "seconds": seconds,
        "radios": radios,
        "messages": messages,
        "calls": calls,
        // What the capture said it decoded when it was made, for a person to compare.
        "live_messages": live_good,
        "live_calls": live["calls"],
        "trp_version": env!("CARGO_PKG_VERSION"),
    }))
}

pub fn run(a: &Args) {
    let folder = a.positional.get(1).unwrap_or_else(|| die("tool submission <folder> --out dir"));
    let out = a.get("out").unwrap_or_else(|| die("tool submission: --out is needed"));
    let out = Path::new(out);
    fs::create_dir_all(out).unwrap_or_else(|e| die(&format!("{}: {e}", out.display())));
    let result = check(Path::new(folder), out);
    let report = match &result {
        Ok(v) => v.clone(),
        Err(e) => json!({ "ok": false, "why": e }),
    };
    fs::write(out.join("checks.json"), serde_json::to_string_pretty(&report).unwrap_or_default()).unwrap_or_else(|e| die(&format!("{}: {e}", out.display())));
    println!("{report}");
    if let Err(e) = result {
        die(&e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn recording(dir: &Path, name: &str, format: SampleFormat, bytes: &[u8]) -> Recording {
        let data = dir.join(name);
        fs::write(&data, bytes).unwrap();
        Recording { data, format, center_hz: 1e6, rate_hz: 1000.0, auto_tune: false, guard_hz: 0.0, skip: 0, gaps: vec![] }
    }

    /// A folder of the test's own: tests run at the same time.
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trp-submission-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn clipping_counts_samples_at_the_rails() {
        let dir = temp("clipping");
        assert_eq!(clipping_pct(&recording(&dir, "a", SampleFormat::Cu8, &[0, 255, 127, 128])).unwrap(), 50.0);
        let mut s16 = Vec::new();
        for v in [i16::MAX, 0, i16::MIN, 5] {
            s16.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(clipping_pct(&recording(&dir, "b", SampleFormat::Cs16, &s16)).unwrap(), 50.0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn something_that_isnt_a_capture_is_refused() {
        let dir = temp("refused");
        assert!(check(&dir.join("nothing.sigmf-meta"), &dir.join("out")).is_err());
        let meta = dir.join("x.sigmf-meta");
        fs::write(&meta, json!({ "global": { "core:datatype": "cu8", "core:sample_rate": 1000.0, "core:sha512": "00" }, "captures": [{ "core:sample_start": 0, "core:frequency": 1e6 }] }).to_string()).unwrap();
        fs::write(dir.join("x.sigmf-data"), [1u8, 2, 3, 4]).unwrap();
        assert!(check(&meta, &dir.join("out")).unwrap_err().contains("hash"));
        fs::write(dir.join("x.sigmf-data"), [1u8, 2, 3]).unwrap();
        assert!(check(&meta, &dir.join("out")).unwrap_err().contains("size"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
