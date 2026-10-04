//! The interface protocol is web/src/protocol.ts, documented for anyone
//! writing their own interface in docs/api/. Its JSON Schema
//! (docs/api/protocol.schema.json, from `npm run schema` in web/) is checked
//! here against what the recorder really sends and handles: messages from a
//! recording session (a conventional call), a survey, the server's replies,
//! and every message type named in the code.

use std::collections::{BTreeSet, VecDeque};
use std::f64::consts::PI;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
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
            Output::Text(t) | Output::Topic { text: t, .. } => into.push(serde_json::from_str(&t).unwrap()),
            // Stored, a call is announced as the platform does it (runtime::finish_one).
            Output::File { entry, .. } => into.push(json!({ "type": "concluded", "entry": entry })),
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

fn conventional_config() -> Config {
    serde_json::from_value(json!({
        "sources": [{ "type": "rtlsdr", "serial": "", "centerHz": 155000000, "rateHz": 2400000, "gainDb": 25, "agc": false, "ppm": 0 }],
        "conventional": [{ "shortName": "conv", "channels": [{ "freqHz": 155200000, "mode": "fm", "name": "Fire", "tone": "151.4" }] }],
        "recording": { "callTimeoutS": 1 }
    }))
    .unwrap()
}

/// What a recording session sends: status (with the call while it's on),
/// spectrum, log, heard, concluded, the dashboard's stats and details.
#[test]
fn a_recording_sessions_messages() {
    let mut s = Session::new(conventional_config(), 1.75e12, &|_| None, |_| 0).unwrap();
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

/// A radio whose first samples come 3 s after recording starts (a USRP
/// loading its FPGA, say): the session sets its clock from the wall clock,
/// so a transmission 0.5 s into its samples is saved as 3.5 s in, not 0.5 s.
#[test]
fn a_late_radio_keeps_to_the_wall_clock() {
    let epoch = 1.75e12;
    // When the call starts, ms after the recording did.
    let start_of = |cfg: Config| {
        let mut s = Session::new(cfg, epoch, &|_| None, |_| 0).unwrap();
        let mut out = Vec::new();
        s.poll(epoch, &mut out);
        let mut files = Vec::new();
        air(2_400_000.0, 4.5, |iq, t| {
            s.push_iq(0, iq, 0, epoch + 3000.0 + t * 1000.0);
            s.poll(epoch + 3000.0 + t * 1000.0, &mut out);
            files.extend(out.drain(..).filter_map(|o| if let Output::File { json, .. } = o { Some(json) } else { None }));
        });
        s.finish(&mut out);
        files.extend(out.drain(..).filter_map(|o| if let Output::File { json, .. } = o { Some(json) } else { None }));
        let call: Value = serde_json::from_str(files.first().expect("a call")).unwrap();
        call["start_time_ms"].as_f64().unwrap() - epoch
    };
    let radio = start_of(conventional_config());
    assert!((radio - 3500.0).abs() < 100.0, "the call starts {radio} ms in");
    // A capture played as fast as it can be keeps its own time.
    let mut replay = conventional_config();
    replay.sources = vec![trunk_app::config::Source::File {
        path: "x.cu8".into(),
        center_hz: 155_000_000.0,
        rate_hz: 2_400_000.0,
        realtime: false,
        format: None,
        auto_tune: false,
        guard_hz: trunk_app::config::DEFAULT_GUARD_HZ,
    }];
    let file = start_of(replay);
    assert!((file - 500.0).abs() < 100.0, "the replayed call starts {file} ms in");
}

/// An engine running 2 s behind its radio (samples queued for it): each
/// buffer is timed by when the driver handed it over, so the call still
/// starts 0.5 s in, not 2.5 s.
#[test]
fn a_busy_engine_does_not_make_the_radio_look_late() {
    let epoch = 1.75e12;
    let mut s = Session::new(conventional_config(), epoch, &|_| None, |_| 0).unwrap();
    let mut out = Vec::new();
    s.poll(epoch, &mut out);
    let mut files = Vec::new();
    air(2_400_000.0, 4.5, |iq, t| {
        s.push_iq(0, iq, 0, epoch + t * 1000.0);
        s.poll(epoch + 2000.0 + t * 1000.0, &mut out);
        files.extend(out.drain(..).filter_map(|o| if let Output::File { json, .. } = o { Some(json) } else { None }));
    });
    s.finish(&mut out);
    files.extend(out.drain(..).filter_map(|o| if let Output::File { json, .. } = o { Some(json) } else { None }));
    let call: Value = serde_json::from_str(files.first().expect("a call")).unwrap();
    let start = call["start_time_ms"].as_f64().unwrap() - epoch;
    assert!((start - 500.0).abs() < 100.0, "the call starts {start} ms in");
}

/// Without subscriptions, the costly messages aren't made.
#[test]
fn unwatched_topics_are_not_sent() {
    let mut s = Session::new(conventional_config(), 1.75e12, &|_| None, |_| 0).unwrap();
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
    msgs.push(s.snapshot());
    msgs.push(crate::survey::idle_json());
    assert!(msgs.iter().any(|m| m["type"] == "surveySpectrum"));
    check_all("FromRecorder", &msgs);
}

fn ctx(cfg: Config, dir: &Path) -> Arc<crate::runtime::Ctx> {
    let (hub, _) = tokio::sync::broadcast::channel(16);
    let shared = trunk_app::stats::Shared::new();
    let series = Arc::new(Mutex::new(trunk_app::stats::History::default()));
    Arc::new(crate::runtime::Ctx {
        config_path: dir.join("config.json"),
        config: Mutex::new(cfg),
        hub: hub.clone(),
        runner: Mutex::new(None),
        lifecycle: Mutex::new(false),
        phase: Mutex::new(crate::runtime::PhaseInfo { phase: "idle", error: None, ended: false }),
        history: Mutex::new(VecDeque::new()),
        quit: tokio::sync::Notify::new(),
        survey: Mutex::new(None),
        survey_last: Mutex::new(None),
        plugins: crate::plugins::manage::Plugins::new(hub.clone(), shared.clone()),
        home_dir: None,
        shared,
        series: series.clone(),
        store: crate::statstore::Store::start(dir.join("stats"), series),
        topics: Mutex::new(Default::default()),
        topics_gen: std::sync::atomic::AtomicU64::new(0),
        engine_cmds: Mutex::new(Vec::new()),
        host_last: Mutex::new(None),
        spool: Mutex::new(None),
    })
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
            { "shortName": "p25", "controlChannelsHz": [851012500], "recording": { "minCallS": 2 }, "siteGroup": "g" },
            { "shortName": "dmr", "type": "dmr", "controlChannelsHz": [452175000], "lcnTableHz": { "101": 452275000 }, "colorCode": 1 },
            { "shortName": "smartnet", "type": "smartnet", "controlChannelsHz": [856112500], "bandplan": "800_standard" }
        ]
    }))
    .unwrap();
    cfg.sources.extend(more.sources);
    cfg.systems = more.systems;
    cfg.server.allowed_origins = vec!["http://localhost:3000".into()];
    cfg.server.interfaces = vec![trunk_app::config::Interface { name: "wall".into(), path: "ui/wall".into() }];
    cfg.server.home = "wall".into();

    let ctx = ctx(cfg.clone(), &dir);
    // Something in the registry and the history to ask about.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    {
        let mut r = ctx.shared.radio.lock().unwrap();
        let c = trunk_core::trunk::Call {
            talkgroup: 101,
            start_s: 0.0,
            last_update_s: 4.0,
            sources: vec![trunk_core::trunk::CallSource { src: 7, time_s: 0.0, emergency: false }, trunk_core::trunk::CallSource { src: 8, time_s: 2.0, emergency: false }],
            ..Default::default()
        };
        r.call_start("p25", &c, now - 10);
        r.call_end("p25", &c, now - 6);
        r.message("p25", &trunk_core::trunk::Message { kind: trunk_core::trunk::MessageType::Affiliation, source: 7, talkgroup: 101, ..Default::default() }, now);
        r.concluded("p25", &json!({ "freq": 851_012_500u64, "snr": 18.5, "errorList": [{ "frames": 50, "error_count": 2, "bad_frames": 0 }] }), now);
    }
    ctx.series.lock().unwrap().insert(&trunk_app::stats::Rollup { t: now / 60 * 60, values: [("sys/p25/cc/good".to_string(), [30.0, 28.0, 33.0])].into() });
    ctx.event(trunk_app::stats::MonitorEvent::ControlLost { system: "p25".into(), freq_hz: Some(851_012_500) });
    ctx.event(trunk_app::stats::MonitorEvent::PluginHealth { plugin: "openmhz".into(), state: "warning".into(), message: "3 uploads waiting".into() });
    let mut agg = trunk_app::stats::Aggregator::default();
    let mut platform = crate::platform::Platform::new(vec![crate::platform::Watched { name: "data", path: dir.clone() }], Arc::new(crate::platform::Probes::default()));
    agg.begin(now as f64);
    let p = platform.sample(&mut agg, true);
    *ctx.host_last.lock().unwrap() = Some(crate::monitor::message(now as f64, &agg, p));
    let hello = crate::server::hello_json(&ctx, serde_json::json!(crate::sdr::devices()), crate::radio::radios_json(false));
    assert_eq!(hello["events"].as_array().map(Vec::len), Some(2));
    let mut queries = vec![crate::server::stats_query(&ctx, &json!({ "id": 1, "series": ["sys/p25/*"], "range": "1h" }))];
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
        queries.push(crate::server::radio_query(&ctx, &q));
    }
    assert_eq!(queries[2]["rows"][0]["alphaTag"], "", "no talkgroup file for p25 here");
    assert_eq!(queries[4]["talkers"].as_array().map(Vec::len), Some(2));
    let msgs = vec![
        hello,
        json!({ "type": "config", "config": cfg }),
        json!({ "type": "config", "config": Config::default() }),
        crate::runtime::PhaseInfo { phase: "running", error: Some("x".into()), ended: false }.to_json(),
        crate::server::devices_json(),
        json!({ "type": "radios", "radios": crate::radio::radios_json(false) }),
        ctx.plugins.list_json(&cfg),
        crate::plugins::manage::store_message(&crate::plugins::store::built_in()),
        crate::server::dir_json(&dir.display().to_string()),
        crate::server::dir_json(&dir.join("missing").display().to_string()),
        crate::server::tr_config_json(&dir.display().to_string()),
        crate::server::tr_config_json(&dir.join("nothing.json").display().to_string()),
        json!({ "type": "quit" }),
        json!({ "type": "subscribed", "topics": ["spectrum:0", "log"] }),
    ];
    let msgs: Vec<Value> = msgs.into_iter().chain(queries).chain(ctx.shared.recent_events()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    check_all("FromRecorder", &msgs);
}

/// Every Rust source file the messages come from.
fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    let mut dirs = vec![root.join("trunk-app/src"), root.join("trunk-pro/src")];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("protocol_tests.rs") {
                files.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    files
}

/// Each `json!({ "type": "…"` (or `["type"] = json!("…")`) in the code: a message sent.
fn sent_types() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (_, text) in sources() {
        let mut rest = text.as_str();
        while let Some(i) = rest.find("\"type\": \"") {
            let before = rest[..i].trim_end();
            let after = &rest[i + 9..];
            let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if before.ends_with("json!({") {
                found.insert(name);
            }
            rest = after;
        }
        let mut rest = text.as_str();
        while let Some(i) = rest.find("[\"type\"] = json!(\"") {
            let after = &rest[i + 18..];
            found.insert(after.chars().take_while(|c| c.is_ascii_alphanumeric()).collect());
            rest = after;
        }
    }
    found
}

/// The types the server's `command` answers.
fn handled_types() -> BTreeSet<String> {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server.rs")).unwrap();
    let body = &text[text.find("async fn command(").unwrap()..];
    let body = &body[..body.find("\n}\n").unwrap()];
    let mut found = BTreeSet::new();
    for line in body.lines().map(str::trim) {
        let Some(arm) = line.split_once(" =>").map(|(a, _)| a) else { continue };
        if !arm.starts_with('"') {
            continue;
        }
        for name in arm.split('|').map(|n| n.trim().trim_matches('"')) {
            found.insert(name.to_string());
        }
    }
    found
}

/// Messages protocol.ts declares that the recorder never sends, or sends
/// without declaring; types the server handles that aren't declared, or
/// declared ones it ignores.
#[test]
fn every_message_type_is_declared() {
    let from = declared("FromRecorder");
    let sent = sent_types();
    let undeclared: Vec<_> = sent.difference(&from).collect();
    assert!(undeclared.is_empty(), "sent but not in protocol.ts's FromRecorder: {undeclared:?}");
    let never: Vec<_> = from.difference(&sent).collect();
    assert!(never.is_empty(), "in protocol.ts's FromRecorder but never sent: {never:?}");

    let to = declared("ToRecorder");
    let handled = handled_types();
    let undeclared: Vec<_> = handled.difference(&to).collect();
    assert!(undeclared.is_empty(), "handled but not in protocol.ts's ToRecorder: {undeclared:?}");
    let ignored: Vec<_> = to.difference(&handled).collect();
    assert!(ignored.is_empty(), "in protocol.ts's ToRecorder but not handled: {ignored:?}");
}

/// What the interface sends is what the server reads (the shapes, as the docs give them).
#[test]
fn messages_to_the_recorder() {
    let msgs = vec![
        json!({ "type": "setConfig", "config": serde_json::to_value(conventional_config()).unwrap() }),
        json!({ "type": "start" }),
        json!({ "type": "stop" }),
        json!({ "type": "listen", "on": true, "system": null, "talkgroup": 101 }),
        json!({ "type": "listDir", "path": "" }),
        json!({ "type": "surveyStart", "source": 0, "bands": ["800"], "findGain": true }),
        json!({ "type": "profileSource", "source": 0, "centerHz": 460_000_000 }),
        json!({ "type": "installPlugin", "id": "openmhz" }),
        json!({ "type": "installPlugin", "repository": "someone/plugin", "tag": "v1.0.0" }),
        json!({ "type": "subscribe", "topics": ["spectrum:0", "rf:0", "decode:dcfd", "log", "platform"] }),
        json!({ "type": "statsQuery", "id": 1, "series": ["sys/dcfd/cc/*"], "range": "24h", "points": 200 }),
        json!({ "type": "statsQuery", "id": 2, "series": ["plat/cpu"], "from": 1_750_000_000, "to": 1_750_086_400 }),
        json!({ "type": "radioQuery", "id": 3, "what": "talkgroups", "system": "dcfd", "hours": 168, "limit": 100 }),
        json!({ "type": "radioQuery", "id": 4, "what": "unit", "system": "dcfd", "key": 1234 }),
    ];
    check_all("ToRecorder", &msgs);
}
