//! The log, as data: what happened ([`Record`]), and how a line says it in
//! Trunk Recorder's style ([`Format`], [`line`]). Where lines go — the
//! console, a file, the system log — is the platform's logger's business
//! (`trunk-pro`'s `logging`); the session only says what happened.
//!
//! A line: `[2026-10-02 14:03:07.123456] (info)   [dcfd]  12C  TG:       3747  Freq: 857.987500 MHz  Starting recorder…`
//! The time and level are the logger's; the rest is [`line`]'s, with Trunk
//! Recorder's options: `frequencyFormat` (exp / mhz / hz),
//! `talkgroupDisplayFormat` (id / id_tag / tag_id), `statusAsString` and
//! colour (ANSI, as Trunk Recorder's console has it).

use serde::{Deserialize, Serialize};

/// Trunk Recorder's (boost's) levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    #[default]
    Info,
    Warning,
    Error,
    Fatal,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warning" | "warn" => Level::Warning,
            "error" => Level::Error,
            "fatal" => Level::Fatal,
            _ => return None,
        })
    }
    /// The syslog severity (RFC 5424).
    pub fn syslog_severity(self) -> u8 {
        match self {
            Level::Trace | Level::Debug => 7,
            Level::Info => 6,
            Level::Warning => 4,
            Level::Error => 3,
            Level::Fatal => 2,
        }
    }
}

/// The call a line is about: Trunk Recorder's line header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallTag {
    pub num: u64,
    pub talkgroup: u32,
    /// The talkgroup's alpha tag ("" when unknown).
    pub tag: String,
    pub encrypted: bool,
    pub freq_hz: f64,
}

/// What a call's state is, for status lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    Recording,
    /// Not recorded, and why (the call view's reason: "unknown_tg", …).
    Monitoring(&'static str),
}

/// What happened.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Text(String),
    /// A call is being recorded (`kind`: "P25", "P25 Phase 2", "analog", "DMR").
    Recording {
        kind: &'static str,
        slot: Option<u8>,
    },
    /// A call is not recorded, and why (the call view's reason).
    NotRecording(&'static str),
    /// A call was saved.
    Concluded {
        length_s: f64,
        signal_db: Option<f64>,
        noise_db: Option<f64>,
        snr_db: Option<f64>,
        clean_pct: Option<f64>,
    },
    /// A call ended without being saved (no audio, or shorter than the minimum).
    Dropped,
    /// Tuned a control channel.
    ControlChannel {
        freq_hz: f64,
    },
    /// The control channel's decode rate (logged below `controlWarnRate`, or always at −1).
    DecodeRate {
        freq_hz: Option<f64>,
        per_s: f64,
        count: u64,
    },
    /// A call's state in the status summary.
    State {
        elapsed_s: f64,
        state: CallState,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub level: Level,
    /// The system's short name.
    pub system: Option<String>,
    pub call: Option<CallTag>,
    pub body: Body,
}

impl Record {
    pub fn text(level: Level, system: Option<&str>, text: impl Into<String>) -> Record {
        Record { level, system: system.map(str::to_string), call: None, body: Body::Text(text.into()) }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrequencyFormat {
    /// 8.579875e+08
    Exp,
    /// 857.987500 MHz
    #[default]
    Mhz,
    /// 857987500 Hz
    Hz,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TalkgroupFormat {
    /// `      3747`
    #[default]
    Id,
    /// `      3747 (DCFD Disp)`
    IdTag,
    /// `(DCFD Disp) 3747`
    TagId,
}

/// How lines read (Trunk Recorder's console options).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Format {
    pub frequency: FrequencyFormat,
    pub talkgroup: TalkgroupFormat,
    /// States as words ("Monitoring: UNKNOWN TG"), else numbers.
    pub status_as_string: bool,
}

impl Default for Format {
    fn default() -> Self {
        Format { frequency: FrequencyFormat::Mhz, talkgroup: TalkgroupFormat::Id, status_as_string: true }
    }
}

const RST: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GRN: &str = "\x1b[32m";
const YEL: &str = "\x1b[33m";
const BLU: &str = "\x1b[34m";
const MAG: &str = "\x1b[35m";
const CYN: &str = "\x1b[36m";

impl Format {
    pub fn freq(&self, hz: f64) -> String {
        match self.frequency {
            FrequencyFormat::Mhz => format!("{:10.6} MHz", hz / 1e6),
            FrequencyFormat::Hz => format!("{hz:.0} Hz"),
            FrequencyFormat::Exp => {
                // C's %e: 8.579875e+08.
                let s = format!("{hz:.6e}");
                match s.split_once('e') {
                    Some((m, e)) => {
                        let n: i32 = e.parse().unwrap_or(0);
                        format!("{m}e{}{:02}", if n < 0 { '-' } else { '+' }, n.abs())
                    }
                    None => s,
                }
            }
        }
    }

    fn talkgroup(&self, c: &CallTag, color: bool) -> String {
        let (on, off) = if color { (if c.encrypted { RED } else { MAG }, RST) } else { ("", "") };
        let tag = if c.tag.trim().is_empty() { "-" } else { c.tag.trim() };
        match self.talkgroup {
            TalkgroupFormat::Id => format!("{on}{:>10}{off}", c.talkgroup),
            TalkgroupFormat::IdTag => format!("{:>10} ({on}{tag:>23}{off})", c.talkgroup),
            TalkgroupFormat::TagId => format!("{on}{tag:>23}{off} ({:>10})", c.talkgroup),
        }
    }

    fn state(&self, s: CallState, color: bool) -> String {
        let c = |code: &'static str| if color { code } else { "" };
        let rst = c(RST);
        if !self.status_as_string {
            // Trunk Recorder's State enum: MONITORING 0, RECORDING 1.
            return match s {
                CallState::Monitoring(_) => "0".into(),
                CallState::Recording => "1".into(),
            };
        }
        match s {
            CallState::Recording => format!("{}Recording{rst}", c(RED)),
            CallState::Monitoring(why) => {
                let (tone, what) = match why {
                    "unknown_tg" => ("", "UNKNOWN TG"),
                    "ignored" => ("", "IGNORED TG"),
                    "no_source" => (YEL, "NO SOURCE COVERING FREQ"),
                    "no_recorder" => (YEL, "NO RECORDER AVAILABLE"),
                    "encrypted" => (RED, "ENCRYPTED"),
                    _ => ("", ""),
                };
                if what.is_empty() {
                    format!("{}Monitoring{rst}", c(CYN))
                } else {
                    format!("{}Monitoring{rst}: {}{what}{}", c(CYN), c(tone), if tone.is_empty() { "" } else { rst })
                }
            }
        }
    }
}

fn db(v: Option<f64>) -> String {
    v.map_or("?".into(), |v| format!("{v:.0}"))
}

/// A record as Trunk Recorder words it, without the time and level.
pub fn line(r: &Record, f: &Format, color: bool) -> String {
    let c = |code: &'static str| if color { code } else { "" };
    let rst = c(RST);
    let mut s = String::new();
    if let Some(sys) = &r.system {
        s.push_str(&format!("[{sys}]\t"));
    }
    if let Some(call) = &r.call {
        s.push_str(&format!("{}{}C{rst}\tTG: {}\tFreq: {}\t", c(BLU), call.num, f.talkgroup(call, color), f.freq(call.freq_hz)));
    }
    match &r.body {
        Body::Text(t) => s.push_str(t),
        Body::Recording { kind, slot } => {
            s.push_str(&format!("{}Starting {kind} Recorder{rst}", c(GRN)));
            if let Some(slot) = slot {
                s.push_str(&format!("\tSlot: {slot}"));
            }
        }
        Body::NotRecording(why) => {
            let (tone, what) = match *why {
                "unknown_tg" => (YEL, "TG not in Talkgroup File"),
                "ignored" => (YEL, "TG marked Ignore"),
                "encrypted" => (RED, "ENCRYPTED"),
                "no_source" => (CYN, "no source covering Freq"),
                "no_recorder" => (YEL, "no recorder available"),
                other => (YEL, other),
            };
            s.push_str(&format!("{}Not Recording: {what}{rst}", c(tone)));
        }
        Body::Concluded { length_s, signal_db, noise_db, snr_db, clean_pct } => {
            s.push_str(&format!("{}Concluding Recorded Call{rst} - Call Length: {length_s:.1}s", c(YEL)));
            if signal_db.is_some() || noise_db.is_some() {
                s.push_str(&format!("\t Signal: {}dBFS\t Noise: {}dBFS\t SNR: {}dB", db(*signal_db), db(*noise_db), db(*snr_db)));
            }
            if let Some(p) = clean_pct {
                s.push_str(&format!("\t Clean voice: {p:.0}%"));
            }
        }
        Body::Dropped => s.push_str(&format!("{}Call not saved{rst} - no audio, or shorter than the minimum", c(CYN))),
        Body::ControlChannel { freq_hz } => s.push_str(&format!("Tuned control channel: {}", f.freq(*freq_hz))),
        Body::DecodeRate { freq_hz, per_s, count } => {
            if let Some(hz) = freq_hz {
                s.push_str(&format!("freq: {}\t", f.freq(*hz)));
            }
            s.push_str(&format!("Control Channel Message Decode Rate: {per_s:.1}/sec, count:  {count}"));
        }
        Body::State { elapsed_s, state } => s.push_str(&format!("Elapsed: {:>4.0} State: {}", elapsed_s, f.state(*state, color))),
    }
    s
}

/// The whole line, given the time as the logger writes it.
pub fn full_line(time: &str, r: &Record, f: &Format, color: bool) -> String {
    format!("[{time}] ({})   {}", r.level.as_str(), line(r, f, color))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> CallTag {
        CallTag { num: 12, talkgroup: 3747, tag: "DCFD Disp".into(), encrypted: false, freq_hz: 857_987_500.0 }
    }

    #[test]
    fn trunk_recorders_lines() {
        let f = Format::default();
        let r = Record { level: Level::Info, system: Some("dcfd".into()), call: Some(call()), body: Body::Recording { kind: "P25", slot: None } };
        assert_eq!(
            full_line("2026-10-02 14:03:07.123456", &r, &f, false),
            "[2026-10-02 14:03:07.123456] (info)   [dcfd]\t12C\tTG:       3747\tFreq: 857.987500 MHz\tStarting P25 Recorder"
        );
        assert!(line(&r, &f, true).contains("\x1b[34m12C\x1b[0m") && line(&r, &f, true).contains("\x1b[35m      3747\x1b[0m"));
        let f = Format { frequency: FrequencyFormat::Exp, talkgroup: TalkgroupFormat::IdTag, status_as_string: true };
        assert_eq!(line(&r, &f, false), "[dcfd]\t12C\tTG:       3747 (              DCFD Disp)\tFreq: 8.579875e+08\tStarting P25 Recorder");
        let f = Format { frequency: FrequencyFormat::Hz, talkgroup: TalkgroupFormat::TagId, status_as_string: false };
        let st = Record { body: Body::State { elapsed_s: 4.0, state: CallState::Monitoring("unknown_tg") }, ..r.clone() };
        assert_eq!(line(&st, &f, false), "[dcfd]\t12C\tTG:               DCFD Disp (      3747)\tFreq: 857987500 Hz\tElapsed:    4 State: 0");
        let f = Format::default();
        assert_eq!(line(&st, &f, false).rsplit('\t').next(), Some("Elapsed:    4 State: Monitoring: UNKNOWN TG"));
        let done =
            Record { body: Body::Concluded { length_s: 8.46, signal_db: Some(-22.4), noise_db: Some(-51.9), snr_db: Some(29.5), clean_pct: Some(100.0) }, ..r };
        assert!(
            line(&done, &f, false).ends_with("Concluding Recorded Call - Call Length: 8.5s\t Signal: -22dBFS\t Noise: -52dBFS\t SNR: 30dB\t Clean voice: 100%")
        );
        assert_eq!(Level::parse("WARN"), Some(Level::Warning));
    }
}
