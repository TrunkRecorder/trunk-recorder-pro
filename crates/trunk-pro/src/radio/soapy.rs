//! Any SDR with a SoapySDR module — HackRF, LimeSDR, SDRplay, PlutoSDR,
//! BladeRF, Airspy HF+, network SDRs through SoapyRemote … — through
//! SoapySDR's C API (SoapySDR/Device.h), loaded at run time. Needs SoapySDR
//! and the device's module: `brew install soapysdr soapyhackrf`, `apt install
//! soapysdr0.8-module-hackrf` (or -module-all), PothosSDR / radioconda on
//! Windows. Works with SoapySDR 0.7 and 0.8, whose `setupStream` differ.
//! Samples arrive as CF32 (SoapySDR converts from the device's format).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_long, c_longlong, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use trunk_app::gain::ReportedStage;

use super::{cstr, ppm_corrected, Caps, Deadline, DriverInfo, Handed, Rates};
use crate::sdr::{Applied, Block, Control, Gain, Radio, Regained, Retuned, Sink, Tunable};

type Dev = *mut c_void;
type Stream = *mut c_void;

#[repr(C)]
struct Kwargs {
    size: usize,
    keys: *mut *mut c_char,
    vals: *mut *mut c_char,
}

const RX: c_int = 1;
const TIMEOUT: c_int = -1;
const CORRUPTION: c_int = -3;
const OVERFLOW: c_int = -4;
const TIME_ERROR: c_int = -6;
const HAS_TIME: c_int = 1 << 2;

/// 0.8 returns the stream; 0.7 fills an out-pointer and returns a status.
enum SetupStream {
    V08(unsafe extern "C" fn(Dev, c_int, *const c_char, *const usize, usize, *const Kwargs) -> Stream),
    V07(unsafe extern "C" fn(Dev, *mut Stream, c_int, *const c_char, *const usize, usize, *const Kwargs) -> c_int),
}

struct Api {
    _lib: Library,
    lib_version: unsafe extern "C" fn() -> *const c_char,
    abi_version: unsafe extern "C" fn() -> *const c_char,
    /// 0.8; strings the library allocates are leaked without it (0.7, rare paths only).
    free: Option<unsafe extern "C" fn(*mut c_void)>,
    strings_clear: unsafe extern "C" fn(*mut *mut *mut c_char, usize),
    kwargs_clear: unsafe extern "C" fn(*mut Kwargs),
    kwargs_list_clear: unsafe extern "C" fn(*mut Kwargs, usize),
    err_to_str: Option<unsafe extern "C" fn(c_int) -> *const c_char>,
    last_error: Option<unsafe extern "C" fn() -> *const c_char>,
    // The module API (0.8): without it, modules load on first use and can't be listed.
    list_modules: Option<unsafe extern "C" fn(*mut usize) -> *mut *mut c_char>,
    list_search_paths: Option<unsafe extern "C" fn(*mut usize) -> *mut *mut c_char>,
    load_module: Option<unsafe extern "C" fn(*const c_char) -> *mut c_char>,
    loader_result: Option<unsafe extern "C" fn(*const c_char) -> Kwargs>,
    module_version: Option<unsafe extern "C" fn(*const c_char) -> *mut c_char>,
    enumerate: unsafe extern "C" fn(*const c_char, *mut usize) -> *mut Kwargs,
    make: unsafe extern "C" fn(*const c_char) -> Dev,
    unmake: unsafe extern "C" fn(Dev) -> c_int,
    setup_stream: SetupStream,
    close_stream: unsafe extern "C" fn(Dev, Stream) -> c_int,
    stream_mtu: unsafe extern "C" fn(Dev, Stream) -> usize,
    activate: unsafe extern "C" fn(Dev, Stream, c_int, c_longlong, usize) -> c_int,
    deactivate: unsafe extern "C" fn(Dev, Stream, c_int, c_longlong) -> c_int,
    read_stream: unsafe extern "C" fn(Dev, Stream, *const *mut c_void, usize, *mut c_int, *mut c_longlong, c_long) -> c_int,
    set_sample_rate: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    get_sample_rate: unsafe extern "C" fn(Dev, c_int, usize) -> f64,
    list_sample_rates: unsafe extern "C" fn(Dev, c_int, usize, *mut usize) -> *mut f64,
    /// SoapySDR 0.6 and later: ranges as well as rates (a module that takes any rate in a span).
    get_sample_rate_range: Option<unsafe extern "C" fn(Dev, c_int, usize, *mut usize) -> *mut SoapyRange>,
    set_frequency: unsafe extern "C" fn(Dev, c_int, usize, f64, *const Kwargs) -> c_int,
    has_gain_mode: unsafe extern "C" fn(Dev, c_int, usize) -> bool,
    set_gain_mode: unsafe extern "C" fn(Dev, c_int, usize, bool) -> c_int,
    set_gain: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    set_gain_element: unsafe extern "C" fn(Dev, c_int, usize, *const c_char, f64) -> c_int,
    /// Its gain elements, by name, and each one's range (what the gain tuner searches).
    list_gains: Option<unsafe extern "C" fn(Dev, c_int, usize, *mut usize) -> *mut *mut c_char>,
    gain_element_range: Option<unsafe extern "C" fn(Dev, c_int, usize, *const c_char) -> SoapyRange>,
    /// The overall gain it's set to (its stages together).
    get_gain: Option<unsafe extern "C" fn(Dev, c_int, usize) -> f64>,
    set_antenna: unsafe extern "C" fn(Dev, c_int, usize, *const c_char) -> c_int,
    write_setting: unsafe extern "C" fn(Dev, *const c_char, *const c_char) -> c_int,
}

/// SoapySDRRange.
#[repr(C)]
struct SoapyRange {
    minimum: f64,
    maximum: f64,
    step: f64,
}

/// A module's sample-rate ranges ((min, max), Hz) as [`Rates`]: single rates when each is one.
fn rates_of(ranges: &[(f64, f64)]) -> Option<Rates> {
    if ranges.is_empty() {
        return None;
    }
    if ranges.iter().all(|(lo, hi)| (hi - lo).abs() < 1.0) {
        return Some(Rates::List(ranges.iter().map(|(lo, _)| lo.round() as u32).collect()));
    }
    let min = ranges.iter().map(|r| r.0).fold(f64::INFINITY, f64::min);
    let max = ranges.iter().map(|r| r.1).fold(0.0, f64::max);
    Some(Rates::Range { min, max })
}

/// Its key among the radios [`super::known`]: its device arguments, as a source names it.
fn key(args: &str) -> String {
    format!("soapy:{}", args.trim())
}

// SAFETY: plain C entry points; each device is used from one thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut names = super::candidates("libSoapySDR.so.0.8", "libSoapySDR.dylib", &["SoapySDR.dll", r"C:\Program Files\PothosSDR\bin\SoapySDR.dll"]);
        if !cfg!(target_os = "macos") && !cfg!(windows) {
            names.extend(["libSoapySDR.so.0.7".to_string(), "libSoapySDR.so".to_string()]);
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lib, path) = super::load("TRUNK_PRO_SOAPY", &names, "SoapySDR", true, "")?;
        // SAFETY: the signatures are SoapySDR's C API (0.7 / 0.8).
        unsafe {
            macro_rules! f {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{path}: {} missing: {e}", $n))?
                };
            }
            macro_rules! opt {
                ($n:literal) => {
                    lib.get(concat!($n, "\0").as_bytes()).ok().map(|s| *s)
                };
            }
            let abi_version: unsafe extern "C" fn() -> *const c_char = f!("SoapySDR_getABIVersion");
            let abi = CStr::from_ptr(abi_version()).to_string_lossy().into_owned();
            let setup_stream = if abi_at_least_08(&abi) { SetupStream::V08(f!("SoapySDRDevice_setupStream")) } else { SetupStream::V07(f!("SoapySDRDevice_setupStream")) };
            Ok(Api {
                lib_version: f!("SoapySDR_getLibVersion"),
                abi_version,
                free: opt!("SoapySDR_free"),
                strings_clear: f!("SoapySDRStrings_clear"),
                kwargs_clear: f!("SoapySDRKwargs_clear"),
                kwargs_list_clear: f!("SoapySDRKwargsList_clear"),
                err_to_str: opt!("SoapySDR_errToStr"),
                last_error: opt!("SoapySDRDevice_lastError"),
                list_modules: opt!("SoapySDR_listModules"),
                list_search_paths: opt!("SoapySDR_listSearchPaths"),
                load_module: opt!("SoapySDR_loadModule"),
                loader_result: opt!("SoapySDR_getLoaderResult"),
                module_version: opt!("SoapySDR_getModuleVersion"),
                enumerate: f!("SoapySDRDevice_enumerateStrArgs"),
                make: f!("SoapySDRDevice_makeStrArgs"),
                unmake: f!("SoapySDRDevice_unmake"),
                setup_stream,
                close_stream: f!("SoapySDRDevice_closeStream"),
                stream_mtu: f!("SoapySDRDevice_getStreamMTU"),
                activate: f!("SoapySDRDevice_activateStream"),
                deactivate: f!("SoapySDRDevice_deactivateStream"),
                read_stream: f!("SoapySDRDevice_readStream"),
                set_sample_rate: f!("SoapySDRDevice_setSampleRate"),
                get_sample_rate: f!("SoapySDRDevice_getSampleRate"),
                list_sample_rates: f!("SoapySDRDevice_listSampleRates"),
                get_sample_rate_range: opt!("SoapySDRDevice_getSampleRateRange"),
                set_frequency: f!("SoapySDRDevice_setFrequency"),
                has_gain_mode: f!("SoapySDRDevice_hasGainMode"),
                set_gain_mode: f!("SoapySDRDevice_setGainMode"),
                set_gain: f!("SoapySDRDevice_setGain"),
                set_gain_element: f!("SoapySDRDevice_setGainElement"),
                list_gains: opt!("SoapySDRDevice_listGains"),
                gain_element_range: opt!("SoapySDRDevice_getGainElementRange"),
                get_gain: opt!("SoapySDRDevice_getGain"),
                set_antenna: f!("SoapySDRDevice_setAntenna"),
                write_setting: f!("SoapySDRDevice_writeSetting"),
                _lib: lib,
            })
        }
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// "0.8", "0.8-3" → true; "0.7" → false.
fn abi_at_least_08(abi: &str) -> bool {
    let mut it = abi.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse::<u32>().ok());
    let (major, minor) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    major > 0 || minor >= 8
}

pub fn info() -> DriverInfo {
    match api() {
        Ok(a) => {
            // SAFETY: static strings.
            let (v, abi) = unsafe { (cstr((a.lib_version)()), cstr((a.abi_version)())) };
            DriverInfo { loaded: true, detail: format!("SoapySDR {v} (ABI {abi})") }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("SoapySDR {e}") },
    }
}

/// A string the library allocated, freed.
unsafe fn owned(a: &Api, p: *mut c_char) -> String {
    unsafe {
        let s = cstr(p);
        if let (Some(free), false) = (a.free, p.is_null()) {
            free(p as *mut c_void);
        }
        s
    }
}

/// A `char**` list the library allocated, freed.
unsafe fn strings(a: &Api, f: unsafe extern "C" fn(*mut usize) -> *mut *mut c_char) -> Vec<String> {
    unsafe {
        let mut n = 0usize;
        let mut p = f(&mut n);
        if p.is_null() {
            return vec![];
        }
        let out = (0..n).map(|i| cstr(*p.add(i))).collect();
        (a.strings_clear)(&mut p, n);
        out
    }
}

unsafe fn kwargs(k: &Kwargs) -> Vec<(String, String)> {
    unsafe {
        (0..k.size).map(|i| (cstr(*k.keys.add(i)), cstr(*k.vals.add(i)))).collect()
    }
}

fn check(a: &Api, what: &str, r: c_int) -> Result<(), String> {
    if r == 0 {
        return Ok(());
    }
    // SAFETY: static / thread-local strings.
    let last = unsafe { a.last_error.map(|f| cstr(f())).unwrap_or_default() };
    let msg = if !last.is_empty() { last } else { unsafe { a.err_to_str.map(|f| cstr(f(r))) }.unwrap_or_else(|| format!("error {r}")) };
    Err(format!("SoapySDR {what}: {msg}"))
}

/// An installed module (one per device family) and what it registered.
#[derive(Clone, Debug)]
pub struct Module {
    /// "rtlsdr" from ".../librtlsdrSupport.so".
    pub name: String,
    pub path: String,
    pub version: String,
    /// Drivers it registered ("driver=…" in device arguments).
    pub drivers: Vec<String>,
    /// Why it didn't load (missing vendor library, built for another ABI …).
    pub error: String,
}

impl Module {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "name": self.name, "path": self.path, "version": self.version, "drivers": self.drivers, "error": self.error })
    }
}

fn module_name(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    let stem = stem.strip_prefix("lib").unwrap_or(stem);
    stem.strip_suffix("Support").unwrap_or(stem).to_string()
}

/// Load each installed module not loaded yet — before any device search,
/// which would load them silently — and report them all. Run again (Refresh),
/// it picks up a module installed since; one already loaded keeps its result.
/// None: this SoapySDR (0.7) can't list its modules.
pub fn modules() -> Option<Vec<Module>> {
    static SEEN: Mutex<Option<HashMap<String, Module>>> = Mutex::new(None);
    let a = api().ok()?;
    let (list, load, result) = (a.list_modules?, a.load_module?, a.loader_result?);
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    // SAFETY: module API calls with NUL-terminated paths; results freed as documented.
    unsafe {
        let paths = strings(a, list);
        for p in &paths {
            if seen.contains_key(p) {
                continue;
            }
            let Ok(c) = CString::new(p.as_str()) else { continue };
            let loaded = owned(a, load(c.as_ptr()));
            let mut m = Module { name: module_name(p), path: p.clone(), version: String::new(), drivers: vec![], error: String::new() };
            if !loaded.is_empty() && !loaded.contains("already loaded") {
                m.error = loaded;
            }
            let mut r = result(c.as_ptr());
            for (k, v) in kwargs(&r) {
                if v.is_empty() {
                    m.drivers.push(k);
                } else {
                    m.error = if m.error.is_empty() { format!("{k}: {v}") } else { format!("{}; {k}: {v}", m.error) };
                }
            }
            (a.kwargs_clear)(&mut r);
            if let Some(f) = a.module_version {
                m.version = owned(a, f(c.as_ptr()));
            }
            seen.insert(p.clone(), m);
        }
        let mut out: Vec<Module> = paths.iter().filter_map(|p| seen.get(p).cloned()).collect();
        out.sort_by_key(|m| m.name.to_lowercase());
        Some(out)
    }
}

/// Where SoapySDR looks for modules (for "none installed").
pub fn search_paths() -> Vec<String> {
    let Ok(a) = api() else { return vec![] };
    // SAFETY: a library-allocated list, freed.
    a.list_search_paths.map(|f| unsafe { strings(a, f) }).unwrap_or_default()
}

/// Drivers this recorder has its own source for: better used that way.
const NATIVE: &[(&str, &str)] = &[("rtlsdr", "RTL-SDR"), ("hackrf", "HackRF"), ("airspy", "Airspy"), ("hydrasdr", "HydraSDR"), ("bladerf", "bladeRF"), ("uhd", "USRP")];

/// Devices the loaded modules can see: [{args, label, driver}]. A real
/// search (USB, network): some modules take seconds, so it runs only when asked.
pub fn find() -> Result<Vec<serde_json::Value>, String> {
    let a = api()?;
    modules();
    // Not while a radio opens: modules open radios to list them (SoapyAirspy, SoapyHackRF), and
    // the SDRplay module's search shares state with its open. (Some search the network for seconds.)
    let found = super::open("SoapySDR search", Deadline::Loads, true, move || {
        let mut n = 0usize;
        // SAFETY: an empty args string; the list is freed below.
        let list = unsafe { (a.enumerate)(c"".as_ptr(), &mut n) };
        Ok((Handed(list), n))
    })?;
    let (list, n) = (found.0.0, found.1);
    if list.is_null() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for i in 0..n {
        // SAFETY: n kwargs.
        let kv = unsafe { kwargs(&*list.add(i)) };
        let get = |k: &str| kv.iter().find(|(key, v)| key == k && !v.is_empty()).map(|(_, v)| v.clone());
        let driver = get("driver").unwrap_or_default();
        let args = match get("serial") {
            Some(s) if !driver.is_empty() => format!("driver={driver},serial={s}"),
            _ => kv.iter().filter(|(k, _)| k != "label").map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","),
        };
        let mut label = get("label").unwrap_or_else(|| driver.clone());
        if let Some((_, native)) = NATIVE.iter().find(|(d, _)| *d == driver) {
            label.push_str(&format!(" — the {native} source is recommended"));
        }
        let mut j = serde_json::json!({ "args": args, "label": label, "driver": driver });
        if let Some(c) = super::known(&key(&args)) {
            c.add_to(&mut j);
        }
        out.push(j);
    }
    // SAFETY: the list enumerate returned.
    unsafe { (a.kwargs_list_clear)(list, n) };
    Ok(out)
}

/// For the interface: `{available, detail, modules, searchPaths, devices}`
/// (`devices` null until searched).
pub fn json(search: bool) -> serde_json::Value {
    let i = info();
    let mut j = i.json();
    if !i.loaded {
        j["devices"] = serde_json::Value::Null;
        return j;
    }
    match modules() {
        Some(m) => j["modules"] = m.iter().map(Module::json).collect(),
        None => j["modules"] = serde_json::Value::Null,
    }
    j["searchPaths"] = serde_json::json!(search_paths());
    j["devices"] = if search {
        match find() {
            Ok(v) => serde_json::Value::Array(v),
            Err(e) => {
                j["detail"] = serde_json::json!(e);
                serde_json::json!([])
            }
        }
    } else {
        serde_json::Value::Null
    };
    j
}

#[derive(Clone, Debug)]
pub struct SoapyConfig {
    /// Device arguments ("driver=hackrf,serial=…"); "" = the first found.
    pub args: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    /// The device's AGC instead of the gains below.
    pub agc: bool,
    /// Overall gain; None = left as the device has it.
    pub gain_db: Option<f64>,
    /// Each gain stage's after the overall one: LNA 32, VGA 20.
    pub gains: Vec<(String, f64)>,
    pub antenna: String,
    /// Device settings: "biastee=true".
    pub settings: String,
    pub ppm: f64,
}

/// "a=1, b = 2" → [("a","1"), ("b","2")].
fn pairs(s: &str) -> Vec<(String, String)> {
    s.split(',').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(k, _)| !k.is_empty()).collect()
}

impl crate::sdr::Radio for SoapyConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        match g {
            Gain::Db(db) => self.gain_db = Some(db as f64),
            Gain::Soapy(s) => (self.gain_db, self.gains) = (s.overall, s.elements().map(|(k, v)| (k.to_string(), v)).collect()),
            _ => {}
        }
    }
    fn gain(&self) -> Option<Gain> {
        if self.agc { Some(Gain::Agc) } else { self.gain_db.map(|g| Gain::Db(g as f32)) }
    }
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// A manual gain's SoapySDR calls, in order: the overall gain as `(None, dB)` (none: left as it
/// is), then each element `stages` sets, by its name — which the overall gain alone would undo.
fn gain_calls(overall: Option<f64>, stages: &[(String, f64)]) -> Vec<(Option<&str>, f64)> {
    overall.map(|db| (None, db)).into_iter().chain(stages.iter().map(|(k, v)| (Some(k.as_str()), *v))).collect()
}

/// A gain set while running, as a SoapySDR device takes it: the overall gain to set, and the
/// elements it keeps setting after it (`stages`). An overall gain keeps them; a [`Gain::Soapy`] is
/// the source's whole gain, so its elements replace them, and one a trial set that the source
/// doesn't is left to the overall gain again. None: a gain it doesn't take.
fn take_gain(stages: &mut Vec<(String, f64)>, g: Gain) -> Option<Option<f64>> {
    match g {
        Gain::Db(db) => Some(Some(db.into())),
        Gain::Soapy(s) => {
            *stages = s.elements().map(|(k, v)| (k.to_string(), v)).collect();
            Some(s.overall)
        }
        _ => None,
    }
}

/// The gain elements each SoapySDR radio reported when it was last opened, by its key ([`key`]).
fn reported() -> &'static Mutex<HashMap<String, Vec<ReportedStage>>> {
    static R: OnceLock<Mutex<HashMap<String, Vec<ReportedStage>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The gain elements the radio `args` opens reported when it was last opened in this run (none: not
/// opened yet, or its SoapySDR can't say): what the gain tuner searches ([`trunk_app::gain::GainPlan::of_reported`]).
pub fn reported_stages(args: &str) -> Vec<ReportedStage> {
    reported().lock().unwrap_or_else(|e| e.into_inner()).get(&key(args)).cloned().unwrap_or_default()
}

/// A USRP through SoapyUHD gets the bigger receive buffer a direct one does.
fn device_args(args: &str) -> String {
    if pairs(args).iter().any(|(k, v)| k == "driver" && v == "uhd") {
        super::uhd::device_args(args)
    } else {
        args.to_string()
    }
}

/// Owns the device and stream so every exit path releases them.
struct Device {
    a: &'static Api,
    dev: Dev,
    stream: Stream,
    active: bool,
    ppm: f64,
    /// Its stages' gains ([`SoapyConfig::gains`]), set after the overall one each time it's set.
    stages: Vec<(String, f64)>,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: handles made by this struct (or null).
        unsafe {
            if self.active {
                (self.a.deactivate)(self.dev, self.stream, 0, 0);
            }
            if !self.stream.is_null() {
                (self.a.close_stream)(self.dev, self.stream);
            }
            if !self.dev.is_null() {
                (self.a.unmake)(self.dev);
            }
        }
    }
}

impl Tunable for Device {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        // SAFETY: an open device.
        check(self.a, "tune", unsafe { (self.a.set_frequency)(self.dev, RX, 0, ppm_corrected(hz, self.ppm), std::ptr::null()) })?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// A manual gain (AGC on or off waits for the next start): overall, or the source's whole
    /// gain, overall and elements ([`take_gain`]): a trial's and, after it, the setting it had.
    fn set_gain(&mut self, g: Gain) -> Result<Regained, String> {
        let Some(overall) = take_gain(&mut self.stages, g) else { return Ok(Regained::LEFT_ALONE) };
        // SAFETY: an open device.
        unsafe { self.gain(overall) }?;
        Ok(Regained { gap: 0, gain: if let Gain::Db(_) = g { self.gain_set() } else { Some(g) } })
    }
}

impl Device {
    /// A manual gain: the AGC off, `db` overall (None: as the device has it),
    /// then each of its stages' — which an overall gain set alone would undo.
    unsafe fn gain(&self, db: Option<f64>) -> Result<(), String> {
        unsafe {
            if (self.a.has_gain_mode)(self.dev, RX, 0) {
                check(self.a, "AGC off", (self.a.set_gain_mode)(self.dev, RX, 0, false))?;
            }
            for (element, db) in gain_calls(db, &self.stages) {
                match element {
                    None => check(self.a, "gain", (self.a.set_gain)(self.dev, RX, 0, db))?,
                    Some(k) => {
                        let ck = CString::new(k).map_err(|e| e.to_string())?;
                        check(self.a, &format!("gain {k}"), (self.a.set_gain_element)(self.dev, RX, 0, ck.as_ptr(), db))?;
                    }
                }
            }
            Ok(())
        }
    }
    /// Its receive gain elements and their ranges, when its SoapySDR can say.
    unsafe fn elements(&self) -> Vec<ReportedStage> {
        let (Some(list), Some(range)) = (self.a.list_gains, self.a.gain_element_range) else { return vec![] };
        unsafe {
            let mut n = 0usize;
            let mut p = list(self.dev, RX, 0, &mut n);
            if p.is_null() {
                return vec![];
            }
            let names: Vec<String> = (0..n).map(|i| cstr(*p.add(i))).collect();
            (self.a.strings_clear)(&mut p, n);
            names
                .into_iter()
                .filter_map(|name| {
                    let c = CString::new(name.as_str()).ok()?;
                    let r = range(self.dev, RX, 0, c.as_ptr());
                    Some(ReportedStage { name, min: r.minimum, max: r.maximum, step: r.step })
                })
                .collect()
        }
    }
    /// The overall gain it's set to, when its SoapySDR can say.
    fn gain_set(&self) -> Option<Gain> {
        // SAFETY: an open device.
        self.a.get_gain.map(|get| Gain::Db(unsafe { get(self.dev, RX, 0) } as f32))
    }
    /// The rates its module says it runs at: a span (min to max of its ranges) when one is a
    /// span, else the rates themselves.
    unsafe fn rates(&self) -> Option<Rates> {
        unsafe {
            if let Some(f) = self.a.get_sample_rate_range {
                let mut n = 0usize;
                let p = f(self.dev, RX, 0, &mut n);
                if !p.is_null() {
                    let v = std::slice::from_raw_parts(p, n).iter().map(|r| (r.minimum, r.maximum)).collect::<Vec<_>>();
                    if let Some(free) = self.a.free {
                        free(p as *mut c_void);
                    }
                    if let Some(r) = rates_of(&v) {
                        return Some(r);
                    }
                }
            }
            let mut n = 0usize;
            let p = (self.a.list_sample_rates)(self.dev, RX, 0, &mut n);
            if p.is_null() {
                return None;
            }
            let v: Vec<u32> = std::slice::from_raw_parts(p, n).iter().map(|r| r.round() as u32).collect();
            if let Some(free) = self.a.free {
                free(p as *mut c_void);
            }
            (!v.is_empty()).then_some(Rates::List(v))
        }
    }
}

fn stream_once(cfg: &SoapyConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    // Modules load here rather than inside make, so a broken one is reported.
    modules();
    let args = CString::new(device_args(&cfg.args)).map_err(|e| e.to_string())?;
    let name = format!("SoapySDR {}", if cfg.args.is_empty() { "(first)" } else { &cfg.args });
    let what = name.clone();
    let sdrplay_args = cfg.args.clone();
    // (Some modules load firmware as they open.) The SDRplay slot is held
    // until the radio is released, and dropped after it.
    let (d, _sdrplay) = super::open(&name, Deadline::Loads, true, move || {
        let mut sdrplay = super::sdrplay::before_open(&sdrplay_args);
        // SAFETY: SoapySDR makes the device; the Device releases it.
        let dev = unsafe { (a.make)(args.as_ptr()) };
        sdrplay.made(!dev.is_null());
        if dev.is_null() {
            let why = a.last_error.map(|f| unsafe { cstr(f()) }).filter(|s| !s.is_empty()).unwrap_or_else(|| "no matching device".into());
            return Err(format!("{what} open: {why}"));
        }
        Ok((Handed(Device { a, dev, stream: std::ptr::null_mut(), active: false, ppm: 0.0, stages: Vec::new() }), sdrplay))
    })?;
    let mut d = d.0;
    d.ppm = cfg.ppm;
    // SAFETY: SoapySDR C API calls on the device and stream `d` owns.
    unsafe {
        // What it runs at, for the list of radios and the refusal.
        let rates = d.rates();
        if let Some(r) = &rates {
            super::remember(&key(&cfg.args), Caps { model: String::new(), rates: r.clone(), gains: Vec::new() });
        }
        // Its gain elements, for the gain tuner.
        let elements = d.elements();
        if !elements.is_empty() {
            reported().lock().unwrap_or_else(|e| e.into_inner()).insert(key(&cfg.args), elements);
        }
        check(a, "sample rate", (a.set_sample_rate)(d.dev, RX, 0, cfg.rate_hz))?;
        let actual = (a.get_sample_rate)(d.dev, RX, 0);
        if (actual - cfg.rate_hz).abs() > 1.0 {
            return Err(match &rates {
                Some(r) if !r.allows(cfg.rate_hz) => super::rate_refusal("device", r, cfg.rate_hz),
                _ => format!("this device runs at {} MSPS, not {}: pick that in Setup → Radios", actual / 1e6, cfg.rate_hz / 1e6),
            });
        }
        if !cfg.antenna.is_empty() {
            let ant = CString::new(cfg.antenna.as_str()).map_err(|e| e.to_string())?;
            check(a, "antenna", (a.set_antenna)(d.dev, RX, 0, ant.as_ptr()))?;
        }
        for (k, v) in pairs(&cfg.settings) {
            let (ck, cv) = (CString::new(k.as_str()).map_err(|e| e.to_string())?, CString::new(v.as_str()).map_err(|e| e.to_string())?);
            check(a, &format!("setting {k}"), (a.write_setting)(d.dev, ck.as_ptr(), cv.as_ptr()))?;
        }
        // AGC or the gains, not both: SDRplay ignores stage gains while its AGC is on.
        let has_agc = (a.has_gain_mode)(d.dev, RX, 0);
        if cfg.agc {
            if !has_agc {
                return Err("this device has no AGC: set a gain instead".into());
            }
            check(a, "AGC", (a.set_gain_mode)(d.dev, RX, 0, true))?;
        } else {
            d.stages = cfg.gains.clone();
            d.gain(cfg.gain_db)?;
        }
        d.tune(cfg.center_hz)?;
        if !cfg.agc {
            sink.gained(cfg.gain(), d.gain_set());
        }

        let chan = [0usize];
        d.stream = match a.setup_stream {
            SetupStream::V08(f) => f(d.dev, RX, c"CF32".as_ptr(), chan.as_ptr(), 1, std::ptr::null()),
            SetupStream::V07(f) => {
                let mut s: Stream = std::ptr::null_mut();
                check(a, "stream", f(d.dev, &mut s, RX, c"CF32".as_ptr(), chan.as_ptr(), 1, std::ptr::null()))?;
                s
            }
        };
        if d.stream.is_null() {
            return check(a, "stream", -1);
        }
        let mtu = (a.stream_mtu)(d.dev, d.stream).max(1);
        // ~10 ms blocks, a whole number of MTUs: some drivers hand over a few hundred samples per read.
        let per = mtu * ((cfg.rate_hz / 100.0) as usize / mtu).max(1);
        check(a, "start", (a.activate)(d.dev, d.stream, 0, 0, 0))?;
        d.active = true;

        let mut stamps = Stamps::new(cfg.rate_hz);
        let mut overflowed = false;
        let mut timeouts = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Some(c) = ctl
                && c.apply(&mut d, cfg.center_hz, sink)? == Applied::Closed
            {
                return Ok(());
            }
            let mut buf = sink.iq_buffer(per);
            buf.vec().resize(per, Complex32::default());
            let (mut got, mut dropped) = (0usize, 0u64);
            while got < per {
                let ptrs = [buf.vec()[got..].as_mut_ptr() as *mut c_void];
                let (mut flags, mut t) = (0 as c_int, 0 as c_longlong);
                let n = (a.read_stream)(d.dev, d.stream, ptrs.as_ptr(), per - got, &mut flags, &mut t, 1_000_000);
                if n > 0 {
                    timeouts = 0;
                    dropped += stamps.read(n as usize, (flags & HAS_TIME != 0).then_some(t));
                    if overflowed && !stamps.timed() {
                        // No timestamps (or none to go by): an overflow lost about one transfer.
                        dropped += mtu as u64;
                    }
                    overflowed = false;
                    got += n as usize;
                    continue;
                }
                match n {
                    TIMEOUT => {
                        timeouts += 1;
                        if timeouts >= 3 {
                            return Err("SoapySDR: no samples for 3 s".into());
                        }
                        break;
                    }
                    OVERFLOW => overflowed = true,
                    CORRUPTION | TIME_ERROR => {}
                    e => return check(a, "receive", e),
                }
            }
            if got == 0 {
                continue;
            }
            buf.vec().truncate(got);
            if !sink.send(Block::Iq(buf), dropped, std::time::Instant::now()) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// A stream's timestamps, as what they say of samples lost: a buffer that
/// starts later than the samples before it end lost the difference.
///
/// A read without a time continues the one before: SoapyRemote stamps only
/// the first read of a network buffer split over several (the rest of it
/// comes back with no time), so those samples count too, or the next
/// buffer's time looks ahead by them (#209). And timestamps that don't keep
/// to the sample count are noticed over the first second, said once, and
/// ignored: samples lost are then counted by overflows, as with none.
///
/// That's judged over windows of samples, not step by step: SoapyRTLSDR
/// before 0.3.4 counts its buffers' times in bytes but the reads within one
/// in samples, so over SoapyRemote's small reads most steps keep to the
/// count and each buffer's first jumps by a buffer (#209: ~2.4 M "lost" a
/// second). Over a window its times run at twice real time. The median
/// window is taken, so a loss (the stream's first buffer, say) doesn't
/// decide it.
struct Stamps {
    rate: f64,
    /// Where the next sample's time should be, ns.
    next_ns: Option<f64>,
    /// Whether any read was stamped.
    stamped: bool,
    trust: Trust,
    /// While learning: the window's first time and its samples since, each
    /// window's time against its samples', and the samples the times say
    /// were lost (counted once they're trusted).
    window: Option<(f64, u64)>,
    ratios: Vec<f64>,
    held: u64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Trust {
    Learning,
    Yes,
    /// They run at this many times real time.
    No(f64),
}

impl Stamps {
    /// A window's samples, s, and the windows looked at before deciding.
    const WINDOW_S: f64 = 0.1;
    const WINDOWS: usize = 9;

    fn new(rate: f64) -> Stamps {
        Stamps { rate, next_ns: None, stamped: false, trust: Trust::Learning, window: None, ratios: Vec::new(), held: 0 }
    }

    /// Whether lost samples are going by the timestamps.
    fn timed(&self) -> bool {
        self.stamped && !matches!(self.trust, Trust::No(_))
    }

    /// `n` samples read, stamped `time_ns` (none: continuing the read
    /// before); the samples lost before them.
    fn read(&mut self, n: usize, time_ns: Option<i64>) -> u64 {
        let rate = self.rate;
        let ns = |k: u64| k as f64 * 1e9 / rate;
        let Some(t) = time_ns.map(|t| t as f64) else {
            if let Some((_, k)) = self.window.as_mut() {
                *k += n as u64;
            }
            self.next_ns = self.next_ns.map(|x| x + ns(n as u64));
            return 0;
        };
        let gap = self.next_ns.map_or(0, |want| ((t - want) * rate / 1e9).round().max(0.0) as u64);
        if self.trust == Trust::Learning {
            match self.window {
                Some((t0, k)) if k as f64 >= rate * Self::WINDOW_S => {
                    self.ratios.push((t - t0) / ns(k));
                    self.window = Some((t, 0));
                }
                None => self.window = Some((t, 0)),
                Some(_) => {}
            }
            if let Some((_, k)) = self.window.as_mut() {
                *k += n as u64;
            }
            self.held += gap;
            if self.ratios.len() >= Self::WINDOWS {
                self.decide();
            }
        }
        self.stamped = true;
        self.next_ns = Some(t + ns(n as u64));
        match self.trust {
            Trust::Yes => gap + std::mem::take(&mut self.held),
            Trust::No(_) | Trust::Learning => 0,
        }
    }

    /// Times that keep to the samples over the median window, or a rate other than real time.
    fn decide(&mut self) {
        let mut r = std::mem::take(&mut self.ratios);
        self.window = None;
        r.sort_by(f64::total_cmp);
        let median = r[r.len() / 2];
        self.trust = if (median - 1.0).abs() < 0.05 { Trust::Yes } else { Trust::No(median) };
        if let Trust::No(k) = self.trust {
            self.held = 0;
            let hint = if (k - 2.0).abs() < 0.1 { " (SoapyRTLSDR before 0.3.4 does: it counts bytes, not samples; update it)" } else { "" };
            log::warn!("SoapySDR: this driver's timestamps run at {k:.2}× real time{hint}; going by the sample count instead");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 2_400_000.0;

    /// Reads of `sizes`, as SoapyRemote gives them: a network buffer of `buf`
    /// samples split over the reads asked for, only each buffer's first read
    /// stamped; `scale`: the time's rate against real time; `lost`: buffers
    /// (by number) the network lost. The samples lost, in all.
    fn remote(stamps: &mut Stamps, reads: usize, ask: usize, buf: usize, scale: f64, lost: &[usize]) -> u64 {
        let (mut total, mut b, mut left) = (0, 0usize, 0usize);
        for _ in 0..reads {
            let mut time = None;
            if left == 0 {
                while lost.contains(&b) {
                    b += 1;
                }
                time = Some((b as f64 * buf as f64 * 1e9 / RATE * scale) as i64);
                left = buf;
                b += 1;
            }
            let n = ask.min(left);
            left -= n;
            total += stamps.read(n, time);
        }
        total
    }

    /// #209: SoapyRemote's buffers of 2012 split by reads of a 10 ms block:
    /// the unstamped rest of each continues it, so nothing is lost.
    #[test]
    fn a_split_buffers_unstamped_rest_continues_it() {
        let mut s = Stamps::new(RATE);
        assert_eq!(remote(&mut s, 5000, 1500, 2012, 1.0, &[]), 0);
        assert_eq!(s.trust, Trust::Yes);
    }

    /// A buffer the network lost: its samples, once.
    #[test]
    fn a_buffer_lost_is_counted() {
        let mut s = Stamps::new(RATE);
        assert_eq!(remote(&mut s, 5000, 1500, 2012, 1.0, &[500]), 2012);
        // (While learning too: held until the times are trusted.)
        let mut s = Stamps::new(RATE);
        assert_eq!(remote(&mut s, 5000, 1500, 2012, 1.0, &[3]), 2012);
    }

    /// SoapyRTLSDR before 0.3.4: times at twice real time aren't believed, and nothing's counted lost.
    #[test]
    fn timestamps_at_twice_real_time_are_ignored() {
        let mut s = Stamps::new(RATE);
        assert_eq!(remote(&mut s, 5000, 16384, 16384, 2.0, &[]), 0);
        assert!(matches!(s.trust, Trust::No(k) if (k - 2.0).abs() < 0.01), "{:?}", s.trust);
        assert!(!s.timed(), "overflows count instead");
    }

    /// #209: SoapyRTLSDR 0.3.3 behind SoapyRemote. Its buffers' times count
    /// bytes (each starts two buffers' worth of time after the last) while
    /// the reads within one count samples, and the remote splits them into
    /// network buffers of 2012, each stamped at its first sample: most steps
    /// keep to the count, but over time the times run at twice real time.
    #[test]
    fn an_old_rtlsdr_behind_soapyremote_is_noticed() {
        const DRIVER: usize = 16_384;
        const NET: usize = 2012;
        let mut s = Stamps::new(RATE);
        let (mut lost, mut sample) = (0u64, 0usize);
        // The network buffers, as (time, samples): within a driver buffer, never across one.
        let net = (0..2_000).flat_map(|b| {
            let base = b * 2 * DRIVER;
            (0..DRIVER).step_by(NET).map(move |o| (((base + o) as f64 * 1e9 / RATE) as i64, NET.min(DRIVER - o)))
        });
        // Read 10 ms at a time, as the source does: a network buffer's rest comes unstamped.
        let ask = (RATE / 100.0) as usize;
        let mut left = 0usize;
        let mut pending = net.peekable();
        while sample < 2_000 * DRIVER - ask {
            let mut got = 0;
            while got < ask {
                let time = if left == 0 {
                    let (t, n) = pending.next().unwrap();
                    left = n;
                    Some(t)
                } else {
                    None
                };
                let n = (ask - got).min(left);
                left -= n;
                got += n;
                lost += s.read(n, time);
            }
            sample += got;
        }
        assert!(matches!(s.trust, Trust::No(k) if (k - 2.0).abs() < 0.1), "{:?}", s.trust);
        assert_eq!(lost, 0, "nothing counted lost: the times aren't believed");
    }

    /// SoapyRemote's first buffer comes late (#209: 54.6 ms): one window off,
    /// the median keeps to the count, and the times are trusted.
    #[test]
    fn a_late_first_buffer_doesnt_decide_it() {
        let late = (0.0546 * RATE) as usize;
        let mut s = Stamps::new(RATE);
        let (mut total, mut t) = (s.read(2012, Some(0)), 2012 + late);
        for _ in 0..3000 {
            total += s.read(2012, Some((t as f64 * 1e9 / RATE) as i64));
            t += 2012;
        }
        assert_eq!(s.trust, Trust::Yes);
        assert_eq!(total, late as u64, "the late start, once");
    }

    /// No timestamps at all: nothing from them.
    #[test]
    fn no_timestamps_counts_nothing() {
        let mut s = Stamps::new(RATE);
        assert_eq!((0..100).map(|_| s.read(2012, None)).sum::<u64>(), 0);
        assert!(!s.timed());
    }

    /// A SoapySDR device as far as its gain goes: an overall gain fills its elements front first,
    /// as SoapySDR's default setGain does (SoapyAirspy's too: 31 is LNA 15, MIX 15, VGA 1); an
    /// element set by name is set alone, and a name it doesn't have is an error (here, a panic).
    struct FakeGains(Vec<(&'static str, f64, f64)>);
    impl FakeGains {
        fn new(elements: &[(&'static str, f64)]) -> FakeGains {
            FakeGains(elements.iter().map(|(n, max)| (*n, *max, 0.0)).collect())
        }
        fn run(&mut self, calls: Vec<(Option<&str>, f64)>) {
            for (element, v) in calls {
                match element {
                    None => {
                        let mut left = v;
                        for e in &mut self.0 {
                            e.2 = left.clamp(0.0, e.1);
                            left -= e.2;
                        }
                    }
                    Some(k) => self.0.iter_mut().find(|e| e.0 == k).unwrap_or_else(|| panic!("no element {k}")).2 = v,
                }
            }
        }
        fn now(&self) -> Vec<f64> {
            self.0.iter().map(|e| e.2).collect()
        }
    }

    /// The radio as the recorder runs it: opened as `src` has it (stream_once: its elements, then the
    /// overall gain and them), then each later gain the way [`Device::set_gain`] takes it.
    struct Running {
        fake: FakeGains,
        stages: Vec<(String, f64)>,
    }
    impl Running {
        fn open(fake: FakeGains, src: &crate::config::Source) -> Running {
            let crate::config::SourceKind::Soapy { gain_db, gains, .. } = &src.kind else { panic!() };
            let mut r = Running { fake, stages: gains.clone().into_iter().collect() };
            r.fake.run(gain_calls(*gain_db, &r.stages));
            r
        }
        /// `to` set while running from `run` (the run's source, which a trial doesn't change), as the
        /// tuner sets a trial, the incumbent, and the radio back when it ends, stops or isn't applied.
        fn set(&mut self, run: &crate::config::Source, to: &crate::config::Source) -> Vec<f64> {
            let g = crate::runtime::live_gain(run, to).expect("taken while running");
            let overall = take_gain(&mut self.stages, g).expect("a gain it takes");
            self.fake.run(gain_calls(overall, &self.stages));
            self.fake.now()
        }
    }

    fn source(v: serde_json::Value) -> crate::config::Source {
        let mut v = v;
        (v["type"], v["centerHz"], v["rateHz"], v["agc"]) = ("soapy".into(), 852e6.into(), 10e6.into(), false.into());
        serde_json::from_value(v).unwrap()
    }

    /// A trial and then the setting the radio had (the incumbent's slices, and the radio put back when the tuning
    /// ends, is stopped or isn't applied) leave every element as it was: an overall gain alone, with some elements
    /// set, or elements alone; a trial's elements the source doesn't set go back to what the overall gain gives.
    #[test]
    fn a_trial_is_undone_exactly() {
        use trunk_app::gain::GainPlan;
        let airspy = || FakeGains::new(&[("LNA", 15.0), ("MIX", 15.0), ("VGA", 15.0)]);
        let args = "driver=remote,remote=tcp://server:55132,remote:driver=airspy,remote:prot=tcp";
        for (start, opened) in [
            (serde_json::json!({ "args": args, "gainDb": 31 }), [15.0, 15.0, 1.0]),
            (serde_json::json!({ "args": args, "gainDb": 31, "gains": { "LNA": 5 } }), [5.0, 15.0, 1.0]),
            (serde_json::json!({ "args": args, "gains": { "LNA": 5, "MIX": 4, "VGA": 3 } }), [5.0, 4.0, 3.0]),
        ] {
            let run = source(start);
            let mut radio = Running::open(airspy(), &run);
            assert_eq!(radio.fake.now(), opened, "{run:?}");
            let plan = GainPlan::of(&run, None).unwrap();
            // Linearity step 14: LNA 9, MIX 6, VGA 10.
            assert_eq!(radio.set(&run, &plan.apply(&run, &vec![14])), [9.0, 6.0, 10.0], "the trial");
            assert_eq!(radio.set(&run, &run), opened, "back as it was: {run:?}");
            assert_eq!(radio.set(&run, &plan.apply(&run, &vec![17])), [12.0, 8.0, 11.0], "the next trial");
            assert_eq!(radio.set(&run, &run), opened);
        }
        // No overall gain and an element the search moves not set: it couldn't be put back, so it isn't tuned.
        assert!(GainPlan::of(&source(serde_json::json!({ "args": args, "gains": { "LNA": 5 } })), None).is_err());
    }

    /// A radio's own element names go all the way to the device, as it reported them: a MIXER isn't set as MIX,
    /// and an element no stage is named after (TIA) is searched and set too.
    #[test]
    fn elements_keep_the_names_the_radio_gave_them() {
        use trunk_app::gain::{GainPlan, ReportedStage};
        let lime = || FakeGains::new(&[("LNA", 30.0), ("MIXER", 15.0), ("TIA", 12.0)]);
        let el = |name: &str, max: f64, step: f64| ReportedStage { name: name.into(), min: 0.0, max, step };
        let reported = [el("LNA", 30.0, 1.0), el("MIXER", 15.0, 1.0), el("TIA", 12.0, 3.0)];
        let run = source(serde_json::json!({ "args": "driver=lime", "gainDb": 20 }));
        let mut radio = Running::open(lime(), &run);
        assert_eq!(radio.fake.now(), [20.0, 0.0, 0.0]);
        let plan = GainPlan::of_reported(&run, None, None, &reported).unwrap();
        assert_eq!(radio.set(&run, &plan.apply(&run, &vec![10, 7, 3])), [10.0, 7.0, 9.0], "set by their own names");
        assert_eq!(radio.set(&run, &run), [20.0, 0.0, 0.0], "and back");
    }

    /// trunk-app's copy of libairspy's tables (the gain tuner's steps for an Airspy through SoapySDR) is libairspy-rs's.
    #[test]
    fn the_tuners_airspy_tables_are_libairspys() {
        for sensitivity in [false, true] {
            for step in 0..=trunk_app::gain::AIRSPY_STEP_MAX {
                assert_eq!(trunk_app::gain::airspy_preset(sensitivity, step), libairspy_rs::composite_gains(sensitivity, step), "{sensitivity} {step}");
            }
        }
    }

    #[test]
    fn rates_from_ranges() {
        assert_eq!(rates_of(&[(2e6, 2e6), (8e6, 8e6)]), Some(Rates::List(vec![2_000_000, 8_000_000])));
        assert_eq!(rates_of(&[(1e6, 20e6), (2e6, 2e6)]), Some(Rates::Range { min: 1e6, max: 20e6 }));
        assert_eq!(rates_of(&[]), None);
    }

    #[test]
    fn helpers() {
        assert!(abi_at_least_08("0.8"));
        assert!(abi_at_least_08("0.8-3"));
        assert!(!abi_at_least_08("0.7"));
        assert!(abi_at_least_08("1.0"));
        assert_eq!(module_name("/opt/homebrew/lib/SoapySDR/modules0.8/librtlsdrSupport.so"), "rtlsdr");
        assert_eq!(module_name(r"C:\PothosSDR\lib\SoapySDR\modules0.8\sdrPlaySupport.dll"), "sdrPlay");
        assert_eq!(pairs("LNA=32, VGA = 20,,bad"), vec![("LNA".into(), "32".into()), ("VGA".into(), "20".into())]);
        assert_eq!(device_args("driver=uhd,serial=31A"), "driver=uhd,serial=31A,num_recv_frames=1024");
        assert_eq!(device_args("driver=hackrf"), "driver=hackrf");
    }
}
