//! USRP through UHD's C API (uhd.h), loaded at run time. Needs UHD installed:
//! `brew install uhd`, `apt install libuhd4.x` (Debian/Ubuntu: `libuhd-dev`
//! or the Ettus PPA), or the Ettus installer on Windows — plus the FPGA
//! images (`uhd_images_downloader`) that UHD needs to open a device.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libloading::Library;
use num_complex::Complex32;

use super::DriverInfo;
use crate::sdr::{Control, SourceMsg};

type Handle = *mut c_void;
type Err = c_int;

#[repr(C)]
struct TuneRequest {
    target_freq: f64,
    rf_freq_policy: c_int,
    rf_freq: f64,
    dsp_freq_policy: c_int,
    dsp_freq: f64,
    args: *mut c_char,
}
#[repr(C)]
#[derive(Default)]
struct TuneResult {
    clipped_rf_freq: f64,
    target_rf_freq: f64,
    actual_rf_freq: f64,
    target_dsp_freq: f64,
    actual_dsp_freq: f64,
}
#[repr(C)]
struct StreamArgs {
    cpu_format: *mut c_char,
    otw_format: *mut c_char,
    args: *mut c_char,
    channel_list: *mut usize,
    n_channels: c_int,
}
#[repr(C)]
struct StreamCmd {
    stream_mode: c_int,
    num_samps: usize,
    stream_now: bool,
    time_spec_full_secs: i64,
    time_spec_frac_secs: f64,
}

const POLICY_AUTO: c_int = 65;
const MODE_START: c_int = 97;
const MODE_STOP: c_int = 111;
const MD_NONE: c_int = 0x0;
const MD_TIMEOUT: c_int = 0x1;
const MD_OVERFLOW: c_int = 0x8;

struct Api {
    _lib: Library,
    get_last_error: unsafe extern "C" fn(*mut c_char, usize) -> Err,
    get_version_string: Option<unsafe extern "C" fn(*mut c_char, usize) -> Err>,
    string_vector_make: unsafe extern "C" fn(*mut Handle) -> Err,
    string_vector_free: unsafe extern "C" fn(*mut Handle) -> Err,
    string_vector_size: unsafe extern "C" fn(Handle, *mut usize) -> Err,
    string_vector_at: unsafe extern "C" fn(Handle, usize, *mut c_char, usize) -> Err,
    usrp_find: unsafe extern "C" fn(*const c_char, *mut Handle) -> Err,
    usrp_make: unsafe extern "C" fn(*mut Handle, *const c_char) -> Err,
    usrp_free: unsafe extern "C" fn(*mut Handle) -> Err,
    set_rx_rate: unsafe extern "C" fn(Handle, f64, usize) -> Err,
    get_rx_rate: unsafe extern "C" fn(Handle, usize, *mut f64) -> Err,
    set_rx_freq: unsafe extern "C" fn(Handle, *mut TuneRequest, usize, *mut TuneResult) -> Err,
    set_rx_gain: unsafe extern "C" fn(Handle, f64, usize, *const c_char) -> Err,
    set_rx_antenna: unsafe extern "C" fn(Handle, *const c_char, usize) -> Err,
    rx_streamer_make: unsafe extern "C" fn(*mut Handle) -> Err,
    rx_streamer_free: unsafe extern "C" fn(*mut Handle) -> Err,
    get_rx_stream: unsafe extern "C" fn(Handle, *mut StreamArgs, Handle) -> Err,
    rx_streamer_max_num_samps: unsafe extern "C" fn(Handle, *mut usize) -> Err,
    rx_streamer_issue_stream_cmd: unsafe extern "C" fn(Handle, *const StreamCmd) -> Err,
    rx_streamer_recv: unsafe extern "C" fn(Handle, *mut *mut c_void, usize, *mut Handle, f64, bool, *mut usize) -> Err,
    rx_metadata_make: unsafe extern "C" fn(*mut Handle) -> Err,
    rx_metadata_free: unsafe extern "C" fn(*mut Handle) -> Err,
    rx_metadata_error_code: unsafe extern "C" fn(Handle, *mut c_int) -> Err,
    rx_metadata_has_time_spec: unsafe extern "C" fn(Handle, *mut bool) -> Err,
    rx_metadata_time_spec: unsafe extern "C" fn(Handle, *mut i64, *mut f64) -> Err,
}

// SAFETY: the function pointers are plain C entry points; UHD objects are
// used from one thread at a time.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<(Api, String), String>> = OnceLock::new();
    API.get_or_init(|| {
        let names = super::candidates("libuhd.so", "libuhd.dylib", &["uhd.dll", r"C:\Program Files\UHD\bin\uhd.dll"]);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (lib, path) = super::load("TRUNK_PRO_UHD", &names, "uhd")?;
        // SAFETY: the signatures are UHD's C API (uhd.h, UHD 3.15 – 4.x).
        unsafe {
            macro_rules! f {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{path}: {} missing: {e}", $n))?
                };
            }
            let api = Api {
                get_last_error: f!("uhd_get_last_error"),
                get_version_string: lib.get(b"uhd_get_version_string\0").ok().map(|s| *s),
                string_vector_make: f!("uhd_string_vector_make"),
                string_vector_free: f!("uhd_string_vector_free"),
                string_vector_size: f!("uhd_string_vector_size"),
                string_vector_at: f!("uhd_string_vector_at"),
                usrp_find: f!("uhd_usrp_find"),
                usrp_make: f!("uhd_usrp_make"),
                usrp_free: f!("uhd_usrp_free"),
                set_rx_rate: f!("uhd_usrp_set_rx_rate"),
                get_rx_rate: f!("uhd_usrp_get_rx_rate"),
                set_rx_freq: f!("uhd_usrp_set_rx_freq"),
                set_rx_gain: f!("uhd_usrp_set_rx_gain"),
                set_rx_antenna: f!("uhd_usrp_set_rx_antenna"),
                rx_streamer_make: f!("uhd_rx_streamer_make"),
                rx_streamer_free: f!("uhd_rx_streamer_free"),
                get_rx_stream: f!("uhd_usrp_get_rx_stream"),
                rx_streamer_max_num_samps: f!("uhd_rx_streamer_max_num_samps"),
                rx_streamer_issue_stream_cmd: f!("uhd_rx_streamer_issue_stream_cmd"),
                rx_streamer_recv: f!("uhd_rx_streamer_recv"),
                rx_metadata_make: f!("uhd_rx_metadata_make"),
                rx_metadata_free: f!("uhd_rx_metadata_free"),
                rx_metadata_error_code: f!("uhd_rx_metadata_error_code"),
                rx_metadata_has_time_spec: f!("uhd_rx_metadata_has_time_spec"),
                rx_metadata_time_spec: f!("uhd_rx_metadata_time_spec"),
                _lib: lib,
            };
            Ok((api, path))
        }
    })
    .as_ref()
    .map(|(a, _)| a)
    .map_err(|e| e.clone())
}

/// Whether UHD could be loaded, and which (for the interface).
pub fn info() -> DriverInfo {
    match api() {
        Ok(a) => {
            let mut buf = [0 as c_char; 128];
            let v = a.get_version_string.map(|f| {
                // SAFETY: buffer and its length.
                unsafe { f(buf.as_mut_ptr(), buf.len()) };
                cstr(&buf)
            });
            DriverInfo { loaded: true, detail: format!("UHD {}", v.unwrap_or_else(|| "(version unknown)".into())) }
        }
        Err(e) => DriverInfo { loaded: false, detail: format!("UHD {e}") },
    }
}

fn cstr(buf: &[c_char]) -> String {
    // SAFETY: UHD NUL-terminates within the buffer we gave it.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn check(a: &Api, what: &str, e: Err) -> Result<(), String> {
    if e == 0 {
        return Ok(());
    }
    let mut buf = [0 as c_char; 512];
    // SAFETY: buffer and its length.
    unsafe { (a.get_last_error)(buf.as_mut_ptr(), buf.len()) };
    let msg = cstr(&buf);
    Err(format!("USRP {what}: {}", if msg.is_empty() { format!("UHD error {e}") } else { msg }))
}

/// USRPs UHD can see, as device-argument strings ("type=b200,serial=…,…").
/// This is a real device search (USB and network broadcast): it can take a
/// few seconds, so it runs only when asked.
pub fn find(args: &str) -> Result<Vec<String>, String> {
    let a = api()?;
    let c = CString::new(args).map_err(|e| e.to_string())?;
    let mut v: Handle = std::ptr::null_mut();
    // SAFETY: C API calls with valid handles and buffers.
    unsafe {
        check(a, "find", (a.string_vector_make)(&mut v))?;
        let r = check(a, "find", (a.usrp_find)(c.as_ptr(), &mut v));
        let mut n = 0usize;
        (a.string_vector_size)(v, &mut n);
        let mut out = Vec::new();
        for i in 0..n {
            let mut buf = [0 as c_char; 512];
            if (a.string_vector_at)(v, i, buf.as_mut_ptr(), buf.len()) == 0 {
                out.push(cstr(&buf));
            }
        }
        (a.string_vector_free)(&mut v);
        r.map(|_| out)
    }
}

#[derive(Clone, Debug)]
pub struct UsrpConfig {
    pub args: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    pub gain_db: f64,
    pub antenna: String,
    pub ppm: f64,
}

/// Stream a USRP until `stop`; reopens after errors with a short back-off.
pub fn run(source: usize, cfg: UsrpConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    run_with(source, cfg, tx, stop, None)
}

/// [`run`], retuned on request.
pub fn run_with(source: usize, mut cfg: UsrpConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<Control>>) {
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

/// Owns the UHD objects so every exit path frees them.
struct Session<'a> {
    a: &'a Api,
    usrp: Handle,
    rx: Handle,
    md: Handle,
    streaming: bool,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        // SAFETY: handles made by this session (or null).
        unsafe {
            if self.streaming {
                let cmd = StreamCmd { stream_mode: MODE_STOP, num_samps: 0, stream_now: true, time_spec_full_secs: 0, time_spec_frac_secs: 0.0 };
                (self.a.rx_streamer_issue_stream_cmd)(self.rx, &cmd);
            }
            if !self.md.is_null() {
                (self.a.rx_metadata_free)(&mut self.md);
            }
            if !self.rx.is_null() {
                (self.a.rx_streamer_free)(&mut self.rx);
            }
            if !self.usrp.is_null() {
                (self.a.usrp_free)(&mut self.usrp);
            }
        }
    }
}

fn stream_once(source: usize, cfg: &UsrpConfig, tx: &SyncSender<SourceMsg>, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let a = api()?;
    let mut s = Session { a, usrp: std::ptr::null_mut(), rx: std::ptr::null_mut(), md: std::ptr::null_mut(), streaming: false };
    let args = CString::new(cfg.args.as_str()).map_err(|e| e.to_string())?;
    // SAFETY: UHD C API calls on handles this session owns.
    unsafe {
        check(a, "open", (a.usrp_make)(&mut s.usrp, args.as_ptr()))?;
        check(a, "set rate", (a.set_rx_rate)(s.usrp, cfg.rate_hz, 0))?;
        let mut actual = 0.0;
        check(a, "get rate", (a.get_rx_rate)(s.usrp, 0, &mut actual))?;
        if (actual - cfg.rate_hz).abs() > 1.0 {
            return Err(format!("USRP can't run at {:.6} MSPS (it offers {:.6}); set that sample rate", cfg.rate_hz / 1e6, actual / 1e6));
        }
        if !cfg.antenna.is_empty() {
            let ant = CString::new(cfg.antenna.as_str()).map_err(|e| e.to_string())?;
            check(a, "set antenna", (a.set_rx_antenna)(s.usrp, ant.as_ptr(), 0))?;
        }
        check(a, "set gain", (a.set_rx_gain)(s.usrp, cfg.gain_db, 0, c"".as_ptr()))?;
        // A reference off by `ppm` makes a requested f come out at f·(1+ppm).
        let mut req = TuneRequest {
            target_freq: cfg.center_hz / (1.0 + cfg.ppm * 1e-6),
            rf_freq_policy: POLICY_AUTO,
            rf_freq: 0.0,
            dsp_freq_policy: POLICY_AUTO,
            dsp_freq: 0.0,
            args: std::ptr::null_mut(),
        };
        let mut res = TuneResult::default();
        check(a, "tune", (a.set_rx_freq)(s.usrp, &mut req, 0, &mut res))?;

        let (cpu, otw, sargs) = (c"fc32".to_owned(), c"sc16".to_owned(), c"".to_owned());
        let mut chan = [0usize];
        let mut sa = StreamArgs {
            cpu_format: cpu.as_ptr() as *mut c_char,
            otw_format: otw.as_ptr() as *mut c_char,
            args: sargs.as_ptr() as *mut c_char,
            channel_list: chan.as_mut_ptr(),
            n_channels: 1,
        };
        check(a, "streamer", (a.rx_streamer_make)(&mut s.rx))?;
        check(a, "stream", (a.get_rx_stream)(s.usrp, &mut sa, s.rx))?;
        check(a, "metadata", (a.rx_metadata_make)(&mut s.md))?;
        let mut max = 0usize;
        check(a, "stream", (a.rx_streamer_max_num_samps)(s.rx, &mut max))?;
        // ~10 ms blocks, a whole number of packets.
        let per = max.max(1) * ((cfg.rate_hz / 100.0) as usize / max.max(1)).max(1);
        let cmd = StreamCmd { stream_mode: MODE_START, num_samps: 0, stream_now: true, time_spec_full_secs: 0, time_spec_frac_secs: 0.0 };
        check(a, "start", (a.rx_streamer_issue_stream_cmd)(s.rx, &cmd))?;
        s.streaming = true;

        let mut next_t: Option<f64> = None;
        let mut timeouts = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Some(c) = ctl {
                let (freq, gain) = c.take();
                if let Some(f) = freq {
                    let mut req = TuneRequest { target_freq: f / (1.0 + cfg.ppm * 1e-6), ..req };
                    check(a, "tune", (a.set_rx_freq)(s.usrp, &mut req, 0, &mut res))?;
                    *c.current_hz.lock().unwrap() = Some(f);
                }
                if let Some(g) = gain {
                    check(a, "set gain", (a.set_rx_gain)(s.usrp, g as f64, 0, c"".as_ptr()))?;
                }
                if freq.is_some() || gain.is_some() {
                    let hz = c.current_hz.lock().unwrap().unwrap_or(cfg.center_hz);
                    if tx.send(SourceMsg::Tuned { center_hz: hz }).is_err() {
                        return Ok(());
                    }
                }
            }
            let mut buf = vec![Complex32::default(); per];
            let mut ptr = buf.as_mut_ptr() as *mut c_void;
            let mut n = 0usize;
            check(a, "receive", (a.rx_streamer_recv)(s.rx, &mut ptr, per, &mut s.md, 1.0, false, &mut n))?;
            let mut code = 0;
            (a.rx_metadata_error_code)(s.md, &mut code);
            match code {
                MD_NONE | MD_OVERFLOW => timeouts = 0,
                MD_TIMEOUT => {
                    timeouts += 1;
                    if timeouts >= 3 {
                        return Err("USRP: no samples for 3 s".into());
                    }
                    continue;
                }
                c => return Err(format!("USRP: receive error 0x{c:x}")),
            }
            // Samples lost (an overflow, 'O') show as a jump in the packet time.
            let mut dropped = 0u64;
            let mut has = false;
            (a.rx_metadata_has_time_spec)(s.md, &mut has);
            if has {
                let (mut full, mut frac) = (0i64, 0.0f64);
                (a.rx_metadata_time_spec)(s.md, &mut full, &mut frac);
                let t = full as f64 + frac;
                if let Some(want) = next_t {
                    let gap = ((t - want) * cfg.rate_hz).round();
                    if gap > 0.0 {
                        dropped = gap as u64;
                    }
                }
                next_t = Some(t + n as f64 / cfg.rate_hz);
            }
            if n == 0 {
                continue;
            }
            buf.truncate(n);
            if tx.send(SourceMsg::Iq { source, samples: buf, dropped }).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}
