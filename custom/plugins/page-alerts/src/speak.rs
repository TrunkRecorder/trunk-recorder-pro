//! Sounds and the spoken agency name, on this computer's speakers: one at a
//! time, in order, on a thread of their own.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::toneboard::expand;

pub enum Say {
    /// A sound: `beep`, or a file in the sounds folder.
    Sound { sound: String, dir: String },
    /// Words, by the voice command ("" = Piper, else espeak-ng).
    Words { text: String, command: String },
}

pub struct Speaker {
    tx: Sender<Say>,
}

impl Speaker {
    pub fn start(data_dir: PathBuf, log: impl Fn(String) + Send + 'static) -> Speaker {
        let (tx, rx) = channel();
        std::thread::spawn(move || play_all(rx, data_dir, log));
        Speaker { tx }
    }
    pub fn say(&self, s: Say) {
        let _ = self.tx.send(s);
    }
}

fn play_all(rx: Receiver<Say>, data_dir: PathBuf, log: impl Fn(String)) {
    for s in rx {
        let r = match s {
            Say::Sound { sound, dir } => play_sound(&sound, &dir, &data_dir),
            Say::Words { text, command } => speak(&text, &command, &data_dir),
        };
        if let Err(e) = r {
            log(e);
        }
    }
}

fn has(cmd: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {cmd} >/dev/null 2>&1")).status().is_ok_and(|s| s.success())
}

/// Plays a WAV (or anything ffplay takes), cut off after 20 s.
pub fn play_file(f: &Path) -> Result<(), String> {
    let wav = f.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    let mut cmd = if has("paplay") && wav {
        let mut c = Command::new("paplay");
        c.arg(f);
        c
    } else if has("ffplay") {
        let mut c = Command::new("ffplay");
        c.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]).arg(f);
        c
    } else if has("aplay") && wav {
        let mut c = Command::new("aplay");
        c.arg("-q").arg(f);
        c
    } else {
        return Err(format!("nothing to play {} with: install pulseaudio-utils (paplay) or ffmpeg (ffplay)", f.display()));
    };
    let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("playing {}: {e}", f.display()))?;
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if t.elapsed() > Duration::from_secs(20) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn play_sound(sound: &str, dir: &str, data_dir: &Path) -> Result<(), String> {
    let s = sound.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(());
    }
    if s.eq_ignore_ascii_case("beep") {
        let f = data_dir.join("beep.wav");
        if !f.exists() {
            // Three short 1 kHz beeps.
            let mut v = Vec::new();
            for _ in 0..3 {
                v.extend(tone(1000.0, 0.15, 0.5));
                v.extend(std::iter::repeat_n(0i16, 8000 * 10 / 100));
            }
            write_wav(&f, &v, 8000).map_err(|e| e.to_string())?;
        }
        return play_file(&f);
    }
    let f = expand(dir).join(s);
    if !f.exists() {
        return Err(format!("sound {} isn't in {}", s, expand(dir).display()));
    }
    play_file(&f)
}

/// Words to a WAV file with Piper (Tone Board's, or one on the PATH), else espeak-ng.
pub fn words_to_wav(text: &str, out: &Path) -> Result<(), String> {
    let tb = expand("~/.local/share/toneboard");
    let piper = [tb.join("piper/piper"), tb.join("piper/bin/piper")].into_iter().find(|p| p.exists()).or_else(|| has("piper").then(|| PathBuf::from("piper")));
    let model = ["voices", "piper", "piper/voices"]
        .iter()
        .filter_map(|d| std::fs::read_dir(tb.join(d)).ok())
        .flat_map(|r| r.flatten().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "onnx"));
    if let (Some(piper), Some(model)) = (piper, model) {
        let mut c = Command::new(&piper)
            .arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("piper: {e}"))?;
        c.stdin.take().map(|mut i| i.write_all(text.as_bytes()));
        let ok = c.wait().is_ok_and(|s| s.success()) && out.exists();
        if ok {
            return Ok(());
        }
    }
    if has("espeak-ng") {
        let ok = Command::new("espeak-ng").arg("-w").arg(out).arg(text).status().is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }
    Err("no voice: run Tone Board's install-voice.sh (Piper) or install espeak-ng".into())
}

fn speak(text: &str, command: &str, data_dir: &Path) -> Result<(), String> {
    if !command.trim().is_empty() {
        let quoted = format!("'{}'", text.replace('\'', "'\\''"));
        let ok = Command::new("sh").arg("-c").arg(command.replace("{text}", &quoted)).status().is_ok_and(|s| s.success());
        return if ok { Ok(()) } else { Err(format!("the voice command failed: {command}")) };
    }
    let f = data_dir.join("say.wav");
    words_to_wav(text, &f)?;
    play_file(&f)
}

/// A sine tone, `secs` long, at `level` of full scale, 8 kHz.
pub fn tone(hz: f64, secs: f64, level: f64) -> Vec<i16> {
    let n = (secs * 8000.0) as usize;
    (0..n)
        .map(|i| {
            let ramp = (i.min(n - i) as f64 / 80.0).min(1.0);
            ((2.0 * std::f64::consts::PI * hz * i as f64 / 8000.0).sin() * level * ramp * 32767.0) as i16
        })
        .collect()
}

pub fn write_wav(f: &Path, s: &[i16], rate: u32) -> std::io::Result<()> {
    let mut b = Vec::with_capacity(44 + s.len() * 2);
    let data = (s.len() * 2) as u32;
    b.extend(b"RIFF");
    b.extend((36 + data).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(rate.to_le_bytes());
    b.extend((rate * 2).to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data.to_le_bytes());
    for v in s {
        b.extend(v.to_le_bytes());
    }
    std::fs::write(f, b)
}
