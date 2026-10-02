//! Trunk Recorder Pro — command line.
//!
//! ```text
//! trunk-pro [serve] [--config file.json] [--port 8080] [--bind 127.0.0.1] [--no-open]
//!     The app: a browser interface at http://localhost:8080 to set up, start
//!     and watch the recorder. Calls go to the capture folder in the config.
//!     Already running on that port? Opens the browser there and exits.
//!     --start (or the config's server.autoStart): start recording right away.
//!
//! trunk-pro replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [options]
//! trunk-pro replay --source cap1.cu8,center,rate --source cap2.cu8,center,rate --cc Hz …
//!     Captures: cu8 (rtl_sdr), cs16 or cf32 (GNU Radio / UHD) — from the
//!     extension (.cf32/.cfile/.fc32, .cs16/.sc16) or `--format`, or a 4th
//!     --source field.
//!     Record a trunked system from rtl_sdr captures (unsigned 8-bit IQ), writing
//!     <out>/<tg>-<epoch>_<freq>.wav + .json like Trunk Recorder.
//!     --out calls  --short-name sys1  --talkgroups tg.csv  --bandplan file
//!     --recorders 32  --preroll 1  --timeout 3  --epoch <unix s>
//!     --record-encrypted  --keep-silent  --no-unknown  --capture-frames  --quiet
//!     More systems (or sites): --system name:Hz[,Hz…][:nac=443,sysid=445,wacn=bee00,rfss=1,site=3,group=name]
//!     (repeatable; the identity is optional — a control channel that
//!     disagrees isn't followed). With several, calls go to <out>/<name>/.
//!     SmartNet: --smartnet 800_standard|800_reband|800_splinter|900|400_custom
//!     (400_custom: --bp-base Hz --bp-spacing Hz --bp-offset N --bp-high Hz)
//!     [--analog-default] (talkgroups never heard granted are analog FM)
//!     Conventional channels (with or without --cc): --fm Hz[,Hz…]  --p25 Hz[,Hz…]
//!     or --channels channels.csv (the channel-file format; see README)
//!     --squelch dB (open threshold above the noise floor, default 8)
//!
//! trunk-pro devices [--usrp [args]]
//!     List RTL-SDRs, Airspys and SoapySDR devices, whether the USRP (UHD),
//!     Airspy and SoapySDR drivers are installed, and SoapySDR's modules;
//!     --usrp also searches for USRPs.
//! trunk-pro capture <out.cu8> --freq Hz --rate Hz [--gain dB] [--ppm 0] [--serial S] [--seconds 10]
//!     Record raw u8 IQ, like rtl_sdr.
//!
//! trunk-pro survey [--serial S] [--bands 800,700,…] [--gain dB | --no-gain] [--ppm 0] [--seconds 30]
//! trunk-pro survey <capture> --center Hz --rate Hz
//!     Find a P25 system: scan the bands (or look at a capture), then listen
//!     to the best control channel; JSON lines of what was found, then the
//!     system (IDs, band plan, alternates, neighbours, voice channels, ppm).
//!
//! trunk-pro tool cc|voice|frames <capture.cu8> --center Hz --rate Hz (--cc Hz | --freq Hz) [options]
//!     One channel's decode, as JSON lines (the research/native-bench format).
//! trunk-pro tool revoice <call.frames.jsonl> <out.wav> [--profile enhanced|mbelib]
//!     Vocode a call's saved frames again.
//!
//! trunk-pro plugin list | describe <executable> | run <executable | id> [calls…]
//!     Plugins (plugins.json beside the config): list them, show one's
//!     manifest, or run one against calls already on disk.
//! ```

mod dmrtool;
mod snrtool;
mod plugins;
mod radio;
mod runtime;
mod sdr;
mod server;
mod survey;
mod tool;

pub use trunk_app::config;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use trunk_core::trunk::{parse_csv, CallConfig, ConvChannel, ConvConfig, ConvMode, Engine, EngineConfig, Event, Identity, SourceConfig, SystemConfig};

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
Trunk Recorder Pro — record a P25 trunked radio system from RTL-SDRs.

usage:
  trunk-pro [serve] [--port 8080] [--bind 127.0.0.1] [--config file.json] [--no-open] [--start]
      Start the recorder and open its web interface (the default). Use
      --bind 0.0.0.0 to reach it from other machines (no authentication!).
      --start begins recording with the saved settings at once.
  trunk-pro devices [--usrp]
      List RTL-SDRs, Airspys and SoapySDR devices (and USRPs with --usrp);
      shows whether the optional USRP (UHD), Airspy (libairspy) and SoapySDR
      drivers are installed, and which SoapySDR modules.
  trunk-pro capture <out.cu8> --freq Hz [--rate 2400000] [--gain dB] [--serial S] [--seconds 10]
      Record raw IQ, like rtl_sdr.
  trunk-pro replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [--out calls] …
  trunk-pro replay --source cap.cu8,center,rate [--source …] --cc Hz …
  trunk-pro replay <capture> … --system name:Hz[,Hz…][:nac=443,site=3] [--system …]
  trunk-pro replay <capture> --center Hz --rate Hz --fm Hz[,Hz…] --p25 Hz[,Hz…] [--squelch 8]
  trunk-pro replay <capture> --center Hz --rate Hz --channels channels.csv
      Record calls from captures instead of dongles (a trunked system from
      --cc, more from --system, conventional analog FM / P25 channels, or both).
  trunk-pro survey [--serial S] [--bands 800,700,900,uhf,vhf,uhf-fed,t-band] [--gain dB] [--seconds 30]
  trunk-pro survey <capture> --center Hz --rate Hz
      Find a P25 system from scratch: scan for control channels, then listen
      to the best one and report its IDs, alternates, neighbours, voice
      channels and the dongle's frequency correction (ppm).
  trunk-pro tool cc|voice|frames|p2 <capture.cu8> …
      One channel's decode as JSON lines (diagnostics).
  trunk-pro tool revoice <call.frames.jsonl> <out.wav> [--profile enhanced|mbelib]
      Vocode a call's saved frames (recording setting \"Save vocoder frames\") again.
  trunk-pro plugin list | describe <executable> | run <executable | id> [calls…]
      Plugins: list them, show one's manifest, or run one against calls
      already on disk (`trunk-pro plugin` for the options).
  trunk-pro --version

Docs: https://github.com/TrunkRecorder/trunk-recorder-pro
";

fn main() {
    // Finder may pass a process serial number (-psn_…) to an app bundle.
    let argv: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    match argv.first().map(|s| s.as_str()) {
        Some("--version" | "-V" | "version") => println!("trunk-pro {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h" | "help") => print!("{USAGE}"),
        None | Some("serve") => serve(&Args::parse(argv.get(1..).unwrap_or(&[]))),
        Some(s) if s.starts_with("--") => serve(&Args::parse(&argv)),
        Some("replay") => replay(&Args::parse(&argv[1..])),
        Some("tool") => tool::run(&Args::parse(&argv[1..])),
        Some("devices") => devices(&Args::parse(&argv[1..])),
        Some("capture") => capture(&Args::parse(&argv[1..])),
        Some("survey") => survey::cli(&Args::parse(&argv[1..])),
        Some("plugin") => plugins::cli::run(&Args::parse(&argv[1..])),
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
            None => println!("  (search with: trunk-pro devices --usrp)"),
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
    let si = radio::soapy::info();
    println!("SoapySDR: {}", si.detail);
    if si.loaded {
        match radio::soapy::modules() {
            None => println!("  modules: (this SoapySDR can't list them)"),
            Some(m) if m.is_empty() => println!("  no modules installed (looked in {})", radio::soapy::search_paths().join(", ")),
            Some(m) => {
                for m in m {
                    let state = if m.error.is_empty() { format!("drivers: {}", m.drivers.join(", ")) } else { format!("FAILED: {}", m.error) };
                    println!("  module {} {} · {state}", m.name, m.version);
                }
            }
        }
        match radio::soapy::find() {
            Ok(v) if v.is_empty() => println!("  no devices found"),
            Ok(v) => v.iter().for_each(|d| println!("  {} · {}", d["label"].as_str().unwrap_or(""), d["args"].as_str().unwrap_or(""))),
            Err(e) => println!("  {e}"),
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
        .chain(hz_list("dmr").into_iter().map(|f| ConvChannel::new(f, ConvMode::Dmr)))
        .chain(a.get("channels").map_or_else(Vec::new, |p| {
            let text = fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{p}: {e}")));
            let parsed = trunk_app::channels::parse(&text).unwrap_or_else(|e| die(&format!("{p}: {e}")));
            for n in &parsed.notes {
                eprintln!("{p}: {n}");
            }
            let mut cfg = config::Config::default();
            cfg.conventional.channels = parsed.channels;
            cfg.engine_config(0.0).conventional
        }))
        .collect();
    let out_dir = a.get("out").unwrap_or("calls").to_string();
    fs::create_dir_all(&out_dir).unwrap_or_else(|e| die(&format!("{out_dir}: {e}")));
    let talkgroups = a.get("talkgroups").map(|p| parse_csv(&fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{p}: {e}"))))).unwrap_or_default();
    let quiet = a.flag("quiet");

    let calls = CallConfig {
        call_timeout_s: a.num("timeout", 3.0),
        record_unknown: !a.flag("no-unknown"),
        record_encrypted: a.flag("record-encrypted"),
        ..Default::default()
    };
    // Trunked systems: --cc (one, named --short-name) and/or --system (repeatable).
    let mut systems: Vec<SystemConfig> = Vec::new();
    if !ccs.is_empty() {
        // --smartnet <band plan>: the --cc system is SmartNet.
        let smartnet = a.get("smartnet").map(|plan| trunk_core::trunk::SmartnetConfig {
            bandplan: tool::smartnet_bandplan(a, if plan == "1" { "800_standard" } else { plan }),
            analog_default: a.flag("analog-default"),
        });
        systems.push(SystemConfig {
            short_name: a.get("short-name").unwrap_or("replay").into(),
            control_channels: ccs,
            calls,
            talkgroups: talkgroups.clone(),
            smartnet,
            // --dmr-trunk: the --cc frequencies are a DMR site's; --dmr-channels
            // more voice frequencies to watch; --lcn 101=452275000,… its channel table.
            dmr: a.flag("dmr-trunk").then(|| trunk_core::dmr::DmrConfig {
                channels: hz_list("dmr-channels"),
                lcn_table: a
                    .get("lcn")
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|e| e.split_once('=').and_then(|(l, h)| Some((l.trim().parse().ok()?, h.trim().parse::<f64>().ok()? as u64))))
                    .collect(),
                color_code: a.get("color-code").and_then(|v| v.parse().ok()),
            }),
            ..Default::default()
        });
    }
    for spec in a.all("system") {
        systems.push(parse_system(spec, calls, &talkgroups).unwrap_or_else(|e| die(&format!("--system {spec}: {e}"))));
    }
    let multi = systems.len() > 1;
    let cfg = EngineConfig {
        systems,
        sources,
        preroll_s: a.num("preroll", 1.0),
        max_recorders: a.num("recorders", 32.0) as usize,
        keep_silent_calls: a.flag("keep-silent"),
        calls,
        epoch_ms_at_zero: a.num("epoch", 0.0) * 1000.0,
        conventional,
        conv: ConvConfig { squelch_db: a.num("squelch", ConvConfig::default().squelch_db), ..Default::default() },
        conv_talkgroups: talkgroups,
        capture_frames: a.flag("capture-frames"),
        ..Default::default()
    };
    let mut engine = Engine::new(cfg).unwrap_or_else(|e| die(&e));
    // One band plan file per system (with several: <file>.<shortName>).
    let bandplans: Vec<String> = a.get("bandplan").map_or_else(Vec::new, |p| {
        engine.systems().iter().map(|s| if multi { format!("{p}.{}", s.short_name) } else { p.to_string() }).collect()
    });
    for (i, p) in bandplans.iter().enumerate() {
        if let Ok(s) = fs::read_to_string(p) {
            engine.load_bandplan(i, &s);
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
        written += handle_events(&mut engine, &out_dir, multi, quiet, a.flag("messages"));
        if !any {
            break;
        }
    }
    engine.finish();
    written += handle_events(&mut engine, &out_dir, multi, quiet, a.flag("messages"));
    let cpu = t0.elapsed().as_secs_f64();
    for (i, p) in bandplans.iter().enumerate() {
        let _ = fs::write(p, engine.bandplan(i));
    }
    let st = engine.status();
    println!("\n{air_s:.1} s of air in {cpu:.2} s ({:.0}× real time). {written} call(s) written to {out_dir}/", air_s / cpu);
    let hex = |v: Option<u32>| v.map_or("?".into(), |v| format!("{v:x}"));
    let dec = |v: Option<u32>| v.map_or("?".into(), |v| v.to_string());
    for s in &st.systems {
        let id = &s.identity;
        println!(
            "  {}: CC {} good / {} bad {}, {}, NAC {} WACN {} SysID {} RFSS {} site {}, {} call(s){}",
            s.short_name,
            s.good,
            s.bad,
            if s.modulation == "2FSK" { "OSWs" } else { "TSBKs" },
            s.modulation,
            hex(id.nac.map(u32::from)),
            hex(id.wacn),
            hex(id.sys_id),
            dec(id.rfss),
            dec(id.site),
            s.calls_concluded,
            s.mismatch.as_ref().map_or(String::new(), |m| format!(" — NOT FOLLOWED: {m}")),
        );
    }
}

/// `--system name:Hz[,Hz…][:nac=443,sysid=445,wacn=bee00,rfss=1,site=3,group=name]` —
/// NAC / SysID / WACN in hex; only a control channel with that identity is followed.
fn parse_system(spec: &str, calls: CallConfig, talkgroups: &trunk_core::trunk::Talkgroups) -> Result<SystemConfig, String> {
    let mut parts = spec.splitn(3, ':');
    let name = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return Err("no short name".into());
    }
    let ccs: Vec<f64> = parts.next().unwrap_or("").split(',').map(|v| v.trim().parse::<f64>().map_err(|_| format!("bad frequency \"{v}\""))).collect::<Result<_, _>>()?;
    let mut expect = Identity::default();
    let mut site_group = String::new();
    for kv in parts.next().unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').ok_or_else(|| format!("\"{kv}\": want key=value"))?;
        let h = || u32::from_str_radix(v.trim(), 16).map_err(|_| format!("bad {k} \"{v}\""));
        let d = || v.trim().parse::<u32>().map_err(|_| format!("bad {k} \"{v}\""));
        match k.trim() {
            "nac" => expect.nac = Some(h()? as u16),
            "sysid" => expect.sys_id = Some(h()?),
            "wacn" => expect.wacn = Some(h()?),
            "rfss" => expect.rfss = Some(d()?),
            "site" => expect.site = Some(d()?),
            "group" => site_group = v.trim().into(),
            other => return Err(format!("unknown key {other} (nac, sysid, wacn, rfss, site, group)")),
        }
    }
    Ok(SystemConfig { short_name: name.into(), control_channels: ccs, calls, talkgroups: talkgroups.clone(), expect, site_group, ..Default::default() })
}

/// "[name] " for a line about `system` when there are several; else "".
fn sys_tag(engine: &Engine, system: u16) -> String {
    match engine.systems().get(system as usize) {
        Some(s) if engine.systems().len() > 1 => format!("[{}] ", s.short_name),
        None if system == trunk_core::trunk::CONVENTIONAL && !engine.systems().is_empty() => "[conv] ".into(),
        _ => String::new(),
    }
}

/// Several systems: each writes to `<out>/<shortName>/`.
fn handle_events(engine: &mut Engine, out_dir: &str, multi: bool, quiet: bool, messages: bool) -> usize {
    let mut written = 0;
    for ev in engine.drain_events() {
        match ev {
            Event::ControlChannel { system, freq_hz } if !quiet => println!("{}control channel {:.4} MHz", sys_tag(engine, system), freq_hz as f64 / 1e6),
            Event::Note { system, text } => eprintln!("{}{text}", sys_tag(engine, system)),
            Event::Message { system, msg } if messages => println!("{:7.2}s  {}{} {}", msg.time_s, sys_tag(engine, system), msg.kind.as_str(), msg.meta),
            Event::CallStart(c) if !quiet => println!(
                "{:7.2}s  {}CALL {} start TG {} {:.4} MHz{}{}{} → {}",
                c.start_s,
                sys_tag(engine, c.system),
                c.id,
                c.talkgroup,
                c.freq_hz as f64 / 1e6,
                if c.analog { " FM" } else { "" },
                if c.phase2_tdma { format!(" slot {}", c.tdma_slot) } else { String::new() },
                if c.patched_talkgroups.is_empty() { String::new() } else { format!(" patched {:?}", c.patched_talkgroups) },
                if c.recording { "recording".into() } else { format!("monitoring ({})", c.reason.map_or("", |r| r.as_str())) }
            ),
            Event::UnitAlias {
                system,
                unit,
                alias,
                talkgroup,
            } if !quiet => {
                println!(
                    "{}ALIAS unit {unit} = \"{alias}\" (TG {talkgroup})",
                    sys_tag(engine, system)
                )
            }
            Event::CallEnd(c) if !quiet => {
                let srcs: Vec<String> = c.sources.iter().map(|s| s.src.to_string()).collect();
                println!("{:7.2}s  {}CALL {} end   TG {} srcs [{}]{}", c.last_update_s.max(c.last_audio_s), sys_tag(engine, c.system), c.id, c.talkgroup, srcs.join(","), if c.encrypted { " ENC" } else { "" });
            }
            Event::Duplicate { call, kept } if !quiet => {
                println!("         {}CALL {} TG {} not saved: a copy of {}CALL {}", sys_tag(engine, call.system), call.id, call.talkgroup, sys_tag(engine, kept.system), kept.id)
            }
            Event::Concluded(k) => {
                // (Appended, not with_extension: a TDMA base name ends in ".<slot>".)
                let dir = if multi { Path::new(out_dir).join(&k.short_name) } else { Path::new(out_dir).to_path_buf() };
                let _ = fs::create_dir_all(&dir);
                let base = dir.join(&k.base_name).display().to_string();
                let _ = fs::write(format!("{base}.wav"), trunk_core::wav::encode(&k.audio, 8000));
                let _ = fs::write(format!("{base}.json"), &k.json);
                if let Some(f) = &k.frames {
                    let _ = fs::write(format!("{base}.frames.jsonl"), f);
                }
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
            Ok(sdr::SourceMsg::Iq { .. } | sdr::SourceMsg::Tuned { .. }) => {}
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
        let text = format!("Trunk Recorder Pro couldn't start.\n\n{msg}");
        if cfg!(target_os = "macos") {
            let script = format!("display alert \"Trunk Recorder Pro\" message {:?} as critical", text);
            let _ = std::process::Command::new("osascript").args(["-e", &script]).status();
        } else if cfg!(target_os = "linux") {
            let _ = std::process::Command::new("notify-send").args(["-u", "critical", "Trunk Recorder Pro", &text]).status();
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
                println!("Trunk Recorder Pro is already running — {url}");
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
        hub: hub.clone(),
        runner: Mutex::new(None),
        phase: Mutex::new(runtime::PhaseInfo { phase: "idle", error: None, ended: false }),
        history: Mutex::new(history.into_iter().collect::<VecDeque<_>>()),
        quit: tokio::sync::Notify::new(),
        survey: Mutex::new(None),
        survey_last: Mutex::new(None),
        plugins: plugins::manage::Plugins::new(&config_path, hub.clone()),
    });
    println!("Trunk Recorder Pro {} — open {url}\nconfig: {}", env!("CARGO_PKG_VERSION"), config_path.display());
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

/// Is trunk-pro what answers on this port?
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
