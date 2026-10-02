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
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use super::DriverInfo;
use crate::sdr::{Control, SourceMsg};

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
    set_frequency: unsafe extern "C" fn(Dev, c_int, usize, f64, *const Kwargs) -> c_int,
    has_gain_mode: unsafe extern "C" fn(Dev, c_int, usize) -> bool,
    set_gain_mode: unsafe extern "C" fn(Dev, c_int, usize, bool) -> c_int,
    set_gain: unsafe extern "C" fn(Dev, c_int, usize, f64) -> c_int,
    set_gain_element: unsafe extern "C" fn(Dev, c_int, usize, *const c_char, f64) -> c_int,
    set_antenna: unsafe extern "C" fn(Dev, c_int, usize, *const c_char) -> c_int,
    write_setting: unsafe extern "C" fn(Dev, *const c_char, *const c_char) -> c_int,
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
        let (lib, path) = super::load("TRUNK_PRO_SOAPY", &names, "SoapySDR", true)?;
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
                set_frequency: f!("SoapySDRDevice_setFrequency"),
                has_gain_mode: f!("SoapySDRDevice_hasGainMode"),
                set_gain_mode: f!("SoapySDRDevice_setGainMode"),
                set_gain: f!("SoapySDRDevice_setGain"),
                set_gain_element: f!("SoapySDRDevice_setGainElement"),
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

/// SAFETY: `p` null or a NUL-terminated string.
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// A string the library allocated, freed.
unsafe fn owned(a: &Api, p: *mut c_char) -> String {
    let s = cstr(p);
    if let (Some(free), false) = (a.free, p.is_null()) {
        free(p as *mut c_void);
    }
    s
}

/// A `char**` list the library allocated, freed.
unsafe fn strings(a: &Api, f: unsafe extern "C" fn(*mut usize) -> *mut *mut c_char) -> Vec<String> {
    let mut n = 0usize;
    let mut p = f(&mut n);
    if p.is_null() {
        return vec![];
    }
    let out = (0..n).map(|i| cstr(*p.add(i))).collect();
    (a.strings_clear)(&mut p, n);
    out
}

unsafe fn kwargs(k: &Kwargs) -> Vec<(String, String)> {
    (0..k.size).map(|i| (cstr(*k.keys.add(i)), cstr(*k.vals.add(i)))).collect()
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
const NATIVE: &[(&str, &str)] = &[("rtlsdr", "RTL-SDR"), ("airspy", "Airspy"), ("uhd", "USRP")];

/// Devices the loaded modules can see: [{args, label, driver}]. A real
/// search (USB, network): some modules take seconds, so it runs only when asked.
pub fn find() -> Result<Vec<serde_json::Value>, String> {
    let a = api()?;
    modules();
    let mut n = 0usize;
    // SAFETY: an empty args string; the list is freed below.
    let list = unsafe { (a.enumerate)(c"".as_ptr(), &mut n) };
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
        out.push(serde_json::json!({ "args": args, "label": label, "driver": driver }));
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

pub fn run(source: usize, cfg: SoapyConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    run_with(source, cfg, tx, stop, None)
}

/// [`run`], retuned on request.
pub fn run_with(source: usize, mut cfg: SoapyConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<Control>>) {
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

/// Owns the device and stream so every exit path releases them.
struct Device<'a> {
    a: &'a Api,
    dev: Dev,
    stream: Stream,
    active: bool,
}

impl Drop for Device<'_> {
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

impl Device<'_> {
    /// A reference off by `ppm` makes a requested f come out at f·(1+ppm).
    unsafe fn tune(&self, hz: f64, ppm: f64) -> Result<(), String> {
        check(self.a, "tune", (self.a.set_frequency)(self.dev, RX, 0, hz / (1.0 + ppm * 1e-6), std::ptr::null()))
    }
    unsafe fn gain(&self, db: f64) -> Result<(), String> {
        if (self.a.has_gain_mode)(self.dev, RX, 0) {
            check(self.a, "AGC off", (self.a.set_gain_mode)(self.dev, RX, 0, false))?;
        }
        check(self.a, "gain", (self.a.set_gain)(self.dev, RX, 0, db))
    }
    unsafe fn rates(&self) -> String {
        let mut n = 0usize;
        let p = (self.a.list_sample_rates)(self.dev, RX, 0, &mut n);
        if p.is_null() {
            return "?".into();
        }
        let s: Vec<String> = std::slice::from_raw_parts(p, n).iter().map(|r| format!("{}", r / 1e6)).collect();
        if let Some(free) = self.a.free {
            free(p as *mut c_void);
        }
        s.join(" / ")
    }
}

fn stream_once(source: usize, cfg: &SoapyConfig, tx: &SyncSender<SourceMsg>, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    // Modules load here rather than inside make, so a broken one is reported.
    modules();
    let args = CString::new(cfg.args.as_str()).map_err(|e| e.to_string())?;
    let mut d = Device { a, dev: std::ptr::null_mut(), stream: std::ptr::null_mut(), active: false };
    // SAFETY: SoapySDR C API calls on the device and stream `d` owns.
    unsafe {
        d.dev = (a.make)(args.as_ptr());
        if d.dev.is_null() {
            let why = a.last_error.map(|f| cstr(f())).filter(|s| !s.is_empty()).unwrap_or_else(|| "no matching device".into());
            return Err(format!("SoapySDR open {}: {why}", if cfg.args.is_empty() { "(first)" } else { &cfg.args }));
        }
        check(a, "sample rate", (a.set_sample_rate)(d.dev, RX, 0, cfg.rate_hz))?;
        let actual = (a.get_sample_rate)(d.dev, RX, 0);
        if (actual - cfg.rate_hz).abs() > 1.0 {
            return Err(format!("this device runs at {} MSPS, not {} (it gave {})", d.rates(), cfg.rate_hz / 1e6, actual / 1e6));
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
            if has_agc {
                check(a, "AGC off", (a.set_gain_mode)(d.dev, RX, 0, false))?;
            }
            if let Some(g) = cfg.gain_db {
                d.gain(g)?;
            }
            for (k, db) in &cfg.gains {
                let ck = CString::new(k.as_str()).map_err(|e| e.to_string())?;
                check(a, &format!("gain {k}"), (a.set_gain_element)(d.dev, RX, 0, ck.as_ptr(), *db))?;
            }
        }
        d.tune(cfg.center_hz, cfg.ppm)?;

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

        let mut next_ns: Option<f64> = None;
        let mut overflowed = false;
        let mut timeouts = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Some(c) = ctl {
                let (freq, gain) = c.take();
                if let Some(f) = freq {
                    d.tune(f, cfg.ppm)?;
                    *c.current_hz.lock().unwrap() = Some(f);
                }
                if let Some(g) = gain {
                    d.gain(g as f64)?;
                }
                if freq.is_some() || gain.is_some() {
                    let hz = c.current_hz.lock().unwrap().unwrap_or(cfg.center_hz);
                    if tx.send(SourceMsg::Tuned { center_hz: hz }).is_err() {
                        return Ok(());
                    }
                }
            }
            let mut buf = vec![Complex32::default(); per];
            let (mut got, mut dropped) = (0usize, 0u64);
            while got < per {
                let ptrs = [buf[got..].as_mut_ptr() as *mut c_void];
                let (mut flags, mut t) = (0 as c_int, 0 as c_longlong);
                let n = (a.read_stream)(d.dev, d.stream, ptrs.as_ptr(), per - got, &mut flags, &mut t, 1_000_000);
                if n > 0 {
                    timeouts = 0;
                    if flags & HAS_TIME != 0 {
                        // Samples lost show as a jump in the buffer time.
                        if let Some(want) = next_ns {
                            let gap = ((t as f64 - want) * cfg.rate_hz / 1e9).round();
                            if gap > 0.0 {
                                dropped += gap as u64;
                            }
                        }
                        next_ns = Some(t as f64 + n as f64 * 1e9 / cfg.rate_hz);
                    } else if overflowed {
                        // No timestamps: an overflow lost about one transfer.
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
            buf.truncate(got);
            if tx.send(SourceMsg::Iq { source, samples: buf, dropped }).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert!(abi_at_least_08("0.8"));
        assert!(abi_at_least_08("0.8-3"));
        assert!(!abi_at_least_08("0.7"));
        assert!(abi_at_least_08("1.0"));
        assert_eq!(module_name("/opt/homebrew/lib/SoapySDR/modules0.8/librtlsdrSupport.so"), "rtlsdr");
        assert_eq!(module_name(r"C:\PothosSDR\lib\SoapySDR\modules0.8\sdrPlaySupport.dll"), "sdrPlay");
        assert_eq!(pairs("LNA=32, VGA = 20,,bad"), vec![("LNA".into(), "32".into()), ("VGA".into(), "20".into())]);
    }
}
