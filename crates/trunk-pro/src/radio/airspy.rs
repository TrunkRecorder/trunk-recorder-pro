//! Airspy R2 / Mini through libairspy, loaded at run time. Needs libairspy
//! installed: `brew install airspy`, `apt install libairspy0`, or airspy.dll
//! (from the airspy-tools release) next to trunk-pro.exe / on PATH on Windows.
//! libairspy hands over real samples, turned into IQ (centred on the tuned
//! frequency, at the chosen rate) on the source's decoding thread; its own
//! conversion is kept behind `TRUNK_PRO_AIRSPY_LIBRARY_IQ=1` ([`library_iq`]).

use std::ffi::{c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use super::{ppm_corrected, DriverInfo};
use crate::sdr::{Applied, Control, Gain, Pool, SourceMsg, Tunable};

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

/// One Airspy opens at a time. libairspy finds a radio by opening every
/// Airspy to read its serial, and skips any it can't open: two sources
/// starting at once (or a device search during one) made one scan miss the
/// radio the other had open — AIRSPY_ERROR_NOT_FOUND until the retry.
static OPEN: Mutex<()> = Mutex::new(());

/// Open the Airspy `serial` (hex; "" = the first).
///
/// SAFETY (callers): the device returned is closed with `a.close`.
unsafe fn open(a: &Api, serial: &str) -> Result<Dev, String> {
    let _one = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    let mut dev: Dev = std::ptr::null_mut();
    // SAFETY: libairspy fills `dev`.
    unsafe {
        if serial.is_empty() {
            check(a, "open", (a.open)(&mut dev))?;
        } else {
            let sn = u64::from_str_radix(serial.trim_start_matches("0x"), 16).map_err(|_| format!("Airspy serial {serial:?} isn't hex"))?;
            check(a, "open", (a.open_sn)(&mut dev, sn))?;
        }
    }
    Ok(dev)
}

/// What an open Airspy says it is: its firmware string ("AirSpy NOS v1.0.0-rc10…" / "AirSpy MINI …")
/// and the IQ rates it runs at (as it lists them for IQ samples: the sample type is left as opened).
///
/// SAFETY (callers): `dev` is open, its sample type as opened (IQ).
unsafe fn identify(a: &Api, dev: Dev) -> (String, Vec<u32>) {
    unsafe {
        let mut v = [0 as std::ffi::c_char; 128];
        let version = if (a.version_string_read)(dev, v.as_mut_ptr(), v.len() as u8) == 0 { super::cstr(v.as_ptr()) } else { String::new() };
        let mut count = 0u32;
        (a.get_samplerates)(dev, &mut count, 0);
        let mut rates = vec![0u32; count.min(16) as usize];
        if !rates.is_empty() && (a.get_samplerates)(dev, rates.as_mut_ptr(), rates.len() as u32) != 0 {
            rates.clear();
        }
        (version, rates)
    }
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

fn msps(rates_iq: &[u32]) -> String {
    rates_iq.iter().map(|r| format!("{}", *r as f64 / 1e6)).collect::<Vec<_>>().join(" / ")
}

/// Why `want_hz` can't be set: the model and its rates, and where to pick one.
fn rate_error(model: &str, rates_iq: &[u32], want_hz: f64) -> String {
    let what = if model.is_empty() { "Airspy".to_string() } else { format!("Airspy {model}") };
    format!("this {what} runs at {} MSPS, not {}: pick one of those in Setup → Radios", msps(rates_iq), want_hz / 1e6)
}

/// A found Airspy for the interface: serial and label; model and rates when it could be
/// opened to ask (`identified`: firmware string, IQ rates).
fn describe(serial: u64, identified: Option<(&str, Vec<u32>)>) -> serde_json::Value {
    let serial = format!("{serial:016X}");
    match identified {
        Some((version, rates)) => {
            let model = model_of(version, &rates);
            let name = if model.is_empty() { "Airspy".to_string() } else { format!("Airspy {model}") };
            serde_json::json!({ "serial": serial, "label": format!("{name} · SN {serial}"), "model": model, "rates": rates })
        }
        None => serde_json::json!({ "serial": serial, "label": format!("Airspy · SN {serial}") }),
    }
}

/// Attached Airspys: [{serial (hex), label, model ("R2" / "Mini"), rates (IQ, Hz)}]. Each is
/// opened for a moment to ask its model and rates (as airspy_info does). One streaming for a
/// source is listed by serial alone if it can't be opened — or not at all: libairspy's own
/// listing opens each Airspy to read its serial, and on Windows an open one is exclusive.
pub fn devices() -> Vec<serde_json::Value> {
    let Ok(a) = api() else { return vec![] };
    let mut sn = [0u64; 16];
    // SAFETY: buffer and its length.
    let n = unsafe { (a.list_devices)(sn.as_mut_ptr(), sn.len() as c_int) };
    sn[..n.clamp(0, 16) as usize]
        .iter()
        .map(|s| {
            // SAFETY: opened here, closed here, nothing else uses the handle.
            let identified = unsafe {
                open(a, &format!("{s:016X}")).ok().map(|dev| {
                    let id = identify(a, dev);
                    (a.close)(dev);
                    id
                })
            };
            describe(*s, identified.as_ref().map(|(v, r)| (v.as_str(), r.clone())))
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
    fn stream_once(&self, source: usize, tx: &SyncSender<SourceMsg>, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(source, self, tx, stop, ctl)
    }
}

/// What libairspy's streaming thread hands samples to.
struct Ctx {
    source: usize,
    tx: SyncSender<SourceMsg>,
    stop: Arc<AtomicBool>,
    closed: AtomicBool,
    /// Buffers for its real samples (its thread allocates none).
    pool: Pool<i16>,
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
    // (dropped_samples: the buffers libairspy dropped just before this one, in this one's samples — not a running total.)
    let msg = match t.sample_type {
        SAMPLE_INT16_REAL => {
            // SAFETY: sample_count 16-bit real samples.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const i16, t.sample_count as usize) };
            SourceMsg::Real16 { source: ctx.source, samples: ctx.pool.copy(s), dropped: t.dropped_samples / 2, at: std::time::Instant::now() }
        }
        SAMPLE_FLOAT32_IQ => {
            // SAFETY: sample_count complex floats.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const Complex32, t.sample_count as usize) };
            SourceMsg::Iq { source: ctx.source, samples: s.to_vec(), dropped: t.dropped_samples, at: std::time::Instant::now() }
        }
        _ => return 0,
    };
    if ctx.tx.send(msg).is_err() {
        ctx.closed.store(true, Ordering::Relaxed);
        return 1;
    }
    0
}

struct Device<'a> {
    a: &'a Api,
    dev: Dev,
    streaming: bool,
    ppm: f64,
}

impl Tunable for Device<'_> {
    fn tune(&mut self, hz: f64) -> Result<f64, String> {
        // SAFETY: an open device.
        check(self.a, "tune", unsafe { (self.a.set_freq)(self.dev, ppm_corrected(hz, self.ppm).round() as u32) })?;
        Ok(hz)
    }
    /// Its own settings, which libairspy takes while streaming (a survey's
    /// dB is left alone: the linearity gain stays as set).
    fn set_gain(&mut self, g: Gain) -> Result<(), String> {
        let Gain::Airspy(g) = g else { return Ok(()) };
        // SAFETY: an open device.
        unsafe { set_gain(self.a, self.dev, g) }
    }
}

impl Drop for Device<'_> {
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

fn stream_once(source: usize, cfg: &AirspyConfig, tx: &SyncSender<SourceMsg>, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    // SAFETY: libairspy calls on the device this function opens.
    unsafe {
        let dev = open(a, &cfg.serial)?;
        let mut d = Device { a, dev, streaming: false, ppm: cfg.ppm };
        // (Asked as opened, for IQ samples: for real samples libairspy lists and takes rates doubled.)
        let (version, rates) = identify(a, d.dev);
        let want_iq = cfg.rate_hz.round() as u32;
        if !rates.contains(&want_iq) {
            return Err(rate_error(model_of(&version, &rates), &rates, cfg.rate_hz));
        }
        let real = !library_iq();
        check(a, "sample type", (a.set_sample_type)(d.dev, if real { SAMPLE_INT16_REAL } else { SAMPLE_FLOAT32_IQ }))?;
        check(a, "sample rate", (a.set_samplerate)(d.dev, if real { want_iq * 2 } else { want_iq }))?;
        d.tune(cfg.center_hz)?;
        // (As a gain set while running is: a reopened radio comes back the same.)
        d.set_gain(Gain::Airspy(cfg.gain))?;
        check(a, "bias-T", (a.set_rf_bias)(d.dev, cfg.bias_tee as u8))?;
        // (Older firmware can't pack: it streams unpacked then, as it always did.)
        if cfg.packing && (a.set_packing)(d.dev, 1) != 0 {
            log::warn!("Airspy {}: this firmware can't pack samples; streaming them unpacked (a third more USB bandwidth)", if cfg.serial.is_empty() { "(first)" } else { &cfg.serial });
        }
        let ctx = Box::new(Ctx { source, tx: tx.clone(), stop: stop.clone(), closed: AtomicBool::new(false), pool: Pool::default() });
        let ctx_ptr = &*ctx as *const Ctx as *mut c_void;
        check(a, "start", (a.start_rx)(d.dev, on_samples, ctx_ptr))?;
        d.streaming = true;
        let result = loop {
            std::thread::sleep(Duration::from_millis(if ctl.is_some() { 10 } else { 100 }));
            if let Some(c) = ctl {
                match c.apply(&mut d, cfg.center_hz, tx) {
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
        let d = describe(0x735863DC2F650AE3, Some(("AirSpy NOS v1.0.0-rc10", vec![10_000_000, 2_500_000])));
        assert_eq!(d["serial"], "735863DC2F650AE3");
        assert_eq!(d["label"], "Airspy R2 · SN 735863DC2F650AE3");
        assert_eq!(d["model"], "R2");
        assert_eq!(d["rates"], serde_json::json!([10_000_000, 2_500_000]));
        // One that couldn't be opened (in use): known by serial only, as before.
        let d = describe(1, None);
        assert_eq!(d["label"], "Airspy · SN 0000000000000001");
        assert!(d.get("model").is_none() && d.get("rates").is_none());
    }

    #[test]
    fn rate_refusal_names_the_model_and_the_fix() {
        assert_eq!(rate_error("R2", &[10_000_000, 2_500_000], 6e6), "this Airspy R2 runs at 10 / 2.5 MSPS, not 6: pick one of those in Setup → Radios");
        assert_eq!(rate_error("", &[6_000_000, 3_000_000], 2.5e6), "this Airspy runs at 6 / 3 MSPS, not 2.5: pick one of those in Setup → Radios");
    }
}
