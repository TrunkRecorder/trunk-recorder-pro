//! Checking which sites of a RadioReference sites export can be heard from
//! here ([`trunk_core::sitecheck`]), with a spare radio, while the recorder
//! keeps recording with the others: `trunk-pro check-sites`, and the same as
//! a background job the interface starts (Setup → Systems → Check sites).
//! Writes the export without the sites heard nothing of, and a report of what
//! each frequency showed, after every window (a long run keeps what it has).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_app::config::Source;
use trunk_app::protocol::{body, ToClient};
use trunk_core::sitecheck::{self, duration, judge_all, miles, plan, FreqReport, ParallelCheck, Quality, SitesFile, SystemType, Verdict, Window};

use crate::runtime::{self, Ctx};
use crate::sdr::{Control, SourceMsg};

/// Air dropped after each retune (the tuner settling, samples already queued).
const SETTLE_S: f64 = 0.25;
/// What a window costs beyond its listening (retune, settling, the source's buffers).
const WINDOW_EXTRA_S: f64 = SETTLE_S + 0.5;
/// How long a radio that reports errors may take to open before the check gives up.
const OPEN_GRACE_S: u64 = 15;

/// The spare radio: its kind ("rtlsdr", "airspy", "bladerf", "soapy",
/// "usrp"), serial or device arguments, and optional rate and gain.
#[derive(Clone, Debug, Default)]
pub struct RadioPick {
    pub kind: String,
    pub id: String,
    pub rate_hz: Option<f64>,
    pub gain: Option<f64>,
    pub ppm: f64,
}

impl RadioPick {
    fn default_rate(&self) -> f64 {
        match self.kind.as_str() {
            "airspy" => 6_000_000.0,
            "bladerf" | "usrp" => 8_000_000.0,
            "soapy" => 2_000_000.0,
            _ => 2_400_000.0,
        }
    }

    /// The source this opens, tuned to `center_hz`.
    pub fn source(&self, center_hz: f64) -> Result<(Source, f64), String> {
        let kind = match self.kind.as_str() {
            "rtl" | "rtlsdr" => "rtlsdr",
            k @ ("airspy" | "bladerf" | "soapy" | "usrp") => k,
            k => return Err(format!("unknown radio kind \"{k}\" (rtlsdr, airspy, bladerf, soapy or usrp)")),
        };
        let rate = self.rate_hz.filter(|r| *r > 0.0).unwrap_or_else(|| self.default_rate());
        if !(250_000.0..=61_440_000.0).contains(&rate) {
            return Err(format!("a sample rate of {:.2} MS/s is outside what any of these radios takes (0.25–61.44)", rate / 1e6));
        }
        let id = if self.id == "1" { "" } else { self.id.trim() };
        let mut v = json!({ "type": kind, "centerHz": center_hz, "rateHz": rate, "ppm": self.ppm });
        match kind {
            "soapy" | "usrp" => v["args"] = json!(id),
            _ => v["serial"] = json!(id),
        }
        match (kind, self.gain) {
            ("airspy", Some(g)) => {
                v["gainMode"] = json!("linearity");
                v["gainStep"] = json!(g.round().clamp(0.0, 21.0) as u8);
            }
            ("airspy", None) => {}
            ("rtlsdr" | "soapy", None) => v["agc"] = json!(true),
            (_, Some(g)) => v["gainDb"] = json!(g),
            (_, None) => {}
        }
        if kind == "rtlsdr" {
            v["ppm"] = json!(self.ppm.round() as i32);
        }
        let s: Source = serde_json::from_value(v).map_err(|e| e.to_string())?;
        Ok((s, rate))
    }
}

/// "RTL-SDR 00000002", "Airspy 3A…", for the header.
fn radio_label(s: &Source) -> String {
    let v = serde_json::to_value(s).unwrap_or_default();
    let kind = match v["type"].as_str().unwrap_or("") {
        "rtlsdr" => "RTL-SDR",
        "airspy" => "Airspy",
        "bladerf" => "bladeRF",
        "soapy" => "SoapySDR",
        "usrp" => "USRP",
        "file" => "capture",
        k => return k.to_string(),
    };
    let id = v["serial"].as_str().or(v["args"].as_str()).or(v["path"].as_str()).unwrap_or("");
    if id.is_empty() {
        format!("{kind} (the first found)")
    } else {
        format!("{kind} {id}")
    }
}

/// The gain the source opens with: "AGC", "30 dB", "linearity step 14".
fn gain_label(s: &Source) -> String {
    let v = serde_json::to_value(s).unwrap_or_default();
    match v["type"].as_str().unwrap_or("") {
        "file" => "—".into(),
        "airspy" => match (v["gainMode"].as_str(), v["gainStep"].as_u64()) {
            (Some(m), Some(n)) => format!("{m} step {n}"),
            _ => "the driver's default".into(),
        },
        _ if v["agc"].as_bool() == Some(true) => "AGC (automatic)".into(),
        _ => v["gainDb"].as_f64().map_or("the driver's default".into(), |g| format!("{g:.1} dB")),
    }
}

/// Whether `pick` is one of the recorder's own radios (two programs can't open one).
fn in_use(pick: &Source, sources: &[Source]) -> Option<usize> {
    let p = serde_json::to_value(pick).unwrap_or_default();
    let key = |v: &Value| (v["type"].as_str().unwrap_or("").to_string(), v["serial"].as_str().or(v["args"].as_str()).unwrap_or("").trim().to_string());
    let (pk, pid) = key(&p);
    sources.iter().position(|s| {
        let (k, id) = key(&serde_json::to_value(s).unwrap_or_default());
        // An empty id opens the first found: possibly the recorder's.
        k == pk && (id == pid || id.is_empty() || pid.is_empty())
    })
}

/// "12h", "90m", "3600s", "3600", "1.5 h" → seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let t = s.trim().to_lowercase();
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let n: f64 = t[..split].trim().parse().ok()?;
    let mult = match t[split..].trim() {
        "" | "s" | "sec" | "secs" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        "d" | "day" | "days" => 86400.0,
        _ => return None,
    };
    (n > 0.0).then_some(n * mult)
}

/// "41.7,-73.9" → (lat, lon).
pub fn parse_near(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.split_once(',')?;
    let (lat, lon) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// What to check, and how.
pub struct Plan {
    pub ty: SystemType,
    pub file: SitesFile,
    /// Listening per window, per pass.
    pub listen_s: f64,
    /// A long run: keep going round the windows this long (None: one pass).
    pub for_s: Option<f64>,
    /// Each site's distance from where the user is (None: no location given or the row has none).
    pub miles: Vec<Option<f64>>,
    /// Each site: within range (all, without `--within`).
    pub near: Vec<bool>,
    pub within: Option<f64>,
    pub freqs: Vec<f64>,
    pub windows: Vec<Window>,
    pub rate: f64,
    pub src: Source,
    /// A capture: one window, as fast as it plays.
    pub fixed: bool,
    /// Something to say about how it was set up (the rate lowered).
    pub note: Option<String>,
    /// How many CPU cores a window is listened with.
    pub cores: usize,
}

/// Cores for listening: half the computer's (the recorder needs the rest), 1–8.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get() / 2).clamp(1, 8)
}

impl Plan {
    /// `origin` and `within` (miles) leave out the sites farther away.
    #[allow(clippy::too_many_arguments)]
    pub fn new(ty: SystemType, file: SitesFile, listen_s: f64, for_s: Option<f64>, origin: Option<(f64, f64)>, within: Option<f64>, radio: Result<&RadioPick, (Source, f64)>) -> Result<Plan, String> {
        let dist: Vec<Option<f64>> = file.sites.iter().map(|s| Some(miles(origin?, s.at?))).collect();
        let near: Vec<bool> = dist.iter().map(|d| match (within, d) {
            (Some(w), Some(d)) => *d <= w,
            _ => true,
        }).collect();
        let mut freqs: Vec<f64> = file.sites.iter().zip(&near).filter(|(_, n)| **n).flat_map(|(s, _)| s.checked(ty)).collect();
        freqs.sort_by(f64::total_cmp);
        freqs.dedup();
        let mut note = None;
        let (src, rate, windows, fixed) = match radio {
            Ok(pick) => {
                let (_, asked) = pick.source(0.0)?;
                let mut pick = pick.clone();
                // A radio that runs at any rate, asked for more than it takes to see them all at once:
                // the least that does (the work grows with the rate).
                if matches!(pick.kind.as_str(), "bladerf" | "usrp") && plan(&freqs, asked).len() == 1 {
                    let least = (4..=(asked / 500_000.0) as u32).map(|k| k as f64 * 500_000.0).find(|r| plan(&freqs, *r).len() == 1);
                    if let Some(r) = least.filter(|r| *r < asked * 0.9) {
                        note = Some(format!("{:.1} MS/s lowered to {:.1}: enough to see every frequency at once, and less for the computer to do", asked / 1e6, r / 1e6));
                        pick.rate_hz = Some(r);
                    }
                }
                let (_, rate) = pick.source(0.0)?;
                let w = plan(&freqs, rate);
                let first = w.first().map_or(0.0, |w| w.center_hz);
                let (s, rate) = pick.source(first)?;
                (s, rate, w, false)
            }
            Err((s, rate)) => {
                let center = match &s {
                    Source::File { center_hz, .. } => *center_hz,
                    _ => 0.0,
                };
                let half = rate * trunk_core::survey::USABLE / 2.0;
                let inside: Vec<f64> = freqs.iter().copied().filter(|f| (f - center).abs() <= half).collect();
                (s, rate, vec![Window { center_hz: center, freqs: inside }], true)
            }
        };
        if freqs.is_empty() {
            return Err(if near.iter().any(|n| *n) { "no frequencies to check".into() } else { format!("no sites within {} miles", within.unwrap_or(0.0)) });
        }
        Ok(Plan { ty, file, listen_s, for_s, miles: dist, near, within, freqs, windows, rate, src, fixed, note, cores: cores() })
    }

    /// One pass round every window.
    pub fn pass_s(&self) -> f64 {
        self.windows.iter().filter(|w| !w.freqs.is_empty()).count() as f64 * (self.listen_s + WINDOW_EXTRA_S)
    }

    /// About how long it all takes.
    pub fn total_s(&self) -> f64 {
        self.for_s.map_or(self.pass_s(), |f| f.max(self.pass_s()))
    }

    pub fn header(&self) -> String {
        let n_near = self.near.iter().filter(|n| **n).count();
        let range = match self.within {
            Some(w) => format!(" ({n_near} within {w:.0} miles)"),
            None => String::new(),
        };
        let time = if self.fixed {
            "as fast as the capture plays".to_string()
        } else if let Some(f) = self.for_s {
            format!("going round them for {} (a pass takes about {})", duration(f), duration(self.pass_s()))
        } else {
            format!("about {}", duration(self.pass_s()))
        };
        format!(
            "{} {} sites{range}, {} frequenc{} in {} window{} of {:.1} MHz, {} each: {time}",
            self.file.sites.len(),
            self.ty.as_str(),
            self.freqs.len(),
            if self.freqs.len() == 1 { "y" } else { "ies" },
            self.windows.len(),
            if self.windows.len() == 1 { "" } else { "s" },
            self.rate / 1e6,
            duration(self.listen_s),
        )
    }

    /// Every site's verdict and why: the far ones out of range, the rest judged together.
    pub fn verdicts(&self, heard: &BTreeMap<u64, FreqReport>) -> Vec<(Verdict, String)> {
        let near: Vec<_> = self.file.sites.iter().zip(&self.near).filter(|(_, n)| **n).map(|(s, _)| s.clone()).collect();
        let mut judged = judge_all(self.ty, &near, heard).into_iter();
        self.near
            .iter()
            .zip(&self.miles)
            .map(|(n, d)| if *n { judged.next().unwrap() } else { (Verdict::OutOfRange, format!("{:.0} miles away, not listened to", d.unwrap_or(0.0))) })
            .collect()
    }

    /// A site's strongest signal: the highest peak, dB over the band's floor, of the frequencies listened to for it.
    pub fn strength(&self, site: usize, heard: &BTreeMap<u64, FreqReport>) -> Option<f64> {
        // (A site out of range wasn't listened to for: a frequency it shares with a near one was.)
        if !self.near[site] {
            return None;
        }
        self.file.sites[site].checked(self.ty).iter().filter_map(|f| heard.get(&(*f as u64))).map(|r| r.peak_db).filter(|d| d.is_finite()).reduce(f64::max)
    }

    /// A site's best decode among the frequencies listened to for it (None: none yet).
    pub fn quality(&self, site: usize, heard: &BTreeMap<u64, FreqReport>) -> Option<Quality> {
        if !self.near[site] {
            return None;
        }
        self.file.sites[site].checked(self.ty).iter().filter_map(|f| heard.get(&(*f as u64))).map(|r| r.quality(self.ty)).max()
    }

    /// The cleaned export, and the report.
    pub fn outputs(&self, heard: &BTreeMap<u64, FreqReport>, verdicts: &[(Verdict, String)]) -> (String, String) {
        let kept = sitecheck::cleaned(&self.file, &self.file.sites.iter().zip(verdicts).map(|(s, (v, _))| (s.line, *v)).collect::<Vec<_>>());
        let mut report = String::from("Site,Miles,Frequency MHz,Control,Heard,Decode,Decoded %,Up %,Peak dB,Listened,Site verdict,Why\n");
        for ((s, (v, why)), d) in self.file.sites.iter().zip(verdicts).zip(&self.miles) {
            for (f, c) in &s.freqs {
                let r = heard.get(&(*f as u64));
                report.push_str(&format!(
                    "{},{},{:.5},{},{},{},{},{},{},{},{},{}\n",
                    cell(&s.label),
                    d.map_or(String::new(), |d| format!("{d:.0}")),
                    f / 1e6,
                    if *c { "c" } else { "" },
                    cell(&r.map_or_else(|| "not checked".to_string(), |r| r.describe())),
                    r.map_or("", |r| r.quality(self.ty).as_str()),
                    r.and_then(|r| r.decoded_share()).map_or(String::new(), |x| format!("{:.0}", x * 100.0)),
                    r.map_or(String::new(), |r| format!("{:.0}", r.on * 100.0)),
                    r.map_or(String::new(), |r| format!("{:.1}", r.peak_db)),
                    r.map_or(String::new(), |r| duration(r.listened_s)),
                    v.as_str(),
                    cell(why)
                ));
            }
        }
        (kept, report)
    }
}

/// How a run is going: after each window (`finished`), and about every second during one.
pub struct Progress<'a> {
    pub pass: usize,
    /// The window (from 0), of `windows`.
    pub window: usize,
    pub windows: usize,
    pub center_hz: f64,
    pub elapsed_s: f64,
    /// The window is done (else: how it's going so far).
    pub finished: bool,
    /// What this window's frequencies showed this time (so far).
    pub just: &'a [FreqReport],
    /// Everything before this window (finished: with it).
    pub heard: &'a BTreeMap<u64, FreqReport>,
    /// The air listened to in this window so far, s.
    pub listened_s: f64,
    /// The share of the radio's samples the computer keeps up with (None: too early to say).
    pub keep_up: Option<f64>,
}

impl Progress<'_> {
    /// Everything so far, this window's too.
    pub fn all(&self) -> BTreeMap<u64, FreqReport> {
        let mut all = self.heard.clone();
        if !self.finished {
            merge(&mut all, self.just);
        }
        all
    }
}

fn merge(heard: &mut BTreeMap<u64, FreqReport>, just: &[FreqReport]) {
    for r in just {
        match heard.get_mut(&(r.freq_hz as u64)) {
            Some(e) => e.merge(r.clone()),
            None => {
                heard.insert(r.freq_hz as u64, r.clone());
            }
        }
    }
}

/// What a run says as it goes.
pub enum Event<'a> {
    Progress(Progress<'a>),
    /// The band now (dB per bin), a few times a second, for a waterfall.
    Spectrum { center_hz: f64, bins: Vec<f32> },
}

/// Run `plan` until done (or `stop`), telling `each` as it goes.
/// Err: the radio failed (what was heard until then is in the last progress).
pub fn run(plan: &Plan, stop: &AtomicBool, mut each: impl FnMut(Event)) -> Result<(), String> {
    let ctl = Arc::new(Control::default());
    let src_stop = Arc::new(AtomicBool::new(false));
    let first = plan.windows.first().map_or(0.0, |w| w.center_hz);
    if !plan.fixed {
        *ctl.current_hz.lock().unwrap() = Some(first);
    }
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let th = runtime::spawn_source(&plan.src, 0, first, tx, src_stop.clone(), Some(ctl.clone()), "check-sites").map_err(|e| e.to_string())?;
    let mut iqf = crate::sdr::IqFrom::default();
    let mut heard: BTreeMap<u64, FreqReport> = BTreeMap::new();
    let t_start = Instant::now();
    let mut at = first;
    let mut opened = false;
    let mut result = Ok(());
    let mut last_error = String::new();
    let n = plan.windows.len();
    'passes: for pass in 0.. {
        for (k, w) in plan.windows.iter().enumerate() {
            if w.freqs.is_empty() {
                continue;
            }
            if stop.load(Ordering::Relaxed) {
                break 'passes;
            }
            // Tuned there: the source says so (the first window: its first samples).
            let retune = !plan.fixed && opened && w.center_hz != at;
            let mut tuned = !retune;
            if retune {
                *ctl.freq_hz.lock().unwrap() = Some(w.center_hz);
                at = w.center_hz;
            }
            let mut check = ParallelCheck::new(w, plan.rate, plan.listen_s, Some(plan.ty), plan.cores);
            let mut skip = if plan.fixed || !retune && opened { 0 } else { (SETTLE_S * plan.rate) as usize };
            let t0 = Instant::now();
            // When samples started coming (how well the computer keeps up is measured from there).
            let mut flowing: Option<Instant> = None;
            let (mut last_live, mut last_spec) = (Instant::now(), Instant::now());
            let mut ended = false;
            while !check.done() && !stop.load(Ordering::Relaxed) {
                let m = match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(m) => iqf.iq(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // A radio that never opened, and said why: no use waiting.
                        if !opened && !last_error.is_empty() && t_start.elapsed() > Duration::from_secs(OPEN_GRACE_S) {
                            result = Err(last_error.clone());
                            ended = true;
                            break;
                        }
                        if flowing.is_none_or(|f| f.elapsed() > Duration::from_secs(30)) && t0.elapsed() > Duration::from_secs_f64(plan.listen_s * 3.0 + 30.0) && check.listened_s() == 0.0 {
                            result = Err(if last_error.is_empty() { "the radio stopped sending samples".to_string() } else { last_error.clone() });
                            ended = true;
                            break;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        ended = true;
                        break;
                    }
                };
                match m {
                    // (Only this asks for retunes: the confirmation is this one's, wherever the tuner landed.)
                    SourceMsg::Tuned { .. } => tuned = true,
                    SourceMsg::Data { bytes, .. } if tuned => {
                        opened = true;
                        flowing.get_or_insert_with(Instant::now);
                        let drop = (skip * 2).min(bytes.len());
                        skip -= drop / 2;
                        check.push_u8(&bytes[drop..]);
                    }
                    SourceMsg::Iq { samples, .. } if tuned => {
                        opened = true;
                        flowing.get_or_insert_with(Instant::now);
                        let drop = skip.min(samples.len());
                        skip -= drop;
                        check.push_iq(&samples[drop..]);
                    }
                    SourceMsg::Error { error, .. } => last_error = error,
                    SourceMsg::End { .. } => {
                        if !last_error.is_empty() && !opened {
                            result = Err(last_error.clone());
                        }
                        ended = true;
                        break;
                    }
                    _ => {}
                }
                if last_spec.elapsed() >= Duration::from_millis(250) && check.listened_s() > 0.0 {
                    last_spec = Instant::now();
                    each(Event::Spectrum { center_hz: w.center_hz, bins: check.spectrum(512) });
                }
                if last_live.elapsed() >= Duration::from_secs(1) && !plan.fixed {
                    last_live = Instant::now();
                    let listened = check.listened_s();
                    let wall = flowing.map_or(0.0, |f| f.elapsed().as_secs_f64());
                    let keep_up = (wall >= 5.0).then(|| (listened / wall).min(1.0));
                    let just: Vec<FreqReport> = check.reports().into_iter().filter(|r| r.listened_s > 0.0).collect();
                    each(Event::Progress(Progress { pass, window: k, windows: n, center_hz: w.center_hz, elapsed_s: t_start.elapsed().as_secs_f64(), finished: false, just: &just, heard: &heard, listened_s: listened, keep_up }));
                }
            }
            // (A stop part way: what this window heard until then still counts.)
            let just: Vec<FreqReport> = check.reports().into_iter().filter(|r| r.listened_s > 0.0).collect();
            merge(&mut heard, &just);
            let listened = check.listened_s();
            let wall = flowing.map_or(0.0, |f| f.elapsed().as_secs_f64());
            let keep_up = (wall >= 5.0).then(|| (listened / wall).min(1.0));
            each(Event::Progress(Progress { pass, window: k, windows: n, center_hz: w.center_hz, elapsed_s: t_start.elapsed().as_secs_f64(), finished: true, just: &just, heard: &heard, listened_s: listened, keep_up }));
            if ended || plan.fixed && k + 1 == n {
                if ended && result.is_ok() && !opened && !last_error.is_empty() {
                    result = Err(last_error.clone());
                }
                break 'passes;
            }
        }
        if plan.fixed || plan.for_s.is_none_or(|f| t_start.elapsed().as_secs_f64() >= f) {
            break;
        }
    }
    src_stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = th.join();
    result
}

/// "the computer keeps up with only 35 % of the radio's samples at 60.0 MS/s: …", when it doesn't.
fn behind(plan: &Plan, keep_up: Option<f64>) -> Option<String> {
    let k = keep_up.filter(|k| *k < 0.8)?;
    Some(format!(
        "the computer keeps up with only {:.0} % of the radio's samples at {:.1} MS/s ({} core{}): the rest are missed. A lower sample rate, or fewer frequencies (a ZIP radius), helps.",
        k * 100.0,
        plan.rate / 1e6,
        plan.cores,
        if plan.cores == 1 { "" } else { "s" }
    ))
}

/// `line` coloured by how well it decoded (green, yellow, red), on a terminal.
fn paint(on: bool, q: Option<Quality>, line: &str) -> String {
    match (on, q) {
        (true, Some(Quality::Good)) => format!("\x1b[32m{line}\x1b[0m"),
        (true, Some(Quality::Marginal)) => format!("\x1b[33m{line}\x1b[0m"),
        (true, Some(Quality::Poor)) => format!("\x1b[31m{line}\x1b[0m"),
        _ => line.to_string(),
    }
}

/// `out` beside `input`: "trs_sites_7687.csv" → "trs_sites_7687-checked.csv".
fn beside(input: &Path, tag: &str) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("sites");
    input.with_file_name(format!("{stem}-{tag}.csv"))
}

fn cell(s: &str) -> String {
    if s.contains([',', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `--zip` / `--near` and `--within`, or none.
fn where_from(zip: Option<&str>, near: Option<&str>) -> Result<Option<(f64, f64)>, String> {
    match (zip.map(str::trim).filter(|z| !z.is_empty()), near.map(str::trim).filter(|z| !z.is_empty())) {
        (Some(z), _) => sitecheck::zip_location(z).map(Some).ok_or_else(|| format!("ZIP code {z} isn't one I know (5 digits, US)")),
        (None, Some(n)) => parse_near(n).map(Some).ok_or_else(|| format!("\"{n}\" isn't a place: lat,lon like 41.70,-73.92")),
        (None, None) => Ok(None),
    }
}

pub fn cli(a: &crate::Args) {
    let die = |e: String| -> ! { crate::die(&format!("check-sites: {e}")) };
    let path = a.positional.first().unwrap_or_else(|| die("which sites file? trunk-pro check-sites trs_sites_NNNN.csv --type p25 --rtl SERIAL".into()));
    let ty = a.get("type").and_then(SystemType::parse).unwrap_or_else(|| die("--type p25, smartnet, edacs, dmr or nxdn".into()));
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("{path}: {e}")));
    let file = sitecheck::parse_sites(&text).unwrap_or_else(|e| die(format!("{path}: {e}")));
    let listen_s = a.num("seconds", ty.default_listen_s());
    let for_s = a.get("for").map(|f| parse_duration(f).unwrap_or_else(|| die(format!("--for {f}: say how long, like 12h, 90m or 3600s"))));
    let origin = where_from(a.get("zip"), a.get("near")).unwrap_or_else(|e| die(e));
    let within = a.get("within").map(|w| w.trim_end_matches("mi").trim().parse::<f64>().unwrap_or_else(|_| die(format!("--within {w}: miles, like 40"))));
    if within.is_some() && origin.is_none() {
        die("--within needs where you are: --zip 12601 or --near 41.70,-73.92".into());
    }

    let plan = if let Some(cap) = a.get("capture") {
        let (center, rate) = (a.num("center", 0.0), a.num("rate", 0.0));
        if center <= 0.0 || rate <= 0.0 {
            die("--capture needs --center Hz and --rate Hz".into());
        }
        let fmt = crate::sample_format(cap, a.get("format"));
        let s = Source::File { path: cap.into(), center_hz: center, rate_hz: rate, realtime: false, format: Some(fmt), auto_tune: false, guard_hz: crate::config::DEFAULT_GUARD_HZ };
        Plan::new(ty, file, listen_s, None, origin, within, Err((s, rate)))
    } else if let Some(j) = a.get("radio") {
        // A source exactly as in config.json.
        let mut v: Value = serde_json::from_str(j).unwrap_or_else(|e| die(format!("--radio: {e}")));
        let rate = v["rateHz"].as_f64().unwrap_or_else(|| die("--radio needs a rateHz".into()));
        let probe: Source = serde_json::from_value(v.clone()).unwrap_or_else(|e| die(format!("--radio: {e}")));
        let p0 = Plan::new(ty, file, listen_s, for_s, origin, within, Err((probe, rate))).unwrap_or_else(|e| die(e));
        let w = plan(&p0.freqs, rate);
        v["centerHz"] = json!(w.first().map_or(0.0, |w| w.center_hz));
        let src: Source = serde_json::from_value(v).unwrap_or_else(|e| die(format!("--radio: {e}")));
        Ok(Plan { windows: w, src, fixed: false, ..p0 })
    } else {
        let (kind, id) = [("rtl", "rtlsdr"), ("airspy", "airspy"), ("bladerf", "bladerf"), ("soapy", "soapy"), ("usrp", "usrp")]
            .iter()
            .find_map(|(flag, kind)| a.get(flag).map(|id| (kind.to_string(), id.to_string())))
            .unwrap_or_else(|| die("which radio? --rtl SERIAL, --airspy SERIAL, --bladerf SERIAL, --soapy \"driver=…\", --usrp \"serial=…\" or --radio '{…}' (a source as in config.json)".into()));
        let pick = RadioPick { kind, id, rate_hz: a.get("rate").and_then(|r| r.parse().ok()), gain: a.get("gain").and_then(|g| g.parse().ok()), ppm: a.num("ppm", 0.0) };
        Plan::new(ty, file, listen_s, for_s, origin, within, Ok(&pick))
    }
    .unwrap_or_else(|e| die(e));

    let input = Path::new(path);
    let out = a.get("out").map(PathBuf::from).unwrap_or_else(|| beside(input, "checked"));
    let report_path = a.get("report").map(PathBuf::from).unwrap_or_else(|| beside(input, "report"));
    eprintln!("check-sites: radio {}, gain {}", radio_label(&plan.src), gain_label(&plan.src));
    if let (Some(o), Some(w)) = (origin, within) {
        eprintln!("check-sites: within {w:.0} miles of {:.4}, {:.4}", o.0, o.1);
        for ((s, d), n) in plan.file.sites.iter().zip(&plan.miles).zip(&plan.near) {
            if !n {
                eprintln!("  out of range  {:>4.0} mi  {}", d.unwrap_or(0.0), s.label);
            }
        }
    }
    eprintln!("check-sites: {}", plan.header());
    if plan.for_s.is_some() {
        eprintln!("check-sites: the files are saved after every window; Ctrl+C any time keeps what was heard so far");
    }
    let save = |heard: &BTreeMap<u64, FreqReport>| -> Vec<(Verdict, String)> {
        let v = plan.verdicts(heard);
        let (kept, report) = plan.outputs(heard, &v);
        if let Err(e) = std::fs::write(&out, kept) {
            die(format!("{}: {e}", out.display()));
        }
        if let Err(e) = std::fs::write(&report_path, report) {
            die(format!("{}: {e}", report_path.display()));
        }
        v
    };
    use std::io::IsTerminal;
    let color_err = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let color_out = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut last = BTreeMap::new();
    let stop = AtomicBool::new(false);
    if let Some(n) = &plan.note {
        eprintln!("check-sites: {n}");
    }
    let mut warned = usize::MAX;
    let r = run(&plan, &stop, |e| {
        let p = match e {
            Event::Progress(p) => p,
            Event::Spectrum { .. } => return,
        };
        if !p.finished {
            if let Some(b) = behind(&plan, p.keep_up).filter(|_| warned != p.window) {
                eprintln!("check-sites: {b}");
                warned = p.window;
            }
            return;
        }
        for r in p.just {
            let line = format!("  {:>10.5} MHz  {:>3.0} % up  {:>5.1} dB  {:<8}  {}", r.freq_hz / 1e6, r.on * 100.0, r.peak_db, r.quality(plan.ty).as_str(), r.describe());
            eprintln!("{}", paint(color_err, Some(r.quality(plan.ty)), &line));
        }
        if p.just.is_empty() {
            return;
        }
        let pass = if plan.for_s.is_some() { format!("pass {}, ", p.pass + 1) } else { String::new() };
        let left = (plan.total_s() - p.elapsed_s).max(0.0);
        eprintln!("check-sites: {pass}window {} of {} done ({:.2} MHz), {} in, about {} left", p.window + 1, p.windows, p.center_hz / 1e6, duration(p.elapsed_s), duration(left));
        save(p.heard);
        last = p.heard.clone();
    });
    if let Err(e) = r {
        eprintln!("check-sites: radio: {e}");
    }

    let verdicts = save(&last);
    println!();
    for (i, (s, (v, why))) in plan.file.sites.iter().zip(&verdicts).enumerate() {
        let db = plan.strength(i, &last).map_or("     ".to_string(), |d| format!("{d:>3.0} dB"));
        let q = if v.keep() || *v == Verdict::Inactive || *v == Verdict::OtherSite { plan.quality(i, &last) } else { None };
        let line = format!("{:<13} {db}  {:<8}  {}  — {}", v.as_str(), q.map_or("", Quality::as_str), s.label, why);
        println!("{}", paint(color_out, q, &line));
    }
    let dropped = verdicts.iter().filter(|(v, _)| !v.keep()).count();
    println!();
    println!("{} of {} sites kept ({dropped} dropped: inactive, another site or out of range) → {}", plan.file.sites.len() - dropped, plan.file.sites.len(), out.display());
    println!("What each frequency showed → {}", report_path.display());
}

// ── the background job (the interface's Check sites) ─────────────────────────

/// A check running beside the recorder.
pub struct Job {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The message a browser gets when no check has run.
pub fn idle() -> ToClient {
    ToClient::SiteCheck(body(json!({ "state": "idle" })))
}

/// `siteCheckStart`: what the interface sent, as a plan.
pub(crate) fn plan_from(b: &Value, sources: &[Source]) -> Result<(Plan, String), String> {
    let ty = b["systemType"].as_str().and_then(SystemType::parse).ok_or("Pick the kind of system.")?;
    let csv = b["csv"].as_str().unwrap_or("");
    if csv.trim().is_empty() {
        return Err("Choose the system's sites file (RadioReference: Download → Sites).".into());
    }
    let file = sitecheck::parse_sites(csv)?;
    let name = b["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("sites.csv").to_string();
    let r = &b["radio"];
    let pick = RadioPick {
        kind: r["kind"].as_str().unwrap_or("").to_string(),
        id: r["id"].as_str().unwrap_or("").to_string(),
        rate_hz: r["rateHz"].as_f64(),
        gain: r["gain"].as_f64(),
        ppm: r["ppm"].as_f64().unwrap_or(0.0),
    };
    if pick.kind.is_empty() {
        return Err("Pick the spare radio to check with.".into());
    }
    let listen_s = b["seconds"].as_f64().filter(|s| *s > 0.0).unwrap_or(ty.default_listen_s());
    let for_s = b["forS"].as_f64().filter(|s| *s > 0.0);
    let origin = where_from(b["zip"].as_str(), b["near"].as_str())?;
    let within = b["withinMiles"].as_f64().filter(|w| *w > 0.0 && origin.is_some());
    let plan = Plan::new(ty, file, listen_s, for_s, origin, within, Ok(&pick))?;
    if let Some(i) = in_use(&plan.src, sources) {
        return Err(format!(
            "That radio is source {} of the recorder (or could be: give its serial) — two programs can't open one radio. Pick a spare one.",
            i + 1
        ));
    }
    Ok((plan, name))
}

/// The snapshot browsers are sent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn snapshot(plan: &Plan, name: &str, state: &str, heard: &BTreeMap<u64, FreqReport>, at: Option<&At>, elapsed_s: f64, error: Option<&str>, saved: &[PathBuf]) -> Value {
    let verdicts = plan.verdicts(heard);
    let (kept, report) = plan.outputs(heard, &verdicts);
    let dropped = verdicts.iter().filter(|(v, _)| !v.keep()).count();
    let freqs: Vec<Value> = plan
        .freqs
        .iter()
        .map(|f| match heard.get(&(*f as u64)) {
            Some(r) => json!({ "freqHz": f, "listenedS": r.listened_s, "on": r.on, "peakDb": r.peak_db, "heard": r.describe(), "quality": r.quality(plan.ty).as_str(), "decodedShare": r.decoded_share() }),
            None => json!({ "freqHz": f, "listenedS": 0.0, "on": 0.0, "peakDb": null, "heard": null, "quality": null, "decodedShare": null }),
        })
        .collect();
    let sites: Vec<Value> = plan
        .file
        .sites
        .iter()
        .zip(&verdicts)
        .zip(&plan.miles)
        .enumerate()
        .map(|(i, ((s, (v, why)), d))| json!({ "label": s.label, "verdict": v.as_str(), "keep": v.keep(), "why": why, "miles": d, "peakDb": plan.strength(i, heard), "quality": plan.quality(i, heard).map(Quality::as_str), "freqsHz": s.checked(plan.ty) }))
        .collect();
    let (pass, window, windows) = at.map_or((0, 0, plan.windows.len()), |a| (a.pass, a.window, a.windows));
    // How far along: a long run by the clock, one pass by the air listened to.
    let counted = plan.windows.iter().filter(|w| !w.freqs.is_empty()).count().max(1) as f64;
    let keep_up = at.and_then(|a| a.keep_up);
    let (progress, left) = match (plan.for_s, at) {
        (Some(f), _) => ((elapsed_s / f).min(1.0), (f - elapsed_s).max(0.0)),
        (None, Some(a)) => {
            let done = (a.done_windows as f64 * plan.listen_s + a.listened_s) / (counted * plan.listen_s);
            let air_left = (counted * plan.listen_s - a.done_windows as f64 * plan.listen_s - a.listened_s).max(0.0);
            (done.min(1.0), air_left / keep_up.unwrap_or(1.0).max(0.05) + (counted - a.done_windows as f64 - 1.0).max(0.0) * WINDOW_EXTRA_S)
        }
        (None, None) => (0.0, plan.total_s()),
    };
    json!({
        "state": state,
        "name": name,
        "systemType": plan.ty.as_str().to_lowercase(),
        "radio": radio_label(&plan.src),
        "gain": gain_label(&plan.src),
        "rateHz": plan.rate,
        "listenS": plan.listen_s,
        "forS": plan.for_s,
        "summary": plan.header(),
        "pass": pass,
        "window": window,
        "windows": windows,
        "elapsedS": elapsed_s,
        "totalS": plan.total_s(),
        "progress": progress,
        "leftS": if state == "running" { left } else { 0.0 },
        "keepUp": keep_up,
        "behind": if state == "running" { behind(plan, keep_up) } else { None },
        "note": plan.note,
        "cores": plan.cores,
        "error": error,
        "freqs": freqs,
        "sites": sites,
        "kept": plan.file.sites.len() - dropped,
        "dropped": dropped,
        "checkedCsv": kept,
        "reportCsv": report,
        "savedTo": saved.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
}

/// Where a run is, for the snapshot.
pub(crate) struct At {
    pub pass: usize,
    /// The window being listened to, from 1 (or just done).
    pub window: usize,
    pub windows: usize,
    /// Windows of this pass done.
    pub done_windows: usize,
    pub listened_s: f64,
    pub keep_up: Option<f64>,
}

fn publish(ctx: &Ctx, v: Value) {
    let m = ToClient::SiteCheck(body(v));
    *ctx.sitecheck_last.lock().unwrap() = Some(m.clone());
    runtime::publish(&ctx.hub, m);
}

/// Start a check from the interface (one at a time): beside the recording, on a radio it doesn't use.
pub fn start(ctx: Arc<Ctx>, b: Value) -> Result<(), String> {
    let mut slot = ctx.sitecheck.lock().unwrap();
    if slot.as_ref().is_some_and(Job::running) {
        return Err("A site check is already running — stop it first.".into());
    }
    let sources = ctx.config.lock().unwrap().sources.clone();
    let (plan, name) = plan_from(&b, &sources)?;
    let dir = crate::paths::data_dir().join("site-checks");
    let stem = Path::new(&name).file_stem().and_then(|s| s.to_str()).unwrap_or("sites").replace(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)), "_");
    let files = [dir.join(format!("{stem}-checked.csv")), dir.join(format!("{stem}-report.csv"))];
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let ctx2 = ctx.clone();
    publish(&ctx, snapshot(&plan, &name, "running", &BTreeMap::new(), None, 0.0, None, &[]));
    let thread = std::thread::Builder::new()
        .name("site-check".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&dir);
            let save = |heard: &BTreeMap<u64, FreqReport>| {
                let v = plan.verdicts(heard);
                let (kept, report) = plan.outputs(heard, &v);
                let ok = std::fs::write(&files[0], kept).and_then(|_| std::fs::write(&files[1], report));
                if let Err(e) = ok {
                    log::warn!("site check: saving {}: {e}", dir.display());
                }
            };
            let mut last = BTreeMap::new();
            let mut at: Option<At> = None;
            let mut elapsed = 0.0;
            let mut tick = Instant::now() - Duration::from_secs(10);
            // Which window of the pass each is (empty ones are skipped).
            let nth: Vec<usize> = plan.windows.iter().scan(0, |n, w| { let i = *n; if !w.freqs.is_empty() { *n += 1; } Some(i) }).collect();
            let counted = plan.windows.iter().filter(|w| !w.freqs.is_empty()).count();
            let r = run(&plan, &stop2, |e| {
                let p = match e {
                    Event::Spectrum { center_hz, bins } => {
                        runtime::publish(&ctx2.hub, ToClient::SiteCheckSpectrum { center_hz, rate_hz: plan.rate, bins });
                        return;
                    }
                    Event::Progress(p) => p,
                };
                let all = p.all();
                if p.finished {
                    save(&all);
                }
                last = all;
                at = Some(At { pass: p.pass, window: nth[p.window] + 1, windows: counted, done_windows: nth[p.window] + usize::from(p.finished), listened_s: if p.finished { 0.0 } else { p.listened_s }, keep_up: p.keep_up });
                elapsed = p.elapsed_s;
                // (At most a snapshot a second, and at each window's end.)
                if p.finished || tick.elapsed() >= Duration::from_secs(1) {
                    tick = Instant::now();
                    publish(&ctx2, snapshot(&plan, &name, "running", &last, at.as_ref(), p.elapsed_s, None, &files));
                }
            });
            save(&last);
            let (state, err) = match &r {
                Err(e) => ("error", Some(e.as_str())),
                Ok(()) if stop2.load(Ordering::Relaxed) => ("stopped", None),
                Ok(()) => ("done", None),
            };
            log::info!("site check of {name}: {state}");
            publish(&ctx2, snapshot(&plan, &name, state, &last, at.as_ref(), elapsed, err, &files));
        })
        .map_err(|e| e.to_string())?;
    if let Some(old) = slot.replace(Job { stop, thread: Some(thread) }) {
        old.stop();
    }
    Ok(())
}

/// Stop the running check (it keeps and saves what it heard).
pub fn stop(ctx: &Ctx) {
    let job = ctx.sitecheck.lock().unwrap().take();
    if let Some(j) = job {
        j.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_places_read() {
        assert_eq!(parse_duration("12h"), Some(43200.0));
        assert_eq!(parse_duration("90m"), Some(5400.0));
        assert_eq!(parse_duration("3600s"), Some(3600.0));
        assert_eq!(parse_duration("3600"), Some(3600.0));
        assert_eq!(parse_duration("1.5 h"), Some(5400.0));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_near("41.70,-73.92"), Some((41.70, -73.92)));
        assert_eq!(parse_near("141,0"), None);
    }

    #[test]
    fn a_radio_the_recorder_uses_is_refused() {
        let rtl = |s: &str| RadioPick { kind: "rtl".into(), id: s.into(), ..Default::default() }.source(851e6).unwrap().0;
        let mine = vec![rtl("00000001")];
        assert_eq!(in_use(&rtl("00000001"), &mine), Some(0));
        assert_eq!(in_use(&rtl("00000002"), &mine), None);
        assert_eq!(in_use(&rtl(""), &mine), Some(0), "the first found could be the recorder's");
        let air = RadioPick { kind: "airspy".into(), id: "ABC".into(), gain: Some(14.0), ..Default::default() }.source(851e6).unwrap().0;
        assert_eq!(in_use(&air, &mine), None);
        assert_eq!(gain_label(&air), "linearity step 14");
        assert_eq!(gain_label(&rtl("1")), "AGC (automatic)");
        assert_eq!(radio_label(&rtl("00000002")), "RTL-SDR 00000002");
    }

    #[test]
    fn far_sites_are_left_out() {
        let file = sitecheck::parse_sites(
            "Site Dec,Site Hex,Description,County Name,Lat,Lon,Range,Frequencies\n\
001,1,\"Near\",\"Dutchess\",41.70,-73.92,20,464.937500c\n\
002,2,\"Far\",\"Erie\",42.88,-78.87,20,463.100000c\n\
003,3,\"Nowhere\",\"\",,,20,462.100000c\n",
        )
        .unwrap();
        let pick = RadioPick { kind: "rtl".into(), id: "2".into(), ..Default::default() };
        let p = Plan::new(SystemType::Dmr, file, 120.0, Some(43200.0), sitecheck::zip_location("12601"), Some(40.0), Ok(&pick)).unwrap();
        assert_eq!(p.near, [true, false, true], "a site without a location is kept");
        assert_eq!(p.freqs, [462_100_000.0, 464_937_500.0]);
        assert_eq!(p.total_s(), 43200.0);
        let v = p.verdicts(&BTreeMap::new());
        assert_eq!(v[1].0, Verdict::OutOfRange);
        assert!(v[1].1.starts_with("2"), "{}", v[1].1);
        assert_eq!(v[0].0, Verdict::NotChecked);
        let (kept, report) = p.outputs(&BTreeMap::new(), &v);
        assert_eq!(kept.lines().count(), 3);
        assert!(report.lines().nth(2).unwrap().contains("out of range"));
        assert!(p.header().contains("2 within 40 miles"), "{}", p.header());
    }
}
