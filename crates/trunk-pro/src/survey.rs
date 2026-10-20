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

use crate::config::{Config, Source};
use crate::runtime::{self, Ctx};
use crate::sdr::{Control, SourceMsg};

/// What the interface asks of a running survey.
pub enum UserCmd {
    Listen(f64),
    Rescan,
}

pub struct SurveyRunner {
    stop: Arc<AtomicBool>,
    cmd: mpsc::Sender<UserCmd>,
    threads: Vec<JoinHandle<()>>,
}

impl SurveyRunner {
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
            (Command::Gain(g), None) => *ctl.gain_db.lock().unwrap() = Some(g),
        }
    }
}

/// Feed one source message; false once the source has ended.
fn feed(session: &mut SurveySession, msg: SourceMsg) -> bool {
    match msg {
        SourceMsg::Data { bytes, .. } => session.survey().push_u8(&bytes),
        SourceMsg::Iq { samples, .. } => session.survey().push_iq(&samples),
        SourceMsg::Tuned { center_hz, .. } => session.survey().tuned(center_hz),
        // (The callers turn these into Iq first.)
        SourceMsg::Real16 { .. } => {}
        SourceMsg::Error { error, .. } => session.source_error(&error),
        SourceMsg::End { .. } => {
            session.survey().finish();
            return false;
        }
    }
    true
}

fn file_center(cfg: &Config, i: usize) -> Option<f64> {
    matches!(cfg.sources[i], Source::File { .. }).then(|| cfg.resolved_centers()[i])
}

pub fn start(ctx: Arc<Ctx>, cfg: Config, req: Request) -> Result<SurveyRunner, String> {
    let mut session = SurveySession::new(&cfg, &req)?;
    let stop = Arc::new(AtomicBool::new(false));
    let ctl = Arc::new(Control::default());
    let fixed = file_center(&cfg, req.source);
    // The first hop is the source's opening frequency.
    let first = match session.command() {
        Some(Command::Tune(f)) => f,
        _ => fixed.unwrap_or(cfg.sources[req.source].center_hz()),
    };
    if fixed.is_none() {
        *ctl.current_hz.lock().unwrap() = Some(first);
    }
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let mut threads = vec![runtime::spawn_source(&cfg.sources[req.source], req.source, first, tx, stop.clone(), Some(ctl.clone()), &format!("survey-source-{}", req.source)).map_err(|e| e.to_string())?];
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
                let mut iqf = crate::sdr::IqFrom::default();
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
                                let m = iqf.iq(m);
                                if !opened && matches!(m, SourceMsg::Data { .. } | SourceMsg::Iq { .. }) {
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
    Ok(SurveyRunner { stop, cmd: cmd_tx, threads })
}

/// The message a browser gets when no survey is running.
pub fn idle() -> ToClient {
    ToClient::Survey(body(json!({ "stage": "idle" })))
}

/// `trunk-pro survey`: scan with a dongle (or look at a capture) and print
/// what is found, then the monitored system, as JSON lines.
pub fn cli(a: &crate::Args) {
    let mut cfg = Config::default();
    let rate = a.num("rate", 2_400_000.0);
    if let Some(path) = a.positional.first() {
        let format = crate::sample_format(path, a.get("format"));
        cfg.sources = vec![Source::File { path: path.clone(), center_hz: a.num("center", 0.0), rate_hz: rate, realtime: false, format: Some(format), auto_tune: false, guard_hz: crate::config::DEFAULT_GUARD_HZ }];
    } else {
        cfg.sources = vec![Source::Rtlsdr {
            serial: a.get("serial").unwrap_or("").into(),
            center_hz: 0.0,
            rate_hz: rate,
            gain_db: a.get("gain").and_then(|g| g.parse().ok()).unwrap_or(crate::config::RTL_DEFAULT_GAIN_DB),
            agc: a.get("gain").is_none(),
            ppm: a.num("ppm", 0.0) as i32,
            auto_tune: false,
            guard_hz: crate::config::DEFAULT_GUARD_HZ,
            buf_len: a.num("buflen", 0.0) as u32,
            buffers: a.num("buffers", 0.0) as u32,
        }];
    }
    let bands = a.get("bands").map(|b| json!(b.split(',').collect::<Vec<_>>())).unwrap_or(Value::Null);
    let req = Request::from_json(&json!({ "source": 0, "bands": bands, "findGain": a.get("gain").is_none() && !a.flag("no-gain") }));
    let mut session = SurveySession::new(&cfg, &req).unwrap_or_else(|e| crate::die(&e));
    let fixed = file_center(&cfg, 0);
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
    let th = runtime::spawn_source(&cfg.sources[0], 0, first, tx, stop.clone(), Some(ctl.clone()), "survey-source-0").unwrap_or_else(|e| crate::die(&e.to_string()));
    let listen_s = a.num("seconds", 30.0);
    let t0 = Instant::now();
    let mut opened = fixed.is_some();
    let mut stage = String::new();
    let mut monitor_since: Option<Instant> = None;
    let mut last;
    let mut n_found = 0;
    let mut last_err: Option<String> = None;
    let mut iqf = crate::sdr::IqFrom::default();
    loop {
        apply(&mut session, &ctl, fixed);
        let running = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => {
                let m = iqf.iq(m);
                if !opened && matches!(m, SourceMsg::Data { .. } | SourceMsg::Iq { .. }) {
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
