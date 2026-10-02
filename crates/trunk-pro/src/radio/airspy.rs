//! Airspy R2 / Mini through libairspy, loaded at run time. Needs libairspy
//! installed: `brew install airspy`, `apt install libairspy0`, or airspy.dll
//! (from the airspy-tools release) next to trunk-pro.exe / on PATH on Windows.
//! libairspy does the real-to-IQ conversion; samples arrive as float IQ at
//! the chosen rate, centred on the tuned frequency.

use std::ffi::{c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use super::DriverInfo;
use crate::sdr::{Control, SourceMsg};

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

type Callback = extern "C" fn(*mut Transfer) -> c_int;

struct Api {
    _lib: Library,
    lib_version: unsafe extern "C" fn(*mut LibVersion),
    list_devices: unsafe extern "C" fn(*mut u64, c_int) -> c_int,
    open_sn: unsafe extern "C" fn(*mut Dev, u64) -> c_int,
    open: unsafe extern "C" fn(*mut Dev) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    get_samplerates: unsafe extern "C" fn(Dev, *mut u32, u32) -> c_int,
    set_samplerate: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_sample_type: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_freq: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_linearity_gain: unsafe extern "C" fn(Dev, u8) -> c_int,
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
                get_samplerates: f!("airspy_get_samplerates"),
                set_samplerate: f!("airspy_set_samplerate"),
                set_sample_type: f!("airspy_set_sample_type"),
                set_freq: f!("airspy_set_freq"),
                set_linearity_gain: f!("airspy_set_linearity_gain"),
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

/// Attached Airspys: [{serial (hex), label}].
pub fn devices() -> Vec<serde_json::Value> {
    let Ok(a) = api() else { return vec![] };
    let mut sn = [0u64; 16];
    // SAFETY: buffer and its length.
    let n = unsafe { (a.list_devices)(sn.as_mut_ptr(), sn.len() as c_int) };
    sn[..n.clamp(0, 16) as usize].iter().map(|s| serde_json::json!({ "serial": format!("{s:016X}"), "label": format!("Airspy {s:016X}") })).collect()
}

#[derive(Clone, Debug)]
pub struct AirspyConfig {
    /// Hex serial; "" = the first.
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub gain: u8,
    pub bias_tee: bool,
    pub ppm: f64,
}

pub fn run(source: usize, cfg: AirspyConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    run_with(source, cfg, tx, stop, None)
}

/// [`run`], retuned on request.
pub fn run_with(source: usize, mut cfg: AirspyConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<Control>>) {
    while !stop.load(Ordering::Relaxed) {
        if let Some(c) = ctl.as_ref().and_then(|c| *c.current_hz.lock().unwrap()) {
            cfg.center_hz = c;
        }
        if let Err(e) = stream_once(source, &cfg, &tx, &stop, ctl.as_deref()) {
            let _ = tx.send(SourceMsg::Error { source, error: e });
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

/// What libairspy's streaming thread hands samples to.
struct Ctx {
    source: usize,
    tx: SyncSender<SourceMsg>,
    stop: Arc<AtomicBool>,
    closed: AtomicBool,
    dropped: AtomicU64,
}

extern "C" fn on_samples(t: *mut Transfer) -> c_int {
    // SAFETY: libairspy passes a valid transfer whose ctx is our Ctx, alive
    // until airspy_stop_rx returns.
    let (t, ctx) = unsafe { (&*t, &*((*t).ctx as *const Ctx)) };
    if ctx.stop.load(Ordering::Relaxed) || ctx.closed.load(Ordering::Relaxed) {
        return 1; // stop streaming
    }
    if t.sample_type != SAMPLE_FLOAT32_IQ || t.sample_count <= 0 {
        return 0;
    }
    // SAFETY: sample_count complex floats.
    let s = unsafe { std::slice::from_raw_parts(t.samples as *const Complex32, t.sample_count as usize) };
    let dropped = t.dropped_samples.saturating_sub(ctx.dropped.swap(t.dropped_samples, Ordering::Relaxed));
    if ctx.tx.send(SourceMsg::Iq { source: ctx.source, samples: s.to_vec(), dropped }).is_err() {
        ctx.closed.store(true, Ordering::Relaxed);
        return 1;
    }
    0
}

struct Device<'a> {
    a: &'a Api,
    dev: Dev,
    streaming: bool,
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

fn stream_once(source: usize, cfg: &AirspyConfig, tx: &SyncSender<SourceMsg>, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    let mut dev: Dev = std::ptr::null_mut();
    // SAFETY: libairspy calls on the device this function opens.
    unsafe {
        if cfg.serial.is_empty() {
            check(a, "open", (a.open)(&mut dev))?;
        } else {
            let sn = u64::from_str_radix(cfg.serial.trim_start_matches("0x"), 16).map_err(|_| format!("Airspy serial {:?} isn't hex", cfg.serial))?;
            check(a, "open", (a.open_sn)(&mut dev, sn))?;
        }
        let mut d = Device { a, dev, streaming: false };
        check(a, "sample type", (a.set_sample_type)(d.dev, SAMPLE_FLOAT32_IQ))?;
        let mut count = 0u32;
        (a.get_samplerates)(d.dev, &mut count, 0);
        let mut rates = vec![0u32; count as usize];
        (a.get_samplerates)(d.dev, rates.as_mut_ptr(), count);
        let want = cfg.rate_hz.round() as u32;
        if !rates.contains(&want) {
            let list: Vec<String> = rates.iter().map(|r| format!("{}", *r as f64 / 1e6)).collect();
            return Err(format!("this Airspy runs at {} MSPS, not {}", list.join(" / "), cfg.rate_hz / 1e6));
        }
        check(a, "sample rate", (a.set_samplerate)(d.dev, want))?;
        check(a, "tune", (a.set_freq)(d.dev, (cfg.center_hz / (1.0 + cfg.ppm * 1e-6)).round() as u32))?;
        check(a, "gain", (a.set_linearity_gain)(d.dev, cfg.gain.min(21)))?;
        check(a, "bias-T", (a.set_rf_bias)(d.dev, cfg.bias_tee as u8))?;
        let ctx = Box::new(Ctx { source, tx: tx.clone(), stop: stop.clone(), closed: AtomicBool::new(false), dropped: AtomicU64::new(0) });
        let ctx_ptr = &*ctx as *const Ctx as *mut c_void;
        check(a, "start", (a.start_rx)(d.dev, on_samples, ctx_ptr))?;
        d.streaming = true;
        let result = loop {
            std::thread::sleep(Duration::from_millis(if ctl.is_some() { 10 } else { 100 }));
            if let Some(c) = ctl {
                // Only the frequency: a survey leaves the linearity gain as set.
                if let (Some(f), _) = c.take() {
                    if let Err(e) = check(a, "tune", (a.set_freq)(d.dev, (f / (1.0 + cfg.ppm * 1e-6)).round() as u32)) {
                        break Err(e);
                    }
                    *c.current_hz.lock().unwrap() = Some(f);
                    let _ = tx.send(SourceMsg::Tuned { center_hz: f });
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
