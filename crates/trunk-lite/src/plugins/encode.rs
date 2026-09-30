//! Compressed audio for plugins (M4A: AAC in MP4), made with whatever encoder
//! the machine has — nothing is linked in. In order of preference: ffmpeg (any
//! platform; Trunk Recorder's settings: 16 kHz mono AAC-LC), afconvert (built
//! into macOS), fdkaac. With none, calls are WAV only and plugins are told so.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Encoder {
    Ffmpeg(PathBuf),
    Afconvert(PathBuf),
    Fdkaac(PathBuf),
}

impl Encoder {
    pub fn name(&self) -> &'static str {
        match self {
            Encoder::Ffmpeg(_) => "ffmpeg",
            Encoder::Afconvert(_) => "afconvert",
            Encoder::Fdkaac(_) => "fdkaac",
        }
    }

    /// `choice`: "auto", or an encoder's name to use only that one; "none" for none.
    pub fn find(choice: &str) -> Option<Encoder> {
        ["ffmpeg", "afconvert", "fdkaac"].into_iter().filter(|n| choice == "auto" || choice == *n).find_map(|n| {
            let p = which(n)?;
            Some(match n {
                "ffmpeg" => Encoder::Ffmpeg(p),
                "afconvert" => Encoder::Afconvert(p),
                _ => Encoder::Fdkaac(p),
            })
        })
    }

    /// Encode `wav` to `out` (written whole, or not at all) at `kbps`.
    pub fn m4a(&self, wav: &Path, out: &Path, kbps: u32) -> Result<(), String> {
        let tmp = out.with_extension("m4a.part");
        let mut c;
        match self {
            Encoder::Ffmpeg(p) => {
                c = Command::new(p);
                c.args(["-y", "-hide_banner", "-loglevel", "error", "-i"]).arg(wav);
                c.args(["-c:a", "aac", "-ar", "16000", "-ac", "1", "-b:a", &format!("{kbps}k"), "-movflags", "+faststart", "-f", "mp4"]).arg(&tmp);
            }
            Encoder::Afconvert(p) => {
                c = Command::new(p);
                c.args(["-f", "m4af", "-d", "aac@16000", "-c", "1", "-b", &(kbps * 1000).to_string()]).arg(wav).arg(&tmp);
            }
            Encoder::Fdkaac(p) => {
                c = Command::new(p);
                c.args(["-S", "--moov-before-mdat", "-b", &kbps.to_string(), "-o"]).arg(&tmp).arg(wav);
            }
        }
        let r = run(c, Duration::from_secs(60));
        let r = r.and_then(|_| std::fs::rename(&tmp, out).map_err(|e| e.to_string()));
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r.map_err(|e| format!("{}: {e}", self.name()))
    }
}

fn run(mut c: Command, limit: Duration) -> Result<(), String> {
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) if s.success() => return Ok(()),
            Some(s) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                return Err(format!("{s}: {}", err.trim()));
            }
            None if t0.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// A program on the PATH — or where package managers put them, since an app
/// started from the desktop doesn't get the shell's PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if cfg!(unix) {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
    }
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every encoder this machine has makes a playable file from a WAV.
    #[test]
    fn encoders_on_this_machine() {
        let dir = std::env::temp_dir().join(format!("trunk-lite-encode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        let tone: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.3).sin() * 0.5).collect();
        std::fs::write(&wav, trunk_core::wav::encode(&tone, 8000)).unwrap();
        for name in ["ffmpeg", "afconvert", "fdkaac"] {
            let Some(e) = Encoder::find(name) else {
                continue;
            };
            let out = dir.join(format!("{name}.m4a"));
            e.m4a(&wav, &out, 32).unwrap();
            let b = std::fs::read(&out).unwrap();
            assert!(b.len() > 1000 && &b[4..8] == b"ftyp", "{name}: not an MP4");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
