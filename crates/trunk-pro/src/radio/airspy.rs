//! Airspy R2 / Mini through libairspy, loaded at run time. Needs libairspy
//! installed: `brew install airspy`, `apt install libairspy0`, or airspy.dll
//! (from the airspy-tools release) next to trunk-pro.exe / on PATH on Windows.
//! libairspy hands over real samples, turned into IQ (centred on the tuned
//! frequency, at the chosen rate) on the source's decoding thread; its own
//! conversion is kept behind `TRUNK_PRO_AIRSPY_LIBRARY_IQ=1` ([`library_iq`]).

use std::ffi::{c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::Library;
use num_complex::Complex32;

use super::{ppm_corrected, Caps, Deadline, DriverInfo, Rates};
use crate::sdr::{Applied, Control, Gain, Retuned, Sink, Tunable};

type Dev = *mut c_void;

#[repr(C)]
struct Transfer {
    device: Dev,
    ctx: *mut c_void,
    samples: *mut c_void,
    sample_count: c_int,
    dropped_samples: u64,
    sample_type: c_int,
}

#[repr(C)]
#[derive(Default)]
struct LibVersion {
    major: u32,
    minor: u32,
    revision: u32,
}

const SAMPLE_FLOAT32_IQ: c_int = 0;
const SAMPLE_INT16_REAL: c_int = 3;

/// libairspy's own IQ conversion, as before (`TRUNK_PRO_AIRSPY_LIBRARY_IQ=1`):
/// by default it hands over real samples and they're converted on the
/// source's decoding thread ([`SourceMsg::Real16`]), so its thread only
/// copies — it drops a buffer when that thread runs late by more than its
/// eight (~50 ms at 10 MS/s).
fn library_iq() -> bool {
    std::env::var_os("TRUNK_PRO_AIRSPY_LIBRARY_IQ").is_some_and(|v| v != "0")
}

type Callback = extern "C" fn(*mut Transfer) -> c_int;

struct Api {
    _lib: Library,
    lib_version: unsafe extern "C" fn(*mut LibVersion),
    list_devices: unsafe extern "C" fn(*mut u64, c_int) -> c_int,
    open_sn: unsafe extern "C" fn(*mut Dev, u64) -> c_int,
    open: unsafe extern "C" fn(*mut Dev) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    version_string_read: unsafe extern "C" fn(Dev, *mut std::ffi::c_char, u8) -> c_int,
    board_partid_serialno_read: unsafe extern "C" fn(Dev, *mut SerialRead) -> c_int,
    get_samplerates: unsafe extern "C" fn(Dev, *mut u32, u32) -> c_int,
    set_samplerate: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_sample_type: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_packing: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_freq: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_linearity_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_sensitivity_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_lna_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_mixer_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_vga_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_lna_agc: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_mixer_agc: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_rf_bias: unsafe extern "C" fn(Dev, u8) -> c_int,
    start_rx: unsafe extern "C" fn(Dev, Callback, *mut c_void) -> c_int,
    stop_rx: unsafe extern "C" fn(Dev) -> c_int,
    is_streaming: unsafe extern "C" fn(Dev) -> c_int,
    error_name: unsafe extern "C" fn(c_int) -> *const std::ffi::c_char,
}

// SAFETY: plain C entry points; each device is used from one thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut names = super::candidates("libairspy.so.0", "libairspy.dylib", &["airspy.dll", "libairspy.dll"]);
        if !cfg!(target_os = "macos") && !cfg!(windows) {
            names.push("libairspy.so".into());
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lib, path) = super::load("TRUNK_PRO_AIRSPY", &names, "airspy", false)?;
        // SAFETY: the signatures are libairspy's (airspy.h, 1.0.x).
        unsafe {
            macro_rules! f {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{path}: {} missing: {e}", $n))?
                };
            }
            Ok(Api {
                lib_version: f!("airspy_lib_version"),
                list_devices: f!("airspy_list_devices"),
                open_sn: f!("airspy_open_sn"),
                open: f!("airspy_open"),
                close: f!("airspy_close"),
                version_string_read: f!("airspy_version_string_read"),
                board_partid_serialno_read: f!("airspy_board_partid_serialno_read"),
                get_samplerates: f!("airspy_get_samplerates"),
                set_samplerate: f!("airspy_set_samplerate"),
                set_sample_type: f!("airspy_set_sample_type"),
                set_packing: f!("airspy_set_packing"),
                set_freq: f!("airspy_set_freq"),
                set_linearity_gain: f!("airspy_set_linearity_gain"),
                set_sensitivity_gain: f!("airspy_set_sensitivity_gain"),
                set_lna_gain: f!("airspy_set_lna_gain"),
                set_mixer_gain: f!("airspy_set_mixer_gain"),
                set_vga_gain: f!("airspy_set_vga_gain"),
                set_lna_agc: f!("airspy_set_lna_agc"),
                set_mixer_agc: f!("airspy_set_mixer_agc"),
                set_rf_bias: f!("airspy_set_rf_bias"),
                start_rx: f!("airspy_start_rx"),
                stop_rx: f!("airspy_stop_rx"),
                is_streaming: f!("airspy_is_streaming"),
                error_name: f!("airspy_error_name"),
                _lib: lib,
            })
        }
    })
    .as_ref()
    .map_err(|e| e.clone())
}

pub fn info() -> DriverInfo {
    match api() {
        Ok(a) => {
            let mut v = LibVersion::default();
            // SAFETY: fills the struct.
            unsafe { (a.lib_version)(&mut v) };
            DriverInfo { loaded: true, detail: format!("libairspy {}.{}.{}", v.major, v.minor, v.revision) }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("libairspy {e}") },
    }
}

fn check(a: &Api, what: &str, r: c_int) -> Result<(), String> {
    if r == 0 {
        return Ok(());
    }
    // SAFETY: returns a static string.
    let name = unsafe { CStr::from_ptr((a.error_name)(r)) }.to_string_lossy().into_owned();
    Err(format!("Airspy {what}: {name}"))
}

/// Open the Airspy `serial` (hex; "" = the first), with a deadline, one
/// radio at a time ([`super::open`]).
fn open(a: &'static Api, serial: &str) -> Result<Device, String> {
    let name = format!("Airspy {}", if serial.is_empty() { "(first)" } else { serial });
    let serial = serial.to_string();
    super::open(&name, Deadline::Quick, true, move || {
        let mut dev: Dev = std::ptr::null_mut();
        // SAFETY: libairspy fills `dev`; the Device closes it.
        unsafe {
            if serial.is_empty() {
                check(a, "open", (a.open)(&mut dev))?;
            } else {
                let sn = u64::from_str_radix(serial.trim_start_matches("0x"), 16).map_err(|_| format!("Airspy serial {serial:?} isn't hex"))?;
                check(a, "open", (a.open_sn)(&mut dev, sn))?;
            }
        }
        Ok(Device { a, dev, streaming: false, ppm: 0.0 })
    })
}

/// What an open Airspy says it is — its model, from its firmware string ("AirSpy NOS
/// v1.0.0-rc10…" / "AirSpy MINI …"), and the IQ rates it lists (as opened, for IQ samples) —
/// remembered by its serial for the list of radios. Its board id can't tell an R2 from a
/// Mini: both say "AIRSPY".
///
/// SAFETY (callers): `dev` is open, its sample type as opened (IQ).
unsafe fn identify(a: &Api, dev: Dev) -> Caps {
    unsafe {
        let mut v = [0 as std::ffi::c_char; 128];
        let version = if (a.version_string_read)(dev, v.as_mut_ptr(), v.len() as u8) == 0 { super::cstr(v.as_ptr()) } else { String::new() };
        let mut count = 0u32;
        (a.get_samplerates)(dev, &mut count, 0);
        let mut rates = vec![0u32; count.min(16) as usize];
        if !rates.is_empty() && (a.get_samplerates)(dev, rates.as_mut_ptr(), rates.len() as u32) != 0 {
            rates.clear();
        }
        let caps = Caps { model: model_of(&version, &rates).to_string(), rates: Rates::List(rates) };
        let mut sn = SerialRead::default();
        if (a.board_partid_serialno_read)(dev, &mut sn) == 0 {
            super::remember(&key(sn.serial()), caps.clone());
        }
        caps
    }
}

/// libairspy's airspy_read_partid_serialno_t.
#[repr(C)]
#[derive(Default)]
struct SerialRead {
    part_id: [u32; 2],
    serial_no: [u32; 4],
}

impl SerialRead {
    /// The 64-bit serial airspy_open_sn takes (as airspy_info prints it).
    fn serial(&self) -> u64 {
        ((self.serial_no[2] as u64) << 32) | self.serial_no[3] as u64
    }
}

/// Its key among the radios [`super::known`].
fn key(serial: u64) -> String {
    format!("airspy:{serial:016X}")
}

/// "R2" or "Mini", from the firmware string, else from the rates (an R2 does 2.5 MSPS,
/// a Mini 6); "" when neither says.
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

/// "Airspy R2", or "Airspy" when the model isn't known.
fn name(model: &str) -> String {
    if model.is_empty() { "Airspy".to_string() } else { format!("Airspy {model}") }
}

/// A found Airspy for the interface: serial and label; model and rates once known.
fn describe(serial: u64, caps: Option<&Caps>) -> serde_json::Value {
    let sn = format!("{serial:016X}");
    let mut j = serde_json::json!({ "serial": sn, "label": format!("{} · SN {sn}", name(caps.map_or("", |c| c.model.as_str()))) });
    if let Some(c) = caps {
        c.add_to(&mut j);
    }
    j
}

/// Attached Airspys: [{serial (hex), label, model ("R2" / "Mini"), rates (IQ, Hz)}]. One not
/// opened yet in this run is opened for a moment to ask (as airspy_info does), once. One
/// streaming for a source is shown as it said when it opened — or isn't listed at all: on
/// Windows libairspy's own listing can't open it to read its serial.
pub fn devices() -> Vec<serde_json::Value> {
    let Ok(a) = api() else { return vec![] };
    let found = super::open("Airspy search", Deadline::Quick, true, move || {
        let mut sn = [0u64; 16];
        // SAFETY: buffer and its length.
        let n = unsafe { (a.list_devices)(sn.as_mut_ptr(), sn.len() as c_int) };
        Ok(sn[..n.clamp(0, 16) as usize].to_vec())
    });
    found
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            // (Opened here and closed when dropped: nothing else uses the handle.)
            // SAFETY: an open device, as opened (IQ).
            let caps = super::known(&key(s)).or_else(|| open(a, &format!("{s:016X}")).ok().map(|d| unsafe { identify(a, d.dev) }));
            describe(s, caps.as_ref())
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct AirspyConfig {
    /// Hex serial; "" = the first.
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub gain: AirspyGain,
    pub bias_tee: bool,
    /// 12-bit packing on the USB.
    pub packing: bool,
    pub ppm: f64,
}

/// How its gain is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AirspyGain {
    /// libairspy's linearity table, step 0..21.
    Linearity(u8),
    /// Its sensitivity table, step 0..21.
    Sensitivity(u8),
    /// Each stage: LNA 0..14, mixer 0..15, VGA 0..15.
    Manual { lna: u8, mixer: u8, vga: u8 },
    /// The Airspy's AGC on the LNA and mixer; the VGA as given.
    Agc { vga: u8 },
}

impl std::fmt::Display for AirspyGain {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AirspyGain::Linearity(g) => write!(f, "linearity {g}"),
            AirspyGain::Sensitivity(g) => write!(f, "sensitivity {g}"),
            AirspyGain::Manual { lna, mixer, vga } => write!(f, "LNA {lna} · mixer {mixer} · VGA {vga}"),
            AirspyGain::Agc { vga } => write!(f, "AGC (VGA {vga})"),
        }
    }
}

impl crate::sdr::Radio for AirspyConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if let Gain::Airspy(g) = g {
            self.gain = g;
        }
    }
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// What libairspy's streaming thread hands samples to.
struct Ctx<'a> {
    sink: Mutex<&'a mut Sink>,
    stop: Arc<AtomicBool>,
    closed: AtomicBool,
}

extern "C" fn on_samples(t: *mut Transfer) -> c_int {
    // SAFETY: libairspy passes a valid transfer whose ctx is our Ctx, alive
    // until airspy_stop_rx returns.
    let (t, ctx) = unsafe { (&*t, &*((*t).ctx as *const Ctx)) };
    if ctx.stop.load(Ordering::Relaxed) || ctx.closed.load(Ordering::Relaxed) {
        return 1; // stop streaming
    }
    if t.sample_count <= 0 {
        return 0;
    }
    let mut sink = ctx.sink.lock().unwrap();
    // (dropped_samples: the buffers libairspy dropped just before this one, in this one's samples — not a running total.)
    let open = match t.sample_type {
        SAMPLE_INT16_REAL => {
            // SAFETY: sample_count 16-bit real samples.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const i16, t.sample_count as usize) };
            sink.real16(s, t.dropped_samples / 2, Instant::now())
        }
        SAMPLE_FLOAT32_IQ => {
            // SAFETY: sample_count complex floats.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const Complex32, t.sample_count as usize) };
            sink.iq(s, t.dropped_samples, Instant::now())
        }
        _ => return 0,
    };
    if !open {
        ctx.closed.store(true, Ordering::Relaxed);
        return 1;
    }
    0
}

/// An open Airspy: closed (and stopped) when dropped.
struct Device {
    a: &'static Api,
    dev: Dev,
    streaming: bool,
    ppm: f64,
}

// SAFETY: libairspy's handle is used from one thread at a time ([`super::open`]'s hands it over).
unsafe impl Send for Device {}

impl Tunable for Device {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        // SAFETY: an open device.
        check(self.a, "tune", unsafe { (self.a.set_freq)(self.dev, ppm_corrected(hz, self.ppm).round() as u32) })?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// Its own settings, which libairspy takes while streaming (a survey's
    /// dB is left alone: the linearity gain stays as set).
    fn set_gain(&mut self, g: Gain) -> Result<u64, String> {
        let Gain::Airspy(g) = g else { return Ok(0) };
        // SAFETY: an open device.
        unsafe { set_gain(self.a, self.dev, g) }.map(|()| 0)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: a device opened by this struct.
        unsafe {
            if self.streaming {
                (self.a.stop_rx)(self.dev);
            }
            (self.a.close)(self.dev);
        }
    }
}

/// Set the gain as `g` says.
///
/// SAFETY (callers): `dev` is open.
unsafe fn set_gain(a: &Api, dev: Dev, g: AirspyGain) -> Result<(), String> {
    unsafe {
        match g {
            AirspyGain::Linearity(g) => check(a, "gain", (a.set_linearity_gain)(dev, g.min(21))),
            AirspyGain::Sensitivity(g) => check(a, "gain", (a.set_sensitivity_gain)(dev, g.min(21))),
            AirspyGain::Manual { lna, mixer, vga } => {
                check(a, "LNA AGC off", (a.set_lna_agc)(dev, 0))?;
                check(a, "mixer AGC off", (a.set_mixer_agc)(dev, 0))?;
                check(a, "LNA gain", (a.set_lna_gain)(dev, lna.min(14)))?;
                check(a, "mixer gain", (a.set_mixer_gain)(dev, mixer.min(15)))?;
                check(a, "VGA gain", (a.set_vga_gain)(dev, vga.min(15)))
            }
            AirspyGain::Agc { vga } => {
                check(a, "LNA AGC", (a.set_lna_agc)(dev, 1))?;
                check(a, "mixer AGC", (a.set_mixer_agc)(dev, 1))?;
                check(a, "VGA gain", (a.set_vga_gain)(dev, vga.min(15)))
            }
        }
    }
}

fn stream_once(cfg: &AirspyConfig, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    // SAFETY: libairspy calls on the device this function opens.
    unsafe {
        let mut d = open(a, &cfg.serial)?;
        d.ppm = cfg.ppm;
        // (Asked as opened, for IQ samples: for real samples libairspy lists rates doubled.)
        let caps = identify(a, d.dev);
        let want_iq = cfg.rate_hz.round() as u32;
        if !caps.rates.allows(cfg.rate_hz) {
            return Err(super::rate_refusal(&name(&caps.model), &caps.rates, cfg.rate_hz));
        }
        let real = !library_iq();
        check(a, "sample type", (a.set_sample_type)(d.dev, if real { SAMPLE_INT16_REAL } else { SAMPLE_FLOAT32_IQ }))?;
        // By the IQ rate even for real samples: libairspy looks rates up in its own
        // (IQ) list and sends the one found as its index, which every firmware takes.
        // A doubled rate isn't in that list (libairspy would send it by value, which
        // firmware before v1.0.0-rc7 refuses), or worse, is: a Mini's 3 MSPS doubled
        // is 6, its first rate, and it would run at twice the rate asked for.
        check(a, "sample rate", (a.set_samplerate)(d.dev, want_iq))?;
        d.tune(cfg.center_hz)?;
        // (As a gain set while running is: a reopened radio comes back the same.)
        d.set_gain(Gain::Airspy(cfg.gain))?;
        check(a, "bias-T", (a.set_rf_bias)(d.dev, cfg.bias_tee as u8))?;
        // (Older firmware can't pack: it streams unpacked then, as it always did.)
        if cfg.packing && (a.set_packing)(d.dev, 1) != 0 {
            log::warn!("Airspy {}: this firmware can't pack samples; streaming them unpacked (a third more USB bandwidth)", if cfg.serial.is_empty() { "(first)" } else { &cfg.serial });
        }
        let ctx = Box::new(Ctx { sink: Mutex::new(sink), stop: stop.clone(), closed: AtomicBool::new(false) });
        let ctx_ptr = &*ctx as *const Ctx as *mut c_void;
        check(a, "start", (a.start_rx)(d.dev, on_samples, ctx_ptr))?;
        d.streaming = true;
        let result = loop {
            std::thread::sleep(Duration::from_millis(if ctl.is_some() { 10 } else { 100 }));
            if let Some(c) = ctl {
                match c.apply(&mut d, cfg.center_hz, &mut ctx.sink.lock().unwrap()) {
                    Err(e) => break Err(e),
                    Ok(Applied::Closed) => break Ok(()),
                    Ok(_) => {}
                }
            }
            if stop.load(Ordering::Relaxed) || ctx.closed.load(Ordering::Relaxed) {
                break Ok(());
            }
            if (a.is_streaming)(d.dev) != 1 {
                break Err("Airspy stopped streaming (unplugged?)".to_string());
            }
        };
        drop(d); // stop_rx before the context goes
        drop(ctx);
        result
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
    }

    #[test]
    fn device_json_carries_model_and_rates() {
        let r2 = Caps { model: "R2".into(), rates: Rates::List(vec![10_000_000, 2_500_000]) };
        let d = describe(0x735863DC2F650AE3, Some(&r2));
        assert_eq!(d["serial"], "735863DC2F650AE3");
        assert_eq!(d["label"], "Airspy R2 · SN 735863DC2F650AE3");
        assert_eq!(d["model"], "R2");
        assert_eq!(d["rates"], serde_json::json!([10_000_000, 2_500_000]));
        // One not asked yet that couldn't be opened (another program has it): by serial only.
        let d = describe(1, None);
        assert_eq!(d["label"], "Airspy · SN 0000000000000001");
        assert!(d.get("model").is_none() && d.get("rates").is_none());
    }

    #[test]
    fn serial_as_airspy_info_prints_it() {
        let r = SerialRead { part_id: [0; 2], serial_no: [0, 0, 0x735863DC, 0x2F650AE3] };
        assert_eq!(key(r.serial()), "airspy:735863DC2F650AE3");
    }
}
