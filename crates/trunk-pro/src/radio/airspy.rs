//! Airspy R2 / Mini and HydraSDR RFOne: radios with the same tuner, gain stages and USB
//! protocol ([`Lib`]), driven by the driver built in (crates/libairspy-rs: libairspy and what
//! libhydrasdr changed in it, ported to Rust over nusb). The operating system's own USB access
//! is enough for it: nothing to install on macOS or Linux; on Windows no airspy.dll /
//! hydrasdr.dll or libusb-1.0.dll, only the WinUSB driver these radios bind by themselves (one
//! given another with Zadig can't be opened).
//!
//! The radio hands over real samples at twice the IQ rate, turned into IQ on the source's
//! decoding thread; they are taken on the source's own thread ([`Device::recv`]), with one
//! thread of the driver's keeping the USB transfers going. An RFOne has three inputs (ANT,
//! with the bias-T, and CABLE1 / CABLE2).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libairspy_rs::commands::SampleType;
use libairspy_rs::{Device, Error, Family, Samples};

use super::{ppm_corrected, Caps, Deadline, DriverInfo, Rates};
use crate::sdr::{Applied, Control, Gain, Preset, Regained, Retuned, Silence, Sink, Stage, Stages, Tunable};

/// The family a source's radio is of: which USB ids, serials and firmware are its.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lib {
    Airspy,
    Hydrasdr,
}

impl Lib {
    pub const ALL: [Lib; 2] = [Lib::Airspy, Lib::Hydrasdr];
    /// "Airspy" / "HydraSDR": its radios' name, before the model.
    pub fn brand(self) -> &'static str {
        match self {
            Lib::Airspy => "Airspy",
            Lib::Hydrasdr => "HydraSDR",
        }
    }
    /// "airspy" / "hydrasdr": its source `type` and its radios' key among those [`super::known`].
    pub fn id(self) -> &'static str {
        match self {
            Lib::Airspy => "airspy",
            Lib::Hydrasdr => "hydrasdr",
        }
    }
    fn family(self) -> Family {
        match self {
            Lib::Airspy => Family::Airspy,
            Lib::Hydrasdr => Family::Rfone,
        }
    }
}

/// The driver's state for the list of drivers: built in, so always there.
pub fn info(_lib: Lib) -> DriverInfo {
    DriverInfo { loaded: true, detail: "built in".into() }
}

fn said(lib: Lib, what: &str, e: &Error) -> String {
    format!("{} {what}: {e}", lib.brand())
}

/// What a radio is opened as ([`super::open`]): "Airspy 735863DC2F650AE3", "Airspy (first)".
fn open_name(lib: Lib, serial: &str) -> String {
    format!("{} {}", lib.brand(), if serial.is_empty() { "(first)" } else { serial })
}

/// A source's `serial` (hex) as a number; None for "" (the first radio).
fn serial_number(lib: Lib, serial: &str) -> Result<Option<u64>, String> {
    if serial.is_empty() {
        return Ok(None);
    }
    u64::from_str_radix(serial.trim_start_matches("0x"), 16).map(Some).map_err(|_| format!("{} serial {serial:?} isn't hex", lib.brand()))
}

/// Open the radio `serial` (hex; "" = the first of its family), with a deadline, one radio
/// at a time ([`super::open`]).
fn open(lib: Lib, serial: &str) -> Result<Device, String> {
    let sn = serial_number(lib, serial)?;
    super::open(&open_name(lib, serial), Deadline::Quick, true, move || Device::open_family(lib.family(), sn.unwrap_or(0)).map_err(|e| said(lib, "open", &e)))
}

/// What an open radio says it is: its model and the IQ rates it lists (ask as opened, for IQ
/// samples: for real samples the rates are listed doubled), remembered by its serial for the
/// list of radios; and its firmware string ("" when it didn't say).
fn identify(lib: Lib, d: &Device) -> (Caps, String) {
    let rates = d.samplerates();
    let version = d.version_string().unwrap_or_default();
    let model = match lib {
        Lib::Airspy => model_of(&version, &rates),
        Lib::Hydrasdr => "RFOne",
    };
    let caps = Caps { model: model.to_string(), rates: Rates::List(rates), gains: Vec::new() };
    if let Ok(id) = d.partid_serialno() {
        super::remember(&key(lib, serial_of(&id.serial_no)), caps.clone());
    }
    (caps, version)
}

/// The 64-bit serial a radio is opened by (as airspy_info prints it), from the four words it reads out.
fn serial_of(serial_no: &[u32; 4]) -> u64 {
    ((serial_no[2] as u64) << 32) | serial_no[3] as u64
}

/// Its key among the radios [`super::known`].
fn key(lib: Lib, serial: u64) -> String {
    format!("{}:{serial:016X}", lib.id())
}

/// An Airspy's model: "R2" or "Mini", from the firmware string (its board id can't tell them
/// apart, both say "AIRSPY"), else from the rates (an R2 does 2.5 MSPS, a Mini 6); "" when
/// neither says.
fn model_of(version: &str, rates_iq: &[u32]) -> &'static str {
    let v = version.to_ascii_uppercase();
    if v.contains("MINI") {
        "Mini"
    } else if v.contains("NOS") || rates_iq.contains(&2_500_000) {
        "R2"
    } else if rates_iq.contains(&6_000_000) {
        "Mini"
    } else {
        ""
    }
}

/// "Airspy R2", "HydraSDR RFOne", or the brand alone when the model isn't known.
fn name(lib: Lib, model: &str) -> String {
    match model {
        "" => lib.brand().to_string(),
        m => format!("{} {m}", lib.brand()),
    }
}

/// A found radio for the interface: serial and label; model and rates once known.
fn describe(lib: Lib, serial: u64, caps: Option<&Caps>) -> serde_json::Value {
    let sn = format!("{serial:016X}");
    let mut j = serde_json::json!({ "serial": sn, "label": format!("{} · SN {sn}", name(lib, caps.map_or("", |c| c.model.as_str()))) });
    if let Some(c) = caps {
        c.add_to(&mut j);
    }
    j
}

/// Attached radios: [{serial (hex), label, model ("R2" / "Mini" / "RFOne"), rates (IQ, Hz)}].
/// Their serials are read from what the operating system knows of each, so one that's
/// streaming is listed too (and isn't disturbed). One not opened yet in this run is opened for
/// a moment to ask its model and rates (as airspy_info does), once; one streaming for a source
/// is shown as it said when it opened.
pub fn devices(lib: Lib) -> Vec<serde_json::Value> {
    let found = super::open(&format!("{} search", lib.brand()), Deadline::Quick, true, move || libairspy_rs::list_family(lib.family()).map_err(|e| said(lib, "search", &e))).unwrap_or_default();
    found
        .into_iter()
        .map(|s| {
            // (Opened here and closed when dropped: nothing else uses the handle.)
            let asked = || open(lib, &format!("{s:016X}")).ok().map(|d| identify(lib, &d).0);
            let caps = super::known(&key(lib, s)).or_else(asked);
            describe(lib, s, caps.as_ref())
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct AirspyConfig {
    pub lib: Lib,
    /// Hex serial; "" = the first.
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    /// A [`Gain::Preset`], or [`Gain::Stages`] (LNA 0–14, mixer 0–15, VGA 0–15 steps; with `agc`, the VGA alone).
    pub gain: Gain,
    pub bias_tee: bool,
    /// 12-bit packing on the USB.
    pub packing: bool,
    /// The input, by number (a HydraSDR's: 0 ANT, 1 CABLE1, 2 CABLE2); an Airspy has only 0.
    pub rf_port: u8,
    pub ppm: f64,
}

impl crate::sdr::Radio for AirspyConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if matches!(g, Gain::Stages(_) | Gain::Preset(..)) {
            self.gain = g;
        }
    }
    fn gain(&self) -> Option<Gain> {
        Some(self.gain)
    }
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// A streaming radio, as [`Control`] retunes and regains it: stopped and closed when dropped.
struct Live {
    lib: Lib,
    d: Device,
    ppm: f64,
    /// It was set to an input other than ANT: put back as it closes. The RFOne keeps its
    /// input after it's closed, and a program that doesn't pick one would hear CABLE1.
    off_ant: bool,
}

impl Live {
    fn set(&self, s: Setting, v: u8) -> Result<(), String> {
        match s {
            Setting::Linearity => self.d.set_linearity_gain(v),
            Setting::Sensitivity => self.d.set_sensitivity_gain(v),
            Setting::LnaAgc => self.d.set_lna_agc(v != 0),
            Setting::MixerAgc => self.d.set_mixer_agc(v != 0),
            Setting::Lna => self.d.set_lna_gain(v),
            Setting::Mixer => self.d.set_mixer_gain(v),
            Setting::Vga => self.d.set_vga_gain(v),
        }
        .map_err(|e| said(self.lib, s.what(v), &e))
    }
}

impl Tunable for Live {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        self.d.set_freq(ppm_corrected(hz, self.ppm).round() as u32).map_err(|e| said(self.lib, "tune", &e))?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// Its own settings, which the radio takes while streaming (a survey's
    /// dB is left alone: the gain stays as set).
    fn set_gain(&mut self, g: Gain) -> Result<Regained, String> {
        for (s, v) in settings(g) {
            self.set(s, v)?;
        }
        Ok(Regained { gap: 0, gain: within(g) })
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if self.off_ant {
            let _ = self.d.stop_rx();
            if self.d.set_rf_port(0).is_err() {
                log::warn!("{}: couldn't put its input back to ANT as it closed", self.lib.brand());
            }
        }
    }
}

/// One of the tuner's gain settings, as both drivers take them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Setting {
    Linearity,
    Sensitivity,
    LnaAgc,
    MixerAgc,
    Lna,
    Mixer,
    Vga,
}

impl Setting {
    /// What it is, for an error setting it to `v`.
    fn what(self, v: u8) -> &'static str {
        match (self, v) {
            (Setting::Linearity | Setting::Sensitivity, _) => "gain",
            (Setting::LnaAgc, 0) => "LNA AGC off",
            (Setting::LnaAgc, _) => "LNA AGC",
            (Setting::MixerAgc, 0) => "mixer AGC off",
            (Setting::MixerAgc, _) => "mixer AGC",
            (Setting::Lna, _) => "LNA gain",
            (Setting::Mixer, _) => "mixer gain",
            (Setting::Vga, _) => "VGA gain",
        }
    }
}

/// `g` as the radio takes it: a table's step, or the stages, each a whole
/// step within its range (LNA 0–14, mixer and VGA 0–15, a table 0–21). None:
/// a kind it doesn't take (a survey's dB), so the gain stays as set.
fn within(g: Gain) -> Option<Gain> {
    let step = |s: &Stages, stage: Stage, most: u8| s.get(stage).unwrap_or(0.0).clamp(0.0, most as f32).trunc();
    Some(match g {
        Gain::Preset(p, n) => Gain::Preset(p, n.min(21)),
        Gain::Stages(s) if s.agc => Gain::Stages(Stages::default().agc().with(Stage::Vga, step(&s, Stage::Vga, 15))),
        Gain::Stages(s) => Gain::Stages(Stages::default().with(Stage::Lna, step(&s, Stage::Lna, 14)).with(Stage::Mixer, step(&s, Stage::Mixer, 15)).with(Stage::Vga, step(&s, Stage::Vga, 15))),
        Gain::Agc | Gain::Db(_) | Gain::Step(_) => return None,
    })
}

/// What `g` sets ([`within`]), in order: a table's step (which turns the AGCs off), or the stages.
fn settings(g: Gain) -> Vec<(Setting, u8)> {
    let level = |s: &Stages, stage: Stage| s.get(stage).unwrap_or(0.0) as u8;
    match within(g) {
        Some(Gain::Preset(Preset::Linearity, n)) => vec![(Setting::Linearity, n)],
        Some(Gain::Preset(Preset::Sensitivity, n)) => vec![(Setting::Sensitivity, n)],
        Some(Gain::Stages(s)) if s.agc => vec![(Setting::LnaAgc, 1), (Setting::MixerAgc, 1), (Setting::Vga, level(&s, Stage::Vga))],
        Some(Gain::Stages(s)) => vec![
            (Setting::LnaAgc, 0),
            (Setting::MixerAgc, 0),
            (Setting::Lna, level(&s, Stage::Lna)),
            (Setting::Mixer, level(&s, Stage::Mixer)),
            (Setting::Vga, level(&s, Stage::Vga)),
        ],
        _ => vec![],
    }
}

/// How long without a block before the radio is taken for stuck (a block is 7–52 ms of samples).
const SILENT: Duration = Duration::from_secs(2);

/// One run of a source's radio, from opening it to its stopping.
fn stream_once(cfg: &AirspyConfig, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let lib = cfg.lib;
    let d = open(lib, &cfg.serial)?;
    let (caps, version) = identify(lib, &d);
    let what = name(lib, &caps.model);
    if !caps.rates.allows(cfg.rate_hz) {
        return Err(super::rate_refusal(&what, &caps.rates, cfg.rate_hz));
    }
    let said = |what: &str, e: &Error| said(lib, what, e);
    let serial = if cfg.serial.is_empty() { "(first)" } else { &cfg.serial };
    let mut live = Live { lib, d, ppm: cfg.ppm, off_ant: false };
    live.d.set_sample_type(SampleType::Int16Real).map_err(|e| said("sample type", &e))?;
    // (By the IQ rate even for real samples: the firmware's rates are listed and set as IQ.)
    live.d.set_samplerate(cfg.rate_hz.round() as u32).map_err(|e| said("sample rate", &e))?;
    live.tune(cfg.center_hz)?;
    // (As a gain set while running is: a reopened radio comes back the same.)
    let r = live.set_gain(cfg.gain)?;
    sink.gained(Some(cfg.gain), r.gain);
    if lib == Lib::Hydrasdr {
        live.off_ant = cfg.rf_port != 0;
        live.d.set_rf_port(cfg.rf_port).map_err(|e| said("input", &e))?;
    }
    live.d.set_rf_bias(cfg.bias_tee).map_err(|e| said("bias-T", &e))?;
    // (Older firmware can't pack: it streams unpacked then, as it always did.)
    if live.d.set_packing(cfg.packing).is_err() && cfg.packing {
        log::warn!("{what} {serial}: this firmware can't pack samples; streaming them unpacked (a third more USB bandwidth)");
    }
    live.d.start_rx_polled().map_err(|e| said("start", &e))?;
    log::info!("{what} {serial} (firmware {}): {:.4} MHz, {:.3} MSPS, {}", version.trim(), cfg.center_hz / 1e6, cfg.rate_hz / 1e6, cfg.gain);
    let wait = Duration::from_millis(if ctl.is_some() { 10 } else { 100 });
    let mut silence = Silence::new(SILENT);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(c) = ctl
            && let Applied::Closed = c.apply(&mut live, cfg.center_hz, sink)?
        {
            return Ok(());
        }
        let open = live.d.recv(wait, |t| match t.samples {
            // (Dropped real samples, as IQ: half as many.)
            Samples::Int16(s) => sink.real16(s, t.dropped_samples / 2, Instant::now()),
            _ => true,
        });
        match open {
            Ok(Some(true)) => silence.heard(Instant::now()),
            Ok(Some(false)) => return Ok(()),
            Ok(None) => silence.check(&what, Instant::now())?,
            Err(_) => return Err(format!("{} stopped streaming (unplugged?)", lib.brand())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_from_firmware_or_rates() {
        assert_eq!(model_of("AirSpy NOS v1.0.0-rc10-6-g4008185 2020-05-08", &[10_000_000, 2_500_000]), "R2");
        assert_eq!(model_of("AirSpy MINI v1.0.0-rc10-6-g4008185 2020-05-08", &[6_000_000, 3_000_000]), "Mini");
        // Firmware that doesn't say: the rates tell them apart.
        assert_eq!(model_of("", &[10_000_000, 2_500_000]), "R2");
        assert_eq!(model_of("", &[6_000_000, 3_000_000, 10_000_000]), "Mini");
        assert_eq!(model_of("", &[]), "");
        assert_eq!(name(Lib::Airspy, "R2"), "Airspy R2");
        assert_eq!(name(Lib::Hydrasdr, "RFOne"), "HydraSDR RFOne");
    }

    #[test]
    fn device_json_carries_model_and_rates() {
        let r2 = Caps { model: "R2".into(), rates: Rates::List(vec![10_000_000, 2_500_000]), gains: Vec::new() };
        let d = describe(Lib::Airspy, 0x735863DC2F650AE3, Some(&r2));
        assert_eq!(d["serial"], "735863DC2F650AE3");
        assert_eq!(d["label"], "Airspy R2 · SN 735863DC2F650AE3");
        assert_eq!(d["model"], "R2");
        assert_eq!(d["rates"], serde_json::json!([10_000_000, 2_500_000]));
        let rfone = Caps { model: "RFOne".into(), rates: Rates::List(vec![10_000_000, 5_000_000, 2_500_000]), gains: Vec::new() };
        assert_eq!(describe(Lib::Hydrasdr, 0x1A2B3C4D5E6F7081, Some(&rfone))["label"], "HydraSDR RFOne · SN 1A2B3C4D5E6F7081");
        // One not asked yet that couldn't be opened (another program has it): by serial only.
        let d = describe(Lib::Airspy, 1, None);
        assert_eq!(d["label"], "Airspy · SN 0000000000000001");
        assert!(d.get("model").is_none() && d.get("rates").is_none());
        assert_eq!(describe(Lib::Hydrasdr, 1, None)["label"], "HydraSDR · SN 0000000000000001");
    }

    #[test]
    fn serial_as_airspy_info_prints_it() {
        let sn = serial_of(&[0, 0, 0x735863DC, 0x2F650AE3]);
        assert_eq!(key(Lib::Airspy, sn), "airspy:735863DC2F650AE3");
        assert_eq!(key(Lib::Hydrasdr, sn), "hydrasdr:735863DC2F650AE3");
    }

    #[test]
    fn gain_settings_in_order() {
        use Setting::*;
        assert_eq!(settings(Gain::Preset(Preset::Linearity, 30)), [(Linearity, 21)]);
        assert_eq!(settings(Gain::Preset(Preset::Sensitivity, 9)), [(Sensitivity, 9)]);
        let stages = Stages::default().with(Stage::Lna, 20.0).with(Stage::Mixer, 8.0).with(Stage::Vga, 7.0);
        assert_eq!(settings(Gain::Stages(stages)), [(LnaAgc, 0), (MixerAgc, 0), (Lna, 14), (Mixer, 8), (Vga, 7)]);
        assert_eq!(settings(Gain::Stages(Stages::default().agc().with(Stage::Vga, 7.0))), [(LnaAgc, 1), (MixerAgc, 1), (Vga, 7)]);
        assert_eq!(settings(Gain::Db(30.0)), []);
        // What it says it set: the steps it took, so a step out of range is told as the one set.
        assert_eq!(within(Gain::Stages(stages)), Some(Gain::Stages(Stages::default().with(Stage::Lna, 14.0).with(Stage::Mixer, 8.0).with(Stage::Vga, 7.0))));
        assert_eq!(within(Gain::Preset(Preset::Linearity, 30)), Some(Gain::Preset(Preset::Linearity, 21)));
        assert_eq!(within(Gain::Db(30.0)), None, "a survey's dB isn't one it takes");
        assert_eq!((LnaAgc.what(0), LnaAgc.what(1), Vga.what(7)), ("LNA AGC off", "LNA AGC", "VGA gain"));
    }

    /// On a real RFOne (`TRP_HYDRASDR_SERIAL`, else the first): each rate streams at that rate,
    /// with something in it; and CABLE1 streams (and is put back to ANT as it closes).
    /// `cargo test -p trunk-pro streams_from_an_rfone -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn streams_from_an_rfone() {
        use crate::sdr::{Block, SourceMsg};
        let serial = std::env::var("TRP_HYDRASDR_SERIAL").unwrap_or_default();
        let run = |rate_hz: f64, rf_port: u8, bias_tee: bool| {
            let cfg = AirspyConfig { lib: Lib::Hydrasdr, serial: serial.clone(), center_hz: 852e6, rate_hz, gain: Gain::Preset(Preset::Linearity, 14), bias_tee, packing: true, rf_port, ppm: 0.0 };
            let (tx, rx) = std::sync::mpsc::sync_channel(256);
            let stop = Arc::new(AtomicBool::new(false));
            let t = std::thread::spawn({
                let stop = stop.clone();
                move || crate::sdr::run_with(0, cfg, tx, stop, None)
            });
            let (mut n, mut dropped, mut power, mut first, mut error) = (0u64, 0u64, 0f64, None, None);
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(SourceMsg::Samples { block: Block::Real16(b), dropped: d, at, .. }) => {
                        // (The first second is the start: counted from after it.)
                        let t0 = *first.get_or_insert(at);
                        if at.duration_since(t0) > Duration::from_secs(1) {
                            // (Real samples: two an IQ sample.)
                            n += b.len() as u64 / 2;
                            dropped += d;
                            power += b.iter().map(|&v| (v as f64 / 32768.0).powi(2)).sum::<f64>() / 2.0;
                        }
                    }
                    Ok(SourceMsg::Error { error: e, .. }) => {
                        error = Some(e);
                        break;
                    }
                    _ => {}
                }
            }
            stop.store(true, Ordering::Relaxed);
            drop(rx);
            t.join().unwrap();
            let secs = first.map_or(0.0, |t0: Instant| t0.elapsed().as_secs_f64() - 1.0);
            (n as f64 / secs.max(1e-9), dropped, power / n.max(1) as f64, error)
        };
        for rate in [10e6, 5e6, 2.5e6] {
            let (got, dropped, power, error) = run(rate, 0, false);
            println!("{} MSPS: {:.3} MSPS, {dropped} dropped, mean power {power:.2e}, error {error:?}", rate / 1e6, got / 1e6);
            assert!(error.is_none(), "{error:?}");
            assert!((got / rate - 1.0).abs() < 0.05, "{got} for {rate}");
            assert!(power > 1e-9 && power < 1.0, "{power}");
        }
        let (_, _, _, cable_ok) = run(2.5e6, 1, false);
        assert!(cable_ok.is_none(), "{cable_ok:?}");
    }
}
