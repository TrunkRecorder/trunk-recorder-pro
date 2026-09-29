//! Trunk Recorder Lite — command line.
//!
//! ```text
//! trunk-lite replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [options]
//! trunk-lite replay --source cap1.cu8,center,rate --source cap2.cu8,center,rate --cc Hz …
//!     Record a trunked system from rtl_sdr captures (unsigned 8-bit IQ), writing
//!     <out>/<tg>-<epoch>_<freq>.wav + .json like Trunk Recorder.
//!     --out calls  --short-name sys1  --talkgroups tg.csv  --bandplan file
//!     --recorders 32  --preroll 1  --timeout 3  --epoch <unix s>
//!     --record-encrypted  --keep-silent  --no-unknown  --quiet
//!
//! trunk-lite tool cc|voice|frames <capture.cu8> --center Hz --rate Hz (--cc Hz | --freq Hz) [options]
//!     One channel's decode, as JSON lines (the research/native-bench format).
//! ```

mod tool;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use trunk_core::trunk::{parse_csv, CallConfig, Engine, EngineConfig, Event, SourceConfig};

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

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(|s| s.as_str()) {
        Some("replay") => replay(&Args::parse(&argv[1..])),
        Some("tool") => tool::run(&Args::parse(&argv[1..])),
        _ => die("usage: trunk-lite replay|tool …  (see the top of crates/trunk-lite/src/main.rs)"),
    }
}

fn replay(a: &Args) {
    // Sources: --source file,center,rate (repeatable) or one positional capture.
    let mut files = Vec::new();
    let mut sources = Vec::new();
    for s in a.all("source") {
        let f: Vec<&str> = s.split(',').collect();
        if f.len() != 3 {
            die("--source wants file,center,rate");
        }
        files.push(f[0].to_string());
        sources.push(SourceConfig { center_hz: f[1].parse().unwrap_or(0.0), rate_hz: f[2].parse().unwrap_or(2_400_000.0) });
    }
    if let Some(p) = a.positional.first() {
        files.push(p.clone());
        sources.push(SourceConfig { center_hz: a.num("center", 0.0), rate_hz: a.num("rate", 2_400_000.0) });
    }
    if files.is_empty() {
        die("replay: no capture given");
    }
    let ccs: Vec<f64> = a.get("cc").unwrap_or("").split(',').filter_map(|s| s.trim().parse().ok()).collect();
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
    let air_s = data[0].len() as f64 / 2.0 / rate0;
    let t0 = Instant::now();
    // Interleave the sources in ~13.6 ms chunks (rtl_sdr's 32768-sample transfers
    // at 2.4 MSPS), so their clocks advance together as they would live.
    let chunk_s = 32768.0 / 2_400_000.0;
    let mut pos = vec![0usize; data.len()];
    let mut written = 0;
    loop {
        let mut any = false;
        for (i, d) in data.iter().enumerate() {
            let n = ((engine.sources()[i].rate_hz * chunk_s) as usize) * 2;
            let end = (pos[i] + n).min(d.len());
            if pos[i] < end {
                engine.push_u8(i, &d[pos[i]..end]);
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
