//! bladeRF (bladeRF 2.0 micro xA4 / xA9, and the original bladeRF x40 /
//! x115) through libbladeRF, loaded at run time. Needs libbladeRF 2.x
//! installed: `apt install libbladerf2` (plus `bladerf-fpga-hostedxa4` or
//! the FPGA image for your board — Nuand's PPA has them), `brew install
//! libbladerf`, or bladeRF.dll from Nuand's Windows installer. Samples come
//! through the synchronous interface as SC16 Q11 with metadata, so overruns
//! show as gaps in the sample timestamps and are reported as dropped.

use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use super::{cstr, ppm_corrected, Caps, Deadline, DriverInfo, Handed, Rates};
use crate::sdr::{Applied, Control, Gain, Retuned, Sink, Tunable};

type Dev = *mut c_void;

const SERIAL_LEN: usize = 33;
const DESC_LEN: usize = 33;

#[repr(C)]
struct DevInfo {
    backend: c_int,
    serial: [c_char; SERIAL_LEN],
    usb_bus: u8,
    usb_addr: u8,
    instance: c_uint,
    manufacturer: [c_char; DESC_LEN],
    product: [c_char; DESC_LEN],
}

/// struct bladerf_range: each value is `field * scale`.
#[repr(C)]
struct Range {
    min: i64,
    max: i64,
    step: i64,
    scale: f32,
}

#[repr(C)]
struct Version {
    major: u16,
    minor: u16,
    patch: u16,
    describe: *const c_char,
}

#[repr(C)]
struct Metadata {
    timestamp: u64,
    flags: u32,
    status: u32,
    actual_count: c_uint,
    reserved: [u8; 32],
}

/// BLADERF_CHANNEL_RX(0).
const RX0: c_int = 0;
/// BLADERF_RX_X1.
const RX_X1: c_int = 0;
/// BLADERF_FORMAT_SC16_Q11_META.
const FORMAT_SC16_Q11_META: c_int = 1;
/// BLADERF_GAIN_DEFAULT (the board's AGC) and BLADERF_GAIN_MGC (manual).
const GAIN_DEFAULT: c_int = 0;
const GAIN_MGC: c_int = 1;
const META_FLAG_RX_NOW: u32 = 1 << 31;
const META_STATUS_OVERRUN: u32 = 1 << 0;
const ERR_TIMEOUT: c_int = -6;

struct Api {
    _lib: Library,
    version: unsafe extern "C" fn(*mut Version),
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
    get_device_list: unsafe extern "C" fn(*mut *mut DevInfo) -> c_int,
    free_device_list: unsafe extern "C" fn(*mut DevInfo),
    open: unsafe extern "C" fn(*mut Dev, *const c_char) -> c_int,
    close: unsafe extern "C" fn(Dev),
    get_board_name: unsafe extern "C" fn(Dev) -> *const c_char,
    get_devinfo: unsafe extern "C" fn(Dev, *mut DevInfo) -> c_int,
    get_sample_rate_range: unsafe extern "C" fn(Dev, c_int, *mut *const Range) -> c_int,
    set_sample_rate: unsafe extern "C" fn(Dev, c_int, c_uint, *mut c_uint) -> c_int,
    set_bandwidth: unsafe extern "C" fn(Dev, c_int, c_uint, *mut c_uint) -> c_int,
    set_frequency: unsafe extern "C" fn(Dev, c_int, u64) -> c_int,
    set_gain_mode: unsafe extern "C" fn(Dev, c_int, c_int) -> c_int,
    set_gain: unsafe extern "C" fn(Dev, c_int, c_int) -> c_int,
    /// To read back what the board runs at (in every libbladeRF 2.x; optional all the same).
    get_gain: Option<unsafe extern "C" fn(Dev, c_int, *mut c_int) -> c_int>,
    get_gain_mode: Option<unsafe extern "C" fn(Dev, c_int, *mut c_int) -> c_int>,
    /// bladeRF 2.0 only (and libbladeRF 2.x).
    set_bias_tee: Option<unsafe extern "C" fn(Dev, c_int, bool) -> c_int>,
    sync_config: unsafe extern "C" fn(Dev, c_int, c_int, c_uint, c_uint, c_uint, c_uint) -> c_int,
    enable_module: unsafe extern "C" fn(Dev, c_int, bool) -> c_int,
    sync_rx: unsafe extern "C" fn(Dev, *mut c_void, c_uint, *mut Metadata, c_uint) -> c_int,
}

// SAFETY: plain C entry points; each device is used from one thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut names = super::candidates("libbladeRF.so.2", "libbladeRF.dylib", &["bladeRF.dll", r"C:\Program Files\bladeRF\x64\bladeRF.dll"]);
        if !cfg!(target_os = "macos") && !cfg!(windows) {
            names.push("libbladeRF.so".into());
        }
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lib, path) = super::load("TRUNK_PRO_BLADERF", &names, "bladeRF", false)?;
        // SAFETY: the signatures are libbladeRF's (libbladeRF.h / bladeRF2.h, 2.x).
        unsafe {
            macro_rules! f {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{path}: {} missing (libbladeRF 2.x is needed): {e}", $n))?
                };
            }
            Ok(Api {
                version: f!("bladerf_version"),
                strerror: f!("bladerf_strerror"),
                get_device_list: f!("bladerf_get_device_list"),
                free_device_list: f!("bladerf_free_device_list"),
                open: f!("bladerf_open"),
                close: f!("bladerf_close"),
                get_board_name: f!("bladerf_get_board_name"),
                get_devinfo: f!("bladerf_get_devinfo"),
                get_sample_rate_range: f!("bladerf_get_sample_rate_range"),
                set_sample_rate: f!("bladerf_set_sample_rate"),
                set_bandwidth: f!("bladerf_set_bandwidth"),
                set_frequency: f!("bladerf_set_frequency"),
                set_gain_mode: f!("bladerf_set_gain_mode"),
                set_gain: f!("bladerf_set_gain"),
                get_gain: lib.get(b"bladerf_get_gain\0").ok().map(|s| *s),
                get_gain_mode: lib.get(b"bladerf_get_gain_mode\0").ok().map(|s| *s),
                set_bias_tee: lib.get(b"bladerf_set_bias_tee\0").ok().map(|s| *s),
                sync_config: f!("bladerf_sync_config"),
                enable_module: f!("bladerf_enable_module"),
                sync_rx: f!("bladerf_sync_rx"),
                _lib: lib,
            })
        }
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Log the gain the board reports (`reported`), and warn when it isn't what `cfg` asks.
fn say_gain(cfg: &BladerfConfig, reported: Option<(c_int, c_int)>) {
    let Some((mode, gain)) = reported else { return };
    let agc_on = mode != GAIN_MGC;
    let text = format!("bladeRF {}: the board reports {}, gain {gain} dB", if cfg.serial.is_empty() { "(first)" } else { &cfg.serial }, if agc_on { "AGC" } else { "manual gain" });
    if agc_on != cfg.agc || (!cfg.agc && (gain as f64 - cfg.gain_db).abs() > 1.0) {
        log::warn!("{text}, not the {} asked for{}", if cfg.agc { "AGC".to_string() } else { format!("{} dB", cfg.gain_db) }, if !cfg.agc && agc_on == cfg.agc { " (outside the board's range here, so clamped)" } else { "" });
    } else {
        log::info!("{text}");
    }
}

/// The gain as `cfg` says: the board's AGC, or manual at `gain_db`.
///
/// SAFETY (callers): `dev` is open.
unsafe fn set_gain(a: &Api, dev: Dev, cfg: &BladerfConfig) -> Result<(), String> {
    unsafe {
        if cfg.agc {
            check(a, "AGC", (a.set_gain_mode)(dev, RX0, GAIN_DEFAULT))
        } else {
            check(a, "manual gain", (a.set_gain_mode)(dev, RX0, GAIN_MGC))?;
            check(a, "gain", (a.set_gain)(dev, RX0, cfg.gain_db.round() as c_int))
        }
    }
}

/// What the board runs at: (gain mode, gain dB), when this libbladeRF can say.
///
/// SAFETY (callers): `dev` is open.
unsafe fn report_gain(a: &Api, dev: Dev) -> Option<(c_int, c_int)> {
    let (gm, gg) = (a.get_gain_mode?, a.get_gain?);
    let (mut mode, mut gain): (c_int, c_int) = (0, 0);
    // SAFETY: the out-pointers are live locals.
    unsafe { (gm(dev, RX0, &mut mode) == 0 && gg(dev, RX0, &mut gain) == 0).then_some((mode, gain)) }
}

fn carr(a: &[c_char]) -> String {
    let bytes: Vec<u8> = a.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Whether libbladeRF could be loaded, and which (for the interface).
pub fn info() -> DriverInfo {
    match api() {
        Ok(a) => {
            let mut v = Version { major: 0, minor: 0, patch: 0, describe: std::ptr::null() };
            // SAFETY: fills the struct.
            unsafe { (a.version)(&mut v) };
            // SAFETY: libbladeRF's static version string (or null).
            let d = unsafe { cstr(v.describe) };
            DriverInfo { loaded: true, detail: format!("libbladeRF {}", if d.is_empty() { format!("{}.{}.{}", v.major, v.minor, v.patch) } else { d }) }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("libbladeRF {e}") },
    }
}

fn check(a: &Api, what: &str, r: c_int) -> Result<(), String> {
    if r == 0 {
        return Ok(());
    }
    Err(format!("bladeRF {what}: {}", unsafe { cstr((a.strerror)(r)) }))
}

/// Its key among the radios [`super::known`].
fn key(serial: &str) -> String {
    format!("bladerf:{serial}")
}

/// Attached bladeRFs: [{serial, label}], and the rates one runs at once it has opened in this
/// run (a bladeRF 2.0 runs at 0.52–61.44 MSPS, the original at up to 40).
pub fn devices() -> Vec<serde_json::Value> {
    let Ok(a) = api() else { return vec![] };
    let found = super::open("bladeRF search", Deadline::Quick, true, move || {
        let mut list: *mut DevInfo = std::ptr::null_mut();
        // SAFETY: libbladeRF allocates `n` entries, freed below.
        let n = unsafe { (a.get_device_list)(&mut list) };
        if n <= 0 || list.is_null() {
            return Ok(vec![]);
        }
        // SAFETY: n entries.
        let out = unsafe { std::slice::from_raw_parts(list, n as usize) }.iter().map(|d| (carr(&d.serial), carr(&d.product), d.usb_bus, d.usb_addr)).collect::<Vec<_>>();
        // SAFETY: the list get_device_list returned.
        unsafe { (a.free_device_list)(list) };
        Ok(out)
    });
    found
        .unwrap_or_default()
        .into_iter()
        .map(|(serial, product, bus, address)| {
            let product = if product.is_empty() { "bladeRF".to_string() } else { product };
            let short = if serial.len() > 12 { &serial[..12] } else { &serial };
            let mut j = serde_json::json!({ "serial": serial, "label": format!("{product} · SN {short}…"), "bus": bus, "address": address });
            if let Some(c) = super::known(&key(&serial)) {
                c.add_to(&mut j);
            }
            j
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct BladerfConfig {
    /// Serial (all 32 hex digits, or enough of the start to be unique); "" = the first.
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    /// The analog low-pass filter; 0 = as wide as the sample rate.
    pub bandwidth_hz: f64,
    /// Overall RX gain in dB (bladeRF 2.0: about -15..60 depending on frequency); ignored with `agc`.
    pub gain_db: f64,
    /// The board's AGC instead of `gain_db`.
    pub agc: bool,
    /// Power an LNA over the RX coax (bladeRF 2.0 only).
    pub bias_tee: bool,
    pub ppm: f64,
}

/// A bladeRF, streamed by [`crate::sdr::run_with`]: reopened after errors with a short back-off.
impl crate::sdr::Radio for BladerfConfig {
    const BACKOFF: Duration = Duration::from_secs(3);
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if let Gain::Db(db) = g {
            self.gain_db = db as f64;
        }
    }
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// Owns the open device so every exit path disables RX and closes it.
struct Device {
    a: &'static Api,
    dev: Dev,
    enabled: bool,
    ppm: f64,
}

impl Tunable for Device {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        // SAFETY: an open device.
        check(self.a, "tune", unsafe { (self.a.set_frequency)(self.dev, RX0, ppm_corrected(hz, self.ppm).round() as u64) })?;
        Ok(Retuned { hz, gap: 0 })
    }
    /// A manual gain (AGC on or off waits for the next start).
    fn set_gain(&mut self, g: Gain) -> Result<u64, String> {
        let Gain::Db(db) = g else { return Ok(0) };
        // SAFETY: an open device.
        unsafe {
            check(self.a, "manual gain", (self.a.set_gain_mode)(self.dev, RX0, GAIN_MGC))?;
            check(self.a, "gain", (self.a.set_gain)(self.dev, RX0, db.round() as c_int))?;
        }
        Ok(0)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: a device opened by this struct.
        unsafe {
            if self.enabled {
                (self.a.enable_module)(self.dev, RX0, false);
            }
            if let (Some(f), true) = (self.a.set_bias_tee, self.enabled) {
                f(self.dev, RX0, false);
            }
            (self.a.close)(self.dev);
        }
    }
}

/// libbladeRF's open string for a serial: "" = the first device.
fn identifier(serial: &str) -> String {
    let s = serial.trim();
    if s.is_empty() {
        String::new()
    } else if s.contains(':') || s.contains('=') {
        // Already a libbladeRF device identifier ("*:serial=…", "libusb:instance=1").
        s.to_string()
    } else {
        format!("*:serial={s}")
    }
}

fn stream_once(cfg: &BladerfConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    let id = CString::new(identifier(&cfg.serial)).map_err(|e| e.to_string())?;
    let first = cfg.serial.trim().is_empty();
    // (Loading its FPGA image can take seconds.)
    let dev = super::open(&format!("bladeRF {}", if first { "(first)" } else { &cfg.serial }), Deadline::Loads, true, move || {
        let mut dev: Dev = std::ptr::null_mut();
        // SAFETY: libbladeRF fills `dev`; the Device closes it.
        let r = unsafe { (a.open)(&mut dev, if first { std::ptr::null() } else { id.as_ptr() }) };
        check(a, "open", r)?;
        Ok(Handed(Device { a, dev, enabled: false, ppm: 0.0 }))
    })?;
    let mut d = dev.0;
    d.ppm = cfg.ppm;
    // SAFETY: libbladeRF calls on the device opened above.
    unsafe {
        let board = cstr((a.get_board_name)(d.dev));

        // What it is and the rates it runs at, for the list of radios and the refusal.
        let mut info: DevInfo = std::mem::zeroed();
        let mut range: *const Range = std::ptr::null();
        if (a.get_devinfo)(d.dev, &mut info) == 0 && (a.get_sample_rate_range)(d.dev, RX0, &mut range) == 0 && !range.is_null() {
            let r = &*range;
            let caps = Caps { model: carr(&info.product), rates: Rates::Range { min: r.min as f64 * r.scale as f64, max: r.max as f64 * r.scale as f64 } };
            super::remember(&key(&carr(&info.serial)), caps.clone());
            if !caps.rates.allows(cfg.rate_hz) {
                let what = if caps.model.is_empty() { "bladeRF".to_string() } else { caps.model.clone() };
                return Err(super::rate_refusal(&what, &caps.rates, cfg.rate_hz));
            }
        }

        let want = cfg.rate_hz.round() as c_uint;
        let mut actual: c_uint = 0;
        check(a, "sample rate", (a.set_sample_rate)(d.dev, RX0, want, &mut actual))?;
        if (actual as f64 - cfg.rate_hz).abs() > 1.0 {
            return Err(format!("bladeRF can't run at {:.6} MSPS (it gave {:.6}); choose another sample rate", cfg.rate_hz / 1e6, actual as f64 / 1e6));
        }
        let bw = if cfg.bandwidth_hz > 0.0 { cfg.bandwidth_hz } else { cfg.rate_hz };
        let mut bw_actual: c_uint = 0;
        check(a, "bandwidth", (a.set_bandwidth)(d.dev, RX0, bw.round() as c_uint, &mut bw_actual))?;
        d.tune(cfg.center_hz)?;
        set_gain(a, d.dev, cfg)?;
        match a.set_bias_tee {
            Some(f) => {
                let r = f(d.dev, RX0, cfg.bias_tee);
                // The original bladeRF has none: only an error when it was asked for.
                if cfg.bias_tee {
                    check(a, "bias-T", r)?;
                }
            }
            None if cfg.bias_tee => return Err("this libbladeRF has no bias-T control (needs 2.x and a bladeRF 2.0)".into()),
            None => {}
        }

        // ~10 ms blocks, a multiple of 1024 samples; buffers of 8192 samples
        // (a multiple of 1024, as the sync interface wants), 32 of them with
        // 16 in flight: ~80 ms of slack at 30 MSPS before an overrun.
        const BUF: c_uint = 8192;
        let per = ((cfg.rate_hz / 100.0) as usize).div_ceil(1024).max(1) * 1024;
        check(a, "stream setup", (a.sync_config)(d.dev, RX_X1, FORMAT_SC16_Q11_META, 32, BUF, 16, 3500))?;
        check(a, "enable RX", (a.enable_module)(d.dev, RX0, true))?;
        d.enabled = true;
        // Again now the receiver runs: enabling it can leave the RFIC's gain as
        // its own (its gain table is reloaded for the band), and then neither the
        // gain nor AGC set beforehand would hold. Then say what the board reports.
        set_gain(a, d.dev, cfg)?;
        let reported = report_gain(a, d.dev);
        log::info!(
            "bladeRF {} ({board}): {:.4} MHz, {:.3} MSPS, bandwidth {:.3} MHz, {}",
            if cfg.serial.is_empty() { "(first)" } else { &cfg.serial },
            cfg.center_hz / 1e6,
            actual as f64 / 1e6,
            bw_actual as f64 / 1e6,
            if cfg.agc { "AGC".to_string() } else { format!("gain {} dB", cfg.gain_db) }
        );
        say_gain(cfg, reported);

        let mut raw = vec![0i16; per * 2];
        let mut next_t: Option<u64> = None;
        let mut timeouts = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Some(c) = ctl {
                match c.apply(&mut d, cfg.center_hz, sink)? {
                    // Samples already queued are from before the change: start over.
                    Applied::Tuned => next_t = None,
                    Applied::Closed => return Ok(()),
                    Applied::Nothing => {}
                }
            }
            let mut md = Metadata { timestamp: 0, flags: META_FLAG_RX_NOW, status: 0, actual_count: 0, reserved: [0; 32] };
            let r = (a.sync_rx)(d.dev, raw.as_mut_ptr() as *mut c_void, per as c_uint, &mut md, 3500);
            if r == ERR_TIMEOUT {
                timeouts += 1;
                if timeouts >= 3 {
                    return Err("bladeRF: no samples for 10 s (unplugged?)".into());
                }
                continue;
            }
            check(a, "receive", r)?;
            let at = std::time::Instant::now();
            timeouts = 0;
            let n = if md.status & META_STATUS_OVERRUN != 0 { (md.actual_count as usize).min(per) } else { per };
            // Samples lost (an overrun) show as a jump in the sample counter.
            let mut dropped = 0u64;
            if let Some(want) = next_t {
                dropped = md.timestamp.saturating_sub(want);
            }
            next_t = Some(md.timestamp + per as u64);
            if md.status & META_STATUS_OVERRUN != 0 {
                // The rest of this block is missing: counted with the next one.
                next_t = Some(md.timestamp + n as u64);
            }
            if n == 0 {
                continue;
            }
            let mut samples = sink.iq_buffer(n);
            samples.vec().extend(raw[..n * 2].as_chunks::<2>().0.iter().map(|&[i, q]| Complex32::new(i as f32 / 2048.0, q as f32 / 2048.0)));
            if !sink.send(crate::sdr::Block::Iq(samples), dropped, at) {
                return Ok(());
            }
        }
        drop(d);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        assert_eq!(identifier(""), "");
        assert_eq!(identifier(" f12ce103 "), "*:serial=f12ce103");
        assert_eq!(identifier("libusb:instance=1"), "libusb:instance=1");
    }

    #[test]
    fn layouts() {
        // libbladeRF.h 2.x: these sizes are what the C compiler gives.
        assert_eq!(std::mem::size_of::<DevInfo>(), 112);
        assert_eq!(std::mem::size_of::<Metadata>(), 56);
    }
}
