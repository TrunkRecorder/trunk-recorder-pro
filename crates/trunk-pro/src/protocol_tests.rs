//! The interface protocol is web/src/protocol.ts, documented for anyone
//! writing their own interface in docs/api/. Its JSON Schema
//! (docs/api/protocol.schema.json, from `npm run schema` in web/) is checked
//! here against what the recorder really sends and handles: messages from a
//! recording session (a conventional call), a survey, the server's replies;
//! and protocol.ts's message types against [`ToClient`] and [`Command`].

use std::collections::BTreeSet;
use std::f64::consts::PI;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use trunk_app::protocol::{body, Command, ToClient};
use trunk_app::survey::{Request, SurveySession};
use trunk_app::{Config, Output, Session};
use trunk_core::Complex32;

const SCHEMA: &str = include_str!("../../../docs/api/protocol.schema.json");

fn schema() -> Value {
    serde_json::from_str(SCHEMA).expect("protocol.schema.json")
}

/// The message types `direction` ("FromRecorder" or "ToRecorder") declares.
fn declared(direction: &str) -> BTreeSet<String> {
    schema()["definitions"][direction]["anyOf"].as_array().unwrap().iter().filter_map(|b| b["properties"]["type"]["const"].as_str().map(str::to_string)).collect()
}

/// Check `msg` against its type's shape in `direction`: Err says each place it differs.
fn check(direction: &str, msg: &Value) -> Result<(), String> {
    let s = schema();
    let ty = msg["type"].as_str().ok_or_else(|| format!("a message with no type: {msg}"))?;
    let branches: Vec<Value> = s["definitions"][direction]["anyOf"].as_array().unwrap().iter().filter(|b| b["properties"]["type"]["const"] == ty).cloned().collect();
    if branches.is_empty() {
        return Err(format!("\"{ty}\" isn't in protocol.ts's {direction}"));
    }
    let mut one = if branches.len() == 1 { branches[0].clone() } else { json!({ "anyOf": branches }) };
    one["definitions"] = s["definitions"].clone();
    let v = jsonschema::draft7::new(&one).map_err(|e| format!("schema: {e}"))?;
    let errors: Vec<String> = v.iter_errors(msg).map(|e| format!("at {}: {e}", e.instance_path())).collect();
    if errors.is_empty() {
        return Ok(());
    }
    let mut text = msg.to_string();
    if text.len() > 600 {
        text = format!("{}…", &text[..text.char_indices().nth(600).map_or(text.len(), |(i, _)| i)]);
    }
    Err(format!("\"{ty}\" doesn't match protocol.ts:\n    {}\n    message: {text}", errors.join("\n    ")))
}

/// Check them all; fail listing every mismatch.
fn check_all(direction: &str, msgs: &[Value]) {
    let mut seen = BTreeSet::new();
    let problems: Vec<String> = msgs
        .iter()
        .filter_map(|m| check(direction, m).err())
        // One report per kind of mismatch is enough.
        .filter(|p| seen.insert(p.lines().take(2).collect::<String>()))
        .collect();
    assert!(problems.is_empty(), "{} message(s) don't match protocol.ts:\n\n{}", problems.len(), problems.join("\n\n"));
}

fn texts(out: &mut Vec<Output>, into: &mut Vec<Value>) {
    for o in out.drain(..) {
        match o {
            Output::Message(m) | Output::Topic { msg: m, .. } => into.push(m.to_value()),
            // Stored, a call is announced as the platform does it (runtime::finish_one).
            Output::File { rel, record, .. } => into.push(json!({ "type": "concluded", "entry": trunk_app::session::call_entry(&rel, &record) })),
            _ => {}
        }
    }
}

/// Noise, and an FM transmission (1 kHz tone, 151.4 Hz CTCSS) at +200 kHz
/// from 0.5 to 2.5 s.
fn air(fs: f64, secs: f64, mut each: impl FnMut(&[Complex32], f64)) {
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut u = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    let sigma2 = 0.01f64;
    let p = sigma2 * 10_000.0 / fs * 10f64.powf(30.0 / 10.0);
    let (total, chunk) = ((fs * secs) as usize, 32768);
    let mut buf = vec![Complex32::default(); chunk];
    let (mut ph, mut i0) = (0.0f64, 0usize);
    while i0 < total {
        let n = chunk.min(total - i0);
        for (k, v) in buf[..n].iter_mut().enumerate() {
            let (a, b) = (u(), u());
            let mut x = Complex32::from_polar(((-a.ln()) * sigma2).sqrt() as f32, (2.0 * PI * b) as f32);
            let t = (i0 + k) as f64 / fs;
            let dev = 2500.0 * (2.0 * PI * 1000.0 * t).sin() + 500.0 * (2.0 * PI * 151.4 * t).sin();
            ph += 2.0 * PI * (200_000.0 + dev) / fs;
            if (0.5..2.5).contains(&t) {
                x += Complex32::from_polar(p.sqrt() as f32, ph as f32);
            }
            *v = x;
        }
        i0 += n;
        each(&buf[..n], i0 as f64 / fs);
    }
}

pub(crate) fn conventional_config() -> Config {
    serde_json::from_value(json!({
        "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
        "conventional": [{ "shortName": "conv", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire", "code": "151.4" }] }],
        "recording": { "callTimeoutS": 1 }
    }))
    .unwrap()
}

/// What a recording session sends: status (with the call while it's on),
/// spectrum, log, heard, concluded, the dashboard's stats and details.
#[test]
fn a_recording_sessions_messages() {
    let mut s = Session::new(conventional_config(), &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
    s.set_topics(trunk_app::stats::Topics::new(["spectrum:0", "log", "rf:0", "decode:conv"].map(String::from)));
    let (mut out, mut msgs) = (Vec::new(), Vec::new());
    air(2_400_000.0, 4.5, |iq, t| {
        s.push_iq(0, iq, 0, 1.75e12 + t * 1000.0);
        s.poll(1.75e12 + t * 1000.0, &mut out);
        texts(&mut out, &mut msgs);
    });
    s.source_error(0, "a test error");
    s.poll(1.75e12 + 5000.0, &mut out);
    s.finish(&mut out);
    texts(&mut out, &mut msgs);
    let types: BTreeSet<&str> = msgs.iter().filter_map(|m| m["type"].as_str()).collect();
    for t in ["status", "spectrum", "log", "concluded", "stats", "rfDetail", "decodeDetail"] {
        assert!(types.contains(t), "no {t} message (got {types:?})");
    }
    assert!(msgs.iter().any(|m| m["type"] == "status" && !m["calls"].as_array().unwrap().is_empty()), "no status with a call in it");
    let stats = msgs.iter().rfind(|m| m["type"] == "stats").unwrap();
    assert!(stats["values"]["src/RTL-SDR (first)/noise"].is_number(), "{stats}");
    assert!(stats["values"]["sys/conv/calls"].is_number() || stats["values"]["all/calls"].is_number(), "{stats}");
    check_all("FromRecorder", &msgs);
}

/// An RTL-SDR hands over 131,072 samples at a time (55 ms at 2.4 MS/s), each
/// a few ms late: the rate shown for it holds steady, not a chunk more or less
/// a window; and falls when it goes quiet.
#[test]
fn a_chunky_radio_shows_a_steady_rate() {
    let epoch = 1.75e12;
    let mut s = Session::new(conventional_config(), &Default::default(), epoch, &|_| None, |_| 0).unwrap();
    let (mut out, mut msgs) = (Vec::new(), Vec::new());
    let chunk = vec![127u8; 2 * 131_072];
    let rate = |msgs: &[Value]| msgs.iter().rfind(|m| m["type"] == "status").unwrap()["sources"][0]["rateMeasured"].as_f64().unwrap();
    let mut now = 0.0;
    let mut rates = Vec::new();
    for k in 1..=200 {
        let at = k as f64 * 131_072.0 / 2.4e6 * 1000.0 + [0.0, 4.0, 1.0, 7.0, 2.0][k % 5];
        while now < at {
            s.poll(epoch + now, &mut out);
            now += 20.0;
        }
        s.push(0, &chunk, 0, epoch + at);
        texts(&mut out, &mut msgs);
        if now > 6000.0 {
            rates.push(rate(&msgs));
        }
    }
    assert!(rates.iter().all(|r| (r / 2.4e6 - 1.0).abs() < 0.002), "{rates:?}");
    for _ in 0..350 {
        s.poll(epoch + now, &mut out);
        now += 20.0;
    }
    texts(&mut out, &mut msgs);
    assert!(rate(&msgs) < 0.1 * 2.4e6, "{}", rate(&msgs));
}

/// A radio whose first samples come 3 s after recording starts (a USRP
/// loading its FPGA, say): the session sets its clock from the wall clock,
/// so a transmission 0.5 s into its samples is saved as 3.5 s in, not 0.5 s.
#[test]
fn a_late_radio_keeps_to_the_wall_clock() {
    let epoch = 1.75e12;
    // When the call starts, ms after the recording did.
    let start_of = |cfg: Config| {
        let mut s = Session::new(cfg, &Default::default(), epoch, &|_| None, |_| 0).unwrap();
        let mut out = Vec::new();
        s.poll(epoch, &mut out);
        let mut files = Vec::new();
        air(2_400_000.0, 4.5, |iq, t| {
            s.push_iq(0, iq, 0, epoch + 3000.0 + t * 1000.0);
            s.poll(epoch + 3000.0 + t * 1000.0, &mut out);
            files.extend(out.drain(..).filter_map(|o| if let Output::File { record, .. } = o { Some(record) } else { None }));
        });
        s.finish(&mut out);
        files.extend(out.drain(..).filter_map(|o| if let Output::File { record, .. } = o { Some(record) } else { None }));
        files.first().expect("a call").start_time_ms as f64 - epoch
    };
    let radio = start_of(conventional_config());
    assert!((radio - 3500.0).abs() < 100.0, "the call starts {radio} ms in");
    // A capture played as fast as it can be keeps its own time.
    let mut replay = conventional_config();
    replay.sources = vec![trunk_app::config::Source { kind: crate::config::SourceKind::File { path: "x.cu8".into(), realtime: false, format: None }, center_hz: 155_000_000.0, rate_hz: 2_400_000.0, ppm: 0.0, auto_tune: false, guard_hz: trunk_app::config::DEFAULT_GUARD_HZ }];
    let file = start_of(replay);
    assert!((file - 500.0).abs() < 100.0, "the replayed call starts {file} ms in");
}

/// An engine running 2 s behind its radio (samples queued for it): each
/// buffer is timed by when the driver handed it over, so the call still
/// starts 0.5 s in, not 2.5 s.
#[test]
fn a_busy_engine_does_not_make_the_radio_look_late() {
    let epoch = 1.75e12;
    let mut s = Session::new(conventional_config(), &Default::default(), epoch, &|_| None, |_| 0).unwrap();
    let mut out = Vec::new();
    s.poll(epoch, &mut out);
    let mut files = Vec::new();
    air(2_400_000.0, 4.5, |iq, t| {
        s.push_iq(0, iq, 0, epoch + t * 1000.0);
        s.poll(epoch + 2000.0 + t * 1000.0, &mut out);
        files.extend(out.drain(..).filter_map(|o| if let Output::File { record, .. } = o { Some(record) } else { None }));
    });
    s.finish(&mut out);
    files.extend(out.drain(..).filter_map(|o| if let Output::File { record, .. } = o { Some(record) } else { None }));
    let start = files.first().expect("a call").start_time_ms as f64 - epoch;
    assert!((start - 500.0).abs() < 100.0, "the call starts {start} ms in");
}

/// AutoTune starts from the last run's score, moved by the ppm set since,
/// and the file isn't rewritten until something is measured.
#[test]
fn autotune_starts_from_the_saved_score() {
    let mut cfg = conventional_config();
    cfg.sources[0] = serde_json::from_value(json!({ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 2, "autoTune": true })).unwrap();
    let mut s = Session::new(cfg, &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
    s.load_tune(r#"{"RTL-SDR (first)": {"errorPpm": 3.0, "atPpm": 1}, "Airspy SN 1": {"errorPpm": -1.0, "atPpm": 0}}"#);
    assert!(!s.due(1.75e12, true).iter().any(|x| matches!(x, trunk_app::saving::Saved::Tune(_))), "nothing new to save");
    let (mut out, mut msgs) = (Vec::new(), Vec::new());
    s.poll(1.75e12, &mut out);
    texts(&mut out, &mut msgs);
    let status = msgs.iter().find(|m| m["type"] == "status").expect("a status");
    let src = &status["sources"][0];
    assert_eq!((src["tunePpm"].as_f64(), src["tuneSeeded"].as_bool()), (Some(4.0), Some(true)), "{src}");
}

/// Without subscriptions, the costly messages aren't made.
#[test]
fn unwatched_topics_are_not_sent() {
    let mut s = Session::new(conventional_config(), &Default::default(), 1.75e12, &|_| None, |_| 0).unwrap();
    let (mut out, mut msgs) = (Vec::new(), Vec::new());
    air(2_400_000.0, 3.0, |iq, t| {
        s.push_iq(0, iq, 0, 1.75e12 + t * 1000.0);
        s.poll(1.75e12 + t * 1000.0, &mut out);
        texts(&mut out, &mut msgs);
    });
    let types: BTreeSet<&str> = msgs.iter().filter_map(|m| m["type"].as_str()).collect();
    for t in ["spectrum", "log", "rfDetail", "decodeDetail"] {
        assert!(!types.contains(t), "{t} sent with nobody watching");
    }
    assert!(types.contains("stats") && types.contains("status"));
}

#[test]
fn a_surveys_messages() {
    let cfg: Config = serde_json::from_value(json!({
        "sources": [{ "type": "file", "path": "x.cu8", "centerHz": 851000000, "rateHz": 2400000, "realtime": false }]
    }))
    .unwrap();
    let mut s = SurveySession::new(&cfg, &Request::from_json(&json!({ "source": 0, "bands": [] }))).unwrap();
    let (mut out, mut msgs) = (Vec::new(), Vec::new());
    air(2_400_000.0, 1.0, |iq, t| {
        while s.command().is_some() {
            s.survey().tuned(851e6);
        }
        s.survey().push_iq(iq);
        s.poll(t * 1000.0, &mut out);
        texts(&mut out, &mut msgs);
    });
    msgs.push(ToClient::Survey(body(s.snapshot())).to_value());
    msgs.push(crate::survey::idle().to_value());
    assert!(msgs.iter().any(|m| m["type"] == "surveySpectrum"));
    // Looking for a system's sites, on a spare radio (a capture here): its sites judged as it goes.
    let req = json!({
        "source": 0, "radio": { "type": "file", "path": "y.cu8", "centerHz": 452000000, "rateHz": 2400000, "realtime": false },
        "lookFor": { "sites": [{ "label": "a", "freqsHz": [452100000], "expect": {} }, { "label": "far", "freqsHz": [470500000], "expect": {} }], "type": "dmr", "listenS": 0.5 },
    });
    let mut s = SurveySession::new(&cfg, &Request::from_json(&req)).unwrap();
    air(2_400_000.0, 1.0, |iq, _| s.survey().push_iq(iq));
    let snap = s.snapshot();
    assert_eq!(snap["source"], Value::Null);
    assert_eq!(snap["radio"], "file y.cu8");
    assert_eq!(snap["lookFor"]["freqs"], 2);
    assert_eq!(snap["sites"][0]["verdict"], "inactive", "{}", snap["sites"]);
    assert_eq!(snap["sites"][1]["verdict"], "notChecked");
    assert_eq!(snap["sites"][1]["why"], "outside what this radio tunes");
    msgs.push(ToClient::Survey(body(snap)).to_value());
    check_all("FromRecorder", &msgs);
}

/// A spare radio scans beside the recording; one a source names is refused (two can't open it).
#[test]
fn a_spare_radio_is_one_no_source_names() {
    let dir = std::env::temp_dir().join(format!("trunk-pro-spare-{}", std::process::id()));
    let cfg: Config = serde_json::from_value(json!({ "sources": [{ "type": "rtlsdr", "serial": "1", "centerHz": 851000000, "rateHz": 2400000, "gainDb": 30, "agc": false, "ppm": 0 }] })).unwrap();
    let ctx = ctx(cfg.clone(), &dir);
    let req = Request::from_json(&json!({ "source": 0, "radio": { "type": "rtlsdr", "serial": "1", "centerHz": 0, "rateHz": 2400000, "gainDb": 30, "agc": false, "ppm": 0 } }));
    let e = ctx.begin_survey(cfg, req).unwrap_err();
    assert!(e.contains("is source 1 of the recorder"), "{e}");
    let rtl = |serial: &str| -> trunk_app::config::Source { serde_json::from_value(json!({ "type": "rtlsdr", "serial": serial, "centerHz": 0, "rateHz": 2400000, "gainDb": 30, "agc": false, "ppm": 0 })).unwrap() };
    assert!(rtl("1").may_be(&rtl("1")) && rtl("").may_be(&rtl("2")) && !rtl("1").may_be(&rtl("2")));
}

pub(crate) fn ctx(cfg: Config, dir: &Path) -> Arc<crate::runtime::Ctx> {
    let name = format!("ctx-{}", dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()));
    ctx_with(cfg, dir, |_| crate::db::Db::for_test_with_batch(&name, std::time::Duration::from_secs(1)).0)
}

/// As [`ctx`], with the call store `db` makes on the hub.
pub(crate) fn ctx_with(mut cfg: Config, dir: &Path, db: impl FnOnce(crate::runtime::Hub) -> crate::db::Db) -> Arc<crate::runtime::Ctx> {
    let (hub, _) = tokio::sync::broadcast::channel(16);
    let shared = trunk_app::stats::Shared::new();
    let db = db(hub.clone());
    // (As a config read or set has them.)
    cfg.assign_system_ids();
    db.set_systems(&cfg);
    let series = Arc::new(Mutex::new(trunk_app::stats::History::new(crate::statstore::memory_minutes(&db))));
    let ctx = Arc::new(crate::runtime::Ctx {
        config_path: dir.join("config.json"),
        config: Mutex::new(cfg),
        config_version: std::sync::atomic::AtomicU64::new(1),
        talkgroups: Mutex::new(Default::default()),
        hub: hub.clone(),
        lifecycle: Mutex::new(crate::runtime::Lifecycle::default()),
        phase: Mutex::new(crate::runtime::PhaseInfo { phase: "idle", error: None, ended: false }),
        recent: Mutex::new(Default::default()),
        db,
        quit: tokio::sync::Notify::new(),
        survey_last: Mutex::new(None),
        plugins: crate::plugins::manage::Plugins::new(hub.clone(), shared.clone()),
        bind: std::net::Ipv4Addr::LOCALHOST.into(),
        interfaces: crate::interfaces::Interfaces::new(None, 8080),
        shared,
        series: series.clone(),
        store: crate::statstore::Store::start(series, crate::db::Db::disabled(), 0, false),
        heard_last: Mutex::new(None),
        topics: Mutex::new(Default::default()),
        engine: Mutex::new(None),
        host_last: Mutex::new(None),
        spool: Mutex::new(None),
        clock: Default::default(),
        unlocked: Default::default(),
    });
    // The stats thread makes its folder as it starts: wait for that, so a
    // test that removes `dir` when done does not see it made again after.
    let t = std::time::Instant::now();
    while !dir.join("stats").exists() && t.elapsed() < std::time::Duration::from_secs(1) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    ctx
}

/// A source's roll-off, from a capture of noise; and why it couldn't be had.
#[test]
fn a_source_profile() {
    let path = std::env::temp_dir().join(format!("trunk-pro-profile-{}.cu8", std::process::id()));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let noise: Vec<u8> = (0..2_400_000 * 2 * 2)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            // Roughly Gaussian (a sum of four), around the middle.
            (96 + (0..4).map(|k| (seed >> (k * 8)) as u8 as u32 / 4).sum::<u32>() / 2) as u8
        })
        .collect();
    std::fs::write(&path, noise).unwrap();
    let cfg: Config = serde_json::from_value(json!({
        "sources": [{ "type": "file", "path": path.display().to_string(), "centerHz": 851000000, "rateHz": 2400000, "realtime": false }]
    }))
    .unwrap();
    let ok = crate::profile::run(&cfg, &trunk_app::profile::Request { source: 0, center_hz: 0.0 });
    let _ = std::fs::remove_file(&path);
    assert!(ok["error"].is_null(), "{}", ok["error"]);
    assert_eq!(ok["centerHz"], 851_000_000.0);
    // White noise: flat to the edge.
    assert!(ok["suggestedGuardHz"].as_f64().unwrap() <= 15_000.0, "{}", ok["suggestedGuardHz"]);
    let missing = crate::profile::run(&cfg, &trunk_app::profile::Request { source: 3, center_hz: 0.0 });
    assert!(missing["error"].is_string());
    check_all("FromRecorder", &[ok, missing]);
}

/// The server's own messages: hello, config, state, devices, radios,
/// plugins, the plugin store, a folder, a Trunk Recorder config.
#[test]
fn the_servers_messages() {
    let dir = std::env::temp_dir().join(format!("trunk-pro-protocol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{ "sources": [{ "center": 851000000, "rate": 2400000, "driver": "osmosdr", "device": "rtl=0" }],
             "systems": [{ "shortName": "x", "type": "p25", "control_channels": [851012500], "talkgroupsFile": "tg.csv" }] }"#,
    )
    .unwrap();
    std::fs::write(dir.join("tg.csv"), "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,Fire,D,Fire,Fire,Fire\n").unwrap();

    // A config with a bit of everything.
    let mut cfg = conventional_config();
    let more: Config = serde_json::from_value(json!({
        "sources": [
            { "type": "rtlsdr", "serial": "", "centerHz": 0, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0, "autoTune": true },
            { "type": "usrp", "args": "", "centerHz": 0, "rateHz": 8000000, "gainDb": 30, "agc": false, "antenna": "RX2", "ppm": 0 },
            { "type": "airspy", "serial": "", "centerHz": 0, "rateHz": 10000000, "gainMode": "linearity", "gainStep": 12, "lnaStep": 0, "mixerStep": 0, "vgaStep": 0, "agc": false, "biasTee": false, "ppm": 0 },
            { "type": "soapy", "args": "driver=hackrf", "centerHz": 0, "rateHz": 8000000, "agc": false, "gainDb": null, "gains": { "LNA": 16 }, "antenna": "", "settings": "", "ppm": 0 },
            { "type": "file", "path": "x.cu8", "centerHz": 851000000, "rateHz": 2400000, "realtime": true, "format": "cu8" }
        ],
        "systems": [
            { "shortName": "p25", "recording": { "minCallS": 2 }, "sites": [{ "controlChannelsHz": [851012500] }, { "name": "p25-2", "controlChannelsHz": [851512500] }] },
            { "shortName": "dmr", "type": "dmr", "sites": [{ "controlChannelsHz": [452175000], "lcnTableHz": { "101": 452275000 }, "colorCode": 1 }] },
            { "shortName": "smartnet", "type": "smartnet", "sites": [{ "controlChannelsHz": [856112500], "bandplan": "800_standard" }] }
        ]
    }))
    .unwrap();
    cfg.sources.extend(more.sources);
    cfg.systems = more.systems;
    cfg.server.allowed_origins = vec!["http://localhost:3000".into()];
    cfg.server.interfaces.insert("wall".into(), trunk_app::config::Interface { port: 8081, path: "ui/wall".into(), ..Default::default() });
    cfg.server.interfaces.insert("trunkamp".into(), trunk_app::config::Interface { port: 8082, access: trunk_app::config::ApiAccess::Control, ..Default::default() });

    let ctx = ctx(cfg.clone(), &dir);
    // Something in the registry and the history to ask about.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    {
        let mut r = ctx.shared.radio.lock().unwrap();
        let c = trunk_core::trunk::Call {
            talkgroup: 101,
            start_s: 0.0,
            last_update_s: 4.0,
            sources: vec![trunk_core::trunk::CallSource { src: 7, ..Default::default() }, trunk_core::trunk::CallSource { src: 8, time_s: 2.0, ..Default::default() }],
            ..Default::default()
        };
        r.call_start("p25", &c, now - 10);
        r.call_end("p25", &c, now - 6);
        r.message("p25", &trunk_core::trunk::Message { kind: trunk_core::trunk::MessageType::Affiliation, source: 7, talkgroup: 101, ..Default::default() }, now);
        r.concluded("p25", &serde_json::from_value(json!({ "freq": 851_012_500u64, "snr": 18.5, "errorList": [{ "frames": 50, "error_count": 2, "bad_frames": 0 }] })).unwrap(), now);
    }
    ctx.series.lock().unwrap().insert(&trunk_app::stats::Rollup { t: now / 60 * 60, values: [("sys/p25/cc/good".to_string(), [30.0, 28.0, 33.0])].into() });
    ctx.event(trunk_app::stats::MonitorEvent::ControlLost { system: "p25".into(), site: Some("south".into()), freq_hz: Some(851_012_500) });
    ctx.event(trunk_app::stats::MonitorEvent::PluginHealth { plugin: "openmhz".into(), state: "warning".into(), message: "3 uploads waiting".into() });
    let mut agg = trunk_app::stats::Aggregator::default();
    let mut platform = crate::platform::Platform::new(vec![crate::platform::Watched { name: "data", path: dir.clone() }], Arc::new(crate::platform::Probes::default()), Default::default());
    agg.begin(now as f64);
    let p = platform.sample(&mut agg, true);
    *ctx.host_last.lock().unwrap() = Some(crate::monitor::message(now as f64, &agg, p));
    ctx.talkgroups.lock().unwrap().insert("p25".into(), trunk_core::trunk::parse_rows("101,Fire Dispatch\n"));
    // The greeting: hello last, after how things are now; less of it for listen access.
    let heard = |control| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let msgs = rt.block_on(crate::server::greeting(&ctx, control));
        msgs.iter().filter_map(|m| crate::server::text_for(&crate::runtime::Out::of(m), control)).map(|t| serde_json::from_str::<Value>(&t).unwrap()).collect::<Vec<Value>>()
    };
    let (greeting, listening) = (heard(true), heard(false));
    let kinds = |g: &[Value]| g.iter().map(|m| m["type"].as_str().unwrap().to_string()).collect::<Vec<String>>();
    let (all, some) = (kinds(&greeting), kinds(&listening));
    assert_eq!(all.last().map(String::as_str), Some("hello"));
    assert_eq!(all.iter().filter(|k| *k == "monitorEvent").count(), 2);
    for k in ["state", "config", "talkgroups", "heard", "history", "radios", "survey", "host", "devices", "keys", "plugins", "interfaces"] {
        assert!(all.iter().any(|a| a == k), "the greeting has no {k}: {all:?}");
    }
    for k in ["devices", "keys", "plugins", "interfaces"] {
        assert!(!some.iter().any(|a| a == k), "a listen-only greeting has {k}");
    }
    // `host` is every connection's heartbeat (transport.ts's frozen and behind checks): it goes to
    // all of them, listen-only and watching no topics, stamped with the server's clock.
    let host = ctx.host_last.lock().unwrap().clone().unwrap();
    assert!(matches!(crate::runtime::Out::of(&host), crate::runtime::Out::Text(_)), "host is sent to some connections only");
    assert!(listening.iter().find(|m| m["type"] == "host").unwrap()["t"].is_number());
    assert!(greeting.last().unwrap()["configPath"].is_string() && listening.last().unwrap().get("configPath").is_none());
    let config = |g: &[Value]| g.iter().find(|m| m["type"] == "config").unwrap()["config"].clone();
    assert_eq!(config(&listening), crate::access::redact_config(config(&greeting)));
    let q = serde_json::from_value(json!({ "id": 1, "series": ["sys/p25/*"], "range": "1h" })).unwrap();
    let mut queries = vec![crate::server::stats_query(&ctx, &q).to_value()];
    assert!(queries[0]["series"]["sys/p25/cc/good"]["v"].is_array(), "{}", queries[0]);
    for q in [
        json!({ "id": 2, "what": "summary" }),
        json!({ "id": 3, "what": "talkgroups", "system": "p25", "hours": 24 }),
        json!({ "id": 4, "what": "units", "system": "p25" }),
        json!({ "id": 5, "what": "tg", "system": "p25", "key": 101 }),
        json!({ "id": 6, "what": "unit", "system": "p25", "key": 7 }),
        json!({ "id": 7, "what": "freqs", "system": "p25", "hours": 24 }),
        json!({ "id": 8, "what": "lengths", "system": "p25", "hours": 24 }),
        json!({ "id": 9, "what": "talkgroups", "system": "nowhere" }),
    ] {
        queries.push(crate::server::radio_query(&ctx, &body(q)).to_value());
    }
    // The call store answers `callsQuery`: a call indexed, then asked for by talkgroup.
    let mut record = crate::db::row::tests::record(9179, now * 1000);
    record.short_name = "p25".into();
    crate::runtime::index_call(&ctx, "p25/2026/10/26/9179-1", &record, true, true);
    ctx.db.flush().unwrap();
    let Ok(Command::CallsQuery { id, query }) = Command::parse(r#"{"type":"callsQuery","id":3,"talkgroup":9179}"#) else { panic!("callsQuery") };
    let calls = crate::runtime::calls_query(&ctx, id, &query).to_value();
    assert_eq!((calls["id"].as_i64(), calls["entries"].as_array().map(Vec::len), calls.get("next")), (Some(3), Some(1), None), "{calls}");
    queries.push(calls);
    assert_eq!(queries[2]["rows"][0]["alphaTag"], "", "no talkgroup file for p25 here");
    assert_eq!(queries[4]["talkers"].as_array().map(Vec::len), Some(2));
    // Named from the name book the server keeps (talkgroup files and the config; aliases as saved).
    let files: trunk_app::config::TalkgroupLists = [("p25".to_string(), trunk_core::trunk::parse_rows("Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,FD Dispatch,D,Dispatch,Fire Dispatch,Fire\n"))].into();
    ctx.shared.names.lock().unwrap().set_config(&cfg, &files);
    ctx.shared.names.lock().unwrap().set_units("p25", "7,Engine 7\n");
    let named = |q: Value| crate::server::radio_query(&ctx, &body(q)).to_value().to_string();
    assert!(named(json!({ "id": 10, "what": "talkgroups", "system": "p25", "hours": 24 })).contains("FD Dispatch"));
    assert!(named(json!({ "id": 11, "what": "units", "system": "p25" })).contains("Engine 7"));
    let msgs = vec![
        ToClient::Config { config: Box::new(cfg.clone()), version: 2 },
        ToClient::Config { config: Box::default(), version: 1 },
        ToClient::ConfigSaved { version: 3, error: None },
        ToClient::ConfigSaved { version: 3, error: Some("changed elsewhere".into()) },
        crate::runtime::PhaseInfo { phase: "running", error: Some("x".into()), ended: false }.message(),
        crate::server::devices(),
        ToClient::Radios { radios: crate::radio::radios_json(false) },
        ctx.plugins.list(&cfg),
        crate::plugins::manage::store_message(&crate::plugins::store::built_in()),
        ctx.interfaces.list(&cfg, &ctx.config_path),
        crate::interfaces::store::message(&crate::interfaces::store::REGISTRY.built_in()),
        ToClient::InstallProgress { kind: trunk_app::protocol::StoreKind::Interface, key: "trunkamp".into(), id: "trunkamp".into(), stage: "downloading", message: None },
        ToClient::InstallProgress { kind: trunk_app::protocol::StoreKind::Plugin, key: "a/b".into(), id: String::new(), stage: "failed", message: Some("no release".into()) },
        crate::server::dir(&dir.display().to_string()),
        crate::server::dir(&dir.join("missing").display().to_string()),
        crate::server::tr_config(&dir.display().to_string()),
        crate::server::tr_config(&dir.join("nothing.json").display().to_string()),
        ToClient::Quit,
        ToClient::LicenseResult { id: 1, results: vec![json!({ "freqHz": 857_987_500, "historical": false, "licenses": [{ "callSign": "WPXT459", "licensee": "DISTRICT OF COLUMBIA", "serviceCode": "YE", "serviceDesc": null, "freqHz": 857_987_500, "emission": "8K10F1E", "distanceM": 1374, "bearingDeg": 115, "bearingCardinal": "ESE", "city": "WASHINGTON", "state": "DC", "status": "A", "locationKind": "fixed" }] })], error: None },
        ToClient::CallSignResult { id: 3, results: vec![json!({ "callSign": "KNJA391", "licensee": "DISTRICT OF COLUMBIA", "serviceCode": "YE", "serviceDesc": null, "status": "A", "city": "WASHINGTON", "state": "DC" })], error: None },
        ToClient::LicenseResult { id: 2, results: vec![], error: Some("Couldn't reach https://trunkrecorder.pro".into()) },
        ToClient::Subscribed { topics: vec!["spectrum:0".into(), "log".into()] },
        ToClient::TalkgroupEdited { system: "dcfd".into(), talkgroup: 101, edit: trunk_app::protocol::TalkgroupEdit { ignore: Some(true), alpha_tag: Some("FD Disp".into()), ..Default::default() } },
        // What the recorder itself logs (logging::to_browsers).
        ToClient::Log { lines: vec![json!({ "timeS": 0, "time": 1_750_000_000.5, "level": "warning", "kind": "error", "text": "No RAM spool" })] },
    ];
    let msgs: Vec<Value> = greeting.into_iter().chain(msgs.iter().map(ToClient::to_value)).chain(queries).chain(ctx.shared.recent_events()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    check_all("FromRecorder", &msgs);
}

/// protocol.ts declares the messages [`ToClient`] can be and the commands
/// [`Command`] reads: no more, no fewer.
#[test]
fn every_message_type_is_declared() {
    let names = |v: &[&str]| v.iter().map(|n| n.to_string()).collect::<BTreeSet<String>>();
    let (from, sent) = (declared("FromRecorder"), names(ToClient::TYPES));
    assert_eq!(sent.difference(&from).collect::<Vec<_>>(), Vec::<&String>::new(), "sent but not in protocol.ts's FromRecorder");
    assert_eq!(from.difference(&sent).collect::<Vec<_>>(), Vec::<&String>::new(), "in protocol.ts's FromRecorder but never sent");
    let (to, read) = (declared("ToRecorder"), names(Command::TYPES));
    assert_eq!(read.difference(&to).collect::<Vec<_>>(), Vec::<&String>::new(), "read but not in protocol.ts's ToRecorder");
    assert_eq!(to.difference(&read).collect::<Vec<_>>(), Vec::<&String>::new(), "in protocol.ts's ToRecorder but not read");
}

/// What the interface sends is what the server reads (the shapes, as the docs give them).
#[test]
fn messages_to_the_recorder() {
    let msgs = vec![
        json!({ "type": "setConfig", "config": serde_json::to_value(conventional_config()).unwrap(), "version": 1 }),
        json!({ "type": "start" }),
        json!({ "type": "stop" }),
        json!({ "type": "listen", "on": true, "system": null, "talkgroup": 101 }),
        json!({ "type": "listDir", "path": "" }),
        json!({ "type": "surveyStart", "source": 0, "bands": ["800"], "findGain": true }),
        json!({
            "type": "surveyStart", "source": 0, "radio": { "type": "rtlsdr", "serial": "2", "centerHz": 0, "rateHz": 2_400_000, "gainDb": 30, "agc": false, "ppm": 0 }, "bands": ["uhf"], "findGain": false,
            "lookFor": { "freqsHz": [], "sites": [{ "label": "1 Walker Tower", "freqsHz": [451_175_000], "expect": { "rfss": 1, "site": 1, "nac": 0x212 } }, { "label": "2", "freqsHz": [452_175_000], "expect": {}, "colorCode": 3 }], "type": "dmr", "listenS": 60 },
        }),
        json!({ "type": "profileSource", "source": 0, "centerHz": 460_000_000 }),
        json!({ "type": "install", "kind": "plugin", "id": "openmhz" }),
        json!({ "type": "install", "kind": "interface", "repository": "someone/ui", "tag": "v1.0.0" }),
        json!({ "type": "store", "kind": "plugin", "refresh": true }),
        json!({ "type": "subscribe", "topics": ["spectrum:0", "rf:0", "decode:dcfd", "log", "platform"] }),
        json!({ "type": "statsQuery", "id": 1, "series": ["sys/dcfd/cc/*"], "range": "24h", "points": 200 }),
        json!({ "type": "statsQuery", "id": 2, "series": ["plat/cpu"], "from": 1_750_000_000, "to": 1_750_086_400 }),
        json!({ "type": "radioQuery", "id": 3, "what": "talkgroups", "system": "dcfd", "hours": 168, "limit": 100 }),
        json!({ "type": "radioQuery", "id": 4, "what": "unit", "system": "dcfd", "key": 1234 }),
        json!({ "type": "channelFile", "index": 0, "path": "" }),
        json!({ "type": "unlockKeys", "source": "county", "password": "p" }),
        json!({ "type": "callsQuery", "id": 5, "system": "dcfd", "talkgroup": 9179, "unit": 1234, "from": 1_750_000_000_000i64, "to": 1_750_086_400_000i64, "cursor": "1750050000000:dcfd/2025/6/15/9179-1750050000_851012500", "limit": 50 }),
        json!({ "type": "surveyListen", "freqHz": 851_012_500 }),
        json!({ "type": "callSignQuery", "id": 3, "callSigns": ["KNJA391", "KNJE616"] }),
        json!({ "type": "licenseQuery", "id": 1, "lat": 38.9, "lon": -77.03, "radiusM": 50_000, "signals": [{ "freqHz": 857_987_500 }, { "freqHz": 489_087_500, "widthHz": 12_500 }] }),
        json!({ "type": "store", "kind": "interface" }),
        json!({ "type": "removeInterface", "id": "trunkamp" }),
        json!({ "type": "setTalkgroups", "system": "dcfd", "talkgroups": [{ "number": 101, "mode": "D", "alphaTag": "FD Disp", "description": "", "tag": "", "category": "", "priority": 1, "preferredNac": 0, "preferredSite": "", "ignore": false, "comment": "# dispatch", "extra": [["Zone", "North"]] }] }),
        json!({ "type": "setTalkgroup", "system": "dcfd", "talkgroup": 101, "edit": { "ignore": true } }),
        json!({ "type": "setTalkgroup", "system": "dcfd", "talkgroup": 102, "edit": { "alphaTag": "FIRE4A", "description": "", "tag": "Fire Tac", "category": "Fire" } }),
    ];
    check_all("ToRecorder", &msgs);
    // And the recorder reads each as the docs give it.
    for m in &msgs {
        if let Err(e) = Command::parse(&m.to_string()) {
            panic!("{e}: {m}");
        }
    }
}

/// A key container with no passwordFile starts locked; a wrong password changes nothing, the right
/// one unlocks it, Lock forgets it, and so does a config that no longer names its file.
#[test]
fn a_key_container_is_unlocked_and_locked_from_the_interface() {
    let dir = std::env::temp_dir().join(format!("trunk-pro-unlock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ekc = concat!(env!("CARGO_MANIFEST_DIR"), "/../trunk-app/src/testdata/dummy-test-keys.ekc");
    let mut cfg = Config::default();
    cfg.keys.insert("county".into(), crate::config::KeySource { file: ekc.into(), ..Default::default() });
    cfg.systems.push(crate::config::System { keys: "county".into(), ..crate::config::System::one_site("fire", crate::config::Site { control_channels_hz: vec![851_012_500.0], ..Default::default() }) });
    let c = ctx(cfg.clone(), &dir);
    let unlocked = |c: &crate::runtime::Ctx| crate::unlock::status(c).to_value()["sources"][0]["unlocked"].as_bool();
    assert_eq!(crate::unlock::status(&c).to_value()["sources"][0]["systems"], json!(["fire"]));
    assert_eq!(unlocked(&c), Some(false));
    let e = crate::unlock::unlock(&c, "county", "nope").unwrap_err();
    assert!(e.contains("wrong password") && !e.contains("nope"), "{e}");
    assert_eq!(unlocked(&c), Some(false));
    assert!(crate::unlock::unlock(&c, "elsewhere", "testing1234").is_err());
    crate::unlock::unlock(&c, "county", "testing1234").unwrap();
    assert_eq!(unlocked(&c), Some(true));
    // A start reads the keys it holds.
    let keys = cfg.load_keys(&c.config_path, &|n| crate::unlock::held(&c, &cfg, n)).unwrap();
    assert_eq!(keys[0].ring.as_ref().map(|r| r.keys().len()), Some(3));
    check_all("FromRecorder", &[crate::unlock::status(&c).to_value()]);
    crate::unlock::lock(&c, "county");
    assert_eq!(unlocked(&c), Some(false));
    assert!(cfg.load_keys(&c.config_path, &|n| crate::unlock::held(&c, &cfg, n)).unwrap()[0].ring.is_none(), "locked");
    // Unlocked, then the source names another file: forgotten.
    crate::unlock::unlock(&c, "county", "testing1234").unwrap();
    let mut moved = cfg.clone();
    moved.keys.get_mut("county").unwrap().file = "elsewhere.ekc".into();
    crate::unlock::config_changed(&c, &moved);
    assert!(c.unlocked.lock().unwrap().is_empty());
}
