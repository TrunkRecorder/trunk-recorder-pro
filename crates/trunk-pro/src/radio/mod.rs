//! Radios whose drivers are vendor C libraries — USRP (UHD), Airspy
//! (libairspy), HydraSDR (libhydrasdr), bladeRF (libbladeRF) and anything with a SoapySDR module.
//! Nothing links against them: each library is looked for at run time, so the same binary records
//! from RTL-SDRs out of the box and from these once their driver is
//! installed. `TRUNK_PRO_UHD` / `TRUNK_PRO_AIRSPY` / `TRUNK_PRO_HYDRASDR` / `TRUNK_PRO_BLADERF` /
//! `TRUNK_PRO_SOAPY` name a library file to use instead of searching.
//! An RTL-SDR on another machine (through `rtl_tcp`, [`rtl`]) and a SpyServer ([`spyserver`]) are
//! radios on the network, over plain TCP ([`net`]): nothing to install either.

pub mod airspy;
pub mod bladerf;
pub mod hackrf;
pub mod net;
pub mod priority;
pub mod rtl;
pub mod rtltcp;
mod sdrplay;
pub mod soapy;
pub mod spyserver;
pub mod uhd;
mod usb_speed;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// Held while a radio opens, and while a driver searches for radios. libairspy,
/// librtlsdr and libhackrf find a radio by opening every one of theirs to read its
/// serial and skip any they can't open (on macOS an open is exclusive), so a
/// search or an open beside another radio's open could miss it: "not found" until
/// the retry. The SDRplay API wants its device selection kept apart too. UHD keeps
/// its own lock, so a USRP opens without this one. Taken by [`open`].
fn opening() -> MutexGuard<'static, ()> {
    static OPENING: Mutex<()> = Mutex::new(());
    OPENING.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long an open (or a driver's search for radios) may take, by what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Deadline {
    /// A radio that opens in well under a second (RTL-SDR, HackRF, Airspy, HydraSDR) or a search: 5 s.
    Quick,
    /// One that loads firmware or an FPGA image as it opens (bladeRF, a SoapySDR module): 15 s.
    Loads,
    /// A USRP: an FPGA image over USB, or a search of the network: 30 s.
    Slow,
}

impl Deadline {
    fn duration(self) -> Duration {
        Duration::from_secs(match self {
            Deadline::Quick => 5,
            Deadline::Loads => 15,
            Deadline::Slow => 30,
        })
    }
}

/// Radios whose open passed its deadline and hasn't returned yet, by name.
static STUCK: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Open the radio `name` ("RTL-SDR 00000050") with `f`, or search for radios,
/// on a thread of its own, under [`opening`] when `shared` (every driver but
/// UHD), given up at the `deadline`.
///
/// A radio whose firmware is stuck (often across a reboot) still enumerates
/// but never answers; the open — usbfs and IOKit calls, a vendor library's —
/// then waits in the kernel for ever, and under [`opening`] no other radio
/// could reopen either (sdr-core's `open_with_deadline`). Given up, the thread
/// is left behind (it holds nothing the program needs: whatever it returns
/// late is dropped, closing it) and `name` is refused at once until it
/// returns, so two opens never talk to one radio together. Other radios open
/// as usual.
pub fn open<T: Send + 'static>(name: &str, deadline: Deadline, shared: bool, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    open_within(name, deadline.duration(), shared, f)
}

fn open_within<T: Send + 'static>(name: &str, deadline: Duration, shared: bool, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let wedged = || format!("{name} isn't answering (its firmware is stuck): unplug it and plug it back in, or power-cycle its USB hub");
    if STUCK.lock().unwrap().iter().any(|s| s == name) {
        return Err(wedged());
    }
    let _one = shared.then(opening);
    let (tx, rx) = std::sync::mpsc::channel();
    let given_up = Arc::new(AtomicBool::new(false));
    let (gave_up, key) = (Arc::clone(&given_up), name.to_string());
    std::thread::Builder::new()
        .name("radio-open".into())
        .spawn(move || {
            let r = f();
            let mut stuck = STUCK.lock().unwrap();
            if gave_up.load(Ordering::SeqCst) {
                stuck.retain(|s| *s != key);
            }
            let _ = tx.send(r);
        })
        .map_err(|e| format!("open {name}: {e}"))?;
    match rx.recv_timeout(deadline) {
        Ok(r) => r,
        Err(_) => {
            // (Under STUCK's lock, so the thread's own check can't come between.)
            let mut stuck = STUCK.lock().unwrap();
            if let Ok(r) = rx.try_recv() {
                return r;
            }
            given_up.store(true, Ordering::SeqCst);
            stuck.push(name.to_string());
            log::error!("{} (no answer in {} s)", wedged(), deadline.as_secs());
            Err(wedged())
        }
    }
}

/// A value from a vendor library (a device handle) that may be handed to the
/// thread that asked for it: the libraries are thread-safe per handle, and
/// [`open`]'s thread only ever passes it on.
pub struct Handed<T>(pub T);
// SAFETY: see above — one thread uses it at a time.
unsafe impl<T> Send for Handed<T> {}

/// What a USB transfer refused for lack of memory means, when it does: Linux
/// gives usbfs 16 MB by default, and every radio keeps ~2 MB in flight.
pub fn usb_memory_hint(e: &str) -> String {
    if cfg!(target_os = "linux") && (e.contains("ENOMEM") || e.contains("out of memory") || e.contains("Cannot allocate memory")) {
        format!("{e} — Linux's USB memory (usbfs_memory_mb, 16 MB by default) is used up by the radios' transfers: raise it with `echo 0 | sudo tee /sys/module/usbcore/parameters/usbfs_memory_mb` (0 = no limit; add usbcore.usbfs_memory_mb=0 to the kernel command line to keep it)")
    } else {
        e.to_string()
    }
}

/// The sample rates a radio runs at, as it said when opened (trunk-app's: Setup and advice read it too).
pub use trunk_app::rates::Rates;
use trunk_app::config::{Source, SourceKind};

/// What a radio said it is when it was opened: its model ("R2", "bladeRF 2.0"), rates, and gains
/// where they aren't its kind's own (an RTL-SDR's tuner behind `rtl_tcp`, a SpyServer's steps).
#[derive(Clone, Debug, PartialEq)]
pub struct Caps {
    pub model: String,
    pub rates: Rates,
    /// The gains it can be set to ([`trunk_app::gain::GainPlan::of_with`]); empty: its kind's.
    pub gains: Vec<f64>,
}

impl Caps {
    /// Its part of a found radio's JSON: `model`, and `rates` or `rateRange` (Hz).
    pub fn add_to(&self, j: &mut serde_json::Value) {
        if !self.model.is_empty() {
            j["model"] = self.model.clone().into();
        }
        match &self.rates {
            Rates::List(v) => j["rates"] = serde_json::json!(v),
            Rates::Range { min, max } => j["rateRange"] = serde_json::json!([min, max]),
        }
        if !self.gains.is_empty() {
            j["gains"] = serde_json::json!(self.gains);
        }
    }
}

/// What each radio said when it was last opened in this run, by `"<kind>:<serial or args>"`.
static KNOWN: LazyLock<Mutex<HashMap<String, Caps>>> = LazyLock::new(Default::default);

/// Remember what the radio `key` ("airspy:<serial>") said when opened: a list of
/// radios then shows it while it records (when it can't be opened to ask), and
/// doesn't open it to ask again.
pub fn remember(key: &str, caps: Caps) {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), caps);
}

/// What the radio `key` said when it was last opened in this run.
pub fn known(key: &str) -> Option<Caps> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
}

/// " · 10 / 2.5 MSPS" for a found radio's JSON whose rates are known (`trunk-pro devices`), else "".
pub fn rates_shown(found: &serde_json::Value) -> String {
    Rates::from_found(found).map_or(String::new(), |rates| format!(" · {rates} MSPS"))
}

/// A source's refusal of a rate its radio doesn't run at, the same for every kind:
/// "this Airspy R2 runs at 10 / 2.5 MSPS, not 6: pick one of those in Setup → Radios".
pub fn rate_refusal(what: &str, rates: &Rates, want_hz: f64) -> String {
    let pick = match rates {
        Rates::List(_) => "pick one of those",
        Rates::Range { .. } => "pick one in that range",
    };
    format!("this {what} runs at {rates} MSPS, not {}: {pick} in Setup → Radios", want_hz / 1e6)
}

/// The frequency to ask a radio for: a reference off by `ppm` makes a
/// requested f come out at f·(1 + ppm).
pub fn ppm_corrected(hz: f64, ppm: f64) -> f64 {
    hz / (1.0 + ppm * 1e-6)
}

/// A C string from a driver ("" for null).
///
/// SAFETY (callers): `p` is null or NUL-terminated.
pub unsafe fn cstr(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: as the caller promises.
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

use std::path::PathBuf;

use libloading::Library;

/// Open the first library that loads: `env` if set, else each candidate
/// name (the system's search path), else versioned files in the usual
/// install folders (`lib<stem>.so.<version>`, highest first). `global`: its
/// symbols resolve the plug-ins it loads itself (SoapySDR's modules).
/// `win_needs`: what it needs beside it on Windows, said when it's there but
/// one of those isn't (" (airspy.dll needs libusb-1.0.dll …)").
pub fn load(env: &str, names: &[&str], stem: &str, global: bool, win_needs: &str) -> Result<(Library, String), String> {
    let try_one = |p: &str| -> Result<Library, String> {
        // SAFETY: loading a vendor driver runs its initialisers; that is the point.
        #[cfg(unix)]
        if global {
            use libloading::os::unix::{Library as Unix, RTLD_GLOBAL, RTLD_NOW};
            return unsafe { Unix::open(Some(p), RTLD_NOW | RTLD_GLOBAL) }.map(Library::from).map_err(|e| load_error(&e, win_needs));
        }
        let _ = global;
        unsafe { Library::new(p) }.map_err(|e| load_error(&e, win_needs))
    };
    if let Ok(p) = std::env::var(env) {
        return try_one(&p).map(|l| (l, p.clone())).map_err(|e| format!("{env}={p}: {e}"));
    }
    let mut tried: Vec<String> = Vec::new();
    // A library that exists but won't load (wrong architecture, refused by
    // code signing, missing its own dependencies) says more than "not found".
    let mut refused: Option<String> = None;
    let mut note = |p: &str, e: String| {
        tried.push(p.to_string());
        if refused.is_none() && beside_or_here(p) {
            refused = Some(e);
        }
    };
    for n in names {
        match try_one(n) {
            Ok(l) => return Ok((l, n.to_string())),
            Err(e) => note(n, e),
        }
    }
    for dir in lib_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut found: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with(&format!("lib{stem}.so.")))).collect();
        found.sort();
        for p in found.iter().rev() {
            let s = p.display().to_string();
            match try_one(&s) {
                Ok(l) => return Ok((l, s)),
                Err(e) => note(&s, e),
            }
        }
    }
    match refused {
        Some(e) => Err(format!("found but couldn't be loaded: {e}")),
        None => Err(format!("not found (looked for {})", tried.join(", "))),
    }
}

/// Whether library `p` is there: as given, or (a bare name, on Windows) beside the program, the
/// first place Windows looks, whatever folder it was started from.
fn beside_or_here(p: &str) -> bool {
    let path = std::path::Path::new(p);
    path.exists() || (cfg!(windows) && path.components().count() == 1 && std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(p).exists())).unwrap_or(false))
}

/// Why a library didn't load, all of it: on Windows libloading says only "LoadLibraryExW failed",
/// the system's reason being its source. With that reason, what it means there: a DLL it needs
/// is missing (126: `win_needs` says which), or it's not a 64-bit Windows DLL (193).
fn load_error(e: &libloading::Error, win_needs: &str) -> String {
    let mut text = e.to_string();
    let mut code = None;
    let mut src = std::error::Error::source(e);
    while let Some(c) = src {
        text.push_str(&format!(": {c}"));
        code = code.or_else(|| c.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error));
        src = c.source();
    }
    let hint = if cfg!(windows) { code.map_or(String::new(), |c| windows_hint(c, win_needs)) } else { String::new() };
    text + &hint
}

/// What Windows' error `code` loading a DLL that's there means: a DLL it needs is missing (126,
/// `win_needs` says which), or it isn't a 64-bit Windows DLL (193, a 32-bit one say).
fn windows_hint(code: i32, win_needs: &str) -> String {
    match code {
        126 => format!(" — a DLL it needs is missing{win_needs}"),
        193 => " — it isn't a 64-bit Windows DLL: take the x64 one".to_string(),
        _ => String::new(),
    }
}

fn lib_dirs() -> Vec<PathBuf> {
    ["/usr/lib", "/usr/lib64", "/usr/local/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib/arm-linux-gnueabihf"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Where a driver usually lives, per platform, after the bare names.
pub fn candidates(unix_name: &str, mac_name: &str, win_names: &[&'static str]) -> Vec<String> {
    if cfg!(target_os = "macos") {
        ["", "/opt/homebrew/lib/", "/usr/local/lib/", "/opt/local/lib/"].iter().map(|d| format!("{d}{mac_name}")).collect()
    } else if cfg!(windows) {
        win_names.iter().map(|s| s.to_string()).collect()
    } else {
        vec![unix_name.to_string()]
    }
}

/// Drivers and the devices found through them, for the interface:
/// `{usrp: {available, detail, devices}, hackrf: {…}, airspy: {…}, hydrasdr: {…}, bladerf: {…}, soapy: {…, modules},
/// network: {…}}`. USRPs and SoapySDR devices are searched only with `search` (a device search can take seconds).
/// `network`: the radios on the network `sources` name ([`network_json`]), asked now with `ask`.
pub fn radios_json(search: bool, sources: &[Source], ask: bool) -> serde_json::Value {
    let u = uhd::info();
    let mut uj = u.json();
    uj["devices"] = if search && u.loaded {
        match uhd::find("") {
            Ok(v) => v.iter().map(|s| serde_json::json!({ "args": usrp_args(s), "label": usrp_label(s) })).collect(),
            Err(e) => {
                uj["detail"] = serde_json::json!(e);
                serde_json::json!([])
            }
        }
    } else {
        serde_json::Value::Null
    };
    let hackrf = serde_json::json!({ "available": true, "detail": "built in", "devices": hackrf::devices() });
    let network = serde_json::json!({ "available": true, "detail": "built in", "devices": network_json(sources, ask) });
    serde_json::json!({ "usrp": uj, "hackrf": hackrf, "airspy": airspy_json(airspy::Lib::Airspy), "hydrasdr": airspy_json(airspy::Lib::Hydrasdr), "bladerf": bladerf_json(), "soapy": soapy::json(search), "network": network })
}

/// A source's radio on the network, if it is one ([`Source::server`]).
pub struct OnNetwork<'a> {
    /// "rtlsdr", "spyserver".
    pub kind: &'static str,
    /// As the source gives it (what Setup matches its listing by).
    pub address: &'a str,
    /// `host:port`.
    pub server: String,
    /// What it's remembered by ([`remember`]).
    pub key: String,
}

pub fn on_network(src: &Source) -> Option<OnNetwork<'_>> {
    let server = src.server()?;
    let (SourceKind::Rtlsdr { address, .. } | SourceKind::Spyserver { address, .. }) = &src.kind else { return None };
    let kind = src.kind_name();
    Some(OnNetwork { kind, address: address.trim(), key: net::key(kind, &server), server })
}

/// The radios on the network `sources` name (an RTL-SDR's `rtl_tcp` server, a SpyServer), as a list of radios has
/// them: `{kind, address, model, rates, gains}`, what each said when it was last opened in this run, or (`ask`, and
/// not opened yet) what it says when asked now; `error` when it can't be. (Asked only when wanted: a server another
/// client has may take seconds to say it won't answer, and one `rtl_tcp` serves one client.)
pub fn network_json(sources: &[Source], ask: bool) -> Vec<serde_json::Value> {
    let mut seen = std::collections::HashSet::new();
    sources
        .iter()
        .filter_map(on_network)
        .filter(|n| seen.insert(n.key.clone()))
        .map(|n| {
            let mut j = serde_json::json!({ "kind": n.kind, "address": n.address, "label": n.address });
            let caps = match known(&n.key) {
                Some(c) => Ok(c),
                None if ask => match n.kind {
                    "rtlsdr" => rtltcp::ask(&n.server),
                    _ => spyserver::ask(&n.server),
                },
                None => Err(String::new()),
            };
            match caps {
                Ok(c) => {
                    j["label"] = format!("{} at {}", c.model, n.address).into();
                    c.add_to(&mut j);
                }
                Err(e) if !e.is_empty() => j["error"] = e.into(),
                Err(_) => {}
            }
            j
        })
        .collect()
}

/// The gains `src`'s radio said it can be set to when it was last opened, where they aren't its kind's own (an
/// RTL-SDR's tuner behind `rtl_tcp`, a SpyServer's steps): what the gain-tuning tool walks.
pub fn reported_gains(src: &Source) -> Option<Vec<f64>> {
    known(&on_network(src)?.key).map(|c| c.gains).filter(|g| !g.is_empty())
}

fn bladerf_json() -> serde_json::Value {
    let b = bladerf::info();
    let mut j = b.json();
    j["devices"] = serde_json::Value::Array(if b.loaded { bladerf::devices() } else { vec![] });
    j
}

fn airspy_json(lib: airspy::Lib) -> serde_json::Value {
    let a = airspy::info(lib);
    let mut j = a.json();
    j["devices"] = serde_json::Value::Array(if a.loaded { airspy::devices(lib) } else { vec![] });
    j
}

/// "type=b200,name=,serial=3070E68,product=B200" → "serial=3070E68" (what opens exactly that one).
fn usrp_args(found: &str) -> String {
    let kv = |k: &str| found.split(',').find_map(|p| p.strip_prefix(&format!("{k}=")).filter(|v| !v.is_empty()).map(str::to_string));
    kv("serial").map(|s| format!("serial={s}")).or_else(|| kv("addr").map(|a| format!("addr={a}"))).unwrap_or_else(|| found.to_string())
}
fn usrp_label(found: &str) -> String {
    let kv = |k: &str| found.split(',').find_map(|p| p.strip_prefix(&format!("{k}=")).filter(|v| !v.is_empty()).map(str::to_string));
    let product = kv("product").or_else(|| kv("type")).unwrap_or_else(|| "USRP".into());
    match (kv("serial"), kv("addr")) {
        (Some(s), _) => format!("{product} · SN {s}"),
        (None, Some(a)) => format!("{product} · {a}"),
        _ => product,
    }
}

/// A driver's state for the interface.
#[derive(Clone, Debug)]
pub struct DriverInfo {
    pub loaded: bool,
    /// The library file and version, or why it isn't available.
    pub detail: String,
}

impl DriverInfo {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "available": self.loaded, "detail": self.detail })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #219: a Windows DLL that's there but won't load says why, not only "LoadLibraryExW failed".
    #[test]
    fn a_driver_that_wont_load_says_why() {
        let airspy = ": airspy.dll needs libusb-1.0.dll and pthreadVC2.dll beside it";
        assert_eq!(windows_hint(126, airspy), " — a DLL it needs is missing: airspy.dll needs libusb-1.0.dll and pthreadVC2.dll beside it");
        assert_eq!(windows_hint(193, airspy), " — it isn't a 64-bit Windows DLL: take the x64 one");
        assert_eq!(windows_hint(5, airspy), "");
        // A library that isn't there at all: not found, with where it was looked for.
        let e = load("TRUNK_PRO_TEST_NONE", &["libtrunk-pro-none.so.0"], "trunk-pro-none", false, airspy).err().unwrap();
        assert!(e.starts_with("not found (looked for libtrunk-pro-none.so.0"), "{e}");
        // One that's there but isn't a library: refused, with the system's reason.
        let dir = std::env::temp_dir().join(format!("trp-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("libbad.so.0");
        std::fs::write(&bad, b"not a library").unwrap();
        let e = load("TRUNK_PRO_TEST_NONE", &[bad.to_str().unwrap()], "bad", false, airspy).err().unwrap();
        assert!(e.starts_with("found but couldn't be loaded: ") && e.len() > "found but couldn't be loaded: ".len() + 10, "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An open that hangs is given up at its deadline, the radio refused at
    /// once while it stays stuck, others opened meanwhile, and it opens again
    /// once the stuck open returns.
    #[test]
    fn a_hung_open_is_given_up_and_the_radio_refused_until_it_returns() {
        let ms = Duration::from_millis;
        let t = std::time::Instant::now();
        let e = open_within("test-wedged", ms(50), true, move || {
            std::thread::sleep(ms(400));
            Ok(())
        })
        .unwrap_err();
        assert!(t.elapsed() < ms(300) && e.contains("isn't answering"), "{e}");
        let t = std::time::Instant::now();
        assert!(open_within("test-wedged", ms(1000), true, || Ok(())).is_err());
        assert!(t.elapsed() < ms(50), "refused at once");
        assert_eq!(open_within("test-other", ms(1000), true, || Ok(7)), Ok(7), "other radios open");
        std::thread::sleep(ms(500));
        assert_eq!(open_within("test-wedged", ms(1000), true, || Ok(8)), Ok(8));
    }

    #[test]
    fn rate_refusal_names_the_radio_and_the_fix() {
        let r2 = Rates::List(vec![10_000_000, 2_500_000]);
        assert!(r2.allows(2.5e6) && !r2.allows(6e6));
        assert_eq!(rate_refusal("Airspy R2", &r2, 6e6), "this Airspy R2 runs at 10 / 2.5 MSPS, not 6: pick one of those in Setup → Radios");
        let blade = Rates::Range { min: 520_834.0, max: 61_440_000.0 };
        assert!(blade.allows(10e6) && !blade.allows(0.5e6) && !blade.allows(80e6));
        assert_eq!(rate_refusal("bladeRF 2.0", &blade, 80e6), "this bladeRF 2.0 runs at 0.521–61.44 MSPS, not 80: pick one in that range in Setup → Radios");
        let mut j = serde_json::json!({ "serial": "1" });
        Caps { model: "bladeRF 2.0".into(), rates: blade, gains: Vec::new() }.add_to(&mut j);
        assert_eq!(j, serde_json::json!({ "serial": "1", "model": "bladeRF 2.0", "rateRange": [520_834.0, 61_440_000.0] }));
    }

    #[test]
    fn usrp_found_strings() {
        let s = "type=b200,name=MyB200,serial=3070E68,product=B200";
        assert_eq!(usrp_args(s), "serial=3070E68");
        assert_eq!(usrp_label(s), "B200 · SN 3070E68");
        let n = "type=usrp2,addr=192.168.10.2,name=,serial=";
        assert_eq!(usrp_args(n), "addr=192.168.10.2");
        assert_eq!(usrp_label(n), "usrp2 · 192.168.10.2");
    }
}
