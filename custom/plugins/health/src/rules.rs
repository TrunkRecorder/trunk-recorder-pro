//! What the figures mean: each part's state, and what to do about it.
//!
//! The rules were tuned on real boxes, and lean towards saying less: a WARN
//! or BAD needs clear evidence; anything that might be something else is
//! INFO and says "likely".

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use trunk_recorder_plugin::CallRecord;

use crate::api::{Live, Series};

// ── thresholds ───────────────────────────────────────────────────────────────
const DROP_BAD_PPM: f64 = 10.0; // dropped samples per million: above this, audio suffers
const CLIP_WARN_PCT: f64 = 0.01; // % of samples at full scale
const CLIP_BAD_PCT: f64 = 0.1;
const PEAK_WARN_DBFS: f64 = -3.0; // a radio's peak sample
const CC_WARN: f64 = 0.90; // share of control channel messages decoded
const CC_BAD: f64 = 0.70;
const LOCK_BAD: f64 = 0.90; // share of the time a control channel was locked
const RECENT_S: f64 = 600.0; // "now": the last 10 minutes
const SNR_WARN_DB: f64 = 18.0;
const SNR_BAD_DB: f64 = 12.0;
const HOT_DBFS: f64 = -28.0;
const NOISE_LIFT_DB: f64 = 8.0;
const LIFT_SNR_DB: f64 = 38.0;
const ADJ_HZ: f64 = 60_000.0; // a channel this close to a control channel hears its edge
const CLEAN_WARN: f64 = 90.0;
const CLEAN_BAD: f64 = 75.0;
const MIN_CALLS: usize = 3;
const DISK_WARN_PCT: f64 = 10.0;
const DISK_BAD_PCT: f64 = 5.0;
const CPU_WARN: f64 = 85.0;
const MEM_WARN: f64 = 90.0;
const TEMP_WARN: f64 = 85.0;
const TEMP_BAD: f64 = 95.0;
const CLOCK_WARN_MS: f64 = 100.0;
const CLOCK_BAD_MS: f64 = 500.0;
const UPLOAD_FAIL_BAD: f64 = 0.10;
const QUEUE_WARN: f64 = 20.0;
const LOAD_WARN: f64 = 0.85;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Hash)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Bad,
}

impl Level {
    pub fn word(self) -> &'static str {
        match self {
            Level::Ok => "OK",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Bad => "BAD",
        }
    }
}

/// One thing found: its level, what it is, the evidence, what to do.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    /// Stable across checks, to tell new from ongoing.
    pub key: String,
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub todo: String,
}

/// A part's state, for the summary line.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Part {
    pub ok: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub overall: Level,
    pub findings: Vec<Finding>,
    pub radios: Part,
    pub control: Part,
    pub sites: Part,
    pub plugins: Part,
    pub calls: usize,
    /// "14% disk free" and the like.
    pub notes: Vec<String>,
}

impl Report {
    /// To look at: WARN and BAD, worst first.
    pub fn problems(&self) -> Vec<&Finding> {
        let mut v: Vec<&Finding> = self.findings.iter().filter(|f| f.level >= Level::Warn).collect();
        v.sort_by(|a, b| b.level.cmp(&a.level));
        v
    }
}

#[allow(dead_code)]
pub struct Input<'a> {
    pub live: Option<&'a Live>,
    pub calls: &'a [CallRecord],
    pub skip: &'a HashSet<String>,
    pub window_s: f64,
    /// Control channels heard on the plugin's own status events: (Hz, "system/site").
    pub control_channels: &'a [(f64, String)],
    /// This plugin's id, left out of the plugins it checks.
    pub me: &'a str,
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 })
}

fn quartile(v: &[f64]) -> Option<f64> {
    if v.len() < 4 {
        return v.iter().copied().reduce(f64::min);
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(s[s.len() / 4])
}

fn mhz(hz: f64) -> String {
    let s = format!("{:.4}", hz / 1e6);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn key_part(s: &str) -> String {
    s.trim().replace('/', "_")
}

struct Radio {
    name: String,
    center: f64,
    rate: f64,
    level: Level,
    dropped: f64,
    drop_ppm: f64,
    clip_max: Option<f64>,
    peak_max: Option<f64>,
    peak_typical: Option<f64>,
    hot_always: bool,
    hot_sometimes: bool,
}

struct Channel {
    system: String,
    site: String,
    hz: f64,
    radio: String,
    calls: usize,
    signal: Option<f64>,
    noise: Option<f64>,
    snr: Option<f64>,
    clean: Option<f64>,
    level: Level,
    issues: Vec<String>,
    lifted: bool,
    hot: bool,
}

pub fn check(inp: &Input) -> Report {
    let mut f: Vec<Finding> = Vec::new();
    let empty = BTreeMap::new();
    let series: &BTreeMap<String, Series> = inp.live.map_or(&empty, |l| &l.series);
    let s = |k: &str| series.get(k);
    let skipped = |sys: &str| inp.skip.contains(&sys.to_lowercase());
    let mut notes = Vec::new();

    // ── the decoder's load ───────────────────────────────────────────────────
    if let Some(load) = inp.live.and_then(|l| l.status["load"].as_f64()) {
        if load > LOAD_WARN {
            f.push(Finding {
                key: "load".into(),
                level: Level::Warn,
                title: format!("The decoder is {:.0}% busy", load * 100.0),
                detail: "Above 100% it falls behind and audio is lost.".into(),
                todo: "Fewer or narrower radios, or a faster computer.".into(),
            });
        }
    }

    // ── radios ───────────────────────────────────────────────────────────────
    let mut radios: Vec<Radio> = Vec::new();
    let sources = inp.live.and_then(|l| l.status["sources"].as_array()).cloned().unwrap_or_default();
    for src in &sources {
        let name = src["label"].as_str().map(str::to_string).unwrap_or_else(|| format!("radio {}", src["index"]));
        let k = format!("src/{}", key_part(&name));
        let g = |n: &str| s(&format!("{k}/{n}"));
        let dropped = g("dropped").map_or(0.0, Series::total);
        let samples = g("samples").map_or(0.0, Series::total);
        let clip = g("clipPct").map(Series::his).unwrap_or_default();
        let peak = g("peak").map(Series::his).unwrap_or_default();
        let mut r = Radio {
            name: name.clone(),
            center: src["centerHz"].as_f64().unwrap_or(0.0),
            rate: src["rateHz"].as_f64().unwrap_or(0.0),
            level: Level::Ok,
            dropped,
            drop_ppm: if samples > 0.0 { dropped / samples * 1e6 } else { 0.0 },
            clip_max: clip.iter().copied().reduce(f64::max),
            peak_max: peak.iter().copied().reduce(f64::max),
            peak_typical: median(&peak),
            hot_always: false,
            hot_sometimes: false,
        };
        let mut why = Vec::new();
        if src["ended"].as_bool() == Some(true) {
            r.level = Level::Bad;
            why.push("it has stopped".to_string());
        }
        if let (Some(m), Some(set)) = (src["rateMeasured"].as_f64(), src["rateHz"].as_f64()) {
            if m > 0.0 && set > 0.0 && m < 0.98 * set {
                r.level = Level::Bad;
                why.push(format!("it delivers {:.0}% of its samples", m / set * 100.0));
            }
        }
        if !why.is_empty() {
            f.push(Finding {
                key: format!("radio-down:{name}"),
                level: Level::Bad,
                title: format!("{name}: not delivering"),
                detail: why.join("; "),
                todo: "Check it's plugged in and seen (lsusb); restart the recorder if it doesn't come back.".into(),
            });
        }
        if dropped > 0.0 {
            let lvl = if r.drop_ppm > DROP_BAD_PPM { Level::Bad } else { Level::Warn };
            r.level = r.level.max(lvl);
            f.push(Finding {
                key: format!("drops:{name}"),
                level: lvl,
                title: format!("{name}: losing samples"),
                detail: format!("{:.0} samples dropped ({:.1} per million). Lost samples sound like clicks, crackle or robotic audio on every channel it carries.", dropped, r.drop_ppm),
                todo: "Its USB link can't keep up: move it off an add-on USB card onto a motherboard port, give it a port of its own, or lower its sample rate.".into(),
            });
        }
        if let Some(e) = src["lastError"].as_str().filter(|e| !e.is_empty()) {
            r.level = r.level.max(Level::Warn);
            f.push(Finding { key: format!("radio-error:{name}"), level: Level::Warn, title: format!("{name}: an error"), detail: e.to_string(), todo: String::new() });
        }
        match (r.clip_max, r.peak_max) {
            (Some(c), _) if c >= CLIP_WARN_PCT => {
                // Said with the overload finding below.
                r.level = r.level.max(if c >= CLIP_BAD_PCT { Level::Bad } else { Level::Warn });
            }
            (_, Some(p)) if p > PEAK_WARN_DBFS => {
                let typ = r.peak_typical.unwrap_or(p);
                if typ > PEAK_WARN_DBFS {
                    r.hot_always = true;
                    r.level = r.level.max(Level::Warn);
                    f.push(Finding {
                        key: format!("hot:{name}"),
                        level: Level::Warn,
                        title: format!("{name}: running close to full scale"),
                        detail: format!("Its strongest samples sit near 0 dBFS most of the time (typically {typ:.1})."),
                        todo: "Turn its gain down a step for headroom before it clips.".into(),
                    });
                } else {
                    r.hot_sometimes = true;
                    let share = peak.iter().filter(|x| **x > PEAK_WARN_DBFS).count() as f64 / peak.len().max(1) as f64;
                    f.push(Finding {
                        key: format!("bursts:{name}"),
                        level: Level::Info,
                        title: format!("{name}: brief bursts reach full scale"),
                        detail: format!("Touched {p:.1} dBFS in {:.0}% of minutes (typically {typ:.0} dBFS): likely a strong transmitter nearby keying up.", share * 100.0),
                        todo: "Nothing to do while its calls sound clean; if audio breaks up at those moments, turn its gain down a step.".into(),
                    });
                }
            }
            _ => {}
        }
        radios.push(r);
    }
    let radio_of = |hz: f64| -> Option<&Radio> {
        radios.iter().filter(|r| r.rate > 0.0 && (hz - r.center).abs() <= r.rate / 2.0).min_by(|a, b| (hz - a.center).abs().total_cmp(&(hz - b.center).abs()))
    };

    // ── control channels ─────────────────────────────────────────────────────
    let mut cc_hz: BTreeMap<(String, String), f64> = BTreeMap::new();
    if let Some(systems) = inp.live.and_then(|l| l.status["status"]["systems"].as_array()) {
        for sy in systems {
            let sn = sy["shortName"].as_str().unwrap_or("").to_string();
            for si in sy["sites"].as_array().into_iter().flatten() {
                if let Some(hz) = si["controlChannelHz"].as_f64() {
                    cc_hz.insert((sn.clone(), si["name"].as_str().unwrap_or("").to_string()), hz);
                }
            }
        }
    }
    let mut cc_freqs: Vec<(f64, String)> = inp.control_channels.to_vec();
    cc_freqs.extend(cc_hz.iter().map(|((sy, si), hz)| (*hz, if si.is_empty() || si == sy { sy.clone() } else { format!("{sy}/{si}") })));

    let mut cc: BTreeMap<(String, String), [Option<&Series>; 3]> = BTreeMap::new();
    for (k, v) in series {
        let Some(rest) = k.strip_prefix("sys/") else { continue };
        let (rest, which) = if let Some(r) = rest.strip_suffix("/cc/good") {
            (r, 0)
        } else if let Some(r) = rest.strip_suffix("/cc/bad") {
            (r, 1)
        } else if let Some(r) = rest.strip_suffix("/cc/locked") {
            (r, 2)
        } else {
            continue;
        };
        let (sy, si) = match rest.split_once("/site/") {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (rest.to_string(), String::new()),
        };
        cc.entry((sy, si)).or_insert([None; 3])[which] = Some(v);
    }
    let mut control = Part::default();
    for ((sy, si), [good, bad, locked]) in &cc {
        if skipped(sy) {
            continue;
        }
        control.total += 1;
        let name = if si.is_empty() { sy.clone() } else { format!("{sy}/{si}") };
        let hz = cc_hz.get(&(sy.clone(), si.clone())).copied();
        let share = |g: f64, b: f64| if g + b > 0.0 { Some(g / (g + b)) } else { None };
        let (g, b) = (good.map_or(0.0, Series::total), bad.map_or(0.0, Series::total));
        let (gr, br) = (good.map_or(0.0, |x| x.total_last(RECENT_S)), bad.map_or(0.0, |x| x.total_last(RECENT_S)));
        let whole = share(g, b);
        let now = share(gr, br).or(whole);
        let per_s = good.map(|x| x.total_last(RECENT_S) / RECENT_S);
        let lock_now = locked.map(|l| {
            let n = ((RECENT_S / l.step_s.max(1.0)).ceil() as usize).max(1);
            let v: Vec<f64> = l.v.iter().rev().take(n).flatten().copied().collect();
            if v.is_empty() { 1.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
        });
        let fig = format!("{}{}", hz.map(|h| format!("{} MHz, ", mhz(h))).unwrap_or_default(), per_s.map(|p| format!("{p:.1} msg/s")).unwrap_or_default());
        let lvl_of = |x: f64| if x < CC_BAD { Level::Bad } else if x < CC_WARN { Level::Warn } else { Level::Ok };
        let mut lvl = now.map_or(Level::Ok, lvl_of);
        let mut detail = Vec::new();
        if let Some(x) = now {
            if lvl != Level::Ok {
                detail.push(format!("only {:.0}% of its messages decode ({fig})", x * 100.0));
            }
        }
        if let Some(l) = lock_now {
            if l < LOCK_BAD {
                lvl = Level::Bad;
                detail.push(format!("locked {:.0}% of the last 10 minutes", l * 100.0));
            }
        }
        if lvl >= Level::Warn {
            f.push(Finding {
                key: format!("cc:{name}"),
                level: lvl,
                title: format!("{name}: control channel decoding poorly"),
                detail: detail.join("; "),
                todo: String::new(), // filled in with its radio's below
            });
        } else {
            control.ok += 1;
            if let (Some(w), Some(n)) = (whole, now) {
                if lvl_of(w) != Level::Ok && n >= CC_WARN {
                    f.push(Finding {
                        key: format!("cc-recovered:{name}"),
                        level: Level::Info,
                        title: format!("{name}: control channel recovered"),
                        detail: format!("{:.0}% decoded over the window, {:.0}% in the last 10 minutes.", w * 100.0, n * 100.0),
                        todo: String::new(),
                    });
                }
            }
        }
    }

    // ── calls, by channel ────────────────────────────────────────────────────
    let mut by_ch: BTreeMap<(String, String, u64), Vec<&CallRecord>> = BTreeMap::new();
    for c in inp.calls.iter().filter(|c| !skipped(&c.short_name)) {
        by_ch.entry((c.short_name.clone(), c.site.clone(), c.freq)).or_default().push(c);
    }
    let radio_name = |hz: f64| radio_of(hz).map(|r| r.name.clone()).unwrap_or_else(|| format!("~{:.0} MHz", (hz / 1e7).round() * 10.0));
    let mut floors: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for ((_, _, hz), v) in &by_ch {
        floors.entry(radio_name(*hz as f64)).or_default().extend(v.iter().filter_map(|c| c.noise));
    }
    let floor: BTreeMap<String, f64> = floors.iter().filter_map(|(k, v)| quartile(v).map(|q| (k.clone(), q))).collect();
    let mut chans: Vec<Channel> = Vec::new();
    for ((sy, si, hz), v) in &by_ch {
        let hz = *hz as f64;
        let get = |g: fn(&CallRecord) -> Option<f64>| -> Vec<f64> { v.iter().filter_map(|c| g(c)).collect() };
        let (sig, noi, snr, cln) = (get(|c| c.signal), get(|c| c.noise), get(|c| c.snr), get(|c| c.clean_voice_pct));
        let mut c = Channel {
            system: sy.clone(),
            site: si.clone(),
            hz,
            radio: radio_name(hz),
            calls: v.len(),
            signal: median(&sig),
            noise: median(&noi),
            snr: median(&snr),
            clean: median(&cln),
            level: Level::Ok,
            issues: Vec::new(),
            lifted: false,
            hot: false,
        };
        if let (Some(x), true) = (c.snr, snr.len() >= MIN_CALLS) {
            if x < SNR_BAD_DB {
                c.level = Level::Bad;
                c.issues.push(format!("weak ({x:.0} dB SNR)"));
            } else if x < SNR_WARN_DB {
                c.level = Level::Warn;
                c.issues.push(format!("weak-ish ({x:.0} dB SNR)"));
            }
        }
        let edge = radio_of(hz).is_some_and(|r| (hz - r.center).abs() > 0.4 * r.rate);
        if let (Some(n), Some(fl)) = (c.noise, floor.get(&c.radio)) {
            if n - fl >= NOISE_LIFT_DB && c.snr.is_none_or(|x| x < LIFT_SNR_DB) && !edge {
                let near = cc_freqs.iter().filter(|(h, _)| (h - hz).abs() > 0.0 && (h - hz).abs() <= ADJ_HZ).min_by(|a, b| (a.0 - hz).abs().total_cmp(&(b.0 - hz).abs()));
                if let Some((h, nm)) = near {
                    c.level = c.level.max(Level::Info);
                    c.issues.push(format!("noise {:.0} dB over its radio's floor: {:.1} kHz from {nm}'s control channel ({} MHz)", n - fl, (h - hz).abs() / 1e3, mhz(*h)));
                } else {
                    c.level = c.level.max(Level::Warn);
                    c.issues.push(format!("noise {:.0} dB over its radio's floor", n - fl));
                    c.lifted = true;
                }
            }
        }
        if c.signal.is_some_and(|x| x > HOT_DBFS) {
            c.hot = true;
        }
        if let (Some(x), true) = (c.clean, cln.len() >= MIN_CALLS) {
            if x < CLEAN_BAD {
                c.level = Level::Bad;
                c.issues.push(format!("clean voice {x:.0}%"));
            } else if x < CLEAN_WARN {
                c.level = c.level.max(Level::Warn);
                c.issues.push(format!("clean voice {x:.0}%"));
            }
        }
        chans.push(c);
    }

    // ── each site's calls as a whole: one weak channel doesn't make a site weak ──
    let mut by_site: BTreeMap<(String, String), Vec<&Channel>> = BTreeMap::new();
    for c in &chans {
        by_site.entry((c.system.clone(), c.site.clone())).or_default().push(c);
    }
    let mut sites = Part::default();
    for ((sy, si), cs) in &by_site {
        sites.total += 1;
        let name = if si.is_empty() || si == sy { sy.clone() } else { format!("{sy}/{si}") };
        let snr: Vec<f64> = cs.iter().flat_map(|c| std::iter::repeat_n(c.snr, c.calls)).flatten().collect();
        let cln: Vec<f64> = cs.iter().flat_map(|c| std::iter::repeat_n(c.clean, c.calls)).flatten().collect();
        let mut lvl = Level::Ok;
        let mut why = Vec::new();
        if snr.len() >= MIN_CALLS {
            let m = median(&snr).unwrap_or(99.0);
            if m < SNR_WARN_DB {
                lvl = if m < SNR_BAD_DB { Level::Bad } else { Level::Warn };
                why.push(format!("typical SNR {m:.0} dB over {} calls", snr.len()));
            }
        }
        if cln.len() >= MIN_CALLS {
            let m = median(&cln).unwrap_or(100.0);
            if m < CLEAN_WARN {
                lvl = lvl.max(if m < CLEAN_BAD { Level::Bad } else { Level::Warn });
                why.push(format!("clean voice {m:.0}%"));
            }
        }
        let chan_notes: Vec<String> = cs.iter().filter(|c| !c.issues.is_empty()).map(|c| format!("{} MHz ({} calls): {}", mhz(c.hz), c.calls, c.issues.join(", "))).collect();
        if lvl >= Level::Warn {
            let r = radios.iter().find(|r| Some(&r.name) == cs.first().map(|c| &c.radio));
            let peaky = r.is_some_and(|r| r.peak_max.is_some_and(|p| p > PEAK_WARN_DBFS));
            let overloaded = r.is_some_and(|r| r.clip_max.is_some_and(|c| c >= CLIP_WARN_PCT));
            let mut detail = why.join(", ");
            if !chan_notes.is_empty() {
                detail += &format!(". {}", chan_notes.join("; "));
            }
            if let Some(r) = r {
                detail += &format!(". Its radio: {}", r.name);
            }
            f.push(Finding {
                key: format!("weak:{name}"),
                level: lvl,
                title: format!("{name}: weak signal"),
                detail,
                todo: if overloaded || peaky {
                    "Its radio already reaches full scale, so more gain would clip: improve the antenna or feed line for it, or filter out the strongest signal on that radio (a notch) so the gain can come up.".into()
                } else {
                    "No sign of overload: raise its radio's gain a step, or improve the antenna / feed line for that direction.".into()
                },
            });
        } else {
            sites.ok += 1;
            if !chan_notes.is_empty() {
                f.push(Finding { key: format!("channels:{name}"), level: Level::Info, title: format!("{name}: a channel to note"), detail: chan_notes.join("; "), todo: String::new() });
            }
        }
    }

    // ── each radio's channels and control channels together: overload? ───────
    let cc_bad: Vec<(String, f64)> = f.iter().filter(|x| x.key.starts_with("cc:")).filter_map(|x| {
        let name = x.key.trim_start_matches("cc:").to_string();
        let (sy, si) = name.split_once('/').map_or((name.clone(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
        cc_hz.get(&(sy, si)).map(|h| (name, *h))
    }).collect();
    for r in &radios {
        let ccs: Vec<&String> = cc_bad.iter().filter(|(_, h)| radio_of(*h).is_some_and(|x| x.name == r.name)).map(|(n, _)| n).collect();
        let lifted: Vec<&Channel> = chans.iter().filter(|c| c.radio == r.name && c.lifted && c.calls >= 2).collect();
        let hot: Vec<&Channel> = chans.iter().filter(|c| c.radio == r.name && c.hot).collect();
        let clip = r.clip_max.filter(|c| *c >= CLIP_WARN_PCT);
        let overload = clip.is_some() || (!ccs.is_empty() && (!lifted.is_empty() || !hot.is_empty()));
        if overload {
            let mut why = Vec::new();
            if let Some(c) = clip {
                why.push(format!("clipping up to {c:.2}% of samples"));
            }
            if !ccs.is_empty() {
                why.push(format!("control channels decoding poorly: {}", ccs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
            if !lifted.is_empty() {
                why.push(format!("noise lifted on {} MHz", lifted.iter().take(5).map(|c| mhz(c.hz)).collect::<Vec<_>>().join(", ")));
            }
            if let Some(m) = hot.iter().filter_map(|c| c.signal).reduce(f64::max) {
                why.push(format!("strongest channels at {m:.0} dBFS"));
            }
            let weak = f.iter().any(|x| x.key.starts_with("weak:") && x.detail.contains(&format!("Its radio: {}", r.name)));
            f.push(Finding {
                key: format!("overload:{}", r.name),
                level: if clip.is_some_and(|c| c >= CLIP_BAD_PCT) { Level::Bad } else { Level::Warn },
                title: format!("{}: overloaded by strong signals", r.name),
                detail: why.join("; "),
                todo: if weak {
                    "Gain can't fix both: use the lowest gain that keeps the weak site above ~15 dB SNR, and add a band-pass filter (or a notch for the strongest transmitter) or a directional antenna.".into()
                } else {
                    "Turn its RF gain down a step (SDRplay: RFGR up one; others: LNA / RF gain down), then check again: the control channels should recover.".into()
                },
            });
        } else if lifted.len() >= 2 {
            f.push(Finding {
                key: format!("noise:{}", r.name),
                level: Level::Info,
                title: format!("{}: noise reads high on a few channels", r.name),
                detail: format!("{} MHz. Not overload on its own (no clipping, control channels fine): likely neighbours on the next frequencies.", lifted.iter().take(5).map(|c| mhz(c.hz)).collect::<Vec<_>>().join(", ")),
                todo: "If those channels sound noisy, try one step less gain and compare.".into(),
            });
        }
        // A control channel failing with no other sign: antenna, or a far site.
        if !overload && !ccs.is_empty() && r.dropped == 0.0 {
            for x in f.iter_mut().filter(|x| x.key.starts_with("cc:") && ccs.iter().any(|n| x.key == format!("cc:{n}"))) {
                x.todo = "Check the antenna and gain; if it's only this site, it may simply be far away.".into();
            }
        }
    }
    for x in f.iter_mut().filter(|x| x.key.starts_with("cc:") && x.todo.is_empty()) {
        x.todo = "See its radio's finding.".into();
    }

    // ── the computer ─────────────────────────────────────────────────────────
    for (k, v) in series {
        let Some(n) = k.strip_prefix("plat/disk/").and_then(|r| r.strip_suffix("/freePct")) else { continue };
        let Some(pct) = v.last() else { continue };
        let free = s(&format!("plat/disk/{n}/free")).and_then(Series::last);
        let nm = if n == "spool" { "RAM spool".to_string() } else { format!("disk {n}") };
        let gb = free.map(|b| if b >= 1e9 { format!(" ({:.0} GB)", b / 1e9) } else { format!(" ({:.0} MB)", b / 1e6) }).unwrap_or_default();
        if n != "spool" {
            notes.push(format!("{nm} {pct:.0}% free{gb}"));
        }
        if pct < DISK_WARN_PCT {
            f.push(Finding {
                key: format!("disk:{n}"),
                level: if pct < DISK_BAD_PCT { Level::Bad } else { Level::Warn },
                title: format!("{nm}: only {pct:.0}% free{gb}"),
                detail: "When it fills, calls stop saving.".into(),
                todo: "Delete or move old recordings, or let the Archive settings prune them; check what's growing.".into(),
            });
        }
    }
    if let Some(typ) = s("plat/cpu").map(Series::vals).and_then(|v| median(&v)) {
        if typ > CPU_WARN {
            f.push(Finding { key: "cpu".into(), level: Level::Warn, title: format!("CPU busy ({typ:.0}% typical)"), detail: String::new(), todo: "Something else may be using it (top), or the radios' sample rates are more than this computer keeps up with.".into() });
        }
    }
    if let Some(m) = s("plat/mem").and_then(Series::last) {
        if m > MEM_WARN {
            f.push(Finding { key: "mem".into(), level: Level::Warn, title: format!("Memory {m:.0}% used"), detail: String::new(), todo: "Check what's using it (top, sorted by memory).".into() });
        }
    }
    if let Some(t) = s("plat/temp").map(Series::his).and_then(|v| v.into_iter().reduce(f64::max)) {
        if t >= TEMP_WARN {
            f.push(Finding {
                key: "temp".into(),
                level: if t >= TEMP_BAD { Level::Bad } else { Level::Warn },
                title: format!("Running hot ({t:.0} °C)"),
                detail: "A CPU that hot slows itself down and can drop samples.".into(),
                todo: "Check the fans and airflow.".into(),
            });
        }
    }

    // ── the clock: its sync and measured offset (the kernel's error bound is only a worst case) ──
    let synced = s("clock/synced").and_then(Series::last);
    let offset = s("clock/offsetMs").map(Series::vals).and_then(|v| v.into_iter().max_by(|a, b| a.abs().total_cmp(&b.abs())));
    let mut clock = Level::Ok;
    let mut cwhy = Vec::new();
    if synced.is_some_and(|x| x <= 0.5) {
        clock = Level::Bad;
        cwhy.push("not synchronized".to_string());
    }
    if let Some(o) = offset {
        if o.abs() > CLOCK_WARN_MS {
            clock = clock.max(if o.abs() > CLOCK_BAD_MS { Level::Bad } else { Level::Warn });
            cwhy.push(format!("off by {o:+.0} ms"));
        }
    }
    if clock != Level::Ok {
        f.push(Finding {
            key: "clock".into(),
            level: clock,
            title: "The clock is off".into(),
            detail: format!("{}. Call times and multi-site duplicate removal depend on it.", cwhy.join(", ")),
            todo: "Run timedatectl (wants 'System clock synchronized: yes'); if not: sudo timedatectl set-ntp true".into(),
        });
    }

    // ── the other plugins (uploads) ──────────────────────────────────────────
    let mut plugins = Part::default();
    let ids: std::collections::BTreeSet<String> = series.keys().filter_map(|k| k.strip_prefix("plg/")).filter_map(|r| r.rsplit_once('/')).map(|(id, _)| id.to_string()).collect();
    for id in ids.iter().filter(|id| id.as_str() != inp.me) {
        let g = |n: &str| s(&format!("plg/{id}/{n}"));
        if g("state").is_none() && g("ok").is_none() {
            continue;
        }
        plugins.total += 1;
        let state = g("state").and_then(Series::last);
        let (ok, failed) = (g("ok").map_or(0.0, Series::total), g("failed").map_or(0.0, Series::total));
        let (dropped, restarts) = (g("dropped").map_or(0.0, Series::total), g("restarts").map_or(0.0, Series::total));
        let q = g("queued").map(Series::vals).unwrap_or_default();
        let mut lvl = Level::Ok;
        let mut why = Vec::new();
        if let Some(st) = state {
            if st >= 1.5 {
                lvl = Level::Bad;
                why.push("in error".to_string());
            } else if st >= 0.5 {
                lvl = Level::Warn;
                why.push("reporting a warning".to_string());
            }
        }
        if failed > 0.0 {
            let share = failed / (ok + failed).max(1.0);
            lvl = lvl.max(if share > UPLOAD_FAIL_BAD { Level::Bad } else { Level::Warn });
            why.push(format!("{failed:.0} failed ({:.0}%)", share * 100.0));
        }
        if dropped > 0.0 {
            lvl = Level::Bad;
            why.push(format!("{dropped:.0} calls never sent"));
        }
        if restarts > 0.0 {
            lvl = lvl.max(Level::Warn);
            why.push(format!("restarted {restarts:.0}×"));
        }
        if let (Some(last), Some(first)) = (q.last(), q.first()) {
            if *last >= QUEUE_WARN && last >= first {
                lvl = lvl.max(Level::Warn);
                why.push(format!("{last:.0} calls waiting"));
            }
        }
        if lvl == Level::Ok {
            plugins.ok += 1;
        } else {
            f.push(Finding {
                key: format!("plugin:{id}"),
                level: lvl,
                title: format!("Plugin {id}: {}", why.join(", ")),
                detail: format!("{ok:.0} sent in the window."),
                todo: "Open its log on the Plugins page: usually the server it sends to is down or unreachable, or its key / URL changed.".into(),
            });
        }
    }

    let radios_part = Part { ok: radios.iter().filter(|r| r.level <= Level::Info).count(), total: radios.len() };
    let overall = f.iter().map(|x| x.level).max().unwrap_or(Level::Ok);
    let overall = if overall == Level::Info { Level::Ok } else { overall };
    f.sort_by(|a, b| b.level.cmp(&a.level).then(a.key.cmp(&b.key)));
    Report { overall, findings: f, radios: radios_part, control, sites, plugins, calls: inp.calls.len(), notes }
}

/// The one line on the plugin's card.
pub fn status_line(r: &Report, skipped: &[String]) -> String {
    let p = r.problems();
    let parts = format!(
        "{}/{} radios, {}/{} control channels, {}/{} sites' calls{}",
        r.radios.ok, r.radios.total, r.control.ok, r.control.total, r.sites.ok, r.sites.total,
        if r.plugins.total > 0 { format!(", {}/{} plugins", r.plugins.ok, r.plugins.total) } else { String::new() }
    );
    let skip = if skipped.is_empty() { String::new() } else { format!(" (not checking {})", skipped.join(", ")) };
    match p.len() {
        0 => format!("Healthy: {parts} OK{skip}"),
        1 => format!("1 to look at: {}. {parts} OK{skip}", p[0].title),
        n => format!("{n} to look at, first: {}. {parts} OK{skip}", p[0].title),
    }
}

/// A finding as one log line.
pub fn log_line(x: &Finding) -> String {
    let mut s = format!("{} — {}", x.level.word(), x.title);
    if !x.detail.is_empty() {
        s += &format!(": {}", x.detail);
    }
    if !x.todo.is_empty() {
        s += &format!(" → {}", x.todo);
    }
    s
}

#[allow(dead_code)]
pub fn as_json(r: &Report) -> Value {
    serde_json::to_value(r).unwrap_or_default()
}
