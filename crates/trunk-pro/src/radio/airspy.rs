//! Airspy R2 / Mini through libairspy, and HydraSDR RFOne through libhydrasdr 1.1 or newer:
//! one driver, as libhydrasdr is libairspy's fork (the same calls, named hydrasdr_…) for a
//! radio with the R2's tuner and gain stages ([`Lib`]; what only libhydrasdr has is in
//! [`hydrasdr`]). Each library is loaded at run time, and needs installing:
//! - libairspy: `brew install airspy`, `apt install libairspy0`, or airspy.dll (from the
//!   airspy-tools release) next to trunk-pro.exe / on PATH on Windows. It hands over real
//!   samples, turned into IQ (centred on the tuned frequency, at the chosen rate) on the
//!   source's decoding thread; its own conversion is kept behind
//!   `TRUNK_PRO_AIRSPY_LIBRARY_IQ=1` ([`library_iq`]).
//! - libhydrasdr: hydrasdr-host from https://github.com/hydrasdr/hydrasdr-host
//!   (`libhydrasdr.so.1`; on Windows hydrasdr.dll next to trunk-pro.exe / on PATH). It turns
//!   samples into IQ itself, on a thread of its own apart from the one draining the USB (and
//!   decimates below 2.5 MSPS), so it hands over float IQ. An RFOne has three inputs (ANT,
//!   with the bias-T, and CABLE1 / CABLE2).

mod hydrasdr;

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use libloading::Library;
use num_complex::Complex32;

use super::{ppm_corrected, Caps, Deadline, DriverInfo, Rates};
use crate::sdr::{Applied, Control, Gain, Preset, Retuned, Sink, Stage, Stages, Tunable};

/// The library a source's radio is driven through.
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
    /// "airspy" / "hydrasdr": its functions' prefix, its source `type` and its radios' key among those [`super::known`].
    pub fn id(self) -> &'static str {
        match self {
            Lib::Airspy => "airspy",
            Lib::Hydrasdr => "hydrasdr",
        }
    }
}

type Dev = *mut c_void;

/// airspy_transfer_t / hydrasdr_transfer_t.
#[repr(C)]
struct Transfer {
    device: Dev,
    ctx: *mut c_void,
    samples: *mut c_void,
    sample_count: c_int,
    /// libairspy's: dropped just before this buffer; libhydrasdr's: since streaming started.
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
type SetU8 = unsafe extern "C" fn(Dev, u8) -> c_int;

/// The calls both libraries have, and [`Own`] those they make differently.
struct Api {
    lib: Lib,
    _lib: Library,
    lib_version: unsafe extern "C" fn(*mut LibVersion),
    list_devices: unsafe extern "C" fn(*mut u64, c_int) -> c_int,
    open_sn: unsafe extern "C" fn(*mut Dev, u64) -> c_int,
    open: unsafe extern "C" fn(*mut Dev) -> c_int,
    close: unsafe extern "C" fn(Dev) -> c_int,
    get_samplerates: unsafe extern "C" fn(Dev, *mut u32, u32) -> c_int,
    set_samplerate: unsafe extern "C" fn(Dev, u32) -> c_int,
    set_sample_type: unsafe extern "C" fn(Dev, c_int) -> c_int,
    set_packing: SetU8,
    set_rf_bias: SetU8,
    start_rx: unsafe extern "C" fn(Dev, Callback, *mut c_void) -> c_int,
    stop_rx: unsafe extern "C" fn(Dev) -> c_int,
    is_streaming: unsafe extern "C" fn(Dev) -> c_int,
    error_name: unsafe extern "C" fn(c_int) -> *const c_char,
    own: Own,
}

/// What the two libraries do differently.
enum Own {
    /// A function per gain setting, a 32-bit frequency, the model in the firmware's string.
    Airspy {
        version_string_read: unsafe extern "C" fn(Dev, *mut c_char, u8) -> c_int,
        board_partid_serialno_read: unsafe extern "C" fn(Dev, *mut SerialRead) -> c_int,
        set_freq: unsafe extern "C" fn(Dev, u32) -> c_int,
        set_linearity_gain: SetU8,
        set_sensitivity_gain: SetU8,
        set_lna_gain: SetU8,
        set_mixer_gain: SetU8,
        set_vga_gain: SetU8,
        set_lna_agc: SetU8,
        set_mixer_agc: SetU8,
    },
    /// One gain function taking the setting's type, a 64-bit frequency, a device-info block, an input to pick.
    Hydrasdr {
        get_device_info: unsafe extern "C" fn(Dev, *mut hydrasdr::DeviceInfo) -> c_int,
        set_freq: unsafe extern "C" fn(Dev, u64) -> c_int,
        set_gain: unsafe extern "C" fn(Dev, c_int, u8) -> c_int,
        set_rf_port: unsafe extern "C" fn(Dev, c_int) -> c_int,
    },
}

// SAFETY: plain C entry points; each device is used from one thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api(lib: Lib) -> Result<&'static Api, String> {
    static AIRSPY: OnceLock<Result<Api, String>> = OnceLock::new();
    static HYDRASDR: OnceLock<Result<Api, String>> = OnceLock::new();
    let api = match lib {
        Lib::Airspy => &AIRSPY,
        Lib::Hydrasdr => &HYDRASDR,
    };
    api.get_or_init(|| load(lib)).as_ref().map_err(|e| e.clone())
}

fn load(lib: Lib) -> Result<Api, String> {
    let (env, mut names, needs) = match lib {
        Lib::Airspy => ("TRUNK_PRO_AIRSPY", super::candidates("libairspy.so.0", "libairspy.dylib", &["airspy.dll", "libairspy.dll"]), ""),
        Lib::Hydrasdr => ("TRUNK_PRO_HYDRASDR", super::candidates("libhydrasdr.so.1", "libhydrasdr.1.dylib", &["hydrasdr.dll", "libhydrasdr.dll"]), " (libhydrasdr 1.1 or newer is needed)"),
    };
    let id = lib.id();
    if !cfg!(target_os = "macos") && !cfg!(windows) {
        names.push(format!("lib{id}.so"));
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let (l, path) = super::load(env, &names, id, false)?;
    // SAFETY: the signatures are airspy.h's (1.0.x) and hydrasdr.h's (1.1.x): the same, but for those in `Own`.
    unsafe {
        macro_rules! f {
            ($n:literal) => {
                *l.get(format!("{id}_{}\0", $n).as_bytes()).map_err(|e| format!("{path}: {id}_{} missing{needs}: {e}", $n))?
            };
        }
        let own = match lib {
            Lib::Airspy => Own::Airspy {
                version_string_read: f!("version_string_read"),
                board_partid_serialno_read: f!("board_partid_serialno_read"),
                set_freq: f!("set_freq"),
                set_linearity_gain: f!("set_linearity_gain"),
                set_sensitivity_gain: f!("set_sensitivity_gain"),
                set_lna_gain: f!("set_lna_gain"),
                set_mixer_gain: f!("set_mixer_gain"),
                set_vga_gain: f!("set_vga_gain"),
                set_lna_agc: f!("set_lna_agc"),
                set_mixer_agc: f!("set_mixer_agc"),
            },
            Lib::Hydrasdr => Own::Hydrasdr { get_device_info: f!("get_device_info"), set_freq: f!("set_freq"), set_gain: f!("set_gain"), set_rf_port: f!("set_rf_port") },
        };
        Ok(Api {
            lib,
            lib_version: f!("lib_version"),
            list_devices: f!("list_devices"),
            open_sn: f!("open_sn"),
            open: f!("open"),
            close: f!("close"),
            get_samplerates: f!("get_samplerates"),
            set_samplerate: f!("set_samplerate"),
            set_sample_type: f!("set_sample_type"),
            set_packing: f!("set_packing"),
            set_rf_bias: f!("set_rf_bias"),
            start_rx: f!("start_rx"),
            stop_rx: f!("stop_rx"),
            is_streaming: f!("is_streaming"),
            error_name: f!("error_name"),
            own,
            _lib: l,
        })
    }
}

pub fn info(lib: Lib) -> DriverInfo {
    let name = format!("lib{}", lib.id());
    match api(lib) {
        Ok(a) => {
            let mut v = LibVersion::default();
            // SAFETY: fills the struct.
            unsafe { (a.lib_version)(&mut v) };
            DriverInfo { loaded: true, detail: format!("{name} {}.{}.{}", v.major, v.minor, v.revision) }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("{name} {e}") },
    }
}

fn check(a: &Api, what: &str, r: c_int) -> Result<(), String> {
    if r == 0 {
        return Ok(());
    }
    // SAFETY: returns a static string.
    let name = unsafe { CStr::from_ptr((a.error_name)(r)) }.to_string_lossy().into_owned();
    Err(format!("{} {what}: {name}", a.lib.brand()))
}

/// Open the radio `serial` (hex; "" = the first), with a deadline, one
/// radio at a time ([`super::open`]).
fn open(a: &'static Api, serial: &str) -> Result<Device, String> {
    let brand = a.lib.brand();
    let name = format!("{brand} {}", if serial.is_empty() { "(first)" } else { serial });
    let serial = serial.to_string();
    super::open(&name, Deadline::Quick, true, move || {
        let mut dev: Dev = std::ptr::null_mut();
        // SAFETY: the library fills `dev`; the Device closes it.
        unsafe {
            if serial.is_empty() {
                check(a, "open", (a.open)(&mut dev))?;
            } else {
                let sn = u64::from_str_radix(serial.trim_start_matches("0x"), 16).map_err(|_| format!("{brand} serial {serial:?} isn't hex"))?;
                check(a, "open", (a.open_sn)(&mut dev, sn))?;
            }
        }
        Ok(Device { a, dev, streaming: false, ppm: 0.0, off_ant: false })
    })
}

/// What a radio can set besides its gain.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Can {
    /// Which input it hears (an RFOne's).
    input: bool,
    bias_tee: bool,
    /// 12-bit packing on the USB (an Airspy's firmware before v1.0.0-rc7 refuses it, and is told so).
    packing: bool,
}

/// What an open radio says it is.
struct Identity {
    caps: Caps,
    /// Its inputs, by number, and whether each has the bias-T; none when it has just the one.
    inputs: Vec<(String, bool)>,
    can: Can,
}

/// What an open radio says it is: its model and the IQ rates it lists (as opened, for IQ
/// samples), remembered by its serial for the list of radios. An Airspy's model is in its
/// firmware string ([`model_of`]: its board id can't tell an R2 from a Mini, both say
/// "AIRSPY"); a HydraSDR's in its device info.
///
/// SAFETY (callers): `dev` is open, its sample type as opened (IQ).
unsafe fn identify(a: &Api, dev: Dev) -> Result<Identity, String> {
    unsafe {
        let mut count = 0u32;
        (a.get_samplerates)(dev, &mut count, 0);
        let mut rates = vec![0u32; count.min(32) as usize];
        if !rates.is_empty() && (a.get_samplerates)(dev, rates.as_mut_ptr(), rates.len() as u32) != 0 {
            rates.clear();
        }
        let (model, serial, inputs, can) = match &a.own {
            Own::Airspy { version_string_read, board_partid_serialno_read, .. } => {
                let mut v = [0 as c_char; 128];
                let version = if version_string_read(dev, v.as_mut_ptr(), v.len() as u8) == 0 { super::cstr(v.as_ptr()) } else { String::new() };
                let mut sn = SerialRead::default();
                let serial = (board_partid_serialno_read(dev, &mut sn) == 0).then(|| sn.serial());
                (model_of(&version, &rates).to_string(), serial, vec![], Can { input: false, bias_tee: true, packing: true })
            }
            Own::Hydrasdr { get_device_info, .. } => {
                let mut info = hydrasdr::DeviceInfo::zeroed();
                check(a, "device info", get_device_info(dev, &mut *info))?;
                (info.model(), Some(info.part_serial.serial()), info.inputs(), info.can())
            }
        };
        let caps = Caps { model, rates: Rates::List(rates) };
        if let Some(sn) = serial {
            super::remember(&key(a.lib, sn), caps.clone());
        }
        Ok(Identity { caps, inputs, can })
    }
}

/// airspy_read_partid_serialno_t / hydrasdr_read_partid_serialno_t.
#[repr(C)]
#[derive(Clone, Copy, Default)]
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
fn key(lib: Lib, serial: u64) -> String {
    format!("{}:{serial:016X}", lib.id())
}

/// "R2" or "Mini", from the firmware string, else from the rates (an R2 does 2.5 MSPS,
/// a Mini 6); "" when neither says. A HydraSDR RFOne (which libairspy opens as "the
/// first" when it comes before an Airspy, and does 2.5 MSPS too) is "HydraSDR RFOne".
fn model_of(version: &str, rates_iq: &[u32]) -> &'static str {
    let v = version.to_ascii_uppercase();
    if v.contains("HYDRASDR") {
        "HydraSDR RFOne"
    } else if v.contains("MINI") {
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
/// (An RFOne libairspy opens is named so already: [`model_of`].)
fn name(lib: Lib, model: &str) -> String {
    match model {
        "" => lib.brand().to_string(),
        m if m.starts_with(Lib::Hydrasdr.brand()) => m.to_string(),
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
/// One not opened yet in this run is opened for a moment to ask (as airspy_info does), once.
/// One streaming for a source is shown as it said when it opened — or isn't listed at all: on
/// Windows libairspy's own listing can't open it to read its serial. (libairspy doesn't list
/// a HydraSDR RFOne, its USB serial string isn't an Airspy's, but may still open one as "the first".)
pub fn devices(lib: Lib) -> Vec<serde_json::Value> {
    let Ok(a) = api(lib) else { return vec![] };
    let found = super::open(&format!("{} search", lib.brand()), Deadline::Quick, true, move || {
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
            let caps = super::known(&key(lib, s)).or_else(|| open(a, &format!("{s:016X}")).ok().and_then(|d| unsafe { identify(a, d.dev) }.ok().map(|i| i.caps)));
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
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// What the library's streaming thread hands samples to.
struct Ctx<'a> {
    sink: Mutex<&'a mut Sink>,
    stop: Arc<AtomicBool>,
    closed: AtomicBool,
    /// The library counts dropped samples from the start (libhydrasdr), not per buffer (libairspy).
    running_total: bool,
    /// That count at the last buffer.
    dropped: AtomicU64,
}

/// The samples dropped since the last buffer, from the library's running count.
fn dropped_since(last: &AtomicU64, total: u64) -> u64 {
    total.saturating_sub(last.swap(total, Ordering::Relaxed))
}

extern "C" fn on_samples(t: *mut Transfer) -> c_int {
    // SAFETY: the library passes a valid transfer whose ctx is our Ctx, alive
    // until its stop_rx returns.
    let (t, ctx) = unsafe { (&*t, &*((*t).ctx as *const Ctx)) };
    if ctx.stop.load(Ordering::Relaxed) || ctx.closed.load(Ordering::Relaxed) {
        return 1; // stop streaming
    }
    let dropped = if ctx.running_total { dropped_since(&ctx.dropped, t.dropped_samples) } else { t.dropped_samples };
    if t.sample_count <= 0 {
        return 0;
    }
    let mut sink = ctx.sink.lock().unwrap();
    let open = match t.sample_type {
        SAMPLE_INT16_REAL => {
            // SAFETY: sample_count 16-bit real samples.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const i16, t.sample_count as usize) };
            sink.real16(s, dropped / 2, Instant::now())
        }
        SAMPLE_FLOAT32_IQ => {
            // SAFETY: sample_count complex floats.
            let s = unsafe { std::slice::from_raw_parts(t.samples as *const Complex32, t.sample_count as usize) };
            sink.iq(s, dropped, Instant::now())
        }
        _ => return 0,
    };
    if !open {
        ctx.closed.store(true, Ordering::Relaxed);
        return 1;
    }
    0
}

/// An open radio: closed (and stopped) when dropped.
struct Device {
    a: &'static Api,
    dev: Dev,
    streaming: bool,
    ppm: f64,
    /// It was set to an input other than ANT: put back as it closes. The RFOne keeps its
    /// input after it's closed, and a program that doesn't pick one would hear CABLE1.
    off_ant: bool,
}

// SAFETY: the library's handle is used from one thread at a time ([`super::open`]'s hands it over).
unsafe impl Send for Device {}

impl Tunable for Device {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        let hz_set = ppm_corrected(hz, self.ppm).round();
        // SAFETY: an open device.
        let r = unsafe {
            match &self.a.own {
                Own::Airspy { set_freq, .. } => set_freq(self.dev, hz_set as u32),
                Own::Hydrasdr { set_freq, .. } => set_freq(self.dev, hz_set as u64),
            }
        };
        check(self.a, "tune", r)?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// Its own settings, which the library takes while streaming (a survey's
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
            if let (true, Own::Hydrasdr { set_rf_port, .. }) = (self.off_ant, &self.a.own)
                && set_rf_port(self.dev, 0) != 0
            {
                log::warn!("{}: couldn't put its input back to ANT as it closed", self.a.lib.brand());
            }
            (self.a.close)(self.dev);
        }
    }
}

/// One of the tuner's gain settings, as both libraries take them.
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

/// What `g` sets, in order: a table's step (which turns the AGCs off), or the stages
/// (a survey's dB is left alone: the gain stays as set).
fn settings(g: Gain) -> Vec<(Setting, u8)> {
    let step = |s: &Stages, stage: Stage, most: u8| s.get(stage).unwrap_or(0.0).clamp(0.0, most as f32) as u8;
    match g {
        Gain::Preset(Preset::Linearity, g) => vec![(Setting::Linearity, g.min(21))],
        Gain::Preset(Preset::Sensitivity, g) => vec![(Setting::Sensitivity, g.min(21))],
        Gain::Stages(s) if s.agc => vec![(Setting::LnaAgc, 1), (Setting::MixerAgc, 1), (Setting::Vga, step(&s, Stage::Vga, 15))],
        Gain::Stages(s) => vec![
            (Setting::LnaAgc, 0),
            (Setting::MixerAgc, 0),
            (Setting::Lna, step(&s, Stage::Lna, 14)),
            (Setting::Mixer, step(&s, Stage::Mixer, 15)),
            (Setting::Vga, step(&s, Stage::Vga, 15)),
        ],
        Gain::Agc | Gain::Db(_) => vec![],
    }
}

/// Set the gain as `g` says ([`settings`]).
///
/// SAFETY (callers): `dev` is open.
unsafe fn set_gain(a: &Api, dev: Dev, g: Gain) -> Result<(), String> {
    for (s, v) in settings(g) {
        let r = unsafe {
            match &a.own {
                Own::Airspy { set_linearity_gain, set_sensitivity_gain, set_lna_gain, set_mixer_gain, set_vga_gain, set_lna_agc, set_mixer_agc, .. } => {
                    let set = match s {
                        Setting::Linearity => set_linearity_gain,
                        Setting::Sensitivity => set_sensitivity_gain,
                        Setting::LnaAgc => set_lna_agc,
                        Setting::MixerAgc => set_mixer_agc,
                        Setting::Lna => set_lna_gain,
                        Setting::Mixer => set_mixer_gain,
                        Setting::Vga => set_vga_gain,
                    };
                    set(dev, v)
                }
                Own::Hydrasdr { set_gain, .. } => set_gain(dev, hydrasdr::gain_type(s), v),
            }
        };
        check(a, s.what(v), r)?;
    }
    Ok(())
}

/// Why a source can't use input `port` with the bias-T as asked, if it can't: an input the
/// radio hasn't, or the bias-T on one without it (it would power the ANT input's coax).
fn input_refusal(what: &str, inputs: &[(String, bool)], port: u8, bias_tee: bool) -> Option<String> {
    let names: Vec<&str> = inputs.iter().map(|(n, _)| n.as_str()).collect();
    match inputs.get(port as usize) {
        None => Some(format!("this {what} has no input {port} (it has {}): pick one in Setup → Radios", names.join(" / "))),
        Some((p, false)) if bias_tee => {
            let on: Vec<&str> = inputs.iter().filter(|(_, b)| *b).map(|(n, _)| n.as_str()).collect();
            Some(format!("this {what}'s bias-T is on {} only, not {p}: turn the bias-T off, or use {}", on.join(" / "), on.first().unwrap_or(&"its antenna input")))
        }
        Some(_) => None,
    }
}

fn stream_once(cfg: &AirspyConfig, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
    let a = api(cfg.lib)?;
    // SAFETY: the library calls on the device this function opens.
    unsafe {
        let mut d = open(a, &cfg.serial)?;
        d.ppm = cfg.ppm;
        // (Asked as opened, for IQ samples: for real samples libairspy lists rates doubled.)
        let id = identify(a, d.dev)?;
        let what = name(a.lib, &id.caps.model);
        if !id.caps.rates.allows(cfg.rate_hz) {
            return Err(super::rate_refusal(&what, &id.caps.rates, cfg.rate_hz));
        }
        if !id.inputs.is_empty()
            && let Some(e) = input_refusal(&what, &id.inputs, cfg.rf_port, cfg.bias_tee)
        {
            return Err(e);
        }
        let sample_type = if a.lib == Lib::Airspy && !library_iq() { SAMPLE_INT16_REAL } else { SAMPLE_FLOAT32_IQ };
        let dev = d.dev;
        let set_type = || check(a, "sample type", (a.set_sample_type)(dev, sample_type));
        // By the IQ rate even for real samples: libairspy looks rates up in its own
        // (IQ) list and sends the one found as its index, which every firmware takes.
        // A doubled rate isn't in that list (libairspy would send it by value, which
        // firmware before v1.0.0-rc7 refuses), or worse, is: a Mini's 3 MSPS doubled
        // is 6, its first rate, and it would run at twice the rate asked for.
        let set_rate = || check(a, "sample rate", (a.set_samplerate)(dev, cfg.rate_hz.round() as u32));
        match a.lib {
            Lib::Airspy => {
                set_type()?;
                set_rate()?;
                d.tune(cfg.center_hz)?;
            }
            // (In the order libhydrasdr asks for: the frequency, the rate, the sample type.)
            Lib::Hydrasdr => {
                d.tune(cfg.center_hz)?;
                set_rate()?;
                set_type()?;
            }
        }
        // (As a gain set while running is: a reopened radio comes back the same.)
        d.set_gain(cfg.gain)?;
        if let (true, Own::Hydrasdr { set_rf_port, .. }) = (id.can.input, &a.own) {
            d.off_ant = cfg.rf_port != 0;
            check(a, "input", set_rf_port(d.dev, cfg.rf_port as c_int))?;
        }
        if id.can.bias_tee {
            check(a, "bias-T", (a.set_rf_bias)(d.dev, cfg.bias_tee as u8))?;
        }
        // (Older firmware can't pack: it streams unpacked then, as it always did.)
        if id.can.packing && (a.set_packing)(d.dev, cfg.packing as u8) != 0 && cfg.packing {
            log::warn!("{what} {}: this firmware can't pack samples; streaming them unpacked (a third more USB bandwidth)", if cfg.serial.is_empty() { "(first)" } else { &cfg.serial });
        }
        let ctx = Box::new(Ctx { sink: Mutex::new(sink), stop: stop.clone(), closed: AtomicBool::new(false), running_total: a.lib == Lib::Hydrasdr, dropped: AtomicU64::new(0) });
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
                break Err(format!("{} stopped streaming (unplugged?)", a.lib.brand()));
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
        // An RFOne, through libairspy: its own name, not an R2's.
        assert_eq!(model_of("HydraSDR RFOne v1.0.0-rc2 2024-09-15", &[10_000_000, 5_000_000, 2_500_000]), "HydraSDR RFOne");
        assert_eq!(name(Lib::Airspy, "HydraSDR RFOne"), "HydraSDR RFOne");
        assert_eq!(name(Lib::Airspy, "R2"), "Airspy R2");
        assert_eq!(name(Lib::Hydrasdr, "RFOne"), "HydraSDR RFOne");
    }

    #[test]
    fn device_json_carries_model_and_rates() {
        let r2 = Caps { model: "R2".into(), rates: Rates::List(vec![10_000_000, 2_500_000]) };
        let d = describe(Lib::Airspy, 0x735863DC2F650AE3, Some(&r2));
        assert_eq!(d["serial"], "735863DC2F650AE3");
        assert_eq!(d["label"], "Airspy R2 · SN 735863DC2F650AE3");
        assert_eq!(d["model"], "R2");
        assert_eq!(d["rates"], serde_json::json!([10_000_000, 2_500_000]));
        let rfone = Caps { model: "RFOne".into(), rates: Rates::List(vec![10_000_000, 5_000_000, 2_500_000]) };
        assert_eq!(describe(Lib::Hydrasdr, 0x1A2B3C4D5E6F7081, Some(&rfone))["label"], "HydraSDR RFOne · SN 1A2B3C4D5E6F7081");
        // One not asked yet that couldn't be opened (another program has it): by serial only.
        let d = describe(Lib::Airspy, 1, None);
        assert_eq!(d["label"], "Airspy · SN 0000000000000001");
        assert!(d.get("model").is_none() && d.get("rates").is_none());
        assert_eq!(describe(Lib::Hydrasdr, 1, None)["label"], "HydraSDR · SN 0000000000000001");
    }

    #[test]
    fn serial_as_airspy_info_prints_it() {
        let r = SerialRead { part_id: [0; 2], serial_no: [0, 0, 0x735863DC, 0x2F650AE3] };
        assert_eq!(key(Lib::Airspy, r.serial()), "airspy:735863DC2F650AE3");
        assert_eq!(key(Lib::Hydrasdr, r.serial()), "hydrasdr:735863DC2F650AE3");
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
        assert_eq!((LnaAgc.what(0), LnaAgc.what(1), Vga.what(7)), ("LNA AGC off", "LNA AGC", "VGA gain"));
        // libhydrasdr's types for them (hydrasdr_gain_type_t).
        assert_eq!([Lna, Mixer, Vga, Linearity, Sensitivity, LnaAgc, MixerAgc].map(hydrasdr::gain_type), [0, 2, 4, 5, 6, 7, 9]);
    }

    #[test]
    fn drops_are_counted_per_buffer() {
        let last = AtomicU64::new(0);
        assert_eq!(dropped_since(&last, 0), 0);
        assert_eq!(dropped_since(&last, 65_536), 65_536);
        assert_eq!(dropped_since(&last, 65_536), 0, "the same total again: none new");
        assert_eq!(dropped_since(&last, 196_608), 131_072);
    }

    #[test]
    fn the_bias_tee_only_on_its_input() {
        let inputs = [("ANT", true), ("CABLE1", false), ("CABLE2", false)].map(|(n, b)| (n.to_string(), b));
        assert_eq!(input_refusal("HydraSDR RFOne", &inputs, 0, true), None);
        assert_eq!(input_refusal("HydraSDR RFOne", &inputs, 2, false), None);
        assert_eq!(
            input_refusal("HydraSDR RFOne", &inputs, 1, true).as_deref(),
            Some("this HydraSDR RFOne's bias-T is on ANT only, not CABLE1: turn the bias-T off, or use ANT")
        );
        assert_eq!(
            input_refusal("HydraSDR RFOne", &inputs, 3, false).as_deref(),
            Some("this HydraSDR RFOne has no input 3 (it has ANT / CABLE1 / CABLE2): pick one in Setup → Radios")
        );
    }

    /// On a real RFOne (`TRP_HYDRASDR_SERIAL`, else the first): each rate streams at that rate,
    /// with something in it; CABLE1 streams (and is put back to ANT as it closes), and the bias-T on a cable input is refused.
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
}
