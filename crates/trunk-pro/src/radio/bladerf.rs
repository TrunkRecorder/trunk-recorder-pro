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
use crate::sdr::{Applied, Block, Control, Gain, Retuned, Sink, Tunable};

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
/// BLADERF_FORMAT_PACKET_META, in libbladeRF 2.5's numbering (later versions insert PACKED before it).
const FORMAT_PACKET_META: c_int = 2;
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
    /// For the FPGA filter bank (`tool bank-test`).
    load_fpga: Option<unsafe extern "C" fn(Dev, *const c_char) -> c_int>,
    wb_read: Option<unsafe extern "C" fn(Dev, u32, *mut u32) -> c_int>,
    wb_write: Option<unsafe extern "C" fn(Dev, u32, u32) -> c_int>,
    init_stream: Option<unsafe extern "C" fn(*mut *mut c_void, Dev, StreamCb, *mut *mut *mut c_void, usize, c_int, usize, usize, *mut c_void) -> c_int>,
    stream: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>,
    deinit_stream: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// bladerf_stream_cb.
type StreamCb = unsafe extern "C" fn(Dev, *mut c_void, *mut Metadata, *mut c_void, usize, *mut c_void) -> *mut c_void;

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
                load_fpga: lib.get(b"bladerf_load_fpga\0").ok().map(|s| *s),
                wb_read: lib.get(b"bladerf_wishbone_master_read\0").ok().map(|s| *s),
                wb_write: lib.get(b"bladerf_wishbone_master_write\0").ok().map(|s| *s),
                init_stream: lib.get(b"bladerf_init_stream\0").ok().map(|s| *s),
                stream: lib.get(b"bladerf_stream\0").ok().map(|s| *s),
                deinit_stream: lib.get(b"bladerf_deinit_stream\0").ok().map(|s| *s),
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
    /// A filter bank image to load into the FPGA's RAM: then this stream is
    /// its packets ([`Block::Bank`]), not wideband samples.
    pub fpga_image: Option<String>,
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
        if self.fpga_image.is_some() {
            return stream_bank(self, sink, stop, ctl);
        }
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

/// Filter bank packets arriving from libbladeRF's stream callback: put back
/// together from USB transfers and handed to `handler` whole.
struct BankRx {
    start: std::time::Instant,
    /// How long to run; INFINITY: until `stop`.
    seconds: f64,
    stop: AtomicBool,
    handler: Box<dyn FnMut(&[u8])>,
    bad_parse: u64,
    /// Bytes of a packet still arriving.
    partial: Vec<u8>,
}

impl BankRx {
    fn new(seconds: f64, handler: Box<dyn FnMut(&[u8])>) -> Self {
        BankRx { start: std::time::Instant::now(), seconds, stop: AtomicBool::new(false), handler, bad_parse: 0, partial: Vec::new() }
    }

    /// One USB transfer. A packet arrives in pieces of 512 DWORDs (2048 bytes,
    /// libbladeRF's message size), so they're put back together here: bladeRF's
    /// 16-byte header gives the payload's length.
    fn on_buffer(&mut self, bytes: &[u8]) {
        use trunk_core::dsp::bank_wire::{CORE_ID, HEADER_BYTES};
        self.partial.extend_from_slice(bytes);
        while self.partial.len() >= HEADER_BYTES {
            if self.partial[3] != CORE_ID {
                self.bad_parse += 1;
                self.partial.clear();
                return;
            }
            let need = HEADER_BYTES + 4 * u16::from_le_bytes([self.partial[0], self.partial[1]]) as usize;
            if self.partial.len() < need {
                return;
            }
            (self.handler)(&self.partial[..need]);
            // The rest of the last piece is padding.
            self.partial.clear();
        }
    }
}

/// How the capture tool's numbers come out.
#[derive(Default)]
struct CaptureCount {
    packets: u64,
    skipped: u64,
    bad_parse: u64,
    bad_time: u64,
    lost: u64,
    last: Option<(u32, u64)>,
    error: Option<String>,
}

impl CaptureCount {
    /// One packet for the file. Blocks before the table took effect aren't
    /// kept: they read as a gap, which keeps the time right.
    fn on_packet(&mut self, out: &mut dyn std::io::Write, bins: usize, bytes: &[u8]) {
        use trunk_core::dsp::bank_wire::{parse, Body};
        /// Samples in a block of 48 hops.
        const SAMPLES_PER_BLOCK: u64 = 12288;
        let Ok((p, _)) = parse(bytes) else {
            self.bad_parse += 1;
            return;
        };
        if p.generation != 1 || !matches!(&p.body, Body::Channels { bins: n, .. } if *n == bins) {
            self.skipped += 1;
            return;
        }
        self.packets += 1;
        if let Err(e) = out.write_all(bytes) {
            self.error.get_or_insert(e.to_string());
        }
        if let Some((block, ts)) = self.last {
            let gap = p.block.wrapping_sub(block) as u64;
            self.lost += gap.wrapping_sub(1);
            // The header's written with the packet's end: a few clocks of slack.
            if p.timestamp.wrapping_sub(ts).abs_diff(gap * SAMPLES_PER_BLOCK) > 8 {
                self.bad_time += 1;
            }
        }
        self.last = Some((p.block, p.timestamp));
    }
}

/// Gives the finished buffer back for the next transfer, so nothing needs a queue.
unsafe extern "C" fn bank_rx_cb(_dev: Dev, _stream: *mut c_void, _meta: *mut Metadata, samples: *mut c_void, n: usize, user: *mut c_void) -> *mut c_void {
    // SAFETY: `user` is the BankRx bank_capture keeps alive for the stream, and `samples` a finished buffer of n DWORDs.
    let st = unsafe { &mut *(user as *mut BankRx) };
    if !samples.is_null() && n > 0 {
        st.on_buffer(unsafe { std::slice::from_raw_parts(samples as *const u8, n * 4) });
    }
    if st.stop.load(Ordering::Relaxed) || st.start.elapsed().as_secs_f64() >= st.seconds {
        return std::ptr::null_mut();
    }
    samples
}

/// What the filter bank needs of libbladeRF.
struct BankCalls {
    load: unsafe extern "C" fn(Dev, *const c_char) -> c_int,
    rd: unsafe extern "C" fn(Dev, u32, *mut u32) -> c_int,
    wr: unsafe extern "C" fn(Dev, u32, u32) -> c_int,
    init: unsafe extern "C" fn(*mut *mut c_void, Dev, StreamCb, *mut *mut *mut c_void, usize, c_int, usize, usize, *mut c_void) -> c_int,
    run: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    deinit: unsafe extern "C" fn(*mut c_void),
}

/// Bank registers (`fb_regs.vhd`), by their byte addresses.
const REG_ID: u32 = 0x00;
const REG_CONTROL: u32 = 0x04;
const REG_COUNT: u32 = 0x08;
const REG_COMMIT: u32 = 0x0C;
const REG_SATURATED: u32 = 0x10;
const REG_DROPPED: u32 = 0x14;
const REG_RX_RATE: u32 = 0x18;
const REG_TABLE: u32 = 0x400;
/// The output gain shift the radios run with (issue 06's status).
const GAIN_SHIFT: u32 = 2;
/// After the stream starts, before RX is enabled: the stream sets the format as it starts.
const BANK_STARTUP: Duration = Duration::from_millis(300);
/// Samples in a filter bank block.
const SAMPLES_PER_BLOCK: u64 = 12288;

impl BankCalls {
    fn of(a: &Api) -> Result<BankCalls, String> {
        match (a.load_fpga, a.wb_read, a.wb_write, a.init_stream, a.stream, a.deinit_stream) {
            (Some(load), Some(rd), Some(wr), Some(init), Some(run), Some(deinit)) => Ok(BankCalls { load, rd, wr, init, run, deinit }),
            _ => Err("this libbladeRF has no Wishbone access or async streaming (2.4 or later is needed)".into()),
        }
    }

    /// # Safety
    /// `dev` is open.
    unsafe fn read(&self, a: &Api, dev: Dev, addr: u32) -> Result<u32, String> {
        let mut v = 0;
        check(a, "register read", unsafe { (self.rd)(dev, addr, &mut v) })?;
        Ok(v)
    }

    /// # Safety
    /// `dev` is open.
    unsafe fn write(&self, a: &Api, dev: Dev, addr: u32, v: u32) -> Result<(), String> {
        check(a, "register write", unsafe { (self.wr)(dev, addr, v) })
    }

    /// Open the board, load the image into RAM, and open it again: setting
    /// the sample rate in the session that loaded it times out in the RFIC's calibration.
    /// # Safety
    /// libbladeRF's calls.
    unsafe fn open_loaded(&self, a: &'static Api, serial: &str, image: &str) -> Result<Device, String> {
        let id = CString::new(identifier(serial)).map_err(|e| e.to_string())?;
        let path = CString::new(image).map_err(|e| e.to_string())?;
        let id_ptr = if serial.trim().is_empty() { std::ptr::null() } else { id.as_ptr() };
        let mut dev: Dev = std::ptr::null_mut();
        check(a, "open", unsafe { (a.open)(&mut dev, id_ptr) })?;
        let d = Device { a, dev, enabled: false, ppm: 0.0 };
        check(a, "load FPGA", unsafe { (self.load)(d.dev, path.as_ptr()) })?;
        drop(d);
        check(a, "reopen", unsafe { (a.open)(&mut dev, id_ptr) })?;
        let d = Device { a, dev, enabled: false, ppm: 0.0 };
        let version = unsafe { self.read(a, d.dev, REG_ID) }?;
        if version >> 24 != trunk_core::dsp::bank_wire::CORE_ID as u32 {
            return Err(format!("{image} isn't a filter bank image: its core id register reads {version:#x}"));
        }
        Ok(d)
    }

    /// The stream running on its own thread, delivering to `st`.
    /// # Safety
    /// `dev` stays open and `st` alive until the handle is joined.
    unsafe fn start(&self, a: &Api, dev: Dev, st: *mut BankRx) -> Result<(*mut c_void, std::thread::JoinHandle<c_int>), String> {
        let (mut stream, mut buffers): (*mut c_void, *mut *mut c_void) = (std::ptr::null_mut(), std::ptr::null_mut());
        check(a, "stream setup", unsafe { (self.init)(&mut stream, dev, bank_rx_cb, &mut buffers, 32, FORMAT_PACKET_META, 2048, 16, st as *mut c_void) })?;
        let (run, addr) = (self.run, stream as usize);
        Ok((stream, std::thread::spawn(move || unsafe { run(addr as *mut c_void, RX_X1) })))
    }

    /// Write `bins` to the pending table and commit them as `generation`,
    /// taking effect at the next block.
    /// # Safety
    /// `dev` is open.
    unsafe fn commit(&self, a: &Api, dev: Dev, bins: &[u16], generation: u8) -> Result<(), String> {
        for (i, b) in bins.iter().enumerate() {
            unsafe { self.write(a, dev, REG_TABLE + 4 * i as u32, *b as u32) }?;
        }
        unsafe {
            self.write(a, dev, REG_COUNT, bins.len() as u32)?;
            self.write(a, dev, REG_COMMIT, generation as u32)
        }
    }
}

/// Recording what a bladeRF's filter bank sends.
pub struct BankCapture<'a> {
    pub serial: &'a str,
    pub image: &'a str,
    pub center_hz: f64,
    pub bins: &'a [u16],
    pub gain_db: i32,
    pub gain_shift: u32,
    pub seconds: f64,
    pub out: &'a str,
}

/// `tool bank-capture`: load the filter bank image into RAM and record what it sends.
pub fn bank_capture(c: &BankCapture) -> Result<serde_json::Value, String> {
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Instant;
    use trunk_core::dsp::bank_wire;
    const RATE: u32 = 8_000_000;
    let a = api()?;
    let calls = BankCalls::of(a)?;
    if c.bins.is_empty() || c.bins.len() > bank_wire::MAX_BINS {
        return Err(format!("{} bins: 1 to {} can be sent", c.bins.len(), bank_wire::MAX_BINS));
    }
    let file = std::fs::File::create(c.out).map_err(|e| format!("{}: {e}", c.out))?;
    let out = Arc::new(Mutex::new((std::io::BufWriter::new(file), CaptureCount::default())));
    // SAFETY: libbladeRF calls on the device opened here, closed by the Device's drop.
    // The stream thread and callback only use `st`, which outlives the join below.
    unsafe {
        let mut d = calls.open_loaded(a, c.serial, c.image)?;
        let mut actual: c_uint = 0;
        check(a, "sample rate", (a.set_sample_rate)(d.dev, RX0, RATE, &mut actual))?;
        check(a, "bandwidth", (a.set_bandwidth)(d.dev, RX0, RATE, &mut actual))?;
        check(a, "frequency", (a.set_frequency)(d.dev, RX0, c.center_hz.round() as u64))?;
        check(a, "gain mode", (a.set_gain_mode)(d.dev, RX0, GAIN_MGC))?;
        check(a, "gain", (a.set_gain)(d.dev, RX0, c.gain_db))?;

        let (bins, sink) = (c.bins.len(), Arc::clone(&out));
        let mut st = Box::new(BankRx::new(
            c.seconds + BANK_STARTUP.as_secs_f64(),
            Box::new(move |bytes| {
                let mut g = sink.lock().unwrap();
                let (w, n) = &mut *g;
                n.on_packet(w, bins, bytes);
            }),
        ));
        let (stream, streamer) = calls.start(a, d.dev, &mut *st)?;
        std::thread::sleep(BANK_STARTUP);
        let setup = (|| -> Result<u32, String> {
            check(a, "enable RX", (a.enable_module)(d.dev, RX0, true))?;
            check(a, "gain", (a.set_gain)(d.dev, RX0, c.gain_db))?;
            // Starting RX clears these, so they're written after it, the enable last.
            calls.commit(a, d.dev, c.bins, 1)?;
            calls.write(a, d.dev, REG_CONTROL, 1 | c.gain_shift << 4)?;
            calls.read(a, d.dev, REG_DROPPED)
        })();
        d.enabled = setup.is_ok();
        if setup.is_err() {
            st.stop.store(true, Ordering::Relaxed);
        }
        let t = Instant::now();
        let r = streamer.join().unwrap_or(ERR_TIMEOUT);
        let seconds = t.elapsed().as_secs_f64();
        (calls.deinit)(stream);
        let dropped_before = setup?;
        check(a, "stream", r)?;
        let (saturated, dropped, rx_rate) = (calls.read(a, d.dev, REG_SATURATED)?, calls.read(a, d.dev, REG_DROPPED)?, calls.read(a, d.dev, REG_RX_RATE)?);
        let bad_transfers = st.bad_parse;
        drop(st);
        let mut g = out.lock().unwrap();
        g.0.flush().map_err(|e| format!("{}: {e}", c.out))?;
        if let Some(e) = &g.1.error {
            return Err(format!("{}: {e}", c.out));
        }
        let n = &g.1;
        let list: Vec<String> = c.bins.iter().map(u16::to_string).collect();
        std::fs::write(format!("{}.bins", c.out), list.join(",") + "\n").map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "serial": c.serial, "bins": c.bins.len(), "seconds": seconds, "out": c.out, "gainShift": c.gain_shift,
            "packets": n.packets, "skipped": n.skipped, "badParse": n.bad_parse + bad_transfers, "badTime": n.bad_time, "blocksLost": n.lost,
            "fpgaDropped": dropped.wrapping_sub(dropped_before), "saturated": saturated, "rxClockHz": rx_rate as u64 * 256,
        }))
    }
}

/// One packet on its way from the stream callback to the driver's thread.
struct BankPacket {
    packet: trunk_core::dsp::bank_wire::Packet,
    at: std::time::Instant,
}

/// The tables committed lately, by generation: a packet says which it was made with.
struct BankTables {
    recent: std::collections::VecDeque<(u8, Arc<[u16]>)>,
    next: u8,
}

impl BankTables {
    fn new() -> BankTables {
        let mut recent = std::collections::VecDeque::new();
        // Generation 0, from reset, has no bins.
        recent.push_back((0, Arc::from(Vec::new())));
        BankTables { recent, next: 0 }
    }

    /// The generation for `bins` (0 is reset's, so it's skipped on wrapping).
    fn add(&mut self, bins: &[u16]) -> u8 {
        self.next = self.next.wrapping_add(1).max(1);
        self.recent.push_back((self.next, Arc::from(bins)));
        if self.recent.len() > 8 {
            self.recent.pop_front();
        }
        self.next
    }

    fn bins(&self, generation: u8) -> Option<Arc<[u16]>> {
        self.recent.iter().rev().find(|(g, _)| *g == generation).map(|(_, b)| Arc::clone(b))
    }
}

/// The stream with a filter bank: [`Block::Bank`]s instead of wideband samples.
fn stream_bank(cfg: &BladerfConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    use trunk_core::dsp::bank_wire::{parse, Body};
    use std::sync::mpsc;
    use std::time::Instant;
    let a = api()?;
    let calls = BankCalls::of(a)?;
    let image = cfg.fpga_image.as_deref().ok_or("no filter bank image")?;
    let center = cfg.center_hz;
    let (d_, serial) = {
        let (image, serial) = (image.to_string(), cfg.serial.clone());
        let calls = BankCalls::of(a)?;
        // (Loading the image takes seconds, and a stuck radio shouldn't hang the thread for good.)
        let d = super::open(&format!("bladeRF {serial}"), Deadline::Slow, true, move || {
            // SAFETY: libbladeRF's calls.
            let d = unsafe { calls.open_loaded(a, &serial, &image) }?;
            Ok(Handed(d))
        })?;
        (d, cfg.serial.clone())
    };
    let mut d = d_.0;
    d.ppm = cfg.ppm;
    let (tx, rx) = mpsc::sync_channel::<BankPacket>(256);
    // SAFETY: libbladeRF calls on the device opened above, which outlives the stream thread;
    // the callback only uses `st`, kept alive until that thread is joined.
    unsafe {
        let mut actual: c_uint = 0;
        check(a, "sample rate", (a.set_sample_rate)(d.dev, RX0, cfg.rate_hz.round() as c_uint, &mut actual))?;
        let bw = if cfg.bandwidth_hz > 0.0 { cfg.bandwidth_hz } else { cfg.rate_hz };
        check(a, "bandwidth", (a.set_bandwidth)(d.dev, RX0, bw.round() as c_uint, &mut actual))?;
        d.tune(center)?;
        set_gain(a, d.dev, cfg)?;
        if let Some(f) = a.set_bias_tee {
            let r = f(d.dev, RX0, cfg.bias_tee);
            if cfg.bias_tee {
                check(a, "bias-T", r)?;
            }
        }
        // On the callback's thread: parsed and queued; if the queue is full it's a lost block.
        let mut st = Box::new(BankRx::new(
            f64::INFINITY,
            Box::new(move |bytes| {
                if let Ok((packet, _)) = parse(bytes) {
                    let _ = tx.try_send(BankPacket { packet, at: Instant::now() });
                }
            }),
        ));
        let (stream, streamer) = calls.start(a, d.dev, &mut *st)?;
        std::thread::sleep(BANK_STARTUP);
        let mut tables = BankTables::new();
        let result = (|| -> Result<(), String> {
            check(a, "enable RX", (a.enable_module)(d.dev, RX0, true))?;
            d.enabled = true;
            set_gain(a, d.dev, cfg)?;
            // Starting RX clears the registers, so they're written now, the enable last.
            let first = ctl.and_then(|c| c.bins.lock().unwrap().take().or_else(|| c.current_bins.lock().unwrap().clone()));
            if let Some(b) = &first {
                calls.commit(a, d.dev, b, tables.add(b))?;
                if let Some(c) = ctl {
                    *c.current_bins.lock().unwrap() = Some(b.clone());
                }
            }
            calls.write(a, d.dev, REG_CONTROL, 1 | GAIN_SHIFT << 4)?;
            let version = calls.read(a, d.dev, REG_ID)?;
            log::info!("bladeRF {serial}: filter bank {version:#x}, {:.4} MHz, {} bins", center / 1e6, first.as_ref().map_or(0, Vec::len));
            let mut last_block = None;
            let (mut silent_since, mut checked, mut saturated, mut dropped) = (Instant::now(), Instant::now(), calls.read(a, d.dev, REG_SATURATED)?, calls.read(a, d.dev, REG_DROPPED)?);
            while !stop.load(Ordering::Relaxed) {
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(BankPacket { packet: p, at }) => {
                        silent_since = at;
                        let lost = last_block.map_or(0, |b: u32| p.block.wrapping_sub(b).wrapping_sub(1) as u64);
                        last_block = Some(p.block);
                        let (Some(bins), Body::Channels { bins: n, iq }) = (tables.bins(p.generation), p.body) else { continue };
                        if n != bins.len() {
                            continue;
                        }
                        let iq = sink.bank_buffer(&iq);
                        if !sink.send(Block::Bank { bins, iq }, lost * SAMPLES_PER_BLOCK, at) {
                            return Ok(());
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) if silent_since.elapsed() > Duration::from_secs(10) => return Err("bladeRF: no packets for 10 s (unplugged?)".into()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err("bladeRF: the stream stopped".into()),
                }
                if let Some(c) = ctl {
                    if let Some(b) = c.bins.lock().unwrap().take() {
                        calls.commit(a, d.dev, &b, tables.add(&b))?;
                        *c.current_bins.lock().unwrap() = Some(b);
                    }
                    match c.apply(&mut d, center, sink)? {
                        Applied::Tuned => last_block = None,
                        Applied::Closed => return Ok(()),
                        Applied::Nothing => {}
                    }
                }
                // Now and then: the core's own counters, for a live run to show them.
                if checked.elapsed() > Duration::from_secs(5) {
                    checked = Instant::now();
                    let (s, l) = (calls.read(a, d.dev, REG_SATURATED)?, calls.read(a, d.dev, REG_DROPPED)?);
                    if s != saturated || l != dropped {
                        log::warn!("bladeRF {serial}: filter bank: {} more saturated, {} more blocks dropped", s.wrapping_sub(saturated), l.wrapping_sub(dropped));
                    }
                    (saturated, dropped) = (s, l);
                }
            }
            Ok(())
        })();
        st.stop.store(true, Ordering::Relaxed);
        let _ = streamer.join();
        (calls.deinit)(stream);
        result
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
    fn packets_cut_into_usb_pieces_are_put_back_together() {
        use trunk_core::dsp::bank_wire::{parse, Body, Packet};
        use std::sync::{Arc, Mutex};
        let kept = Arc::new(Mutex::new((Vec::new(), CaptureCount::default())));
        let sink = Arc::clone(&kept);
        let mut rx = BankRx::new(1.0, Box::new(move |b| {
            let mut g = sink.lock().unwrap();
            let (w, n): &mut (Vec<u8>, CaptureCount) = &mut g;
            n.on_packet(w, 42, b);
        }));
        let packet = |block: u32, generation| {
            let iq: Vec<[i16; 2]> = (0..48 * 42).map(|k| [k as u16 as i16, block as i16]).collect();
            let mut bytes = Packet { timestamp: 12288 * block as u64, generation, block, body: Body::Channels { bins: 42, iq } }.encode();
            // Padded to whole pieces, as a transfer is.
            bytes.resize(bytes.len().next_multiple_of(2048), 0);
            bytes
        };
        // The first is from before the table took effect: not kept.
        let stream: Vec<u8> = (0..3).flat_map(|b| packet(b, if b == 0 { 0 } else { 1 })).collect();
        for piece in stream.chunks(2048) {
            rx.on_buffer(piece);
        }
        let count = |f: &dyn Fn(&CaptureCount) -> u64| f(&kept.lock().unwrap().1);
        assert_eq!((count(&|n| n.packets), count(&|n| n.skipped), count(&|n| n.bad_parse), count(&|n| n.bad_time), count(&|n| n.lost)), (2, 1, 0, 0, 0));
        let bytes = kept.lock().unwrap().0.clone();
        let (first, more) = parse(&bytes).unwrap();
        assert_eq!((first.block, parse(more).unwrap().0.block), (1, 2));
        // A piece that isn't a packet's start is dropped, and the next packet still reads.
        rx.on_buffer(&[0xee; 2048]);
        assert_eq!(rx.bad_parse, 1);
        rx.on_buffer(&packet(3, 1));
        assert_eq!((count(&|n| n.packets), count(&|n| n.lost)), (3, 0));
    }

    #[test]
    fn a_packet_finds_the_table_it_was_made_with() {
        let mut t = BankTables::new();
        assert_eq!(t.bins(0).as_deref(), Some(&[][..]));
        let one = t.add(&[1, 2]);
        let two = t.add(&[3]);
        assert_eq!((one, two), (1, 2));
        assert_eq!(t.bins(one).as_deref(), Some(&[1u16, 2][..]));
        assert_eq!(t.bins(7), None);
        // Generation 0 is never reused, and old tables are forgotten.
        for _ in 0..300 {
            let g = t.add(&[9]);
            assert_ne!(g, 0);
        }
        assert_eq!(t.bins(0), None);
    }

    #[test]
    fn layouts() {
        // libbladeRF.h 2.x: these sizes are what the C compiler gives.
        assert_eq!(std::mem::size_of::<DevInfo>(), 112);
        assert_eq!(std::mem::size_of::<Metadata>(), 56);
    }
}
