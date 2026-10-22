//! Trunk Recorder Pro — command line.
//!
//! ```text
//! trunk-pro [serve] [--config file.json] [--port 8080] [--bind 127.0.0.1] [--no-open] [--ui folder]
//!     The app: a browser interface at http://localhost:8080 to set up, start
//!     and watch the recorder. Calls go to the capture folder in the config.
//!     --ui: an interface of your own (a folder with an index.html; docs/api)
//!     at / instead; the built-in one is at /builtin/.
//!     Already running on that port? Opens the browser there and exits.
//!     --start (or the config's server.autoStart): start recording right away.
//!     --log-level trace|debug|info|warning|error|fatal: over the config's
//!     log.level. The log goes to stderr (and files / syslog as the config's
//!     `log` section says), in Trunk Recorder's format.
//!
//! trunk-pro replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [options]
//! trunk-pro replay --source cap1.cu8,center,rate --source cap2.cu8,center,rate --cc Hz …
//!     Captures: cu8 (rtl_sdr), cs16 or cf32 (GNU Radio / UHD) — from the
//!     extension (.cf32/.cfile/.fc32, .cs16/.sc16) or `--format cu8|cs16|cf32`
//!     (anything else is refused), or a 4th --source field.
//!     Record a trunked system from rtl_sdr captures (unsigned 8-bit IQ), writing
//!     <out>/<tg>-<epoch>_<freq>.wav + .json like Trunk Recorder.
//!     --out calls  --short-name sys1  --talkgroups tg.csv  --bandplan file
//!     --recorders 32  --preroll 1  --timeout 3  --epoch <unix s>
//!     --record-encrypted  --keep-silent  --no-unknown  --capture-frames  --quiet
//!     --keys keys.json|keys.ekc [--keys-password-file F] (P25 encryption keys for every system: see trunk-app's keys.rs)
//!     --messages (print every control channel message)
//!     --min-call s  --max-call s  --min-transmission s (drop short calls, split long
//!     ones, leave out short transmissions)  --auto-tune (correct the sources'
//!     frequency error as measured on the control channels; reported either way)
//!     --guard Hz (left unused at each edge of a source's band; default 75000)
//!     More systems (or sites): --system name[/site]:Hz[,Hz…][:nac=443,sysid=445,wacn=bee00,rfss=1,site=3]
//!     (repeatable; "dcfd/south" is site south of system dcfd, and a call
//!     heard on several sites of a system is saved once; the identity is
//!     optional — a control channel that disagrees isn't followed). With
//!     several systems, calls go to <out>/<name>/.
//!     SmartNet: --smartnet 800_standard|800_reband|800_splinter|900|400_custom
//!     (400_custom: --bp-base Hz --bp-spacing Hz --bp-offset N --bp-high Hz)
//!     [--analog-default] (talkgroups never heard granted are analog FM)
//!     DMR: --dmr-trunk (the --cc frequencies are a Capacity Plus / Capacity
//!     Max / Tier III site's) [--dmr-channels Hz,…] (more voice frequencies to
//!     watch) [--lcn n=Hz,…] (its channel table) [--color-code N]
//!     NXDN: --nxdn-trunk typeC|typeD [--nxdn-rate 48|96] [--nxdn-channels Hz,…]
//!     [--lcn n=Hz,…] [--ran N]
//!     Conventional channels (with or without --cc): --fm Hz[,Hz…]  --am Hz[,Hz…]  --p25 Hz[,Hz…]
//!     --dmr Hz[,Hz…]  --nxdn48 Hz[,Hz…]  --nxdn96 Hz[,Hz…]
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
//! trunk-pro survey [--serial S] [--bands 800,700,…] [--gain dB | --no-gain] [--ppm 0] [--rate Hz] [--seconds 30]
//! trunk-pro survey <capture> --center Hz --rate Hz [--format cu8|cs16|cf32]
//!     either: [--identify lat,lon[,km]] [--api URL] (FCC licenses near there, from trunkrecorder.pro)
//!     Find P25, SmartNet, trunked DMR and NXDN systems: scan the bands (800,
//!     700, 900, uhf, vhf, uhf-fed, biz-uhf, biz-vhf, t-band; or look at a
//!     capture), then listen to the best control channel; JSON lines of what
//!     was found, then the system (IDs, band plan, alternates, neighbours,
//!     voice channels, ppm). --no-gain: keep the default gain rather than
//!     search for the best.
//!
//! trunk-pro rolloff [--serial S] --center Hz [--rate Hz] [--gain dB] [--full]
//! trunk-pro rolloff <capture> --center Hz --rate Hz [--format cu8|cs16|cf32] [--full]
//!     How far in from each edge the band's noise floor sags, and the guard
//!     band to use (as Setup's "Profile roll-off"); JSON (--full: with the
//!     spectra and waterfall).
//!
//! trunk-pro tool cc|voice|frames <capture.cu8> --center Hz --rate Hz (--cc Hz | --freq Hz) [options]
//!     One P25 channel's decode, as JSON lines (the research/native-bench
//!     format); `frames` only counts the frames of each type.
//! trunk-pro tool p2 | smartnet | dmr | dmrscan | nxdn | nxdnscan | nxdnsynth | p25synth | snr …
//!     Phase 2 slots, a SmartNet control channel, one DMR / NXDN channel,
//!     every DMR / NXDN carrier in a capture, a synthetic NXDN or P25 capture,
//!     weak-signal curves (each tool's module documents its options).
//! trunk-pro tool revoice <call.sdr | call.frames.jsonl> <out.wav> [--profile fixed|enhanced|mbelib]
//!     Vocode a call's saved frames again.
//! trunk-pro tool sdr <call.sdr> [--frames]
//!     A saved .sdr call's metadata as MimoSDR reads it, or its frames as JSON lines.
//!
//! trunk-pro plugin search | install | update | uninstall | list | describe | run …
//!     Plugins (set up in the config's `plugins`, and each system's): find them in the registry,
//!     install, update and uninstall them, list them, show one's manifest, or
//!     run one against calls already on disk.
//!
//! trunk-pro interface search | install | update | add | uninstall | list …
//!     Interfaces besides the built-in one (the config's `server.interfaces`), each on a
//!     port of its own: find them in the store, install, update, add a folder of your own.
//! ```

mod access;
mod clock;
mod directory;
mod dmrtool;
mod interfaces;
mod licenses;
mod logging;
mod monitor;
mod nxdntool;
mod p25synth;
mod paths;
mod platform;
mod snrtool;
mod plugins;
mod profile;
mod radio;
mod runtime;
mod sdr;
mod server;
mod spool;
mod spotlight;
mod statstore;
mod store;
mod toolkit;
#[cfg(test)]
mod protocol_tests;
mod survey;
mod tool;
mod unlock;

pub use trunk_app::config;

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use trunk_core::trunk::{parse_csv, CallConfig, ConvChannel, ConvConfig, ConvMode, Engine, EngineConfig, SystemConfig, Voice, Event, IdField, Identity, Protocol, SaveRules, SourceConfig, SiteConfig, DEFAULT_GUARD_HZ};

/// `--key value` / `--flag` arguments after the positionals.
pub struct Args {
    pub positional: Vec<String>,
    pub opts: HashMap<String, Vec<String>>,
}

/// Options that are on or off: they never take the next argument as their
/// value (`--quiet capture.cu8` is a flag and a positional). `--k=v` sets
/// any option, these included (`--start=0`).
const FLAGS: &[&str] = &[
    "analog-default", "auto-tune", "bursts", "capture-frames", "diversity", "dmr-trunk", "frames", "full", "hard-fec", "keep-silent", "messages", "no-gain", "no-open",
    "no-unknown", "osw", "quality", "quiet", "record-encrypted", "repeat", "s16", "separation", "start", "threads",
];

impl Args {
    pub fn parse(args: &[String]) -> Args {
        let mut a = Args { positional: vec![], opts: HashMap::new() };
        let mut i = 0;
        while i < args.len() {
            if let Some((k, v)) = args[i].strip_prefix("--").and_then(|kv| kv.split_once('=')) {
                a.opts.entry(k.to_string()).or_default().push(v.to_string());
            } else if let Some(k) = args[i].strip_prefix("--") {
                let v = if !FLAGS.contains(&k) && i + 1 < args.len() && !args[i + 1].starts_with("--") {
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

/// A capture's sample format: `given` (`--format`), else from `path`'s extension.
pub fn sample_format(path: &str, given: Option<&str>) -> config::SampleFormat {
    match given {
        Some("cu8") => config::SampleFormat::Cu8,
        Some("cs16") => config::SampleFormat::Cs16,
        Some("cf32") => config::SampleFormat::Cf32,
        Some(f) => die(&format!("unknown sample format {f} (cu8, cs16, cf32)")),
        None => config::SampleFormat::from_path(path),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

const USAGE: &str = "\
Trunk Recorder Pro — record P25, SmartNet, DMR and NXDN trunked systems and
conventional channels from SDRs.

usage:
  trunk-pro [serve] [--port 8080] [--bind 127.0.0.1] [--config file.json] [--no-open] [--start] [--ui folder]
      Start the recorder and open its web interface (the default). Use
      --bind 0.0.0.0 to reach it from other machines (no authentication!).
      --ui serves a folder with an index.html as an interface for this run,
      on the next free port, and opens it. See docs/interfaces.
      --start begins recording with the saved settings at once.
      --log-level debug: more in the log (stderr; files and syslog as set up).
  trunk-pro devices [--usrp]
      List RTL-SDRs, Airspys, bladeRFs and SoapySDR devices (and USRPs with
      --usrp); shows whether the optional USRP (UHD), Airspy (libairspy),
      bladeRF (libbladeRF) and SoapySDR
      drivers are installed, and which SoapySDR modules.
  trunk-pro capture <out.cu8> --freq Hz [--rate 2400000] [--gain dB] [--ppm 0] [--serial S] [--seconds 10]
      Record raw IQ, like rtl_sdr.
  trunk-pro replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [--out calls] …
  trunk-pro replay --source cap.cu8,center,rate [--source …] --cc Hz …
  trunk-pro replay <capture> … --system name[/site]:Hz[,Hz…][:nac=443,site=3] [--system …]
  trunk-pro replay <capture> --center Hz --rate Hz --fm Hz[,Hz…] --am Hz[,Hz…] --p25 Hz[,Hz…] [--squelch 8]
  trunk-pro replay <capture> --center Hz --rate Hz --dmr Hz,… --nxdn48 Hz,… --nxdn96 Hz,…
  trunk-pro replay <capture> --center Hz --rate Hz --cc Hz,… --dmr-trunk [--dmr-channels Hz,…] [--lcn n=Hz,…] [--color-code N]
  trunk-pro replay <capture> --center Hz --rate Hz --cc Hz,… --nxdn-trunk typeC|typeD [--nxdn-rate 48|96] [--nxdn-channels Hz,…] [--lcn n=Hz,…] [--ran N]
  trunk-pro replay <capture> --center Hz --rate Hz --cc Hz --smartnet 800_standard|800_reband|800_splinter|900|400_custom
  trunk-pro replay <capture> --center Hz --rate Hz --channels channels.csv
      Record calls from captures instead of dongles (a trunked system from
      --cc, more from --system, conventional analog FM / AM / P25 / DMR / NXDN
      channels, or both). --format cu8|cs16|cf32 (else from the extension),
      --guard Hz (each band edge left unused; 75000), --auto-tune (correct
      the frequency error measured on the control channel), --messages
      (print every control channel message), --quiet.
  trunk-pro survey [--serial S] [--bands 800,700,900,uhf,vhf,uhf-fed,biz-uhf,biz-vhf,t-band] [--gain dB | --no-gain] [--ppm 0] [--rate Hz] [--seconds 30]
  trunk-pro survey <capture> --center Hz --rate Hz [--format cu8|cs16|cf32]
      either: [--identify lat,lon[,km]] [--api URL]
      Find P25, SmartNet, trunked DMR and NXDN systems from scratch: scan for
      control channels, then listen to the best one and report its IDs,
      alternates, neighbours, voice channels and the dongle's frequency
      correction (ppm). --no-gain: don't search for the best gain.
  trunk-pro rolloff [--serial S] --center Hz [--rate Hz] [--gain dB] [--full]
  trunk-pro rolloff <capture> --center Hz --rate Hz [--format cu8|cs16|cf32] [--full]
      How far in from each edge of the band the noise floor sags, and the
      guard band to leave there (--full: with the spectra and waterfall).
  trunk-pro tool cc|voice|frames|p2 <capture.cu8> --center Hz --rate Hz (--cc Hz | --freq Hz) …
      One P25 channel's decode as JSON lines (diagnostics; frames: counts
      of each frame type; p2: a Phase 2 channel, --nac --sysid --wacn).
  trunk-pro tool smartnet <capture.cu8> --center Hz --rate Hz --cc Hz [--osw]
      A SmartNet control channel's messages.
  trunk-pro tool dmrscan|nxdnscan <capture.cu8> --center Hz
      Every DMR / NXDN carrier in a capture.
  trunk-pro tool dmr|nxdn <capture.cu8> --center Hz --freq Hz [--nxdn 48|96] [--audio out.f32]
      One DMR / NXDN channel's messages.
  trunk-pro tool nxdnsynth <out.cu8> --center Hz [--kind conv|typeC|typeD] [--nxdn 48|96]
      A synthetic NXDN capture, for checking a setup.
  trunk-pro tool p25synth <out.cu8> [--center Hz] [--freq Hz] [--keys keys.json] [--voice call.frames.jsonl] [--wav out.wav]
      A synthetic P25 Phase 1 transmission, encrypted with the first key of
      --keys (or clear), for checking decryption (replay --p25 Hz --keys).
  trunk-pro tool snr <capture.cu8> --center Hz --rate Hz --freq Hz --kind dmr|p25|smartnet|p2 [--snr 40,20,…]
      Weak-signal curves: one channel decoded again with noise added.
  trunk-pro tool revoice <call.sdr | call.frames.jsonl> <out.wav> [--profile fixed|enhanced|mbelib]
      Vocode a call's saved frames (recording setting \"Save vocoder frames\") again.
  trunk-pro tool sdr <call.sdr> [--frames]
      A saved .sdr call's metadata as MimoSDR reads it, or (--frames) its frames as JSON lines.
  trunk-pro plugin search | install | update | uninstall | list | describe | run …
      Plugins: find them in the registry, install, update and uninstall them,
      list them, show one's manifest, or run one against calls already on
      disk (`trunk-pro plugin` for the options).
  trunk-pro interface search | install | update | add | uninstall | list …
      Interfaces besides the built-in one, each on a port of its own: find
      them in the store, install and update them, add a folder of your own
      (`trunk-pro interface` for the options).
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
        Some("rolloff") => profile::cli(&Args::parse(&argv[1..])),
        Some("plugin") => plugins::cli::run(&Args::parse(&argv[1..])),
        Some("interface") => interfaces::cli::run(&Args::parse(&argv[1..])),
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
    let bi = radio::bladerf::info();
    println!("bladeRF: {}", bi.detail);
    for d in radio::bladerf::devices() {
        println!("  {} · serial {}", d["label"].as_str().unwrap_or(""), d["serial"].as_str().unwrap_or(""));
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
    let format_of = |path: &str, given: Option<&str>| sample_format(path, given.or(a.get("format")));
    for s in a.all("source") {
        let f: Vec<&str> = s.split(',').collect();
        if !(3..=4).contains(&f.len()) {
            die("--source wants file,center,rate[,cu8|cs16|cf32]");
        }
        files.push(f[0].to_string());
        formats.push(format_of(f[0], f.get(3).copied()));
        sources.push(SourceConfig { center_hz: f[1].parse().unwrap_or(0.0), rate_hz: f[2].parse().unwrap_or(2_400_000.0), auto_tune: a.flag("auto-tune"), guard_hz: a.num("guard", DEFAULT_GUARD_HZ) });
    }
    if let Some(p) = a.positional.first() {
        files.push(p.clone());
        formats.push(format_of(p, None));
        sources.push(SourceConfig { center_hz: a.num("center", 0.0), rate_hz: a.num("rate", 2_400_000.0), auto_tune: a.flag("auto-tune"), guard_hz: a.num("guard", DEFAULT_GUARD_HZ) });
    }
    if files.is_empty() {
        die("replay: no capture given");
    }
    let hz_list = |k: &str| -> Vec<f64> { a.get(k).unwrap_or("").split(',').filter_map(|s| s.trim().parse().ok()).collect() };
    let ccs = hz_list("cc");
    let conventional: Vec<ConvChannel> = hz_list("fm")
        .into_iter()
        .map(|f| ConvChannel::new(f, ConvMode::Fm))
        .chain(hz_list("am").into_iter().map(|f| ConvChannel::new(f, ConvMode::Am)))
        .chain(hz_list("p25").into_iter().map(|f| ConvChannel::new(f, ConvMode::P25)))
        .chain(hz_list("dmr").into_iter().map(|f| ConvChannel::new(f, ConvMode::Dmr)))
        .chain(hz_list("nxdn48").into_iter().map(|f| ConvChannel::new(f, ConvMode::Nxdn(trunk_core::nxdn::Rate::N48))))
        .chain(hz_list("nxdn96").into_iter().map(|f| ConvChannel::new(f, ConvMode::Nxdn(trunk_core::nxdn::Rate::N96))))
        .chain(a.get("channels").map_or_else(Vec::new, |p| {
            let text = fs::read_to_string(p).unwrap_or_else(|e| die(&format!("{p}: {e}")));
            let parsed = trunk_app::channels::parse(&text).unwrap_or_else(|e| die(&format!("{p}: {e}")));
            for n in &parsed.notes {
                eprintln!("{p}: {n}");
            }
            let mut cfg = config::Config::default();
            cfg.conventional = vec![config::Conventional { channels: parsed.channels, ..Default::default() }];
            cfg.engine_config(0.0, &Default::default()).conventional
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
        max_call_s: a.num("max-call", 0.0),
        ..Default::default()
    };
    let save = SaveRules {
        keep_silent: a.flag("keep-silent"),
        keep_encrypted: a.flag("record-encrypted"),
        min_call_s: a.num("min-call", 0.0),
        min_transmission_s: a.num("min-transmission", 0.0),
        ..Default::default()
    };
    // --keys: every system decrypts with the same keys file (a KFDtool container, with --keys-password-file).
    let keys = a.get("keys").map(|p| {
        let l = trunk_app::keys::load(std::path::Path::new(p), a.get("keys-password-file").map(std::path::Path::new)).unwrap_or_else(|e| die(&e));
        for s in &l.skipped {
            eprintln!("--keys: key left out: {s}");
        }
        for w in l.ring.warnings() {
            eprintln!("--keys: {w}");
        }
        std::sync::Arc::new(l.ring)
    });
    // Every system goes by the same rules, talkgroups and keys.
    let system = |name: &str, sites| SystemConfig { calls, save, talkgroups: talkgroups.clone(), keys: keys.clone(), ..SystemConfig::trunked(name, sites) };
    // Trunked systems: --cc (one, named --short-name) and/or --system (repeatable; sites of one system share its name).
    let mut systems: Vec<SystemConfig> = Vec::new();
    if !ccs.is_empty() {
        // --smartnet <band plan>: the --cc system is SmartNet.
        let smartnet = a.get("smartnet").map(|plan| trunk_core::trunk::SmartnetConfig {
            bandplan: tool::smartnet_bandplan(a, if plan == "1" { "800_standard" } else { plan }),
            analog_default: a.flag("analog-default"),
        });
        let site = SiteConfig {
            control_channels: ccs,
            // --dmr-trunk: the --cc frequencies are a DMR site's; --dmr-channels
            // more voice frequencies to watch; --lcn 101=452275000,… its channel table.
            // --nxdn-trunk [typeC|typeD]: an NXDN site's (--nxdn-rate 48|96,
            // --nxdn-channels, --lcn for its channel numbers, --ran).
            protocol: match a.flag("dmr-trunk").then(|| trunk_core::dmr::DmrConfig {
                channels: hz_list("dmr-channels"),
                lcn_table: lcn_table(a),
                color_code: a.get("color-code").and_then(|v| v.parse().ok()),
            }) {
                Some(dc) => Protocol::Dmr(dc),
                None if a.get("nxdn-trunk").is_some() => {
                    use trunk_core::nxdn::trunking::{Kind, NxdnConfig};
                    Protocol::Nxdn(NxdnConfig {
                        kind: if a.get("nxdn-trunk").is_some_and(|k| k.eq_ignore_ascii_case("typeD") || k.eq_ignore_ascii_case("d")) { Kind::TypeD } else { Kind::TypeC },
                        rate: trunk_core::nxdn::Rate::parse(a.get("nxdn-rate").unwrap_or("48")).unwrap_or_else(|| die("--nxdn-rate 48|96")),
                        channel_table: lcn_table(a).into_iter().collect(),
                        channels: hz_list("nxdn-channels"),
                        ran: a.get("ran").and_then(|v| v.parse().ok()),
                    })
                }
                None => smartnet.map_or(Protocol::P25, Protocol::SmartNet),
            },
            ..Default::default()
        };
        systems.push(system(a.get("short-name").unwrap_or("replay"), vec![site]));
    }
    for spec in a.all("system") {
        let (name, site) = parse_system(spec).unwrap_or_else(|e| die(&format!("--system {spec}: {e}")));
        match systems.iter_mut().find(|s| s.short_name == name) {
            Some(s) => s.sites.push(site),
            None => systems.push(system(&name, vec![site])),
        }
    }
    let multi = systems.len() > 1;
    let sites = trunk_core::trunk::sites(&systems).count() > 1;
    // The conventional channels: one system of their own, "conv".
    let mut conventional = conventional;
    if !conventional.is_empty() {
        conventional.iter_mut().for_each(|c| c.system = systems.len());
        systems.push(system("conv", vec![]));
    }
    let cfg = EngineConfig {
        systems,
        sources,
        preroll_s: a.num("preroll", 1.0),
        max_recorders: a.num("recorders", 32.0) as usize,
        epoch_ms_at_zero: a.num("epoch", 0.0) * 1000.0,
        conventional,
        conv: ConvConfig { squelch_db: a.num("squelch", ConvConfig::default().squelch_db), ..Default::default() },
        capture_frames: a.flag("capture-frames"),
        ..Default::default()
    };
    let engine = Engine::new(cfg).unwrap_or_else(|e| die(&e));
    // One band plan file per site (with several: <file>.<shortName>, <file>.<shortName>.<site>).
    let bandplans: Vec<String> = a.get("bandplan").map_or_else(Vec::new, |p| {
        let systems = engine.systems();
        trunk_core::trunk::sites(&systems).map(|(n, s)| if sites { format!("{p}.{}", trunk_app::session::bandplan_key(&systems[n as usize], s)) } else { p.to_string() }).collect()
    });
    for (i, p) in bandplans.iter().enumerate() {
        if let Ok(s) = fs::read_to_string(p) {
            engine.load_bandplan(i, &s);
        }
    }

    // Streamed from disk, not read whole: a long capture is gigabytes (a
    // small Raspberry Pi has 1–2 GB of memory).
    let mut data: Vec<(fs::File, usize)> = files
        .iter()
        .map(|f| {
            let h = fs::File::open(f).unwrap_or_else(|e| die(&format!("{f}: {e}")));
            let len = h.metadata().map(|m| m.len() as usize).unwrap_or_else(|e| die(&format!("{f}: {e}")));
            (h, len)
        })
        .collect();
    let rate0 = engine.sources()[0].rate_hz;
    let air_s = (data[0].1 / formats[0].bytes_per_sample()) as f64 / rate0;
    let mut buf = Vec::new();
    let t0 = Instant::now();
    // Interleave the sources in ~13.6 ms chunks (rtl_sdr's 32768-sample transfers
    // at 2.4 MSPS), so their clocks advance together as they would live.
    let chunk_s = 32768.0 / 2_400_000.0;
    let mut written = 0;
    if a.flag("threads") {
        // --threads: each source fed from a thread of its own, as live — none
        // more than LEAD chunks (~55 ms of air) ahead of the slowest.
        const LEAD: usize = 4;
        let done: Vec<std::sync::atomic::AtomicUsize> = data.iter().map(|_| Default::default()).collect();
        let running = std::sync::atomic::AtomicUsize::new(data.len());
        std::thread::scope(|sc| {
            for (i, (mut file, len)) in data.drain(..).enumerate() {
                let (engine, done, running, files, formats) = (&engine, &done, &running, &files, &formats);
                std::thread::Builder::new()
                    .name(format!("dsp-{i}"))
                    .spawn_scoped(sc, move || {
                        use std::sync::atomic::Ordering::SeqCst;
                        let bps = formats[i].bytes_per_sample();
                        let n = ((engine.sources()[i].rate_hz * chunk_s) as usize) * bps;
                        let (mut pos, mut k, mut buf) = (0usize, 0usize, Vec::new());
                        loop {
                            let end = (pos + n).min(len - len % bps);
                            if pos >= end {
                                break;
                            }
                            while done.iter().enumerate().any(|(j, d)| j != i && d.load(SeqCst) + LEAD < k && d.load(SeqCst) != usize::MAX) {
                                std::thread::yield_now();
                            }
                            buf.resize(end - pos, 0);
                            file.read_exact(&mut buf).unwrap_or_else(|e| die(&format!("{}: {e}", files[i])));
                            match formats[i] {
                                config::SampleFormat::Cu8 => engine.push_u8(i, &buf),
                                f => engine.push_iq(i, &trunk_app::samples::to_iq(f, &buf)),
                            }
                            pos = end;
                            k += 1;
                            done[i].store(k, SeqCst);
                        }
                        // (Finished: no longer holds the others back.)
                        done[i].store(usize::MAX, SeqCst);
                        running.fetch_sub(1, SeqCst);
                    })
                    .expect("thread");
            }
            while running.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                written += handle_events(&engine, &out_dir, multi, quiet, a.flag("messages"));
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
    } else {
        let mut pos = vec![0usize; data.len()];
        loop {
            let mut any = false;
            for (i, (file, len)) in data.iter_mut().enumerate() {
                let bps = formats[i].bytes_per_sample();
                let n = ((engine.sources()[i].rate_hz * chunk_s) as usize) * bps;
                let end = (pos[i] + n).min(*len - *len % bps);
                if pos[i] < end {
                    buf.resize(end - pos[i], 0);
                    file.read_exact(&mut buf).unwrap_or_else(|e| die(&format!("{}: {e}", files[i])));
                    match formats[i] {
                        config::SampleFormat::Cu8 => engine.push_u8(i, &buf),
                        f => engine.push_iq(i, &trunk_app::samples::to_iq(f, &buf)),
                    }
                    pos[i] = end;
                    any = true;
                }
            }
            written += handle_events(&engine, &out_dir, multi, quiet, a.flag("messages"));
            if !any {
                break;
            }
        }
    }
    engine.finish();
    written += handle_events(&engine, &out_dir, multi, quiet, a.flag("messages"));
    let cpu = t0.elapsed().as_secs_f64();
    for (i, p) in bandplans.iter().enumerate() {
        let _ = fs::write(p, engine.bandplan(i));
    }
    let st = engine.status();
    println!("\n{air_s:.1} s of air in {cpu:.2} s ({:.0}× real time). {written} call(s) written to {out_dir}/", air_s / cpu);
    let hex = |v: Option<u32>| v.map_or("?".into(), |v| format!("{v:x}"));
    let dec = |v: Option<u32>| v.map_or("?".into(), |v| v.to_string());
    for s in &st.sites {
        let id = &s.identity;
        println!(
            "  {}: CC {} good / {} bad {}, {}, NAC {} WACN {} SysID {} RFSS {} site {}, {} call(s){}",
            if s.site_name == s.short_name { s.short_name.clone() } else { format!("{}/{}", s.short_name, s.site_name) },
            s.good,
            s.bad,
            if s.modulation == "2FSK" { "OSWs" } else { "TSBKs" },
            s.modulation,
            hex(id.get(IdField::Nac)),
            hex(id.wacn()),
            hex(id.sys_id()),
            dec(id.rfss()),
            dec(id.site()),
            s.calls_concluded,
            s.mismatch.as_ref().map_or(String::new(), |m| format!(" — NOT FOLLOWED: {m}")),
        );
    }
    for (i, t) in st.sources.iter().enumerate() {
        if let Some(e) = t.error_ppm {
            println!("  source {}: frequency error {e:+.2} ppm, corrected by {:+.2} ppm", i + 1, t.applied_ppm);
        }
    }
}

/// `--system name[/site]:Hz[,Hz…][:nac=443,sysid=445,wacn=bee00,rfss=1,site=3]` —
/// NAC / SysID / WACN in hex; only a control channel with that identity is
/// followed. `dcfd/south`: site south of system dcfd.
fn parse_system(spec: &str) -> Result<(String, SiteConfig), String> {
    let mut parts = spec.splitn(3, ':');
    let head = parts.next().unwrap_or("");
    let (name, site) = head.split_once('/').unwrap_or((head, ""));
    let (name, site) = (name.trim(), site.trim());
    if name.is_empty() {
        return Err("no short name".into());
    }
    let ccs: Vec<f64> = parts.next().unwrap_or("").split(',').map(|v| v.trim().parse::<f64>().map_err(|_| format!("bad frequency \"{v}\""))).collect::<Result<_, _>>()?;
    let mut expect = Identity::default();
    for kv in parts.next().unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').ok_or_else(|| format!("\"{kv}\": want key=value"))?;
        let h = || u32::from_str_radix(v.trim(), 16).map_err(|_| format!("bad {k} \"{v}\""));
        let d = || v.trim().parse::<u32>().map_err(|_| format!("bad {k} \"{v}\""));
        match k.trim() {
            "nac" => expect.set(IdField::Nac, h()? as u16 as u32),
            "sysid" => expect.set(IdField::SysId, h()?),
            "wacn" => expect.set(IdField::Wacn, h()?),
            "rfss" => expect.set(IdField::Rfss, d()?),
            "site" => expect.set(IdField::Site, d()?),
            other => return Err(format!("unknown key {other} (nac, sysid, wacn, rfss, site)")),
        }
    }
    Ok((name.into(), SiteConfig { name: site.into(), control_channels: ccs, expect, ..Default::default() }))
}

/// "[name] " ("[name/site] ") for a line about `system` (its site `site`)
/// when there are several sites (a conventional one: any); else "".
fn sys_tag(engine: &Engine, system: u16, site: Option<u16>) -> String {
    let systems = engine.systems();
    let sites: Vec<_> = trunk_core::trunk::sites(&systems).collect();
    match site.and_then(|s| sites.get(s as usize)) {
        Some(&(n, s)) if sites.len() > 1 => format!("[{}] ", systems[n as usize].site_key(s)),
        None if site.is_none() && !sites.is_empty() => systems.get(system as usize).map_or(String::new(), |s| format!("[{}] ", s.short_name)),
        _ => String::new(),
    }
}

/// [`sys_tag`] for site `site`.
fn site_tag(engine: &Engine, site: u16) -> String {
    let system = trunk_core::trunk::sites(&engine.systems()).nth(site as usize).map_or(0, |(n, _)| n);
    sys_tag(engine, system, Some(site))
}

/// "[name] " for a line about system `system` when there are several trunked
/// ones (a conventional one: any); else "".
fn system_tag(engine: &Engine, system: u16) -> String {
    let systems = engine.systems();
    let trunked = systems.iter().filter(|s| !s.is_conventional()).count();
    match systems.get(system as usize) {
        Some(s) if trunked > if s.is_conventional() { 0 } else { 1 } => format!("[{}] ", s.short_name),
        _ => String::new(),
    }
}

/// Several systems: each writes to `<out>/<shortName>/`.
fn handle_events(engine: &Engine, out_dir: &str, multi: bool, quiet: bool, messages: bool) -> usize {
    let mut written = 0;
    for ev in engine.drain_events() {
        match ev {
            Event::ControlChannel { site, freq_hz } if !quiet => println!("{}control channel {:.4} MHz", site_tag(engine, site), freq_hz as f64 / 1e6),
            Event::Note { system, site, text, .. } => eprintln!("{}{text}", sys_tag(engine, system, site)),
            Event::Message { site, msg } if messages => println!("{:7.2}s  {}{} {}", msg.time_s, site_tag(engine, site), msg.kind.as_str(), msg.meta),
            Event::CallStart(c) if !quiet => println!(
                "{:7.2}s  {}CALL {} start TG {} {:.4} MHz{}{}{} → {}",
                c.start_s,
                sys_tag(engine, c.system, c.site),
                c.id,
                c.talkgroup,
                c.freq_hz as f64 / 1e6,
                match c.voice {
                    Voice::Am => " AM",
                    Voice::Fm => " FM",
                    _ => "",
                },
                if let Voice::P25Tdma { slot } = c.voice { format!(" slot {slot}") } else { String::new() },
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
                    system_tag(engine, system)
                )
            }
            Event::CallEnd(c) if !quiet => {
                let srcs: Vec<String> = c.sources.iter().map(|s| s.src.to_string()).collect();
                println!("{:7.2}s  {}CALL {} end   TG {} srcs [{}]{}", c.last_update_s.max(c.last_audio_s), sys_tag(engine, c.system, c.site), c.id, c.talkgroup, srcs.join(","), if c.decrypted { " ENC DEC" } else if c.encrypted { " ENC" } else { "" });
            }
            Event::Duplicate { call, kept } if !quiet => {
                println!("         {}CALL {} TG {} not saved: a copy of {}CALL {}", sys_tag(engine, call.system, call.site), call.id, call.talkgroup, sys_tag(engine, kept.system, kept.site), kept.id)
            }
            Event::Concluded(k) => {
                // (Appended, not with_extension: a TDMA base name ends in ".<slot>".)
                let dir = if multi { Path::new(out_dir).join(&k.short_name) } else { Path::new(out_dir).to_path_buf() };
                let _ = fs::create_dir_all(&dir);
                let base = dir.join(&k.base_name).display().to_string();
                let _ = fs::write(format!("{base}.wav"), trunk_core::wav::encode(&k.audio, 8000));
                let _ = fs::write(format!("{base}.json"), k.record.to_json());
                if let Some(c) = &k.frames {
                    let _ = fs::write(format!("{base}.{}", c.ext), &c.bytes);
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
        buf_len: a.num("buflen", 0.0) as u32,
        buffers: a.num("buffers", 0.0) as u32,
    };
    let want = (a.num("seconds", 10.0) * cfg.rate_hz as f64) as u64 * 2;
    let (tx, rx) = mpsc::sync_channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let c2 = cfg.clone();
    let th = std::thread::spawn(move || sdr::run_with(0, c2, tx, s2, None));
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
            Ok(sdr::SourceMsg::Iq { .. } | sdr::SourceMsg::Real16 { .. } | sdr::SourceMsg::Tuned { .. }) => {}
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
    let config_path = paths::config_path(a.get("config"));
    paths::init(&config_path);
    let cfg = config::Config::load(&config_path).unwrap_or_else(|e| fatal(&e));
    let level = a.get("log-level").map(|l| trunk_app::log::Level::parse(l).unwrap_or_else(|| fatal(&format!("--log-level {l}: trace, debug, info, warning, error or fatal"))));
    let (hub, _) = tokio::sync::broadcast::channel(4096);
    logging::init(&cfg.log, config_path.parent().unwrap_or(Path::new(".")), level, hub.clone());
    // An older config, upgraded: saved so (the old one kept beside it), and told.
    if !cfg.notes.is_empty() {
        match trunk_app::migrate::save(&cfg, &config_path) {
            Ok(old) => log::info!("{} updated (the old one is {})", config_path.display(), old.display()),
            Err(e) => log::warn!("{}: couldn't save the updated config ({e})", config_path.display()),
        }
        for n in &cfg.notes {
            log::info!("{n}");
        }
    }
    // (For this run only: the config, saved from the interface, keeps its own.)
    let port = match a.get("port") {
        Some(p) => p.parse::<u16>().unwrap_or_else(|_| fatal(&format!("--port {p}: a port number, 1–65535"))),
        None => cfg.server.port,
    };
    let bind = a.get("bind").unwrap_or(&cfg.server.bind);
    let addr: std::net::SocketAddr = format!("{bind}:{port}").parse().unwrap_or_else(|e| fatal(&format!("bind address: {e}")));
    let host = if addr.ip().is_unspecified() { "localhost".into() } else { addr.ip().to_string() };
    let url = format!("http://{host}:{}", addr.port());
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Launched twice (a double-click on an app that is already
            // running): show the running one.
            if already_running(addr.port()) {
                log::info!("Trunk Recorder Pro is already running — {url}");
                if !a.flag("no-open") {
                    open_browser(&url);
                }
                return;
            }
            fatal(&format!("Port {} is in use by another program. Start with --port <another port>.", addr.port()));
        }
        Err(e) => fatal(&format!("web server on {addr}: {e}")),
    };
    // A folder served as an interface for this run (an author's test bench), on a port of its own.
    let dev = a.get("ui").map(|d| {
        let d = std::fs::canonicalize(d).unwrap_or_else(|e| fatal(&format!("--ui {d}: {e}")));
        if !d.join("index.html").is_file() {
            fatal(&format!("--ui {}: no index.html there", d.display()));
        }
        let id = match interfaces::Manifest::read(&d) {
            Ok(Some(m)) => m.id,
            Ok(None) => interfaces::slug(&d.file_name().unwrap_or_default().to_string_lossy()),
            Err(e) => fatal(&format!("--ui {}: {e}", d.display())),
        };
        let mut s = cfg.server.clone();
        s.port = port;
        s.interfaces.remove(&id);
        interfaces::Dev { id, folder: d, port: s.free_port(&[]) }
    });
    let ui_url = dev.as_ref().map(|d| format!("http://{host}:{}/", d.port));
    // The dashboard's measurements: the radio registry (from earlier runs) and a week of history.
    let shared = trunk_app::stats::Shared::new();
    runtime::load_registry(&shared);
    let series = Arc::new(Mutex::new(trunk_app::stats::History::default()));
    let store = statstore::Store::start(paths::data_dir().join("stats"), series.clone());
    let talkgroups = runtime::load_talkgroups(&cfg);
    runtime::load_names(&shared, &cfg, &talkgroups);
    let ctx = Arc::new(runtime::Ctx {
        config_path: config_path.clone(),
        config: Mutex::new(cfg),
        config_version: std::sync::atomic::AtomicU64::new(1),
        talkgroups: Mutex::new(talkgroups),
        hub: hub.clone(),
        lifecycle: Mutex::new(runtime::Lifecycle::Idle),
        phase: Mutex::new(runtime::PhaseInfo { phase: "idle", error: None, ended: false }),
        history: Mutex::new(VecDeque::new()),
        quit: tokio::sync::Notify::new(),
        survey_last: Mutex::new(None),
        plugins: plugins::manage::Plugins::new(hub.clone(), shared.clone()),
        bind: addr.ip(),
        interfaces: interfaces::Interfaces::new(dev, addr.port()),
        shared,
        series,
        store,
        topics: Mutex::new(Default::default()),
        engine: Mutex::new(None),
        host_last: Mutex::new(None),
        spool: Mutex::new(None),
        clock: Default::default(),
        unlocked: Default::default(),
    });
    monitor::start(ctx.clone());
    // What each call kept once its plugins are done, for the history list.
    let weak = Arc::downgrade(&ctx);
    ctx.plugins.archive.on_settle(Box::new(move |rel, audio, json| {
        if let Some(ctx) = weak.upgrade() {
            runtime::call_files(&ctx, rel, audio, json);
        }
    }));
    // Calls held back from plugins by the run before (the archive is told of them, so after on_settle).
    let cfg = ctx.config.lock().unwrap().clone();
    ctx.plugins.open_hold(&cfg);
    runtime::load_history(ctx.clone());
    {
        let c = ctx.config.lock().unwrap();
        let l = &c.log;
        log::info!("Trunk Recorder Pro {} — open {url}", env!("CARGO_PKG_VERSION"));
        log::info!("Using Config file: {}", config_path.display());
        log::info!("Capture Directory: {}", c.recording.capture_dir);
        let dir = if l.dir.is_empty() { config_path.parent().unwrap_or(Path::new(".")).join("logs") } else { config_path.parent().unwrap_or(Path::new(".")).join(&l.dir) };
        log::info!("Log Level: {} · Log to File: {}{} · System log: {}", level.unwrap_or(l.level).as_str(), l.file, if l.file { format!(" ({})", dir.display()) } else { String::new() }, l.syslog);
    }
    let auto = a.flag("start") || ctx.config.lock().unwrap().server.auto_start;
    if auto {
        let cfg = ctx.config.lock().unwrap().clone();
        if let Err(e) = ctx.begin_recording(cfg) {
            log::error!("Not started: {e}");
        }
    }
    if let Some(u) = &ui_url {
        log::info!("--ui: your interface is at {u}");
    }
    if !a.flag("no-open") {
        open_browser(ui_url.as_deref().unwrap_or(&url));
    }
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fatal(&e.to_string()));
    if let Err(e) = rt.block_on(server::serve(ctx.clone(), listener)) {
        fatal(&format!("web server on {addr}: {e}"));
    }
    runtime::save_registry(&ctx.shared);
    log::info!("Cleaning up & Exiting...");
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

/// `--lcn 101=452275000,102=…`: a channel table.
fn lcn_table(a: &Args) -> std::collections::BTreeMap<u32, u64> {
    a.get("lcn")
        .unwrap_or("")
        .split(',')
        .filter_map(|e| e.split_once('=').and_then(|(l, h)| Some((l.trim().parse().ok()?, h.trim().parse::<f64>().ok()? as u64))))
        .collect()
}
