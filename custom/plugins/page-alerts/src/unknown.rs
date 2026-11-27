//! Pages whose tones match no tone set, kept to listen to later: the call's
//! audio (the dispatch after it joined on) in a folder of their own, a note
//! beside it saying what was heard, and a line in the folder's index.csv.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

use trunk_recorder_plugin::{ConcludedCall, TonePattern};

use crate::toneboard::expand;

/// Local time of Unix seconds `t`, as `date` gives it with `fmt` (no time zone crate).
fn local(t: i64, fmt: &str) -> String {
    Command::new("date").arg("-d").arg(format!("@{t}")).arg(format!("+{fmt}")).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| t.to_string())
}

/// A file name's worth of `s`: no slashes or other troublesome characters.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' }).collect::<String>().trim().to_string()
}

/// "1820-992" (two tones), "1004" (one): a pattern's tones, rounded.
fn tones(p: &TonePattern) -> String {
    p.tones.iter().map(|t| format!("{}", t.round() as i64)).collect::<Vec<_>>().join("-")
}

/// Saves `call`'s audio (on `channel`, with the unknown `pats` and the tone sets `named` heard with them) in `dir`.
/// The audio file it wrote.
pub fn save(dir: &str, channel: &str, call: &ConcludedCall, pats: &[&TonePattern], named: &[String]) -> Result<PathBuf, String> {
    let dir = expand(dir);
    let day = local(call.call.start_time, "%Y-%m-%d");
    let sub = dir.join(&day);
    std::fs::create_dir_all(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
    let audio = call.files.m4a.clone().filter(|p| p.exists()).unwrap_or_else(|| call.files.wav.clone());
    let ext = audio.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_string();
    let when = local(call.call.start_time, "%Y-%m-%d_%H-%M-%S");
    let first = pats.first().map(|p| tones(p)).unwrap_or_default();
    let base = clean(&format!("{when} {channel} {first}"));
    let file = sub.join(format!("{base}.{ext}"));
    std::fs::copy(&audio, &file).map_err(|e| format!("{} → {}: {e}", audio.display(), file.display()))?;
    // The note beside it.
    let mut note = format!("Channel: {channel}\nStarted: {}\nFrequency: {:.5} MHz\nLength: {:.1} s\n\nTones heard that match no tone set:\n", local(call.call.start_time, "%Y-%m-%d %H:%M:%S"), call.call.freq as f64 / 1e6, call.call.call_length);
    for p in pats {
        note.push_str(&format!("  {} (at {:.1} s)\n", p.text, p.pos));
    }
    if !named.is_empty() {
        note.push_str(&format!("\nPaged in the same call: {}\n", named.join(", ")));
    }
    note.push_str(&format!("\nTRP's call: {}\n", call.path));
    let _ = std::fs::write(sub.join(format!("{base}.txt")), note);
    // A line in the index, for a spreadsheet.
    let index = dir.join("index.csv");
    let new = !index.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&index) {
        if new {
            let _ = writeln!(f, "time,channel,tones,lengths,also_paged,file");
        }
        for p in pats {
            let lengths = p.lengths.iter().map(|l| format!("{l:.2}")).collect::<Vec<_>>().join("+");
            let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            let _ = writeln!(f, "{},{},{},{},{},{}", local(call.call.start_time, "%Y-%m-%d %H:%M:%S"), q(channel), q(&p.tones.iter().map(|t| format!("{t:.1}")).collect::<Vec<_>>().join(" → ")), lengths, q(&named.join("; ")), q(&file.strip_prefix(&dir).unwrap_or(&file).display().to_string()));
        }
    }
    Ok(file)
}

/// Day folders older than `keep_days` go (0: kept for ever).
pub fn tidy(dir: &str, keep_days: u32) {
    if keep_days == 0 {
        return;
    }
    let dir = expand(dir);
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d.as_secs() > keep_days as u64 * 86_400));
        if p.is_dir() && old && p.file_name().is_some_and(|n| n.to_string_lossy().chars().next().is_some_and(|c| c.is_ascii_digit())) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::testing;

    #[test]
    fn keeps_the_audio_and_a_note() {
        let dir = testing::temp_dir("page-unknown");
        let call = testing::call(&dir, "VHF", 105);
        let p = TonePattern { pattern: "two_tone".into(), tones: vec![1820.0, 992.0], lengths: vec![2.0, 3.0], len: 5.0, text: "two-tone 1820 → 992 Hz, 2 + 3 s".into(), ..Default::default() };
        let out = dir.join("unknown");
        let f = save(out.to_str().unwrap(), "Ulster Co/Fire", &call, &[&p], &["Mobile Life".into()]).unwrap();
        assert!(f.exists() && f.to_string_lossy().contains("Ulster Co_Fire 1820-992"), "{}", f.display());
        let note = std::fs::read_to_string(f.with_extension("txt")).unwrap();
        assert!(note.contains("two-tone 1820 → 992 Hz") && note.contains("Paged in the same call: Mobile Life"), "{note}");
        let index = std::fs::read_to_string(out.join("index.csv")).unwrap();
        assert_eq!(index.lines().count(), 2, "{index}");
        assert!(index.contains("\"1820.0 → 992.0\",2.00+3.00,\"Mobile Life\""), "{index}");
    }
}
