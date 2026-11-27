//! A page and the dispatch after it, as one recording. When the dispatcher
//! keys up again a few seconds after the tones (the carrier dropped between),
//! TRP records two calls: the page's call waits here until the channel has
//! been quiet for the follow-on window, and every call that started on it in
//! the meantime is added to its audio (with a short pause between) before it
//! goes to Rdio Scanner.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use trunk_recorder_plugin::{ConcludedCall, Host, Outcome};

/// The pause put between two calls joined, seconds.
const PAUSE_S: f64 = 0.5;
/// The most calls added after a page's.
const MAX_FOLLOW: usize = 6;
/// However busy the channel, a page goes up this long after its call ended at the latest.
const MAX_WAIT_MS: i64 = 180_000;

/// A page's call, waiting for what follows on its channel.
pub struct Waiting {
    pub calls: Vec<ConcludedCall>,
    /// Calls started on the channel and not concluded yet (their call_num).
    pub open: HashSet<u64>,
    /// When the last call ended, and the page's call (Unix ms).
    pub last_end_ms: i64,
    pub first_end_ms: i64,
}

/// What happens to a concluded call on a paging channel.
#[derive(Debug, PartialEq)]
pub enum Take {
    /// Added to the page waiting on its channel.
    Joined,
    /// A page of its own: wait for what follows (the page waiting before it goes now: `flush`).
    Waits { flush: bool },
    /// Nothing to do with it.
    Skip,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn end_ms(c: &ConcludedCall) -> i64 {
    if c.call.stop_time_ms > 0 { c.call.stop_time_ms } else if c.call.stop_time > 0 { c.call.stop_time * 1000 } else { now_ms() }
}

fn start_ms(c: &ConcludedCall) -> i64 {
    if c.call.start_time_ms > 0 { c.call.start_time_ms } else { c.call.start_time * 1000 }
}

/// The pages waiting, by channel.
#[derive(Default)]
pub struct Joiner {
    pub waiting: HashMap<usize, Waiting>,
}

impl Joiner {
    /// A call started on channel `ch`: a page waiting there waits for it too.
    pub fn started(&mut self, ch: usize, call_num: u64, window_ms: i64, now: i64) {
        if let Some(w) = self.waiting.get_mut(&ch)
            && now <= w.last_end_ms + window_ms
            && w.calls.len() + w.open.len() <= MAX_FOLLOW
        {
            w.open.insert(call_num);
        }
    }

    /// Call `c` concluded on channel `ch` (`paged`: it has pages of its own).
    pub fn concluded(&mut self, ch: usize, c: &ConcludedCall, paged: bool, window_ms: i64) -> Take {
        let w = self.waiting.get_mut(&ch);
        let follows = w.as_ref().is_some_and(|w| w.open.contains(&c.call.call_num) || start_ms(c) <= w.last_end_ms + window_ms) && w.as_ref().is_some_and(|w| w.calls.len() <= MAX_FOLLOW);
        // More tones (a stacked page) or the dispatch: the page waiting takes it.
        if paged && !follows {
            let flush = w.is_some();
            return Take::Waits { flush };
        }
        match w {
            Some(w) if follows => {
                w.open.remove(&c.call.call_num);
                w.last_end_ms = w.last_end_ms.max(end_ms(c));
                w.calls.push(c.clone());
                Take::Joined
            }
            Some(w) => {
                w.open.remove(&c.call.call_num);
                Take::Skip
            }
            None => Take::Skip,
        }
    }

    /// Start waiting with page call `c` on channel `ch`.
    pub fn wait(&mut self, ch: usize, c: ConcludedCall) {
        let e = end_ms(&c);
        self.waiting.insert(ch, Waiting { calls: vec![c], open: HashSet::new(), last_end_ms: e, first_end_ms: e });
    }

    /// The pages whose wait is over at `now` (all of them: `all`).
    pub fn due(&mut self, window_ms: i64, now: i64, all: bool) -> Vec<Waiting> {
        let done: Vec<usize> = self
            .waiting
            .iter()
            .filter(|(_, w)| all || now >= w.first_end_ms + MAX_WAIT_MS || (w.open.is_empty() && now > w.last_end_ms + window_ms))
            .map(|(k, _)| *k)
            .collect();
        done.into_iter().filter_map(|k| self.waiting.remove(&k)).collect()
    }
}

/// A WAV's 16-bit mono samples and rate.
pub fn read_wav(f: &Path) -> Result<(Vec<i16>, u32), String> {
    let b = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(format!("{} isn't a WAV", f.display()));
    }
    let (mut i, mut fmt, mut data) = (12, None, None);
    while i + 8 <= b.len() {
        let n = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let body = &b[i + 8..(i + 8 + n).min(b.len())];
        match &b[i..i + 4] {
            b"fmt " if body.len() >= 16 => fmt = Some((u16::from_le_bytes([body[0], body[1]]), u16::from_le_bytes([body[2], body[3]]), u32::from_le_bytes([body[4], body[5], body[6], body[7]]), u16::from_le_bytes([body[14], body[15]]))),
            b"data" => data = Some(body),
            _ => {}
        }
        i += 8 + n + (n & 1);
    }
    match (fmt, data) {
        (Some((1, 1, rate, 16)), Some(d)) => Ok((d.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]])).collect(), rate)),
        (Some(_), _) => Err(format!("{}: not 16-bit mono PCM", f.display())),
        _ => Err(format!("{}: no audio in it", f.display())),
    }
}

fn has(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(cmd)).find(|f| f.is_file()))
}

/// The calls' audio, one after another with a short pause, in `dir`: an .m4a when ffmpeg is there, else a WAV.
pub fn join(calls: &[ConcludedCall], dir: &Path) -> Result<PathBuf, String> {
    let mut all: Vec<i16> = Vec::new();
    let mut rate = 0;
    for c in calls {
        let (s, r) = read_wav(&c.files.wav)?;
        if rate != 0 && r != rate {
            return Err(format!("calls recorded at {rate} and {r} Hz"));
        }
        if rate != 0 {
            all.extend(std::iter::repeat_n(0i16, (r as f64 * PAUSE_S) as usize));
        }
        rate = r;
        all.extend(s);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name: String = calls[0].path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let wav = dir.join(format!("{name}.wav"));
    crate::speak::write_wav(&wav, &all, rate).map_err(|e| format!("{}: {e}", wav.display()))?;
    if let Some(ff) = has("ffmpeg") {
        let m4a = dir.join(format!("{name}.m4a"));
        let ok = Command::new(ff)
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&wav)
            .args(["-c:a", "aac", "-ar", "16000", "-ac", "1", "-b:a", "32k", "-movflags", "+faststart"])
            .arg(&m4a)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok && m4a.exists() {
            let _ = std::fs::remove_file(&wav);
            return Ok(m4a);
        }
    }
    Ok(wav)
}

/// The page call to upload for `w`: its own audio, or (with calls after it) the joined audio; and the calls after it reported done.
pub fn ready(w: Waiting, dir: &Path, host: &Host) -> ConcludedCall {
    let mut page = w.calls[0].clone();
    if w.calls.len() == 1 {
        return page;
    }
    let after = &w.calls[1..];
    // Every tone set heard counts as the page's, whichever call it was in.
    for c in after {
        page.call.tone_patterns.extend(c.call.tone_patterns.iter().cloned());
    }
    match join(&w.calls, dir) {
        Ok(f) => {
            let secs: f64 = after.iter().map(|c| c.call.call_length).sum();
            host.info(format!("Joined {} call(s) after the page ({secs:.0} s) into its recording", after.len()));
            if f.extension().is_some_and(|e| e == "m4a") {
                page.files.m4a = Some(f);
            } else {
                page.files.m4a = None;
                page.files.wav = f;
            }
            for c in after {
                host.call_result(&c.path, Outcome::Skipped, format!("joined to the page in {}", w.calls[0].path), "");
            }
        }
        Err(e) => {
            host.warn(format!("Couldn't join the dispatch after the page, so the page goes up alone: {e}"));
            for c in after {
                host.call_result(&c.path, Outcome::Skipped, "not joined", "");
            }
        }
    }
    page
}

/// Joined files of uploads done or given up on, a day old.
pub fn tidy(dir: &Path) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d.as_secs() > 86_400)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::testing;

    fn call(dir: &Path, n: u64, start_ms: i64, secs: u32) -> ConcludedCall {
        let mut c = testing::call(dir, "UHF", 1);
        c.path = format!("UHF/2026/11/24/1-{n}");
        c.call.call_num = n;
        c.call.start_time_ms = start_ms;
        c.call.stop_time_ms = start_ms + secs as i64 * 1000;
        c.files.wav = dir.join(format!("{n}.wav"));
        crate::speak::write_wav(&c.files.wav, &vec![1000i16; 8000 * secs as usize], 8000).unwrap();
        c
    }

    #[test]
    fn a_page_waits_for_the_dispatch() {
        let dir = testing::temp_dir("page-join");
        let w = 15_000;
        let mut j = Joiner::default();
        let page = call(&dir, 1, 0, 3);
        assert_eq!(j.concluded(0, &page, true, w), Take::Waits { flush: false });
        j.wait(0, page);
        // The dispatcher keys up 10 s after the tones ended, and talks 20 s.
        j.started(0, 2, w, 13_000);
        assert!(j.due(w, 20_000, false).is_empty(), "still waiting on it");
        assert!(j.due(w, 32_000, false).is_empty(), "it's still talking past the window");
        assert_eq!(j.concluded(0, &call(&dir, 2, 13_000, 20), false, w), Take::Joined);
        // Another channel's call isn't its.
        assert_eq!(j.concluded(1, &call(&dir, 3, 14_000, 2), false, w), Take::Skip);
        assert!(j.due(w, 40_000, false).is_empty(), "15 s of quiet first");
        let mut due = j.due(w, 48_001, false);
        assert_eq!(due.len(), 1);
        let w0 = due.remove(0);
        assert_eq!(w0.calls.len(), 2);
        let f = join(&w0.calls, &dir.join("joined")).unwrap();
        if f.extension().is_some_and(|e| e == "wav") {
            let (s, r) = read_wav(&f).unwrap();
            assert_eq!((r, s.len()), (8000, 8000 * 23 + 4000), "3 s, a half-second pause, 20 s");
        }
    }

    #[test]
    fn quiet_after_the_page_and_a_new_page() {
        let dir = testing::temp_dir("page-join2");
        let w = 15_000;
        let mut j = Joiner::default();
        j.wait(0, call(&dir, 1, 0, 3));
        // Nothing for 15 s: it goes alone.
        assert_eq!(j.due(w, 18_001, false).len(), 1);
        // A page, then more tones on the channel 5 s later: one stacked page.
        j.wait(0, call(&dir, 2, 100_000, 3));
        assert_eq!(j.concluded(0, &call(&dir, 3, 108_000, 3), true, w), Take::Joined);
        assert_eq!(j.due(w, 200_000, false).len(), 1);
        // More tones a minute later: a new page (the one before, if still waiting, goes first).
        j.wait(0, call(&dir, 6, 300_000, 3));
        assert_eq!(j.concluded(0, &call(&dir, 7, 380_000, 3), true, w), Take::Waits { flush: true });
        // A call 40 s after: not the page's.
        j.wait(0, call(&dir, 4, 200_000, 3));
        assert_eq!(j.concluded(0, &call(&dir, 5, 243_000, 3), false, w), Take::Skip);
    }
}
