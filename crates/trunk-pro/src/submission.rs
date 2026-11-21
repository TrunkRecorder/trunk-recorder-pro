//! `trunk-pro tool submission <folder> --out dir`: is a contributed capture
//! what it says it is (research/iq-corpus.md)? Its checks and waterfalls go in
//! `dir`, as counts and levels: the service publishes those, not what's decoded.
//! Exits non-zero if it fails (it isn't a radio's IQ, or doesn't match its
//! hash): the service deletes those. What it decodes to, nothing included, is
//! counted, not judged.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha512};
use trunk_app::config::SampleFormat;

use crate::corpus::ReplaySummary;
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

/// What a wideband capture decodes to, and its replay's summary.
fn decode_wideband(folder: &Path, out: &Path) -> Result<(corpus::Results, ReplaySummary), String> {
    let id = "submission";
    let calls = out.join(id);
    fs::create_dir_all(&calls).map_err(|e| format!("{}: {e}", calls.display()))?;
    let stdout = fs::File::create(out.join(format!("{id}.stdout"))).map_err(|e| e.to_string())?;
    let summary = out.join(format!("{id}.summary.json"));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = Command::new(exe).arg("replay").arg(folder).arg("--out").arg(&calls).args(["--capture-frames", "--messages"]).arg("--summary").arg(&summary).stdout(stdout).stderr(Stdio::null()).status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("It wouldn't replay.".into());
    }
    let r = corpus::summarize(out, id)?;
    let s = fs::read_to_string(&summary).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())).map_err(|e| format!("Its replay's summary: {e}"))?;
    let _ = fs::remove_dir_all(&calls);
    let _ = fs::remove_file(out.join(format!("{id}.stdout")));
    let _ = fs::remove_file(&summary);
    Ok((r, s))
}

/// Each site's control channel blocks as live decoded them and as the
/// replay did, over the same part of the capture: the replay's over the
/// window live counted, when the capture says which (else all of it).
fn compare_sites(live: &Value, replay: &ReplaySummary) -> Vec<Value> {
    let replayed = replay.live_window.as_ref().map_or(&replay.sites, |w| &w.sites);
    let mut rows: Vec<Value> = live["sites"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            let (system, site) = (l["system"].as_str().unwrap_or(""), l["site"].as_str().unwrap_or(""));
            let r = replayed.iter().find(|r| r.system == system && r.site == site);
            json!({ "system": system, "site": site, "live_good": l["messages_good"], "live_bad": l["messages_bad"], "replay_good": r.map(|r| r.good), "replay_bad": r.map(|r| r.bad) })
        })
        .collect();
    // (A site the replay found that live didn't run.)
    let unlived: Vec<Value> = replayed.iter().filter(|r| !rows.iter().any(|x| x["system"] == r.system.as_str() && x["site"] == r.site.as_str())).map(|r| json!({ "system": r.system, "site": r.site, "live_good": null, "live_bad": null, "replay_good": r.good, "replay_bad": r.bad })).collect();
    rows.extend(unlived);
    rows
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
        let (image, shape) = waterfall::of_recording(r)?;
        // It must be a radio's IQ; what it decodes to doesn't matter (one that
        // decodes nothing is what a new decoder, or a debugged failure, needs).
        shape.check().map_err(|why| format!("Recording {i} isn't IQ: {why}"))?;
        fs::write(out.join(format!("waterfall-{i}.bin")), image.to_bytes()).map_err(|e| e.to_string())?;
        radios.push(json!({ "center_hz": r.center_hz, "rate_hz": r.rate_hz, "clipping_pct": clipping_pct(r)?, "lost_samples": r.gaps.iter().map(|g| g.1).sum::<u64>(), "spectrum_spread_db": (shape.spread_db * 10.0).round() / 10.0 }));
    }
    let meta: Value = serde_json::from_str(&fs::read_to_string(opened.recordings[0].data.with_extension("sigmf-meta")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let tier = meta["global"]["trp:tier"].as_str().unwrap_or("wideband").to_string();
    let (decoded, replay) = if tier == "wideband" {
        if opened.config.is_none() {
            return Err("It doesn't say what systems it's of.".into());
        }
        let (d, s) = decode_wideband(folder, out)?;
        (d, Some(s))
    } else {
        (tape_file::run(folder)?, None)
    };
    let (messages, calls): (u64, u64) = (decoded.messages.values().sum(), decoded.calls.len() as u64);
    // (In case it becomes a test: hashes and counts, nothing decoded.)
    fs::write(out.join("expected.jsonl"), corpus::expected_text("submission", env!("CARGO_PKG_VERSION"), &decoded)).map_err(|e| e.to_string())?;
    let live = &meta["global"]["trp:live"];
    let live_good: u64 = live["sites"].as_array().map_or(0, |a| a.iter().filter_map(|s| s["messages_good"].as_u64()).sum());
    let sites = replay.as_ref().map_or_else(Vec::new, |r| compare_sites(live, r));
    let total = |k: &str| sites.iter().filter_map(|s| s[k].as_u64()).sum::<u64>();
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
        // Like for like: control channel blocks, good and bad, live and replayed, over the part of
        // the capture live counted (seconds from where replay starts; none: an older capture, and
        // the replay's are over all of it).
        "live_window": live["from_s"].as_f64().zip(live["to_s"].as_f64()).map(|(from_s, to_s)| json!({ "from_s": from_s, "to_s": to_s })),
        "sites": sites,
        "live_good": total("live_good"),
        "live_bad": total("live_bad"),
        "replay_good": total("replay_good"),
        "replay_bad": total("replay_bad"),
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
    fn live_and_replay_compare_over_the_same_window() {
        use crate::corpus::{SiteFrames, WindowFrames};
        let frames = |site: &str, good, bad| SiteFrames { system: "nx".into(), site: site.into(), good, bad };
        let live = json!({ "from_s": 0.4, "to_s": 40.0, "sites": [{ "system": "nx", "site": "a", "messages_good": 641, "messages_bad": 3 }] });
        let replay = ReplaySummary { air_s: 40.0, sites: vec![frames("a", 700, 5), frames("b", 9, 0)], live_window: Some(WindowFrames { from_s: 0.4, to_s: 40.0, sites: vec![frames("a", 658, 4), frames("b", 9, 0)] }) };
        let rows = compare_sites(&live, &replay);
        assert_eq!(rows[0], json!({ "system": "nx", "site": "a", "live_good": 641, "live_bad": 3, "replay_good": 658, "replay_bad": 4 }), "the replay's over live's window, not all of it");
        assert_eq!((&rows[1]["live_good"], &rows[1]["replay_good"]), (&Value::Null, &json!(9)), "a site live didn't run");
        let older = ReplaySummary { live_window: None, ..replay };
        assert_eq!(compare_sites(&live, &older)[0]["replay_good"], 700, "a capture that doesn't say: all of it");
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
