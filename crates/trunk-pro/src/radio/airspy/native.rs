//! An Airspy R2 / Mini without libairspy: the driver built in (crates/libairspy-rs, libairspy
//! ported to Rust over nusb), which the operating system's own USB access is enough for.
//! Nothing to install on macOS or Linux; on Windows no airspy.dll, libusb-1.0.dll,
//! pthreadVC2.dll or Visual C++ 2010 runtime, only the WinUSB driver an Airspy binds by
//! itself (one given another with Zadig can't be opened this way).
//!
//! Tried first for every Airspy ([`on`]); libairspy, where it's installed, is what a radio
//! this can't open falls back to ([`super::stream_once`]). The radio is set up and read as
//! the libairspy path does: real samples by default, converted on the source's decoding
//! thread; here they are taken on the source's own thread ([`Device::recv`]), with one
//! thread of the driver's keeping the USB transfers going.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use libairspy_rs::commands::SampleType;
use libairspy_rs::{Device, Error, Samples};
use num_complex::Complex32;

use super::{AirspyConfig, Can, Identity, Lib, SerialRead, Setting};
use crate::radio::{Caps, Deadline, Rates, ppm_corrected};
use crate::sdr::{Applied, Control, Gain, Regained, Retuned, Sink, Tunable};

/// Whether Airspys are driven by the built-in driver: unless `TRUNK_PRO_AIRSPY_NATIVE=0`
/// (libairspy alone then, as before it: to tell one's fault from the other's).
pub(super) fn on() -> bool {
    std::env::var_os("TRUNK_PRO_AIRSPY_NATIVE").is_none_or(|v| v != "0")
}

/// "built in", for the list of drivers.
pub(super) const DETAIL: &str = "built in";

/// Why a radio wasn't opened.
pub(super) enum Unopened {
    /// None attached (or none with that serial): nothing another driver would find either.
    Absent(String),
    /// It's there and wouldn't open (no WinUSB driver bound to it, another program has it), or
    /// didn't answer in time: what libairspy is worth trying for.
    Refused(String),
}

impl Unopened {
    pub(super) fn said(self) -> String {
        match self {
            Unopened::Absent(e) | Unopened::Refused(e) => e,
        }
    }
}

fn said(what: &str, e: &Error) -> String {
    format!("Airspy {what}: {e}")
}

/// Open the radio `serial` (hex; "" = the first), with a deadline, one radio at a time
/// ([`crate::radio::open`], under the name the libairspy path opens it by).
pub(super) fn open(serial: &str) -> Result<Device, Unopened> {
    let sn = super::serial_number(Lib::Airspy, serial).map_err(Unopened::Absent)?;
    let opened = crate::radio::open(&super::open_name(Lib::Airspy, serial), Deadline::Quick, true, move || Ok(Device::open_serial(sn.unwrap_or(0))));
    match opened {
        Ok(Ok(d)) => Ok(d),
        Ok(Err(e @ Error::NotFound)) => Err(Unopened::Absent(said("open", &e))),
        Ok(Err(e)) => Err(Unopened::Refused(said("open", &e))),
        Err(e) => Err(Unopened::Refused(e)),
    }
}

/// The serials of the Airspys attached: read from what the operating system knows of each, so
/// one that's streaming is listed too (and isn't disturbed).
pub(super) fn list() -> Vec<u64> {
    crate::radio::open("Airspy search", Deadline::Quick, true, || libairspy_rs::list_devices().map_err(|e| said("search", &e))).unwrap_or_default()
}

/// What an open radio says it is ([`super::identify`]'s, asked of the built-in driver):
/// call it as opened, for IQ samples.
pub(super) fn identify(d: &Device) -> Identity {
    let rates = d.samplerates();
    let version = d.version_string().unwrap_or_default();
    let caps = Caps { model: super::model_of(&version, &rates).to_string(), rates: Rates::List(rates), gains: Vec::new() };
    if let Ok(id) = d.partid_serialno() {
        let sn = SerialRead { part_id: id.part_id, serial_no: id.serial_no }.serial();
        crate::radio::remember(&super::key(Lib::Airspy, sn), caps.clone());
    }
    Identity { caps, inputs: vec![], can: Can { input: false, bias_tee: true, packing: true }, version }
}

/// A streaming radio, as [`Control`] retunes and regains it.
struct Live {
    d: Device,
    ppm: f64,
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
        .map_err(|e| said(s.what(v), &e))
    }
}

impl Tunable for Live {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        self.d.set_freq(ppm_corrected(hz, self.ppm).round() as u32).map_err(|e| said("tune", &e))?;
        Ok(Retuned { hz, gap: 0 })
    }
    fn set_gain(&mut self, g: Gain) -> Result<Regained, String> {
        for (s, v) in super::settings(g) {
            self.set(s, v)?;
        }
        Ok(Regained { gap: 0, gain: super::within(g) })
    }
}

/// How long without a block before the radio is taken for stuck (a block is 7–52 ms of samples).
const SILENT: Duration = Duration::from_secs(2);

/// Stream from `d`, just opened, until told to stop: [`super::stream_once`]'s for the built-in driver.
pub(super) fn stream(d: Device, cfg: &AirspyConfig, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    // (Asked as opened, for IQ samples: for real samples the rates are listed doubled.)
    let id = identify(&d);
    let what = super::name(Lib::Airspy, &id.caps.model);
    if !id.caps.rates.allows(cfg.rate_hz) {
        return Err(crate::radio::rate_refusal(&what, &id.caps.rates, cfg.rate_hz));
    }
    let serial = if cfg.serial.is_empty() { "(first)" } else { &cfg.serial };
    let mut live = Live { d, ppm: cfg.ppm };
    live.d.set_sample_type(if super::library_iq() { SampleType::Float32Iq } else { SampleType::Int16Real }).map_err(|e| said("sample type", &e))?;
    // (By the IQ rate even for real samples, for the reasons the libairspy path gives.)
    live.d.set_samplerate(cfg.rate_hz.round() as u32).map_err(|e| said("sample rate", &e))?;
    live.tune(cfg.center_hz)?;
    // (As a gain set while running is: a reopened radio comes back the same.)
    let r = live.set_gain(cfg.gain)?;
    sink.gained(Some(cfg.gain), r.gain);
    live.d.set_rf_bias(cfg.bias_tee).map_err(|e| said("bias-T", &e))?;
    // (Older firmware can't pack: it streams unpacked then, as it always did.)
    if live.d.set_packing(cfg.packing).is_err() && cfg.packing {
        log::warn!("{what} {serial}: this firmware can't pack samples; streaming them unpacked (a third more USB bandwidth)");
    }
    live.d.start_rx_polled().map_err(|e| said("start", &e))?;
    log::info!("{what} {serial} (built-in driver, firmware {}): {:.4} MHz, {:.3} MSPS, {}", id.version.trim(), cfg.center_hz / 1e6, cfg.rate_hz / 1e6, cfg.gain);
    let wait = Duration::from_millis(if ctl.is_some() { 10 } else { 100 });
    let mut heard = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(c) = ctl
            && let Applied::Closed = c.apply(&mut live, cfg.center_hz, sink)?
        {
            return Ok(());
        }
        let open = live.d.recv(wait, |t| {
            let at = Instant::now();
            match t.samples {
                // (Dropped real samples, as IQ: half as many.)
                Samples::Int16(s) => sink.real16(s, t.dropped_samples / 2, at),
                // SAFETY: interleaved I and Q floats, which a Complex32 is a pair of (repr(C)).
                Samples::Float32(s) => sink.iq(unsafe { std::slice::from_raw_parts(s.as_ptr() as *const Complex32, s.len() / 2) }, t.dropped_samples, at),
                _ => true,
            }
        });
        match open {
            Ok(Some(true)) => heard = Instant::now(),
            Ok(Some(false)) => return Ok(()),
            Ok(None) if heard.elapsed() < SILENT => {}
            Ok(None) => return Err(format!("{what}: no samples for {} s", SILENT.as_secs())),
            Err(_) => return Err("Airspy stopped streaming (unplugged?)".into()),
        }
    }
}
