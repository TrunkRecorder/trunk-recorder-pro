//! `trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--dwell 45]
//! [--settle 8] [--max-steps 20] [--margin 1] [--mode manual|linearity|sensitivity] [--apply]` —
//! finds the gain at which a radio decodes its systems best, on a recorder that is
//! recording.
//!
//! It is a client of the recorder's API (`/api/ws`), like the interface: it changes
//! the radio's gain with `setConfig` (which the radio takes at once, as a gain changed
//! in Setup does), listens to the `stats` it publishes each second, and scores each
//! setting by what the radio's systems decoded while it was set:
//!
//! - the control channel first (P25's coverage, else the share of messages decoded,
//!   counted only while it was locked): without it nothing is recorded;
//! - then the voice frames lost, when there was enough voice to tell;
//! - a setting that clips (more than [`CLIP_MAX_PCT`] of samples at full scale) is
//!   refused, whatever it decoded.
//!
//! The weakest system counts twice, so a gain that helps the strong systems a little
//! at the cost of a weak one isn't chosen. The search ([`search`]) moves one gain
//! stage at a time from where the radio is, coarse steps first, keeping a move only
//! when it scores better by `--margin`; then the best and the starting setting are
//! measured once more each, since traffic changes from minute to minute.
//!
//! Recording goes on throughout, so a bad setting costs that radio's systems while
//! it is tried: a setting under which a control channel that was decoding stops for
//! [`ABANDON_S`] seconds is given up at once. However it ends — done, an error,
//! Ctrl-C — the radio is put back as it was, unless `--apply` (then it keeps the best).

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_core::metrics::{key_part, system_key};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::{die, Args};

/// A setting with more of its samples than this at full scale (%) is refused.
pub const CLIP_MAX_PCT: f64 = 0.5;
/// A control channel that was decoding, silent this long under a setting (s): the setting is given up.
pub const ABANDON_S: f64 = 10.0;
/// Voice frames a system needs in a step (P25: 50 a second) before its voice counts.
const MIN_VOICE_FRAMES: f64 = 250.0;
/// How much of a system's score is its control channel, when its voice counts (the rest is voice).
const CC_WEIGHT: f64 = 0.7;
/// The R820T / R828D's gains (dB): what an RTL-SDR can be set to.
const RTL_GAINS: [f64; 29] = [
    0.0, 0.9, 1.4, 2.7, 3.7, 7.7, 8.7, 12.5, 14.4, 15.7, 16.6, 19.7, 20.7, 22.9, 25.4, 28.0, 29.7, 32.8, 33.8, 36.4, 37.2, 38.6, 40.2, 42.1, 43.4, 43.9, 44.5, 48.0, 49.6,
];

// ───────────────────────────── the radio's gain stages ─────────────────────────────

/// One gain stage the search moves: the values it can take, and the steps (in places
/// along `values`) it tries, coarse first.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub name: &'static str,
    /// The config key it is written to.
    pub key: &'static str,
    pub values: Vec<f64>,
    pub steps: Vec<usize>,
}

/// A radio's gain, as the search sees it: its stages, and how they are written into
/// its config entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Knobs {
    /// What else its config entry needs set (an Airspy's `gainMode`, AGC off).
    fixed: Vec<(&'static str, Value)>,
    pub axes: Vec<Axis>,
}

fn range(lo: i32, hi: i32, by: i32) -> Vec<f64> {
    (lo..=hi).step_by(by as usize).map(f64::from).collect()
}

impl Knobs {
    /// The stages of the radio `src` (a config `sources` entry). `mode` searches an
    /// Airspy's other gain mode (`manual`, `linearity`, `sensitivity`) than the one it is in.
    pub fn of(src: &Value, mode: Option<&str>) -> Result<Knobs, String> {
        let kind = src["type"].as_str().unwrap_or("").to_ascii_lowercase();
        match kind.as_str() {
            "airspy" | "hydrasdr" => {
                let now = src["gainMode"].as_str().unwrap_or("linearity").to_ascii_lowercase();
                let mode = mode.map(str::to_ascii_lowercase).unwrap_or(if now.is_empty() { "linearity".into() } else { now });
                match mode.as_str() {
                    "manual" => Ok(Knobs {
                        fixed: vec![("gainMode", json!("manual")), ("agc", json!(false))],
                        axes: vec![
                            Axis { name: "LNA", key: "lnaStep", values: range(0, 14, 1), steps: vec![3, 1] },
                            Axis { name: "mixer", key: "mixerStep", values: range(0, 15, 1), steps: vec![3, 1] },
                            Axis { name: "VGA", key: "vgaStep", values: range(0, 15, 1), steps: vec![3, 1] },
                        ],
                    }),
                    "linearity" | "sensitivity" => Ok(Knobs {
                        fixed: vec![("gainMode", json!(mode))],
                        axes: vec![Axis { name: if mode == "linearity" { "linearity" } else { "sensitivity" }, key: "gainStep", values: range(0, 21, 1), steps: vec![4, 1] }],
                    }),
                    m => Err(format!("--mode {m}: manual, linearity or sensitivity")),
                }
            }
            "hackrf" => Ok(Knobs {
                fixed: vec![],
                axes: vec![
                    Axis { name: "LNA dB", key: "lnaDb", values: range(0, 40, 8), steps: vec![1] },
                    Axis { name: "VGA dB", key: "vgaDb", values: range(0, 62, 2), steps: vec![3, 1] },
                    Axis { name: "amp", key: "amp", values: vec![0.0, 1.0], steps: vec![1] },
                ],
            }),
            "rtlsdr" => Ok(Knobs { fixed: vec![("agc", json!(false))], axes: vec![Axis { name: "gain dB", key: "gainDb", values: RTL_GAINS.to_vec(), steps: vec![4, 1] }] }),
            "" => Err("the source has no type".into()),
            k => Err(format!("tuning a {k} source's gain isn't supported yet (Airspy, HydraSDR, HackRF and RTL-SDR are)")),
        }
    }

    /// Where `src`'s gain is now, as a place on each axis (the nearest value). A stage
    /// it doesn't set starts at the radio's default.
    pub fn point(&self, src: &Value) -> Vec<usize> {
        self.axes
            .iter()
            .map(|a| {
                let v = match &src[a.key] {
                    Value::Bool(b) => f64::from(u8::from(*b)),
                    v => v.as_f64().unwrap_or_else(|| default_of(a.key)),
                };
                nearest(&a.values, v)
            })
            .collect()
    }

    /// `src` with its gain set to `p`.
    pub fn apply(&self, src: &Value, p: &[usize]) -> Value {
        let mut s = src.clone();
        for (k, v) in &self.fixed {
            s[*k] = v.clone();
        }
        for (a, &i) in self.axes.iter().zip(p) {
            let v = a.values[i];
            s[a.key] = match a.key {
                "amp" => json!(v != 0.0),
                "gainDb" => json!(v),
                _ => json!(v as i64),
            };
        }
        s
    }

    /// "LNA 10 · mixer 7 · VGA 7".
    pub fn describe(&self, p: &[usize]) -> String {
        self.axes
            .iter()
            .zip(p)
            .map(|(a, &i)| match a.key {
                "amp" => format!("amp {}", if a.values[i] != 0.0 { "on" } else { "off" }),
                _ => format!("{} {}", a.name, a.values[i]),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A stage's value when the config leaves it out (the recorder's defaults).
fn default_of(key: &str) -> f64 {
    match key {
        "gainStep" => 14.0,
        "lnaStep" | "mixerStep" | "vgaStep" => 10.0,
        "lnaDb" => 16.0,
        "vgaDb" => 20.0,
        "gainDb" => 25.4,
        _ => 0.0,
    }
}

fn nearest(values: &[f64], v: f64) -> usize {
    (0..values.len()).min_by(|&a, &b| (values[a] - v).abs().total_cmp(&(values[b] - v).abs())).unwrap_or(0)
}

// ───────────────────────────── what a step measured ─────────────────────────────

/// One system's control channel (its sites on this radio, together) and voice over a step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysTally {
    pub seconds: f64,
    pub locked_s: f64,
    pub good: f64,
    pub bad: f64,
    /// P25: control channel symbols decoded; `None` when the system doesn't report them.
    pub air_sym: Option<f64>,
    pub snr: Mean,
    pub phase_err: Mean,
    pub voice_frames: f64,
    pub voice_bad: f64,
}

/// The radio over a step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SrcTally {
    pub seconds: f64,
    pub clip: Mean,
    pub clip_max: f64,
    pub dropped: f64,
    pub noise: Mean,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mean {
    sum: f64,
    n: u32,
}

impl Mean {
    pub fn add(&mut self, v: f64) {
        if v.is_finite() {
            self.sum += v;
            self.n += 1;
        }
    }
    pub fn get(&self) -> Option<f64> {
        (self.n > 0).then(|| self.sum / f64::from(self.n))
    }
}

/// What one setting measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub systems: BTreeMap<String, SysTally>,
    pub src: SrcTally,
    /// Given up early, and why.
    pub abandoned: Option<String>,
}

impl SysTally {
    /// The control channel's share decoded, 0–1: P25's coverage (symbols decoded
    /// against the 4800 a second it carries), else messages decoded of those framed,
    /// for the share of the step it was locked.
    pub fn control(&self) -> f64 {
        if self.seconds <= 0.0 {
            return 0.0;
        }
        match self.air_sym {
            Some(sym) => (sym / (4800.0 * self.seconds)).min(1.0),
            None if self.good + self.bad > 0.0 => self.good / (self.good + self.bad) * (self.locked_s / self.seconds),
            None => 0.0,
        }
    }

    /// Voice frames lost, 0–1, when there were enough to tell.
    pub fn voice_lost(&self) -> Option<f64> {
        (self.voice_frames >= MIN_VOICE_FRAMES).then(|| (self.voice_bad / self.voice_frames).clamp(0.0, 1.0))
    }

    /// 0–100.
    pub fn score(&self) -> f64 {
        let cc = self.control();
        match self.voice_lost() {
            Some(lost) => 100.0 * (CC_WEIGHT * cc + (1.0 - CC_WEIGHT) * (1.0 - lost)),
            None => 100.0 * cc,
        }
    }
}

impl Step {
    /// 0–100, the weakest system counted twice; `None` when the setting is refused
    /// (given up, or clipping).
    pub fn score(&self) -> Option<f64> {
        if self.abandoned.is_some() || self.clipping() || self.systems.is_empty() {
            return None;
        }
        let scores: Vec<f64> = self.systems.values().map(SysTally::score).collect();
        let worst = scores.iter().copied().fold(f64::INFINITY, f64::min);
        Some((scores.iter().sum::<f64>() + worst) / (scores.len() as f64 + 1.0))
    }

    pub fn clipping(&self) -> bool {
        self.src.clip.get().is_some_and(|c| c > CLIP_MAX_PCT)
    }
}

/// Where one system's numbers are in a `stats` message: its control channels'
/// (`sys/<short>` or `sys/<short>/site/<site>`, one per site on this radio) and its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub short_name: String,
    pub cc_keys: Vec<String>,
}

impl Watch {
    pub fn new(short_name: &str, sites: &[&str]) -> Watch {
        let sys = system_key(short_name);
        let cc_keys = sites
            .iter()
            .map(|site| if site.is_empty() || *site == short_name { sys.clone() } else { format!("{sys}/site/{}", key_part(site)) })
            .collect();
        Watch { short_name: short_name.to_string(), cc_keys }
    }
}

/// Adds one `stats` message (`values`, covering `dt` seconds) to `step`.
pub fn tally(step: &mut Step, watches: &[Watch], src_label: &str, values: &Value, dt: f64) {
    let v = |k: &str| values.get(k).and_then(Value::as_f64);
    for w in watches {
        let t = step.systems.entry(w.short_name.clone()).or_default();
        t.seconds += dt;
        // Locked while any of its control channels on this radio is.
        if w.cc_keys.iter().any(|k| v(&format!("{k}/cc/locked")).unwrap_or(0.0) >= 0.5) {
            t.locked_s += dt;
        }
        for k in &w.cc_keys {
            t.good += v(&format!("{k}/cc/good")).unwrap_or(0.0) * dt;
            t.bad += v(&format!("{k}/cc/bad")).unwrap_or(0.0) * dt;
            if let Some(s) = v(&format!("{k}/cc/airSym")) {
                *t.air_sym.get_or_insert(0.0) += s * dt;
            }
            if v(&format!("{k}/cc/locked")).unwrap_or(0.0) >= 0.5 {
                if let Some(s) = v(&format!("{k}/cc/snr")) {
                    t.snr.add(s);
                }
                if let Some(p) = v(&format!("{k}/cc/phaseErr")) {
                    t.phase_err.add(p);
                }
            }
        }
        let sys = system_key(&w.short_name);
        t.voice_frames += v(&format!("{sys}/voice/frames")).unwrap_or(0.0) * dt;
        t.voice_bad += v(&format!("{sys}/voice/bad")).unwrap_or(0.0) * dt;
    }
    let src = format!("src/{}", key_part(src_label));
    step.src.seconds += dt;
    if let Some(c) = v(&format!("{src}/clipPct")) {
        step.src.clip.add(c);
        step.src.clip_max = step.src.clip_max.max(c);
    }
    step.src.dropped += v(&format!("{src}/dropped")).unwrap_or(0.0) * dt;
    if let Some(n) = v(&format!("{src}/noise")) {
        step.src.noise.add(n);
    }
}

/// The systems whose control channels were locked at the start of a step but
/// haven't been for the last [`ABANDON_S`]: `quiet` is how long each has been.
pub fn went_quiet(quiet: &BTreeMap<String, f64>, decoding_before: &[String]) -> Option<String> {
    let lost: Vec<&str> = decoding_before.iter().filter(|s| quiet.get(*s).is_some_and(|q| *q >= ABANDON_S)).map(String::as_str).collect();
    (!lost.is_empty()).then(|| format!("control channel lost: {}", lost.join(", ")))
}

// ───────────────────────────── the search ─────────────────────────────

/// Why a search stopped before it was done.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// Ctrl-C, or the recorder went away: put things back and leave.
    Interrupted(String),
}

/// What the search found: each setting tried and its score, in order.
#[derive(Debug, Default)]
pub struct Found {
    pub tried: Vec<(Vec<usize>, Option<f64>)>,
    pub best: Vec<usize>,
    pub best_score: f64,
    pub out_of_steps: bool,
}

/// Moves one axis at a time from `start` — each axis's coarse step first, then
/// finer — and keeps a move only when it scores more than `margin` better.
/// `measure` scores a setting (`None`: refused). At most `budget` settings are measured.
pub fn search(axes: &[Axis], start: Vec<usize>, margin: f64, budget: usize, measure: &mut dyn FnMut(&[usize]) -> Result<Option<f64>, Stop>) -> Result<Found, Stop> {
    let mut found = Found::default();
    let mut seen: BTreeMap<Vec<usize>, Option<f64>> = BTreeMap::new();
    let mut eval = |p: &[usize], found: &mut Found| -> Result<Option<Option<f64>>, Stop> {
        if let Some(s) = seen.get(p) {
            return Ok(Some(*s));
        }
        if seen.len() >= budget {
            found.out_of_steps = true;
            return Ok(None);
        }
        let s = measure(p)?;
        seen.insert(p.to_vec(), s);
        found.tried.push((p.to_vec(), s));
        Ok(Some(s))
    };
    let Some(Some(base)) = eval(&start, &mut found)? else {
        return Err(Stop::Interrupted("the starting setting couldn't be measured (its control channels aren't decoding)".into()));
    };
    let (mut best, mut best_s) = (start, base);
    let levels = axes.iter().map(|a| a.steps.len()).max().unwrap_or(0);
    'levels: for level in 0..levels {
        // Passes over the axes at this step size, while one of them moves.
        for _pass in 0..3 {
            let mut moved = false;
            for (ax, a) in axes.iter().enumerate() {
                let step = a.steps[level.min(a.steps.len() - 1)];
                for dir in [1i64, -1] {
                    let mut went = false;
                    loop {
                        let to = best[ax] as i64 + dir * step as i64;
                        if to < 0 || to >= a.values.len() as i64 {
                            break;
                        }
                        let mut p = best.clone();
                        p[ax] = to as usize;
                        match eval(&p, &mut found)? {
                            None => break 'levels,
                            Some(Some(s)) if s > best_s + margin => {
                                (best, best_s, went, moved) = (p, s, true, true);
                            }
                            Some(_) => break,
                        }
                    }
                    // Up helped: no need to look down from where it got to.
                    if went {
                        break;
                    }
                }
            }
            if !moved {
                break;
            }
        }
    }
    found.best = best;
    found.best_score = best_s;
    Ok(found)
}

// ───────────────────────────── talking to the recorder ─────────────────────────────

struct Conn {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    config: Value,
    version: Value,
    status: Option<Value>,
    phase: String,
    stop: Arc<AtomicBool>,
}

enum Got {
    Msg(Value),
    Nothing,
}

impl Conn {
    fn open(url: &str, stop: Arc<AtomicBool>) -> Result<Conn, String> {
        let ws_url = ws_url(url)?;
        let (ws, _) = tungstenite::connect(&ws_url).map_err(|e| format!("couldn't connect to {ws_url}: {e}"))?;
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_millis(500))).map_err(|e| e.to_string())?;
        }
        let mut c = Conn { ws, config: Value::Null, version: Value::Null, status: None, phase: String::new(), stop };
        // The greeting, up to hello.
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            match c.next()? {
                Got::Msg(m) if m["type"] == "hello" => break,
                _ if Instant::now() > until => return Err("the recorder didn't finish greeting".into()),
                _ => {}
            }
        }
        if c.config.is_null() {
            return Err("the recorder didn't send its config".into());
        }
        Ok(c)
    }

    /// The next message (keeping the config, its version, the status and the state up to date).
    fn next(&mut self) -> Result<Got, String> {
        if self.stop.load(Ordering::SeqCst) {
            return Err("interrupted".into());
        }
        let m = match self.ws.read() {
            Ok(Message::Text(t)) => serde_json::from_str::<Value>(t.as_str()).map_err(|e| e.to_string())?,
            Ok(Message::Close(_)) => return Err("the recorder closed the connection".into()),
            Ok(_) => return Ok(Got::Nothing),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Got::Nothing),
            Err(e) => return Err(format!("connection to the recorder: {e}")),
        };
        match m["type"].as_str() {
            Some("config") => {
                self.config = m["config"].clone();
                self.version = m["version"].clone();
            }
            Some("status") => self.status = Some(m.clone()),
            Some("state") => self.phase = m["phase"].as_str().unwrap_or("").to_string(),
            _ => {}
        }
        Ok(Got::Msg(m))
    }

    fn send(&mut self, v: Value) -> Result<(), String> {
        self.ws.send(Message::text(v.to_string())).map_err(|e| format!("sending to the recorder: {e}"))
    }

    /// Writes `src` as source `i` with `setConfig`, and waits for it to be taken.
    /// Tried again once if the config changed elsewhere meanwhile.
    fn set_source(&mut self, i: usize, src: &Value) -> Result<(), String> {
        for attempt in 0..2 {
            let mut cfg = self.config.clone();
            cfg["sources"][i] = src.clone();
            let version = self.version.clone();
            self.send(json!({ "type": "setConfig", "config": cfg, "version": version }))?;
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                match self.next()? {
                    Got::Msg(m) if m["type"] == "configSaved" => {
                        match m["error"].as_str() {
                            None => return Ok(()),
                            Some(e) if attempt == 0 && e.contains("changed elsewhere") => break,
                            Some(e) => return Err(format!("the recorder refused the gain: {e}")),
                        }
                    }
                    Got::Msg(m) if m["type"] == "error" => return Err(format!("the recorder refused the gain: {}", m["message"].as_str().or(m["error"].as_str()).unwrap_or("error"))),
                    _ if Instant::now() > until => return Err("the recorder didn't answer setConfig".into()),
                    _ => {}
                }
            }
            // The newer config arrives as a `config` message before configSaved; take one more look for it.
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until {
                self.next()?;
            }
        }
        Err("the config kept changing elsewhere; try again".into())
    }
}

fn ws_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let rest = u.strip_prefix("http://").or_else(|| u.strip_prefix("ws://")).unwrap_or(u);
    if u.starts_with("https://") || u.starts_with("wss://") {
        return Err("--url: https isn't supported; use http:// (or an SSH tunnel)".into());
    }
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        return Err(format!("--url {url}: no host"));
    }
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:8080") };
    Ok(format!("ws://{host}/api/ws"))
}

/// The systems with a control channel on the radio `src` (a `status.sources` entry),
/// from the `status` message: each with the names of its sites there.
fn systems_on(status: &Value, src: &Value) -> Vec<Watch> {
    let center = src["centerHz"].as_f64().unwrap_or(0.0);
    let rate = src["rateHz"].as_f64().unwrap_or(0.0);
    let guard = src["settings"]["guardHz"].as_f64().unwrap_or(75_000.0);
    let half = rate / 2.0 - guard;
    let mut out = vec![];
    for y in status["status"]["systems"].as_array().into_iter().flatten() {
        let name = y["shortName"].as_str().unwrap_or("");
        let sites: Vec<&str> = y["sites"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["controlChannelHz"].as_f64().is_some_and(|f| (f - center).abs() <= half))
            .map(|s| s["name"].as_str().unwrap_or(""))
            .collect();
        if !name.is_empty() && !sites.is_empty() {
            out.push(Watch::new(name, &sites));
        }
    }
    out
}

// ───────────────────────────── the command ─────────────────────────────

pub fn cli(a: &Args) {
    if a.flag("help") || a.positional.first().is_some_and(|p| p == "help") {
        print!("{HELP}");
        return;
    }
    // Ctrl-C: put the radio back, then leave. A second one leaves at once.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
                eprintln!("\nStopping: putting the radio back (Ctrl-C again to leave at once)…");
                stop.store(true, Ordering::SeqCst);
                let _ = tokio::signal::ctrl_c().await;
                std::process::exit(130);
            });
        });
    }
    match run(a, stop) {
        Ok(()) => {}
        Err(e) => die(&format!("tune: {e}")),
    }
}

const HELP: &str = "\
trunk-pro tune [--url http://127.0.0.1:8080] [--source N] [--systems a,b] [--dwell 45]
               [--settle 8] [--max-steps 20] [--margin 1] [--mode manual|linearity|sensitivity] [--apply]

Finds the gain at which a radio decodes its systems best, on a recorder that is
recording. Each setting is held for --dwell seconds (after --settle) while the radio's
systems are scored on control channel decoding first, then voice frames lost; a
setting that clips is refused. Recording goes on throughout; the radio is put back as
it was at the end (or on Ctrl-C) unless --apply keeps the best.

  --url        the recorder (its built-in interface's address)
  --source     the radio, by its place in Setup's list from 0 (needed with several)
  --systems    only score these systems (default: every system with a control channel on the radio)
  --mode       Airspy: search this gain mode instead of the one it is in
  --max-steps  at most this many settings measured (each takes --settle + --dwell seconds)
  --margin     a move is kept only when it scores this much better (0–100 scale)
  --apply      keep the best setting
";

fn run(a: &Args, stop: Arc<AtomicBool>) -> Result<(), String> {
    let url = a.get("url").unwrap_or("http://127.0.0.1:8080");
    let dwell = a.num("dwell", 45.0).max(5.0);
    let settle = a.num("settle", 8.0).max(0.0);
    let budget = a.num("max-steps", 20.0).max(2.0) as usize;
    let margin = a.num("margin", 1.0).max(0.0);
    let apply = a.flag("apply");

    let mut c = Conn::open(url, stop.clone())?;
    if c.phase != "running" {
        return Err(format!("the recorder at {url} isn't recording: start it first (the tuner scores what it decodes)"));
    }
    // A status, for where the radios are and which systems they carry.
    let until = Instant::now() + Duration::from_secs(10);
    while c.status.is_none() {
        if Instant::now() > until {
            return Err("no status from the recorder".into());
        }
        c.next()?;
    }
    let status = c.status.clone().unwrap_or_default();
    let sources = status["sources"].as_array().cloned().unwrap_or_default();
    let i = match a.get("source") {
        Some(s) => s.parse::<usize>().map_err(|_| format!("--source {s}: a number from 0"))?,
        None if sources.len() == 1 => 0,
        None => {
            let list: Vec<String> = sources.iter().map(|s| format!("  {}  {}", s["index"], s["label"].as_str().unwrap_or(""))).collect();
            return Err(format!("this recorder has {} radios; say which with --source:\n{}", sources.len(), list.join("\n")));
        }
    };
    let run_src = sources.iter().find(|s| s["index"].as_u64() == Some(i as u64)).ok_or_else(|| format!("--source {i}: no such radio"))?.clone();
    let label = run_src["label"].as_str().unwrap_or("").to_string();
    let original = c.config["sources"].get(i).cloned().ok_or_else(|| format!("--source {i}: not in the config"))?;
    let knobs = Knobs::of(&original, a.get("mode"))?;
    let mut watches = systems_on(&status, &run_src);
    if let Some(only) = a.get("systems") {
        let only: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if let Some(missing) = only.iter().find(|s| !watches.iter().any(|w| w.short_name == **s)) {
            let on: Vec<&str> = watches.iter().map(|w| w.short_name.as_str()).collect();
            return Err(format!("--systems: {missing} has no control channel on {label} (it has: {})", on.join(", ")));
        }
        watches.retain(|w| only.contains(&w.short_name.as_str()));
    }
    if watches.is_empty() {
        return Err(format!("{label} carries no trunked system's control channel: nothing to score"));
    }
    let start = knobs.point(&original);
    let names: Vec<&str> = watches.iter().map(|w| w.short_name.as_str()).collect();
    println!("Tuning {label} (source {i}) on {url}");
    println!("  systems: {}", names.join(", "));
    println!("  now: {}", knobs.describe(&start));
    println!("  each setting: {settle:.0} s to settle + {dwell:.0} s measured; at most {budget} settings (~{:.0} min)", budget as f64 * (settle + dwell) / 60.0);
    println!("  to put it back by hand if this is killed: {}", original);
    println!();

    let mut log: Vec<(Vec<usize>, Step)> = vec![];
    let result = {
        let mut measure = |p: &[usize]| -> Result<Option<f64>, Stop> {
            let step = measure_one(&mut c, i, &original, &knobs, p, &watches, &label, settle, dwell).map_err(Stop::Interrupted)?;
            let s = step.score();
            println!("{}", line(&knobs, p, &step, &names));
            log.push((p.to_vec(), step));
            Ok(s)
        };
        search(&knobs.axes, start.clone(), margin, budget, &mut measure).and_then(|found| {
            // Traffic changes minute to minute: the best and the start once more each.
            if found.best != start {
                println!("  confirming: the best and the starting setting once more each");
                let again_best = measure(&found.best)?;
                let again_start = measure(&start)?;
                Ok((found, again_best, again_start))
            } else {
                Ok((found, None, None))
            }
        })
    };

    // Whatever happened, the radio goes back unless the best is to be kept.
    let finish = |c: &mut Conn, keep: Option<&[usize]>| -> Result<(), String> {
        let src = match keep {
            Some(p) => knobs.apply(&original, p),
            None => original.clone(),
        };
        c.set_source(i, &src)
    };
    let (found, again_best, again_start) = match result {
        Ok(r) => r,
        Err(Stop::Interrupted(why)) => {
            // A fresh connection: the old one may be what failed.
            stop.store(false, Ordering::SeqCst);
            let restored = Conn::open(url, stop).and_then(|mut c2| finish(&mut c2, None));
            return Err(match restored {
                Ok(()) => format!("{why}; the radio is back at {}", knobs.describe(&start)),
                Err(e) => format!("{why}; and putting the radio back failed ({e}): set it by hand to {original}"),
            });
        }
    };

    // The best and the start, over every measurement of each.
    let avg = |p: &[usize]| {
        let s: Vec<f64> = log.iter().filter(|(q, _)| q == p).filter_map(|(_, st)| st.score()).collect();
        (!s.is_empty()).then(|| s.iter().sum::<f64>() / s.len() as f64)
    };
    let (base, best) = (avg(&start), avg(&found.best));
    let better = found.best != start && matches!((base, best), (Some(b0), Some(b1)) if b1 > b0 + margin) && again_best.is_some() && again_best >= again_start;
    println!();
    println!("Tried {} settings{}.", found.tried.len(), if found.out_of_steps { " (stopped at --max-steps)" } else { "" });
    match (better, base, best) {
        (true, Some(b0), Some(b1)) => println!("Best: {}  score {b1:.1} (was {b0:.1} at {})", knobs.describe(&found.best), knobs.describe(&start)),
        (_, Some(b0), _) => println!("Best: keep {}  score {b0:.1}: nothing scored better by more than {margin}", knobs.describe(&start)),
        _ => println!("Best: keep {}", knobs.describe(&start)),
    }
    // Frequency correction, as Setup works it out from the control channels.
    if let Some(now) = c.status.as_ref().and_then(|s| s["sources"].as_array()).and_then(|v| v.iter().find(|s| s["index"].as_u64() == Some(i as u64))) {
        let set = original["ppm"].as_f64().unwrap_or(0.0);
        if let Some(apply_ppm) = now["ppmApply"].as_f64() {
            if (apply_ppm - set).abs() >= 0.3 {
                println!("PPM: measured error {:+.2} ppm: set the correction to {apply_ppm} ppm (now {set}){}", now["errorPpm"].as_f64().unwrap_or(0.0), if original["autoTune"] == true { "; AutoTune is covering it meanwhile" } else { "" });
            } else {
                println!("PPM: {set} is right (within 0.3 ppm of what the control channels measure)");
            }
        }
    }
    let keep = (apply && better).then_some(found.best.as_slice());
    finish(&mut c, keep)?;
    match (apply, better) {
        (true, true) => println!("Applied: {} is saved in the config.", knobs.describe(&found.best)),
        (true, false) => println!("Nothing to apply: the radio is back at {}.", knobs.describe(&start)),
        (false, true) => println!("The radio is back at {}. Run again with --apply, or set it in Setup, to keep the best.", knobs.describe(&start)),
        (false, false) => println!("The radio is back at {}.", knobs.describe(&start)),
    }
    Ok(())
}

/// Sets `p`, lets it settle, and measures it for `dwell` seconds.
#[allow(clippy::too_many_arguments)]
fn measure_one(c: &mut Conn, i: usize, original: &Value, knobs: &Knobs, p: &[usize], watches: &[Watch], label: &str, settle: f64, dwell: f64) -> Result<Step, String> {
    c.set_source(i, &knobs.apply(original, p))?;
    let settle_until = Instant::now() + Duration::from_secs_f64(settle);
    let mut step = Step::default();
    // Which control channels were decoding as the step began (they're the ones that count if lost).
    let mut decoding: Vec<String> = vec![];
    let mut quiet: BTreeMap<String, f64> = BTreeMap::new();
    let mut last_t: Option<f64> = None;
    let mut measuring = false;
    let mut silent = Instant::now();
    loop {
        if Instant::now() - silent > Duration::from_secs(10) {
            return Err("the recorder stopped sending stats (has recording stopped?)".into());
        }
        let Got::Msg(m) = c.next()? else { continue };
        if m["type"] == "state" && m["phase"] != "running" {
            return Err("recording stopped".into());
        }
        if m["type"] != "stats" {
            continue;
        }
        silent = Instant::now();
        let t = m["t"].as_f64().unwrap_or(0.0);
        let dt = last_t.map_or(1.0, |l| (t - l).clamp(0.2, 5.0));
        last_t = Some(t);
        let values = &m["values"];
        if !measuring {
            if Instant::now() < settle_until {
                continue;
            }
            // Measuring from here: the dwell is counted in stats seconds below.
            measuring = true;
            decoding = watches
                .iter()
                .filter(|w| w.cc_keys.iter().any(|k| values.get(format!("{k}/cc/locked")).and_then(Value::as_f64).unwrap_or(0.0) >= 0.5))
                .map(|w| w.short_name.clone())
                .collect();
            step = Step::default();
        }
        tally(&mut step, watches, label, values, dt);
        for w in watches {
            let locked = w.cc_keys.iter().any(|k| values.get(format!("{k}/cc/locked")).and_then(Value::as_f64).unwrap_or(0.0) >= 0.5);
            let q = quiet.entry(w.short_name.clone()).or_default();
            *q = if locked { 0.0 } else { *q + dt };
        }
        if let Some(why) = went_quiet(&quiet, &decoding) {
            step.abandoned = Some(why);
            return Ok(step);
        }
        if step.src.clip.get().is_some_and(|c| c > 4.0 * CLIP_MAX_PCT) && step.src.seconds >= 5.0 {
            step.abandoned = Some(format!("clipping ({:.1}% of samples at full scale)", step.src.clip.get().unwrap_or(0.0)));
            return Ok(step);
        }
        if step.src.seconds >= dwell {
            return Ok(step);
        }
    }
}

/// One line of the table: the setting, its score, then each system.
fn line(knobs: &Knobs, p: &[usize], step: &Step, names: &[&str]) -> String {
    let score = match (&step.abandoned, step.score()) {
        (Some(why), _) => format!("given up: {why}"),
        (None, None) if step.clipping() => format!("refused: clipping {:.2}%", step.src.clip.get().unwrap_or(0.0)),
        (None, None) => "—".into(),
        (None, Some(s)) => format!("score {s:5.1}"),
    };
    let mut out = format!("  {:<34} {score}", knobs.describe(p));
    for n in names {
        if let Some(t) = step.systems.get(*n) {
            let voice = t.voice_lost().map_or("voice —".to_string(), |l| format!("voice lost {:.1}%", l * 100.0));
            let q = match (t.phase_err.get(), t.snr.get()) {
                (Some(pe), _) => format!(" · {pe:.0}°"),
                (None, Some(snr)) => format!(" · {snr:.0} dB"),
                _ => String::new(),
            };
            out.push_str(&format!("\n      {n:<16} control {:5.1}% · {voice}{q}", t.control() * 100.0));
        }
    }
    let clip = step.src.clip.get().unwrap_or(0.0);
    if clip > 0.0 || step.src.dropped > 0.0 {
        out.push_str(&format!("\n      radio: clipping {clip:.2}%{}", if step.src.dropped > 0.0 { format!(" · {:.0} samples dropped", step.src.dropped) } else { String::new() }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn airspy() -> Value {
        json!({ "type": "airspy", "serial": "644064DC042A24CD", "gainMode": "manual", "lnaStep": 14, "mixerStep": 7, "vgaStep": 7, "centerHz": 772000000, "rateHz": 10000000 })
    }

    #[test]
    fn an_airspys_manual_gain_is_read_and_written_stage_by_stage() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let p = k.point(&airspy());
        assert_eq!(p, vec![14, 7, 7]);
        assert_eq!(k.describe(&p), "LNA 14 · mixer 7 · VGA 7");
        let s = k.apply(&airspy(), &[10, 8, 9]);
        assert_eq!((s["lnaStep"].clone(), s["mixerStep"].clone(), s["vgaStep"].clone(), s["gainMode"].clone()), (json!(10), json!(8), json!(9), json!("manual")));
        // Everything else stays as it was.
        assert_eq!((s["serial"].clone(), s["centerHz"].clone()), (airspy()["serial"].clone(), airspy()["centerHz"].clone()));
    }

    #[test]
    fn an_airspy_can_be_searched_in_another_mode() {
        let k = Knobs::of(&airspy(), Some("linearity")).unwrap();
        assert_eq!(k.axes.len(), 1);
        // Not in that mode now: it starts from the default step.
        assert_eq!(k.point(&airspy()), vec![14]);
        let s = k.apply(&airspy(), &[17]);
        assert_eq!((s["gainMode"].clone(), s["gainStep"].clone()), (json!("linearity"), json!(17)));
        assert!(Knobs::of(&airspy(), Some("loud")).is_err());
    }

    #[test]
    fn a_hackrfs_stages_and_amp() {
        let h = json!({ "type": "hackrf", "lnaDb": 40, "vgaDb": 18, "amp": false });
        let k = Knobs::of(&h, None).unwrap();
        let p = k.point(&h);
        assert_eq!(k.describe(&p), "LNA dB 40 · VGA dB 18 · amp off");
        let s = k.apply(&h, &[3, 10, 1]);
        assert_eq!((s["lnaDb"].clone(), s["vgaDb"].clone(), s["amp"].clone()), (json!(24), json!(20), json!(true)));
    }

    #[test]
    fn an_rtl_sdrs_gain_snaps_to_its_table_and_agc_goes_off() {
        let r = json!({ "type": "rtlsdr", "gainDb": 30, "agc": true });
        let k = Knobs::of(&r, None).unwrap();
        let p = k.point(&r);
        assert_eq!(k.axes[0].values[p[0]], 29.7);
        let s = k.apply(&r, &p);
        assert_eq!((s["gainDb"].clone(), s["agc"].clone()), (json!(29.7), json!(false)));
    }

    #[test]
    fn other_radios_are_refused_for_now() {
        assert!(Knobs::of(&json!({ "type": "usrp" }), None).unwrap_err().contains("isn't supported"));
    }

    #[test]
    fn a_systems_score_is_its_control_channel_then_its_voice() {
        // P25 coverage: 4320 of 4800 symbols a second = 90%.
        let t = SysTally { seconds: 10.0, locked_s: 10.0, air_sym: Some(43_200.0), ..Default::default() };
        assert!((t.control() - 0.9).abs() < 1e-9);
        assert!((t.score() - 90.0).abs() < 1e-9, "no voice to judge: the control channel alone");
        let v = SysTally { voice_frames: 1000.0, voice_bad: 100.0, ..t.clone() };
        assert!((v.voice_lost().unwrap() - 0.1).abs() < 1e-9);
        assert!((v.score() - (70.0 * 0.9 + 30.0 * 0.9)).abs() < 1e-9);
        // Too little voice to tell.
        assert_eq!(SysTally { voice_frames: 100.0, voice_bad: 50.0, ..t.clone() }.voice_lost(), None);
        // No coverage reported: messages decoded, for the time it was locked.
        let s = SysTally { seconds: 10.0, locked_s: 5.0, good: 90.0, bad: 10.0, ..Default::default() };
        assert!((s.control() - 0.45).abs() < 1e-9);
    }

    #[test]
    fn the_weakest_system_counts_twice_and_clipping_is_refused() {
        let sys = |cov: f64| SysTally { seconds: 1.0, locked_s: 1.0, air_sym: Some(4800.0 * cov), ..Default::default() };
        let mut step = Step::default();
        step.systems.insert("a".into(), sys(1.0));
        step.systems.insert("b".into(), sys(0.4));
        assert!((step.score().unwrap() - (100.0 + 40.0 + 40.0) / 3.0).abs() < 1e-9);
        step.src.clip.add(0.8);
        assert_eq!(step.score(), None);
        let given_up = Step { abandoned: Some("x".into()), ..Default::default() };
        assert_eq!(given_up.score(), None);
    }

    #[test]
    fn stats_are_tallied_per_system_and_site() {
        let watches = vec![Watch::new("Dallas", &["Dallas"]), Watch::new("Metro", &["North", "South"])];
        assert_eq!(watches[1].cc_keys, vec!["sys/Metro/site/North".to_string(), "sys/Metro/site/South".to_string()]);
        let values = json!({
            "sys/Dallas/cc/locked": 1, "sys/Dallas/cc/good": 40, "sys/Dallas/cc/bad": 2, "sys/Dallas/cc/airSym": 4700, "sys/Dallas/cc/phaseErr": 9.0,
            "sys/Dallas/voice/frames": 100, "sys/Dallas/voice/bad": 4,
            "sys/Metro/site/North/cc/locked": 0, "sys/Metro/site/South/cc/locked": 1, "sys/Metro/site/South/cc/good": 10, "sys/Metro/site/South/cc/snr": 14.0,
            "src/Airspy SN 1/clipPct": 0.1, "src/Airspy SN 1/dropped": 0, "src/Airspy SN 1/noise": -90,
        });
        let mut step = Step::default();
        tally(&mut step, &watches, "Airspy SN 1", &values, 1.0);
        tally(&mut step, &watches, "Airspy SN 1", &values, 1.0);
        let d = &step.systems["Dallas"];
        assert_eq!((d.seconds, d.locked_s, d.good, d.bad, d.air_sym, d.voice_frames, d.voice_bad), (2.0, 2.0, 80.0, 4.0, Some(9400.0), 200.0, 8.0));
        assert_eq!(d.phase_err.get(), Some(9.0));
        let m = &step.systems["Metro"];
        assert_eq!((m.locked_s, m.good, m.air_sym), (2.0, 20.0, None), "locked while either site is");
        assert_eq!(m.snr.get(), Some(14.0));
        assert_eq!((step.src.seconds, step.src.clip.get()), (2.0, Some(0.1)));
    }

    #[test]
    fn a_source_label_with_a_slash_is_looked_up_as_the_stats_name_it() {
        let mut step = Step::default();
        tally(&mut step, &[], "USRP addr=1/2", &json!({ "src/USRP addr=1_2/clipPct": 3.0 }), 1.0);
        assert_eq!(step.src.clip.get(), Some(3.0));
    }

    #[test]
    fn a_control_channel_that_stops_decoding_gives_the_setting_up() {
        let decoding = vec!["a".to_string()];
        let mut quiet = BTreeMap::from([("a".to_string(), ABANDON_S - 1.0), ("b".to_string(), 100.0)]);
        assert_eq!(went_quiet(&quiet, &decoding), None, "b wasn't decoding to begin with");
        quiet.insert("a".into(), ABANDON_S);
        assert_eq!(went_quiet(&quiet, &decoding).as_deref(), Some("control channel lost: a"));
    }

    #[test]
    fn systems_are_on_the_radio_whose_band_has_their_control_channel() {
        let status = json!({ "status": { "systems": [
            { "shortName": "In", "sites": [{ "name": "In", "controlChannelHz": 856_000_000.0 }] },
            // 856.5 ± 5 MHz, less 75 kHz at each edge: up to 861.425.
            { "shortName": "Edge", "sites": [{ "name": "", "controlChannelHz": 861_450_000.0 }] },
            { "shortName": "Out", "sites": [{ "name": "Out", "controlChannelHz": 770_000_000.0 }] },
            { "shortName": "Two", "sites": [{ "name": "A", "controlChannelHz": 852_000_000.0 }, { "name": "B", "controlChannelHz": 771_000_000.0 }] },
        ] } });
        let src = json!({ "centerHz": 856_500_000.0, "rateHz": 10_000_000.0, "settings": { "guardHz": 75_000 } });
        let w = systems_on(&status, &src);
        assert_eq!(w.iter().map(|w| w.short_name.as_str()).collect::<Vec<_>>(), vec!["In", "Two"], "861.45 is in the guard band");
        assert_eq!(w[1].cc_keys, vec!["sys/Two/site/A".to_string()]);
    }

    #[test]
    fn urls() {
        assert_eq!(ws_url("http://10.1.100.43:8080").unwrap(), "ws://10.1.100.43:8080/api/ws");
        assert_eq!(ws_url("10.1.100.43").unwrap(), "ws://10.1.100.43:8080/api/ws");
        assert_eq!(ws_url("http://host:9000/setup/").unwrap(), "ws://host:9000/api/ws");
        assert!(ws_url("https://host").is_err());
    }

    /// A smooth hill: best at LNA 9, mixer 12, VGA 4.
    fn hill(p: &[usize]) -> f64 {
        let d = |a: usize, b: f64| (a as f64 - b).powi(2);
        100.0 - d(p[0], 9.0) - d(p[1], 12.0) - d(p[2], 4.0)
    }

    #[test]
    fn the_search_climbs_one_stage_at_a_time_to_the_best() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let mut measured = 0;
        let found = search(&k.axes, vec![14, 7, 7], 0.5, 60, &mut |p| {
            measured += 1;
            Ok(Some(hill(p)))
        })
        .unwrap();
        assert_eq!(found.best, vec![9, 12, 4]);
        assert_eq!(found.best_score, 100.0);
        assert!(!found.out_of_steps);
        assert!(measured < 40, "{measured} settings: each is seen once");
        assert_eq!(found.tried.len(), measured);
    }

    #[test]
    fn a_refused_setting_isnt_moved_to_and_the_budget_is_kept() {
        let k = Knobs::of(&airspy(), None).unwrap();
        // Above LNA 11 it clips: refused, though the hill would like more.
        let found = search(&k.axes, vec![8, 12, 4], 0.5, 60, &mut |p| Ok(if p[0] > 11 { None } else { Some(100.0 - (p[0] as f64 - 14.0).powi(2)) })).unwrap();
        assert_eq!(found.best[0], 11);
        let few = search(&k.axes, vec![14, 7, 7], 0.5, 4, &mut |p| Ok(Some(hill(p)))).unwrap();
        assert!(few.out_of_steps);
        assert_eq!(few.tried.len(), 4);
    }

    #[test]
    fn noise_within_the_margin_doesnt_move_it() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let found = search(&k.axes, vec![10, 10, 10], 2.0, 60, &mut |p| Ok(Some(80.0 + if p == [10, 13, 10] { 1.5 } else { 0.0 }))).unwrap();
        assert_eq!(found.best, vec![10, 10, 10]);
    }

    #[test]
    fn it_stops_when_interrupted_and_when_the_start_cant_be_measured() {
        let k = Knobs::of(&airspy(), None).unwrap();
        let mut n = 0;
        let r = search(&k.axes, vec![10, 10, 10], 1.0, 60, &mut |_| {
            n += 1;
            if n == 3 {
                Err(Stop::Interrupted("ctrl-c".into()))
            } else {
                Ok(Some(50.0))
            }
        });
        assert_eq!(r.unwrap_err(), Stop::Interrupted("ctrl-c".into()));
        assert!(search(&k.axes, vec![10, 10, 10], 1.0, 60, &mut |_| Ok(None)).is_err());
    }
}
