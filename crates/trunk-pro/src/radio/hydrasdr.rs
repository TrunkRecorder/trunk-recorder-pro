//! HydraSDR RFOne through libhydrasdr (1.1 or newer), loaded at run time.
//! Needs libhydrasdr installed: hydrasdr-host from https://github.com/hydrasdr/hydrasdr-host
//! (`libhydrasdr.so.1`; on Windows hydrasdr.dll next to trunk-pro.exe / on PATH). Its tuner
//! and gain stages are an Airspy R2's, so it takes an Airspy's gain settings; it also has three
//! inputs (ANT, with the bias-T, and CABLE1 / CABLE2). libhydrasdr turns its samples into IQ
//! itself, on a thread of its own apart from the one draining the USB (and decimates below
//! 2.5 MSPS), so it hands over float IQ.
//!
//! An RFOne with the old USB ID shares the Airspy's (1d50:60a1). libairspy doesn't list it (its
//! serial string isn't an Airspy's) but opens it as "the first" when it comes first; libhydrasdr
//! opens only HydraSDR firmware, never an Airspy.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::Library;
use num_complex::Complex32;

use super::{ppm_corrected, Caps, Deadline, DriverInfo, Rates};
use crate::sdr::{Applied, Control, Gain, Preset, Retuned, Sink, Stage, Stages, Tunable};

type Dev = *mut c_void;

/// hydrasdr_transfer_t.
#[repr(C)]
struct Transfer {
    device: Dev,
    ctx: *mut c_void,
    samples: *mut c_void,
    sample_count: c_int,
    /// Since streaming started (libairspy's is per buffer).
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

// hydrasdr_gain_type_t.
const GAIN_LNA: c_int = 0;
const GAIN_MIXER: c_int = 2;
const GAIN_VGA: c_int = 4;
const GAIN_LINEARITY: c_int = 5;
const GAIN_SENSITIVITY: c_int = 6;
const GAIN_LNA_AGC: c_int = 7;
const GAIN_MIXER_AGC: c_int = 9;

// hydrasdr_capability_t.
const CAP_BIAS_TEE: u32 = 1 << 11;
const CAP_PACKING: u32 = 1 << 12;
const CAP_RF_PORT_SELECT: u32 = 1 << 13;

/// hydrasdr_read_partid_serialno_t.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SerialRead {
    part_id: [u32; 2],
    serial_no: [u32; 4],
}

impl SerialRead {
    /// The 64-bit serial hydrasdr_open_sn takes (as hydrasdr_info prints it).
    fn serial(&self) -> u64 {
        ((self.serial_no[2] as u64) << 32) | self.serial_no[3] as u64
    }
}

/// hydrasdr_gain_range_t.
#[repr(C)]
#[derive(Clone, Copy)]
struct GainRange {
    min: u8,
    max: u8,
    step: u8,
    default: u8,
    _reserved: [u8; 4],
}

/// hydrasdr_rf_port_info_t.
#[repr(C)]
#[derive(Clone, Copy)]
struct PortInfo {
    name: [c_char; 16],
    min_frequency: u64,
    max_frequency: u64,
    has_bias_tee: u8,
    _reserved1: [u8; 7],
    bias_tee_volts: f32,
    bias_tee_max_ma: f32,
    _reserved2: [u8; 8],
}

/// hydrasdr_device_info_t (4096 bytes), as far as it's read here; the rest is kept as bytes.
#[repr(C)]
struct DeviceInfo {
    struct_version: [u16; 4],
    board_id: u8,
    _board_reserved: [u8; 7],
    board_name: [c_char; 32],
    firmware_version: [c_char; 128],
    part_serial: SerialRead,
    features: u32,
    _features_reserved: [u32; 3],
    /// LNA at 0, RF 5, mixer 10, filter 15, VGA 20, linearity 25, sensitivity 26 (the others reserved).
    gains: [GainRange; 31],
    /// The RF front end's and the clock generator's names.
    _components: [u8; 8 + 2 * 64],
    min_frequency: u64,
    max_frequency: u64,
    _freq_reserved: [u32; 8],
    rf_ports: u32,
    rf_port_count: u8,
    _rf_port_reserved: [u8; 3],
    rf_port_info: [PortInfo; 32],
    _rest: [u8; 4096 - 2448],
}

const _: () = {
    assert!(std::mem::size_of::<PortInfo>() == 56);
    assert!(std::mem::offset_of!(DeviceInfo, firmware_version) == 48);
    assert!(std::mem::offset_of!(DeviceInfo, features) == 200);
    assert!(std::mem::offset_of!(DeviceInfo, min_frequency) == 600);
    assert!(std::mem::offset_of!(DeviceInfo, rf_port_info) == 656);
    assert!(std::mem::size_of::<DeviceInfo>() == 4096);
};

impl DeviceInfo {
    fn zeroed() -> Box<DeviceInfo> {
        // SAFETY: plain integers, floats and arrays of them: all zeroes is a value.
        unsafe { Box::new(std::mem::zeroed()) }
    }
    /// "RFOne", from "HydraSDR RFOne".
    fn model(&self) -> String {
        // SAFETY: the library NUL-terminates it (and the struct was zeroed).
        let name = unsafe { super::cstr(self.board_name.as_ptr()) };
        name.trim().strip_prefix("HydraSDR").unwrap_or(&name).trim().to_string()
    }
    /// Its inputs' names ("ANT", "CABLE1", "CABLE2"), by number.
    fn ports(&self) -> Vec<String> {
        // SAFETY: as above.
        self.rf_port_info[..(self.rf_port_count as usize).min(32)].iter().map(|p| unsafe { super::cstr(p.name.as_ptr()) }).collect()
    }
}

type Callback = extern "C" fn(*mut Transfer) -> c_int;

struct Api {
    _lib: Library,
    lib_version: unsafe extern "C" fn(*mut LibVersion),
    list_devices: unsafe extern "C" fn(*mut u64, c_int) -> c_int,
    open_sn: unsafe extern "C" fn(*mut Dev, u64) -> c_int,
    open: unsafe extern "C" fn(*mut Dev) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    get_device_info: unsafe extern "C" fn(Dev, *mut DeviceInfo) -> c_int,
    get_samplerates: unsafe extern "C" fn(Dev, *mut u32, u32) -> c_int,
    set_samplerate: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_sample_type: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_packing: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_freq: unsafe extern "C" fn(Dev, u64) -> c_int,
    set_gain: unsafe extern "C" fn(Dev, c_int, u8) -> c_int,
    set_rf_bias: unsafe extern "C" fn(Dev, u8) -> c_int,
    set_rf_port: unsafe extern "C" fn(Dev, c_int) -> c_int,
    start_rx: unsafe extern "C" fn(Dev, Callback, *mut c_void) -> c_int,
    stop_rx: unsafe extern "C" fn(Dev) -> c_int,
    is_streaming: unsafe extern "C" fn(Dev) -> c_int,
    error_name: unsafe extern "C" fn(c_int) -> *const c_char,
}

// SAFETY: plain C entry points; each device is used from one thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut names = super::candidates("libhydrasdr.so.1", "libhydrasdr.1.dylib", &["hydrasdr.dll", "libhydrasdr.dll"]);
        if !cfg!(target_os = "macos") && !cfg!(windows) {
            names.push("libhydrasdr.so".into());
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lib, path) = super::load("TRUNK_PRO_HYDRASDR", &names, "hydrasdr", false)?;
        // SAFETY: the signatures are libhydrasdr's (hydrasdr.h, 1.1.x).
        unsafe {
            macro_rules! f {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{path}: {} missing (libhydrasdr 1.1 or newer is needed): {e}", $n))?
                };
            }
            Ok(Api {
                lib_version: f!("hydrasdr_lib_version"),
                list_devices: f!("hydrasdr_list_devices"),
                open_sn: f!("hydrasdr_open_sn"),
                open: f!("hydrasdr_open"),
                close: f!("hydrasdr_close"),
                get_device_info: f!("hydrasdr_get_device_info"),
                get_samplerates: f!("hydrasdr_get_samplerates"),
                set_samplerate: f!("hydrasdr_set_samplerate"),
                set_sample_type: f!("hydrasdr_set_sample_type"),
                set_packing: f!("hydrasdr_set_packing"),
                set_freq: f!("hydrasdr_set_freq"),
                set_gain: f!("hydrasdr_set_gain"),
                set_rf_bias: f!("hydrasdr_set_rf_bias"),
                set_rf_port: f!("hydrasdr_set_rf_port"),
                start_rx: f!("hydrasdr_start_rx"),
                stop_rx: f!("hydrasdr_stop_rx"),
                is_streaming: f!("hydrasdr_is_streaming"),
                error_name: f!("hydrasdr_error_name"),
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
            DriverInfo { loaded: true, detail: format!("libhydrasdr {}.{}.{}", v.major, v.minor, v.revision) }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("libhydrasdr {e}") },
    }
}

fn check(a: &Api, what: &str, r: c_int) -> Result<(), String> {
    if r == 0 {
        return Ok(());
    }
    // SAFETY: returns a static string.
    let name = unsafe { CStr::from_ptr((a.error_name)(r)) }.to_string_lossy().into_owned();
    Err(format!("HydraSDR {what}: {name}"))
}

/// Open the HydraSDR `serial` (hex; "" = the first), with a deadline, one
/// radio at a time ([`super::open`]).
fn open(a: &'static Api, serial: &str) -> Result<Device, String> {
    let name = format!("HydraSDR {}", if serial.is_empty() { "(first)" } else { serial });
    let serial = serial.to_string();
    super::open(&name, Deadline::Quick, true, move || {
        let mut dev: Dev = std::ptr::null_mut();
        // SAFETY: libhydrasdr fills `dev`; the Device closes it.
        unsafe {
            if serial.is_empty() {
                check(a, "open", (a.open)(&mut dev))?;
            } else {
                let sn = u64::from_str_radix(serial.trim_start_matches("0x"), 16).map_err(|_| format!("HydraSDR serial {serial:?} isn't hex"))?;
                check(a, "open", (a.open_sn)(&mut dev, sn))?;
            }
        }
        Ok(Device { a, dev, streaming: false, ppm: 0.0 })
    })
}

/// What an open HydraSDR says it is: its device info, and its rates (IQ, as
/// opened: with the decimated ones) — remembered by its serial for the list of radios.
///
/// SAFETY (callers): `dev` is open, its sample type as opened (IQ).
unsafe fn identify(a: &Api, dev: Dev) -> Result<(Caps, Box<DeviceInfo>), String> {
    unsafe {
        let mut info = DeviceInfo::zeroed();
        check(a, "device info", (a.get_device_info)(dev, &mut *info))?;
        let mut count = 0u32;
        (a.get_samplerates)(dev, &mut count, 0);
        let mut rates = vec![0u32; count.min(32) as usize];
        if !rates.is_empty() && (a.get_samplerates)(dev, rates.as_mut_ptr(), rates.len() as u32) != 0 {
            rates.clear();
        }
        let caps = Caps { model: info.model(), rates: Rates::List(rates) };
        super::remember(&key(info.part_serial.serial()), caps.clone());
        Ok((caps, info))
    }
}

/// Its key among the radios [`super::known`].
fn key(serial: u64) -> String {
    format!("hydrasdr:{serial:016X}")
}

/// "HydraSDR RFOne", or "HydraSDR" when the model isn't known.
fn name(model: &str) -> String {
    if model.is_empty() { "HydraSDR".to_string() } else { format!("HydraSDR {model}") }
}

/// A found HydraSDR for the interface: serial and label; model and rates once known.
fn describe(serial: u64, caps: Option<&Caps>) -> serde_json::Value {
    let sn = format!("{serial:016X}");
    let mut j = serde_json::json!({ "serial": sn, "label": format!("{} · SN {sn}", name(caps.map_or("", |c| c.model.as_str()))) });
    if let Some(c) = caps {
        c.add_to(&mut j);
    }
    j
}

/// The serials libhydrasdr finds (by each one's USB serial string).
fn serials() -> Vec<u64> {
    let Ok(a) = api() else { return vec![] };
    super::open("HydraSDR search", Deadline::Quick, true, move || {
        let mut sn = [0u64; 16];
        // SAFETY: buffer and its length.
        let n = unsafe { (a.list_devices)(sn.as_mut_ptr(), sn.len() as c_int) };
        Ok(sn[..n.clamp(0, 16) as usize].to_vec())
    })
    .unwrap_or_default()
}

/// Attached HydraSDRs: [{serial (hex), label, model ("RFOne"), rates (IQ, Hz)}]. One not
/// opened yet in this run is opened for a moment to ask, once; one streaming for a source
/// is shown as it said when it opened.
pub fn devices() -> Vec<serde_json::Value> {
    let Ok(a) = api() else { return vec![] };
    serials()
        .into_iter()
        .map(|s| {
            // (Opened here and closed when dropped: nothing else uses the handle.)
            // SAFETY: an open device, as opened (IQ).
            let caps = super::known(&key(s)).or_else(|| open(a, &format!("{s:016X}")).ok().and_then(|d| unsafe { identify(a, d.dev) }.ok().map(|(c, _)| c)));
            describe(s, caps.as_ref())
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct HydrasdrConfig {
    /// Hex serial; "" = the first.
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    /// A [`Gain::Preset`], or [`Gain::Stages`] (LNA 0–14, mixer 0–15, VGA 0–15 steps; with `agc`, the VGA alone).
    pub gain: Gain,
    pub bias_tee: bool,
    /// 12-bit packing on the USB.
    pub packing: bool,
    /// The input: 0 ANT, 1 CABLE1, 2 CABLE2.
    pub rf_port: u8,
    pub ppm: f64,
}

impl crate::sdr::Radio for HydrasdrConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if matches!(g, Gain::Stages(_) | Gain::Preset(..)) {
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

/// What libhydrasdr's streaming thread hands samples to.
struct Ctx<'a> {
    sink: Mutex<&'a mut Sink>,
    stop: Arc<AtomicBool>,
    closed: AtomicBool,
    /// The library's count of dropped samples at the last buffer (it counts from the start).
    dropped: AtomicU64,
}

/// The samples dropped since the last buffer, from the library's running count.
fn dropped_since(last: &AtomicU64, total: u64) -> u64 {
    total.saturating_sub(last.swap(total, Ordering::Relaxed))
}

extern "C" fn on_samples(t: *mut Transfer) -> c_int {
    // SAFETY: libhydrasdr passes a valid transfer whose ctx is our Ctx, alive
    // until hydrasdr_stop_rx returns.
    let (t, ctx) = unsafe { (&*t, &*((*t).ctx as *const Ctx)) };
    if ctx.stop.load(Ordering::Relaxed) || ctx.closed.load(Ordering::Relaxed) {
        return 1; // stop streaming
    }
    let dropped = dropped_since(&ctx.dropped, t.dropped_samples);
    if t.sample_count <= 0 || t.sample_type != SAMPLE_FLOAT32_IQ {
        return 0;
    }
    // SAFETY: sample_count complex floats.
    let s = unsafe { std::slice::from_raw_parts(t.samples as *const Complex32, t.sample_count as usize) };
    if !ctx.sink.lock().unwrap().iq(s, dropped, Instant::now()) {
        ctx.closed.store(true, Ordering::Relaxed);
        return 1;
    }
    0
}

/// An open HydraSDR: closed (and stopped) when dropped.
struct Device {
    a: &'static Api,
    dev: Dev,
    streaming: bool,
    ppm: f64,
}

// SAFETY: libhydrasdr's handle is used from one thread at a time ([`super::open`]'s hands it over).
unsafe impl Send for Device {}

impl Tunable for Device {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        // SAFETY: an open device.
        check(self.a, "tune", unsafe { (self.a.set_freq)(self.dev, ppm_corrected(hz, self.ppm).round() as u64) })?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// Its own settings, which libhydrasdr takes while streaming (a survey's
    /// dB is left alone: the gain stays as set).
    fn set_gain(&mut self, g: Gain) -> Result<u64, String> {
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

/// Set the gain as `g` says: a table's step (which turns the AGCs off), or the
/// stages (a survey's dB is left alone: the gain stays as set).
///
/// SAFETY (callers): `dev` is open.
unsafe fn set_gain(a: &Api, dev: Dev, g: Gain) -> Result<(), String> {
    let step = |s: &Stages, stage: Stage, most: u8| s.get(stage).unwrap_or(0.0).clamp(0.0, most as f32) as u8;
    let set = |what: &str, ty: c_int, v: u8| check(a, what, unsafe { (a.set_gain)(dev, ty, v) });
    match g {
        Gain::Preset(Preset::Linearity, g) => set("gain", GAIN_LINEARITY, g.min(21)),
        Gain::Preset(Preset::Sensitivity, g) => set("gain", GAIN_SENSITIVITY, g.min(21)),
        Gain::Stages(s) if s.agc => {
            set("LNA AGC", GAIN_LNA_AGC, 1)?;
            set("mixer AGC", GAIN_MIXER_AGC, 1)?;
            set("VGA gain", GAIN_VGA, step(&s, Stage::Vga, 15))
        }
        Gain::Stages(s) => {
            set("LNA AGC off", GAIN_LNA_AGC, 0)?;
            set("mixer AGC off", GAIN_MIXER_AGC, 0)?;
            set("LNA gain", GAIN_LNA, step(&s, Stage::Lna, 14))?;
            set("mixer gain", GAIN_MIXER, step(&s, Stage::Mixer, 15))?;
            set("VGA gain", GAIN_VGA, step(&s, Stage::Vga, 15))
        }
        Gain::Agc | Gain::Db(_) => Ok(()),
    }
}

/// Why a source can't use input `port` with the bias-T as asked, if it can't: a port the
/// radio hasn't, or the bias-T on one without it (it would power the ANT port's coax).
fn port_refusal(what: &str, ports: &[String], bias_ports: &[bool], port: u8, bias_tee: bool) -> Option<String> {
    let names = ports.join(" / ");
    match ports.get(port as usize) {
        None => Some(format!("this {what} has no input {port} (it has {names}): pick one in Setup → Radios")),
        Some(p) if bias_tee && !bias_ports.get(port as usize).copied().unwrap_or(false) => {
            let on: Vec<&str> = ports.iter().zip(bias_ports).filter(|(_, b)| **b).map(|(n, _)| n.as_str()).collect();
            Some(format!("this {what}'s bias-T is on {} only, not {p}: turn the bias-T off, or use {}", on.join(" / "), on.first().unwrap_or(&"its antenna port")))
        }
        Some(_) => None,
    }
}

fn stream_once(cfg: &HydrasdrConfig, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    // SAFETY: libhydrasdr calls on the device this function opens.
    unsafe {
        let mut d = open(a, &cfg.serial)?;
        d.ppm = cfg.ppm;
        // (In the order libhydrasdr asks for: its device info, the frequency, the rate, the sample type.)
        let (caps, info) = identify(a, d.dev)?;
        let what = name(&caps.model);
        if !caps.rates.allows(cfg.rate_hz) {
            return Err(super::rate_refusal(&what, &caps.rates, cfg.rate_hz));
        }
        let ports = info.ports();
        let bias_ports: Vec<bool> = info.rf_port_info[..ports.len()].iter().map(|p| p.has_bias_tee == 1 && info.features & CAP_BIAS_TEE != 0).collect();
        if let Some(e) = port_refusal(&what, &ports, &bias_ports, cfg.rf_port, cfg.bias_tee) {
            return Err(e);
        }
        d.tune(cfg.center_hz)?;
        check(a, "sample rate", (a.set_samplerate)(d.dev, cfg.rate_hz.round() as u32))?;
        check(a, "sample type", (a.set_sample_type)(d.dev, SAMPLE_FLOAT32_IQ))?;
        // (As a gain set while running is: a reopened radio comes back the same.)
        d.set_gain(cfg.gain)?;
        if info.features & CAP_RF_PORT_SELECT != 0 {
            check(a, "input", (a.set_rf_port)(d.dev, cfg.rf_port as c_int))?;
        }
        if info.features & CAP_BIAS_TEE != 0 {
            check(a, "bias-T", (a.set_rf_bias)(d.dev, cfg.bias_tee as u8))?;
        }
        if info.features & CAP_PACKING != 0 {
            check(a, "packing", (a.set_packing)(d.dev, cfg.packing as u8))?;
        }
        let ctx = Box::new(Ctx { sink: Mutex::new(sink), stop: stop.clone(), closed: AtomicBool::new(false), dropped: AtomicU64::new(0) });
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
                break Err("HydraSDR stopped streaming (unplugged?)".to_string());
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
    fn drops_are_counted_per_buffer() {
        let last = AtomicU64::new(0);
        assert_eq!(dropped_since(&last, 0), 0);
        assert_eq!(dropped_since(&last, 65_536), 65_536);
        assert_eq!(dropped_since(&last, 65_536), 0, "the same total again: none new");
        assert_eq!(dropped_since(&last, 196_608), 131_072);
    }

    #[test]
    fn device_json_carries_model_and_rates() {
        let rfone = Caps { model: "RFOne".into(), rates: Rates::List(vec![10_000_000, 5_000_000, 2_500_000]) };
        let d = describe(0x1A2B3C4D5E6F7081, Some(&rfone));
        assert_eq!(d["serial"], "1A2B3C4D5E6F7081");
        assert_eq!(d["label"], "HydraSDR RFOne · SN 1A2B3C4D5E6F7081");
        assert_eq!(d["model"], "RFOne");
        assert_eq!(d["rates"], serde_json::json!([10_000_000, 5_000_000, 2_500_000]));
        let d = describe(1, None);
        assert_eq!(d["label"], "HydraSDR · SN 0000000000000001");
        assert!(d.get("model").is_none() && d.get("rates").is_none());
    }

    #[test]
    fn the_bias_tee_only_on_its_port() {
        let ports = ["ANT", "CABLE1", "CABLE2"].map(String::from);
        let bias = [true, false, false];
        assert_eq!(port_refusal("HydraSDR RFOne", &ports, &bias, 0, true), None);
        assert_eq!(port_refusal("HydraSDR RFOne", &ports, &bias, 2, false), None);
        assert_eq!(
            port_refusal("HydraSDR RFOne", &ports, &bias, 1, true).as_deref(),
            Some("this HydraSDR RFOne's bias-T is on ANT only, not CABLE1: turn the bias-T off, or use ANT")
        );
        assert_eq!(
            port_refusal("HydraSDR RFOne", &ports, &bias, 3, false).as_deref(),
            Some("this HydraSDR RFOne has no input 3 (it has ANT / CABLE1 / CABLE2): pick one in Setup → Radios")
        );
    }

    /// On a real RFOne (`TRP_HYDRASDR_SERIAL`, else the first): each rate streams at that rate,
    /// with something in it, and the bias-T on a cable input is refused before it opens.
    /// `cargo test -p trunk-pro streams_from_an_rfone -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn streams_from_an_rfone() {
        use crate::sdr::{Block, SourceMsg};
        let serial = std::env::var("TRP_HYDRASDR_SERIAL").unwrap_or_default();
        let run = |rate_hz: f64, rf_port: u8, bias_tee: bool| {
            let cfg = HydrasdrConfig { serial: serial.clone(), center_hz: 852e6, rate_hz, gain: Gain::Preset(Preset::Linearity, 14), bias_tee, packing: true, rf_port, ppm: 0.0 };
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
                    Ok(SourceMsg::Samples { block: Block::Iq(b), dropped: d, at, .. }) => {
                        // (The first second is the start: counted from after it.)
                        let t0 = *first.get_or_insert(at);
                        if at.duration_since(t0) > Duration::from_secs(1) {
                            n += b.len() as u64;
                            dropped += d;
                            power += b.iter().map(|c| c.norm_sqr() as f64).sum::<f64>();
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
        for rate in [10e6, 5e6, 2.5e6, 1.25e6] {
            let (got, dropped, power, error) = run(rate, 0, false);
            println!("{} MSPS: {:.3} MSPS, {dropped} dropped, mean power {power:.2e}, error {error:?}", rate / 1e6, got / 1e6);
            assert!(error.is_none(), "{error:?}");
            assert!((got / rate - 1.0).abs() < 0.05, "{got} for {rate}");
            assert!(power > 1e-9 && power < 1.0, "{power}");
        }
        let (_, _, _, cable_ok) = run(2.5e6, 1, false);
        assert!(cable_ok.is_none(), "{cable_ok:?}");
        let (_, _, _, refused) = run(2.5e6, 1, true);
        assert!(refused.as_deref().is_some_and(|e| e.contains("bias-T is on ANT only")), "{refused:?}");
    }

    #[test]
    fn model_from_board_name() {
        let mut info = DeviceInfo::zeroed();
        for (i, b) in b"HydraSDR RFOne".iter().enumerate() {
            info.board_name[i] = *b as c_char;
        }
        assert_eq!(info.model(), "RFOne");
    }
}
