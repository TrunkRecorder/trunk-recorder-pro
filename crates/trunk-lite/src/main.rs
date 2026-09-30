//! Trunk Recorder Lite — command line.
//!
//! ```text
//! trunk-lite [serve] [--config file.json] [--port 8080] [--bind 127.0.0.1] [--no-open]
//!     The app: a browser interface at http://localhost:8080 to set up, start
//!     and watch the recorder. Calls go to the capture folder in the config.
//!     Already running on that port? Opens the browser there and exits.
//!     --start (or the config's server.autoStart): start recording right away.
//!
//! trunk-lite replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [options]
//! trunk-lite replay --source cap1.cu8,center,rate --source cap2.cu8,center,rate --cc Hz …
//!     Captures: cu8 (rtl_sdr), cs16 or cf32 (GNU Radio / UHD) — from the
//!     extension (.cf32/.cfile/.fc32, .cs16/.sc16) or `--format`, or a 4th
//!     --source field.
//!     Record a trunked system from rtl_sdr captures (unsigned 8-bit IQ), writing
//!     <out>/<tg>-<epoch>_<freq>.wav + .json like Trunk Recorder.
//!     --out calls  --short-name sys1  --talkgroups tg.csv  --bandplan file
//!     --recorders 32  --preroll 1  --timeout 3  --epoch <unix s>
//!     --record-encrypted  --keep-silent  --no-unknown  --quiet
//!     Conventional channels (with or without --cc): --fm Hz[,Hz…]  --p25 Hz[,Hz…]
//!     --squelch dB (open threshold above the noise floor, default 8)
//!
//! trunk-lite devices [--usrp [args]]
//!     List RTL-SDRs and Airspys, and whether the USRP (UHD) and Airspy
//!     drivers are installed; --usrp also searches for USRPs.
//! trunk-lite capture <out.cu8> --freq Hz --rate Hz [--gain dB] [--ppm 0] [--serial S] [--seconds 10]
//!     Record raw u8 IQ, like rtl_sdr.
//!
//! trunk-lite tool cc|voice|frames <capture.cu8> --center Hz --rate Hz (--cc Hz | --freq Hz) [options]
//!     One channel's decode, as JSON lines (the research/native-bench format).
//! ```

mod radio;
mod runtime;
mod sdr;
mod server;
mod tool;

pub use trunk_app::config;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use trunk_core::trunk::{parse_csv, CallConfig, ConvChannel, ConvConfig, ConvMode, Engine, EngineConfig, Event, SourceConfig};

/// `--key value` / `--flag` arguments after the positionals.
pub struct Args {
    pub positional: Vec<String>,
    pub opts: HashMap<String, Vec<String>>,
}

impl Args {
    pub fn parse(args: &[String]) -> Args {
        let mut a = Args { positional: vec![], opts: HashMap::new() };
        let mut i = 0;
        while i < args.len() {
            if let Some(k) = args[i].strip_prefix("--") {
                let v = if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    args[i].clone()
                } else {
                    "1".into()
                };
                a.opts.entry(k.to_string()).or_default().push(v);
            } else {
                a.positional.push(args[i].clone());
            }
            i += 1;
        }
        a
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.opts.get(k).and_then(|v| v.last()).map(|s| s.as_str())
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.opts.get(k).map(|v| v.iter().map(|s| s.as_str()).collect()).unwrap_or_default()
    }
    pub fn num(&self, k: &str, d: f64) -> f64 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    pub fn flag(&self, k: &str) -> bool {
        self.get(k).is_some_and(|v| v != "0")
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

const USAGE: &str = "\
Trunk Recorder Lite — record a P25 trunked radio system from RTL-SDRs.

usage:
  trunk-lite [serve] [--port 8080] [--bind 127.0.0.1] [--config file.json] [--no-open] [--start]
      Start the recorder and open its web interface (the default). Use
      --bind 0.0.0.0 to reach it from other machines (no authentication!).
      --start begins recording with the saved settings at once.
  trunk-lite devices [--usrp]
      List RTL-SDRs and Airspys (and USRPs with --usrp); shows whether the
      optional USRP (UHD) and Airspy (libairspy) drivers are installed.
  trunk-lite capture <out.cu8> --freq Hz [--rate 2400000] [--gain dB] [--serial S] [--seconds 10]
      Record raw IQ, like rtl_sdr.
  trunk-lite replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [--out calls] …
  trunk-lite replay --source cap.cu8,center,rate [--source …] --cc Hz …
  trunk-lite replay <capture> --center Hz --rate Hz --fm Hz[,Hz…] --p25 Hz[,Hz…] [--squelch 8]
      Record calls from captures instead of dongles (a trunked system from
      --cc, conventional analog FM / P25 channels, or both).
  trunk-lite tool cc|voice|frames|p2 <capture.cu8> …
      One channel's decode as JSON lines (diagnostics).
  trunk-lite --version

Docs: https://github.com/TrunkRecorder/trunk-recorder-lite
";

fn main() {
    // Finder may pass a process serial number (-psn_…) to an app bundle.
    let argv: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    match argv.first().map(|s| s.as_str()) {
        Some("--version" | "-V" | "version") => println!("trunk-lite {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h" | "help") => print!("{USAGE}"),
        None | Some("serve") => serve(&Args::parse(argv.get(1..).unwrap_or(&[]))),
        Some(s) if s.starts_with("--") => serve(&Args::parse(&argv)),
        Some("replay") => replay(&Args::parse(&argv[1..])),
        Some("tool") => tool::run(&Args::parse(&argv[1..])),
        Some("devices") => devices(&Args::parse(&argv[1..])),
        Some("capture") => capture(&Args::parse(&argv[1..])),
        _ => die(USAGE),
    }
}

fn devices(a: &Args) {
    println!("RTL-SDR (built in):");
    for d in sdr::devices() {
        println!("  {} · SN {}", d["product"].as_str().unwrap_or(""), d["serial"].as_str().unwrap_or(""));
    }
    let ai = radio::airspy::info();
    println!("Airspy: {}", ai.detail);
    for d in radio::airspy::devices() {
        println!("  {}", d["label"].as_str().unwrap_or(""));
    }
    let ui = radio::uhd::info();
    println!("USRP: {}", ui.detail);
    if ui.loaded {
        match a.get("usrp") {
            None => println!("  (search with: trunk-lite devices --usrp)"),
            Some(args) => {
                let args = if args == "1" { "" } else { args };
                match radio::uhd::find(args) {
                    Ok(v) if v.is_empty() => println!("  none found"),
                    Ok(v) => v.iter().for_each(|d| println!("  {d}")),
                    Err(e) => println!("  {e}"),
                }
            }
        }
    }
}

fn replay(a: &Args) {
    // Sources: --source file,center,rate (repeatable) or one positional capture.
    let mut files = Vec::new();
    let mut formats = Vec::new();
    let mut sources = Vec::new();
    let format_of = |path: &str, given: Option<&str>| match given.or(a.get("format")) {
        Some("cu8") => config::SampleFormat::Cu8,
        Some("cs16") => config::SampleFormat::Cs16,
        Some("cf32") => config::SampleFormat::Cf32,
        Some(f) => die(&format!("unknown sample format {f} (cu8, cs16, cf32)")),
        None => config::SampleFormat::from_path(path),
    };
    for s in a.all("source") {
        let f: Vec<&str> = s.split(',').collect();
        if f.len() != 3 {
            die("--source wants file,center,rate[,cu8|cs16|cf32]");
        }
        files.push(f[0].to_string());
        formats.push(format_of(f[0], f.get(3).copied()));
        sources.push(SourceConfig { center_hz: f[1].parse().unwrap_or(0.0), rate_hz: f[2].parse().unwrap_or(2_400_000.0) });
    }
    if let Some(p) = a.positional.first() {
        files.push(p.clone());
        formats.push(format_of(p, None));
        sources.push(SourceConfig { center_hz: a.num("center", 0.0), rate_hz: a.num("rate", 2_400_000.0) });
    }
    if files.is_empty() {
        die("replay: no capture given");
    }
    let hz_list = |k: &str| -> Vec<f64> { a.get(k).unwrap_or("").split(',').filter_map(|s| s.trim().parse().ok()).collect() };
    let ccs = hz_list("cc");
    let conventional: Vec<ConvChannel> = hz_list("fm")
        .into_iter()
        .map(|f| ConvChannel::new(f, ConvMode::Fm))
        .chain(hz_list("p25").into_iter().map(|f| ConvChannel::new(f, ConvMode::P25)))
        .collect();
    let out_dir = a.get("out").unwrap_or("calls").to_string();
    fs::create_dir_all(&out_dir).unwrap_or_else(|e| die(&format!("{out_dir}: {e}")));
    let talkgroups = a.get("talkgroups").map(|p| parse_csv(&fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{p}: {e}"))))).unwrap_or_default();
    let quiet = a.flag("quiet");

    let cfg = EngineConfig {
        short_name: a.get("short-name").unwrap_or("replay").into(),
        control_channels: ccs,
        sources,
        preroll_s: a.num("preroll", 1.0),
        max_recorders: a.num("recorders", 32.0) as usize,
        keep_silent_calls: a.flag("keep-silent"),
        calls: CallConfig {
            call_timeout_s: a.num("timeout", 3.0),
            record_unknown: !a.flag("no-unknown"),
            record_encrypted: a.flag("record-encrypted"),
            ..Default::default()
        },
        epoch_ms_at_zero: a.num("epoch", 0.0) * 1000.0,
        conventional,
        conv: ConvConfig { squelch_db: a.num("squelch", ConvConfig::default().squelch_db), ..Default::default() },
        ..Default::default()
    };
    let mut engine = Engine::new(cfg, talkgroups).unwrap_or_else(|e| die(&e));
    let bandplan = a.get("bandplan").map(str::to_string);
    if let Some(p) = &bandplan {
        if let Ok(s) = fs::read_to_string(p) {
            engine.load_bandplan(&s);
        }
    }

    let data: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap_or_else(|e| die(&format!("{f}: {e}")))).collect();
    let rate0 = engine.sources()[0].rate_hz;
    let air_s = (data[0].len() / formats[0].bytes_per_sample()) as f64 / rate0;
    let t0 = Instant::now();
    // Interleave the sources in ~13.6 ms chunks (rtl_sdr's 32768-sample transfers
    // at 2.4 MSPS), so their clocks advance together as they would live.
    let chunk_s = 32768.0 / 2_400_000.0;
    let mut pos = vec![0usize; data.len()];
    let mut written = 0;
    loop {
        let mut any = false;
        for (i, d) in data.iter().enumerate() {
            let bps = formats[i].bytes_per_sample();
            let n = ((engine.sources()[i].rate_hz * chunk_s) as usize) * bps;
            let end = (pos[i] + n).min(d.len() - d.len() % bps);
            if pos[i] < end {
                match formats[i] {
                    config::SampleFormat::Cu8 => engine.push_u8(i, &d[pos[i]..end]),
                    f => engine.push_iq(i, &trunk_app::samples::to_iq(f, &d[pos[i]..end])),
                }
                pos[i] = end;
                any = true;
            }
        }
        written += handle_events(&mut engine, &out_dir, quiet);
        if !any {
            break;
        }
    }
    engine.finish();
    written += handle_events(&mut engine, &out_dir, quiet);
    let cpu = t0.elapsed().as_secs_f64();
    if let Some(p) = &bandplan {
        let _ = fs::write(p, engine.bandplan());
    }
    let st = engine.status();
    let id = &st.identity;
    println!(
        "\n{air_s:.1} s of air in {cpu:.2} s ({:.0}× real time). CC: {} good / {} bad TSBKs, {}, NAC {} WACN {} SysID {}. {written} call(s) written to {out_dir}/",
        air_s / cpu,
        st.good,
        st.bad,
        st.modulation,
        id.nac.map_or("?".into(), |v| format!("{v:x}")),
        id.wacn.map_or("?".into(), |v| format!("{v:x}")),
        id.sys_id.map_or("?".into(), |v| format!("{v:x}")),
    );
}

fn handle_events(engine: &mut Engine, out_dir: &str, quiet: bool) -> usize {
    let mut written = 0;
    for ev in engine.drain_events() {
        match ev {
            Event::ControlChannel { freq_hz } if !quiet => println!("control channel {:.4} MHz", freq_hz as f64 / 1e6),
            Event::CallStart(c) if !quiet => println!(
                "{:7.2}s  CALL {} start TG {} {:.4} MHz{} → {}",
                c.start_s,
                c.id,
                c.talkgroup,
                c.freq_hz as f64 / 1e6,
                if c.phase2_tdma { format!(" slot {}", c.tdma_slot) } else { String::new() },
                if c.recording { "recording".into() } else { format!("monitoring ({})", c.reason.map_or("", |r| r.as_str())) }
            ),
            Event::CallEnd(c) if !quiet => {
                let srcs: Vec<String> = c.sources.iter().map(|s| s.src.to_string()).collect();
                println!("{:7.2}s  CALL {} end   TG {} srcs [{}]{}", c.last_update_s.max(c.last_audio_s), c.id, c.talkgroup, srcs.join(","), if c.encrypted { " ENC" } else { "" });
            }
            Event::Concluded(k) => {
                // (Appended, not with_extension: a TDMA base name ends in ".<slot>".)
                let base = Path::new(out_dir).join(&k.base_name).display().to_string();
                let _ = fs::write(format!("{base}.wav"), trunk_core::wav::encode(&k.audio, 8000));
                let _ = fs::write(format!("{base}.json"), &k.json);
                if !quiet {
                    println!("         wrote {}.wav  ({:.1} s audio)", k.base_name, k.audio.len() as f64 / 8000.0);
                }
                written += 1;
            }
            _ => {}
        }
    }
    written
}

fn capture(a: &Args) {
    use std::io::Write;
    use std::sync::{atomic::AtomicBool, mpsc, Arc};
    let out = a.positional.first().unwrap_or_else(|| die("capture <out.cu8> --freq Hz …"));
    let cfg = sdr::RtlConfig {
        serial: a.get("serial").unwrap_or("").into(),
        center_hz: a.num("freq", 0.0) as u64,
        rate_hz: a.num("rate", 2_400_000.0) as u32,
        gain_db: a.get("gain").and_then(|g| g.parse().ok()),
        ppm: a.num("ppm", 0.0) as i32,
    };
    let want = (a.num("seconds", 10.0) * cfg.rate_hz as f64) as u64 * 2;
    let (tx, rx) = mpsc::sync_channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let c2 = cfg.clone();
    let th = std::thread::spawn(move || sdr::run(0, c2, tx, s2));
    let mut f = std::io::BufWriter::new(fs::File::create(out).unwrap_or_else(|e| die(&format!("{out}: {e}"))));
    let (mut got, mut dropped) = (0u64, 0u64);
    let t0 = Instant::now();
    while got < want {
        match rx.recv() {
            Ok(sdr::SourceMsg::Data { bytes, dropped: d, .. }) => {
                let n = (bytes.len() as u64).min(want - got) as usize;
                let _ = f.write_all(&bytes[..n]);
                got += n as u64;
                dropped += d;
            }
            Ok(sdr::SourceMsg::Error { error, .. }) => eprintln!("{error}"),
            Ok(sdr::SourceMsg::Iq { .. }) => {}
            Ok(sdr::SourceMsg::End { .. }) | Err(_) => break,
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(rx);
    let _ = th.join();
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("{} samples in {secs:.2} s ({:.3} of real time), {dropped} dropped", got / 2, (got / 2) as f64 / cfg.rate_hz as f64 / secs);
}

/// A startup failure of the app. Started from Finder / Explorer / a desktop
/// launcher there is no terminal to print to, so also show a dialog.
fn fatal(msg: &str) -> ! {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        let text = format!("Trunk Recorder Lite couldn't start.\n\n{msg}");
        if cfg!(target_os = "macos") {
            let script = format!("display alert \"Trunk Recorder Lite\" message {:?} as critical", text);
            let _ = std::process::Command::new("osascript").args(["-e", &script]).status();
        } else if cfg!(target_os = "linux") {
            let _ = std::process::Command::new("notify-send").args(["-u", "critical", "Trunk Recorder Lite", &text]).status();
        }
    }
    die(msg)
}

fn serve(a: &Args) {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    let config_path = a.get("config").map(std::path::PathBuf::from).unwrap_or_else(|| config::config_dir().join("config.json"));
    let mut cfg = config::Config::load(&config_path);
    if let Some(p) = a.get("port").and_then(|p| p.parse().ok()) {
        cfg.server.port = p;
    }
    if let Some(b) = a.get("bind") {
        cfg.server.bind = b.to_string();
    }
    let addr: std::net::SocketAddr = format!("{}:{}", cfg.server.bind, cfg.server.port).parse().unwrap_or_else(|e| fatal(&format!("bind address: {e}")));
    let url = format!("http://{}:{}", if addr.ip().is_unspecified() { "localhost".into() } else { addr.ip().to_string() }, addr.port());
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Launched twice (a double-click on an app that is already
            // running): show the running one.
            if already_running(addr.port()) {
                println!("Trunk Recorder Lite is already running — {url}");
                if !a.flag("no-open") {
                    open_browser(&url);
                }
                return;
            }
            fatal(&format!("Port {} is in use by another program. Start with --port <another port>.", addr.port()));
        }
        Err(e) => fatal(&format!("web server on {addr}: {e}")),
    };
    let history = runtime::scan_history(Path::new(&cfg.recording.capture_dir), 300);
    let (hub, _) = tokio::sync::broadcast::channel(4096);
    let ctx = Arc::new(runtime::Ctx {
        config_path: config_path.clone(),
        config: Mutex::new(cfg),
        hub,
        runner: Mutex::new(None),
        phase: Mutex::new(runtime::PhaseInfo { phase: "idle", error: None, ended: false }),
        history: Mutex::new(history.into_iter().collect::<VecDeque<_>>()),
        quit: tokio::sync::Notify::new(),
    });
    println!("Trunk Recorder Lite {} — open {url}\nconfig: {}", env!("CARGO_PKG_VERSION"), config_path.display());
    let auto = a.flag("start") || ctx.config.lock().unwrap().server.auto_start;
    if auto {
        let cfg = ctx.config.lock().unwrap().clone();
        ctx.set_phase("starting", None, false);
        match runtime::start(ctx.clone(), cfg) {
            Ok(r) => *ctx.runner.lock().unwrap() = Some(r),
            Err(e) => {
                eprintln!("not started: {e}");
                ctx.set_phase("idle", Some(e), false);
            }
        }
    }
    if !a.flag("no-open") {
        open_browser(&url);
    }
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fatal(&e.to_string()));
    if let Err(e) = rt.block_on(server::serve(ctx.clone(), listener)) {
        fatal(&format!("web server on {addr}: {e}"));
    }
    println!("Stopped.");
}

/// Is trunk-lite what answers on this port?
fn already_running(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_secs(1)) else { return false };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    if s.write_all(b"GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut body = String::new();
    let _ = s.read_to_string(&mut body);
    body.contains(&format!("\"app\":\"{}\"", server::APP_ID))
}

fn open_browser(url: &str) {
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    let _ = r;
}
