//! The first-run survey on this machine: one source thread (retuned through
//! a [`Control`]) and a survey thread that drives [`SurveySession`] and
//! publishes what it finds to the browsers. Also `trunk-pro survey`, the
//! same from the command line.

use trunk_app::protocol::{body, ToClient};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_app::survey::{Request, SurveySession};
use trunk_app::Output;
use trunk_core::survey::{Command, Stage};

use crate::config::{Config, Source, SourceKind};
use crate::runtime::{self, Ctx};
use crate::sdr::{Block, Control, Gain, Inbox, SourceMsg};

/// What the interface asks of a running survey.
pub enum UserCmd {
    Listen(f64),
    Rescan,
}

pub struct SurveyRunner {
    stop: Arc<AtomicBool>,
    cmd: mpsc::Sender<UserCmd>,
    threads: Vec<JoinHandle<()>>,
    /// The radio it scans with, and whether that is a spare (no source).
    radio: Source,
    spare: bool,
}

impl SurveyRunner {
    /// On a spare radio: it may go on while recording.
    pub fn spare(&self) -> bool {
        self.spare
    }
    /// Whether its radio is (or may be) one of `sources`.
    pub fn uses_any(&self, sources: &[Source]) -> bool {
        !self.spare || sources.iter().any(|s| s.may_be(&self.radio))
    }
    pub fn send(&self, c: UserCmd) {
        let _ = self.cmd.send(c);
    }
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

/// Pass the survey's radio commands to the source; a capture confirms at once.
fn apply(session: &mut SurveySession, ctl: &Control, file_center: Option<f64>) {
    while let Some(c) = session.command() {
        match (c, file_center) {
            (_, Some(center)) => session.survey().tuned(center),
            (Command::Tune(f), None) => *ctl.freq_hz.lock().unwrap() = Some(f),
            (Command::Gain(g), None) => *ctl.gain.lock().unwrap() = Some(Gain::Db(g)),
        }
    }
}

/// Feed one source message; false once the source has ended.
fn feed(session: &mut SurveySession, msg: SourceMsg) -> bool {
    match msg {
        SourceMsg::Samples { block: Block::U8(b), .. } => session.survey().push_u8(&b),
        SourceMsg::Samples { block: Block::Iq(b), .. } => session.survey().push_iq(&b),
        // (The callers' inbox makes these I/Q.)
        SourceMsg::Samples { block: Block::Real16(_), .. } => {}
        SourceMsg::Tuned { center_hz, .. } => session.survey().tuned(center_hz),
        SourceMsg::Error { error, .. } => session.source_error(&error),
        SourceMsg::End { .. } => {
            session.survey().finish();
            return false;
        }
    }
    true
}

/// A capture's centre (it can't be tuned); None for a radio.
fn file_center(cfg: &Config, req: &Request) -> Option<f64> {
    match (&req.radio, req.radio(cfg).ok()?) {
        (_, s) if !matches!(s.kind, SourceKind::File { .. }) => None,
        (Some(_), s) => Some(s.center_hz),
        (None, _) => Some(cfg.resolved_centers()[req.source]),
    }
}

pub fn start(ctx: Arc<Ctx>, cfg: Config, req: Request) -> Result<SurveyRunner, String> {
    let mut session = SurveySession::new(&cfg, &req)?;
    let stop = Arc::new(AtomicBool::new(false));
    let ctl = Arc::new(Control::default());
    let fixed = file_center(&cfg, &req);
    let radio = req.radio(&cfg)?.clone();
    // The first hop is the source's opening frequency.
    let first = match session.command() {
        Some(Command::Tune(f)) => f,
        _ => fixed.unwrap_or(radio.center_hz),
    };
    if fixed.is_none() {
        *ctl.current_hz.lock().unwrap() = Some(first);
    }
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let mut rx = Inbox::new(rx);
    let mut threads = vec![runtime::spawn_source(&radio, req.source, first, tx, stop.clone(), Some(ctl.clone()), &format!("survey-source-{}", req.source)).map_err(|e| e.to_string())?];
    // The opening tune is confirmed by the first samples.
    let mut opened = fixed.is_some();
    let (cmd_tx, cmd_rx) = mpsc::channel::<UserCmd>();
    let stop2 = stop.clone();
    threads.push(
        std::thread::Builder::new()
            .name("survey".into())
            .spawn(move || {
                let t0 = Instant::now();
                let mut out = Vec::new();
                let mut running = true;
                while !stop2.load(Ordering::Relaxed) {
                    while let Ok(c) = cmd_rx.try_recv() {
                        match c {
                            UserCmd::Listen(f) => session.survey().listen(f),
                            UserCmd::Rescan => session.survey().rescan(),
                        }
                    }
                    apply(&mut session, &ctl, fixed);
                    if running {
                        match rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(m) => {
                                if !opened && matches!(m, SourceMsg::Samples { .. }) {
                                    opened = true;
                                    session.survey().tuned(first);
                                }
                                running = feed(&mut session, m);
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => running = false,
                        }
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    session.poll(t0.elapsed().as_secs_f64() * 1000.0, &mut out);
                    for o in out.drain(..) {
                        if let Output::Message(m) = o {
                            if let ToClient::Survey(_) = m {
                                *ctx.survey_last.lock().unwrap() = Some(m.clone());
                            }
                            runtime::publish(&ctx.hub, m);
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?,
    );
    Ok(SurveyRunner { stop, cmd: cmd_tx, threads, spare: req.radio.is_some(), radio })
}

/// The message a browser gets when no survey is running.
pub fn idle() -> ToClient {
    ToClient::Survey(body(json!({ "stage": "idle" })))
}

/// `trunk-pro survey`: scan with a dongle (`--radio '{…}'`: any radio, as
/// a source in config.json; or look at a capture) and print what is found,
/// then the monitored system, as JSON lines. `--freqs 851.0125,852.5` (MHz):
/// just these frequencies, each probed (`--type p25`: with its receivers
/// only; `--listen 5`: for so many seconds).
pub fn cli(a: &crate::Args) {
    let mut cfg = Config::default();
    let rate = a.num("rate", 2_400_000.0);
    if let Some(r) = a.get("radio") {
        cfg.sources = vec![serde_json::from_str(r).unwrap_or_else(|e| crate::die(&format!("--radio: {e}")))];
    } else if let Some(path) = a.positional.first() {
        let format = crate::sample_format(path, a.get("format"));
        cfg.sources = vec![Source { kind: crate::config::SourceKind::File { path: path.clone(), realtime: false, format: Some(format) }, center_hz: a.num("center", 0.0), rate_hz: rate, ppm: 0.0, auto_tune: false, guard_hz: crate::config::DEFAULT_GUARD_HZ }];
    } else {
        cfg.sources = vec![Source { kind: crate::config::SourceKind::Rtlsdr { serial: a.get("serial").unwrap_or("").into(), gain_db: a.get("gain").and_then(|g| g.parse().ok()).unwrap_or(crate::config::RTL_DEFAULT_GAIN_DB), agc: a.get("gain").is_none(), buf_len: a.num("buflen", 0.0) as u32, buffers: a.num("buffers", 0.0) as u32 }, center_hz: 0.0, rate_hz: rate, ppm: a.num("ppm", 0.0), auto_tune: false, guard_hz: crate::config::DEFAULT_GUARD_HZ }];
    }
    let bands = a.get("bands").map(|b| json!(b.split(',').collect::<Vec<_>>())).unwrap_or(Value::Null);
    let freqs: Vec<f64> = a.get("freqs").map(|f| f.split(',').filter_map(|x| x.trim().parse::<f64>().ok()).map(|m| (m * 1e6).round()).collect()).unwrap_or_default();
    let look_for = (!freqs.is_empty()).then(|| json!({ "freqsHz": freqs, "type": a.get("type"), "listenS": a.get("listen").and_then(|s| s.parse::<f64>().ok()) }));
    let mut req = Request::from_json(&json!({ "source": 0, "bands": bands, "findGain": a.get("gain").is_none() && !a.flag("no-gain"), "lookFor": look_for }));
    req.threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let mut session = SurveySession::new(&cfg, &req).unwrap_or_else(|e| crate::die(&e));
    let fixed = file_center(&cfg, &req);
    let ctl = Arc::new(Control::default());
    let stop = Arc::new(AtomicBool::new(false));
    let first = match session.command() {
        Some(Command::Tune(f)) => f,
        _ => fixed.unwrap_or(0.0),
    };
    if fixed.is_none() {
        *ctl.current_hz.lock().unwrap() = Some(first);
    }
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let mut rx = Inbox::new(rx);
    let th = runtime::spawn_source(&cfg.sources[0], 0, first, tx, stop.clone(), Some(ctl.clone()), "survey-source-0").unwrap_or_else(|e| crate::die(&e.to_string()));
    let listen_s = a.num("seconds", 30.0);
    let t0 = Instant::now();
    let mut opened = fixed.is_some();
    let mut stage = String::new();
    let mut monitor_since: Option<Instant> = None;
    let mut last;
    let mut n_found = 0;
    let mut last_err: Option<String> = None;
    loop {
        apply(&mut session, &ctl, fixed);
        let running = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => {
                if !opened && matches!(m, SourceMsg::Samples { .. }) {
                    opened = true;
                    session.survey().tuned(first);
                }
                feed(&mut session, m)
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        };
        let snap = session.snapshot();
        let st = snap["stage"].as_str().unwrap_or("").to_string();
        let found = snap["candidates"].as_array().map_or(0, |c| c.len());
        if found != n_found {
            for c in snap["candidates"].as_array().unwrap().iter().skip(n_found) {
                println!("{}", json!({ "found": c }));
            }
            n_found = found;
        }
        if let Some(e) = snap["error"].as_str() {
            if last_err.as_deref() != Some(e) {
                eprintln!("survey: source error: {e}");
                last_err = Some(e.to_string());
            }
        }
        if st != stage {
            eprintln!("survey: {st}{}", if snap["message"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" — {}", snap["message"].as_str().unwrap()) });
            stage = st.clone();
            if st == "monitoring" {
                monitor_since = Some(Instant::now());
            }
        }
        last = snap;
        let listened = monitor_since.is_some_and(|t| t.elapsed().as_secs_f64() >= listen_s);
        if !running || st == "done" || listened || session.survey().stage() == Stage::Done {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = th.join();
    println!("{}", json!({ "monitor": last["monitor"], "suggest": last["suggest"], "message": last["message"], "wallS": t0.elapsed().as_secs_f64() }));
    // --identify lat,lon[,km]: who is licensed near there on what was heard.
    if let Some(at) = a.get("identify") {
        let n: Vec<f64> = at.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if n.len() < 2 {
            crate::die("--identify takes lat,lon or lat,lon,km");
        }
        let api = a.get("api").unwrap_or("https://trunkrecorder.pro").trim_end_matches('/').to_string();
        let answer = trunk_app::licenses::request(n[0], n[1], n.get(2).copied().unwrap_or(50.0) * 1000.0, &session.heard()).and_then(|req| crate::licenses::identify(&api, &req));
        match answer {
            Ok(v) => println!("{}", json!({ "licenses": v["results"] })),
            Err(e) => eprintln!("survey: {e}"),
        }
    }
}
