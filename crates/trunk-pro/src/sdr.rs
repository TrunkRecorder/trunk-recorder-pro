//! RTL-SDR input over USB (rtlsdr-nusb: pure Rust, no libusb/librtlsdr).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rtlsdr_nusb::{Device, GainConfig, MaybeFuture};

#[derive(Clone, Debug)]
pub struct RtlConfig {
    /// Serial number ("" = first dongle).
    pub serial: String,
    pub center_hz: u64,
    pub rate_hz: u32,
    /// None = tuner AGC.
    pub gain_db: Option<f32>,
    pub ppm: i32,
}

/// Opening a dongle whose tuner rtlsdr-nusb doesn't drive (only R820T / R828D).
pub const UNSUPPORTED_TUNER: &str = "its tuner isn't supported natively (only R820T / R828D; not E4000, FC0012, FC0013 or FC2580) — add it as a SoapySDR source with device arguments driver=rtlsdr instead (needs SoapySDR's RTL-SDR module)";

/// Attached dongles, for the browser: [{serial, product, index, busy}].
/// `busy`: why it can't be opened now (another program has it), or null.
pub fn devices() -> Vec<serde_json::Value> {
    let usb: Vec<nusb::DeviceInfo> = nusb::list_devices().wait().map(|v| v.collect()).unwrap_or_default();
    Device::list()
        .wait()
        .map(|v| {
            v.iter()
                .map(|d| {
                    // The same USB device: by serial, else by place among its model's.
                    let same: Vec<&nusb::DeviceInfo> = usb.iter().filter(|u| u.vendor_id() == d.vid && u.product_id() == d.pid).collect();
                    let nth = v.iter().filter(|o| o.vid == d.vid && o.pid == d.pid).position(|o| o.index == d.index).unwrap_or(0);
                    let info = match &d.serial {
                        Some(sn) => same.iter().find(|u| u.serial_number() == Some(sn.as_str())).copied(),
                        None => same.get(nth).copied(),
                    };
                    serde_json::json!({
                        "index": d.index,
                        "serial": d.serial.clone().unwrap_or_default(),
                        "product": d.product.clone().unwrap_or_else(|| "RTL-SDR".into()),
                        "busy": info.and_then(busy),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Why a dongle can't be opened now, or None: claim its interface and let it
/// go. Nothing is seized — another program's claim makes this fail (macOS
/// opens without seizing; Linux detaches kernel drivers such as the DVB one,
/// as opening it to record does, but never usbfs, i.e. another program).
fn busy(info: &nusb::DeviceInfo) -> Option<String> {
    let why = |e: nusb::Error| match e.kind() {
        nusb::ErrorKind::Busy => "in use by another program".to_string(),
        nusb::ErrorKind::PermissionDenied => "no permission to open it".to_string(),
        _ => format!("can't be opened ({e})"),
    };
    let dev = match info.open().wait() {
        Ok(d) => d,
        Err(e) => return Some(why(e)),
    };
    dev.detach_and_claim_interface(0).wait().err().map(why)
}

/// What a source thread reports.
pub enum SourceMsg {
    /// Raw u8 IQ, samples the driver knows were dropped just before it, and
    /// when the driver handed it over (the source's clock is measured by it).
    Data { source: usize, bytes: Vec<u8>, dropped: u64, at: Instant },
    /// Float IQ (USRP, Airspy, float captures).
    Iq { source: usize, samples: Vec<num_complex::Complex32>, dropped: u64, at: Instant },
    Error { source: usize, error: String },
    /// A finite source (a capture file) has no more data.
    End { source: usize },
    /// A [`Control`] request was applied; the samples after this are at `center_hz`.
    Tuned { center_hz: f64 },
}

/// Retune / gain requests for a running source (the survey). The source
/// applies them between blocks and answers each with [`SourceMsg::Tuned`].
#[derive(Default)]
pub struct Control {
    pub freq_hz: Mutex<Option<f64>>,
    pub gain_db: Mutex<Option<f32>>,
    /// The centre last applied, so a reopened device comes back there.
    pub current_hz: Mutex<Option<f64>>,
}

impl Control {
    /// Pending requests (taken).
    pub fn take(&self) -> (Option<f64>, Option<f32>) {
        (self.freq_hz.lock().unwrap().take(), self.gain_db.lock().unwrap().take())
    }
}

/// Stream a dongle until `stop` is set; blocks go to `tx`. Reopens the device
/// after USB errors (a stall, a sleep/wake) with a short back-off.
pub fn run(source: usize, cfg: RtlConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    run_with(source, cfg, tx, stop, None)
}

/// [`run`], retuned on request.
pub fn run_with(source: usize, mut cfg: RtlConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>, ctl: Option<Arc<Control>>) {
    while !stop.load(Ordering::Relaxed) {
        if let Some(c) = ctl.as_ref().and_then(|c| *c.current_hz.lock().unwrap()) {
            cfg.center_hz = c as u64;
        }
        if let Err(e) = stream_once(source, &cfg, &tx, &stop, ctl.as_deref()) {
            let _ = tx.send(SourceMsg::Error { source, error: e });
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

fn stream_once(source: usize, cfg: &RtlConfig, tx: &SyncSender<SourceMsg>, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let mut b = Device::builder().raw_iq().frequency_hz(cfg.center_hz).sample_rate_hz(cfg.rate_hz).correction_ppm(cfg.ppm);
    b = b.gain(cfg.gain_db.map_or(GainConfig::Auto, GainConfig::Manual));
    if !cfg.serial.is_empty() {
        b = b.serial(cfg.serial.clone());
    }
    let mut dev = b.open().wait().map_err(|e| {
        let name = if cfg.serial.is_empty() { "(first)" } else { &cfg.serial };
        if matches!(e, rtlsdr_nusb::Error::UnsupportedTuner) {
            format!("open RTL-SDR {name}: {UNSUPPORTED_TUNER}")
        } else {
            format!("open RTL-SDR {name}: {e}")
        }
    })?;
    let mut rx = dev.rx_stream().map_err(|e| e.to_string())?;
    rx.start().wait().map_err(|e| e.to_string())?;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        if let Some(c) = ctl {
            let (freq, gain) = c.take();
            if let Some(f) = freq {
                // Stopped while retuning, so no queued block is from the old frequency.
                if let Err(e) = rx.stop().wait() {
                    break Err(format!("USB: {e}"));
                }
                if let Err(e) = dev.set_frequency_hz(f.round() as u64).wait() {
                    break Err(format!("tune {:.4} MHz: {e}", f / 1e6));
                }
                if let Err(e) = rx.start().wait() {
                    break Err(format!("USB: {e}"));
                }
            }
            if let Some(g) = gain {
                if let Err(e) = dev.set_gain(GainConfig::Manual(g)).wait() {
                    break Err(format!("gain {g} dB: {e}"));
                }
            }
            if freq.is_some() || gain.is_some() {
                let hz = dev.actual_frequency_hz() as f64;
                *c.current_hz.lock().unwrap() = Some(hz);
                if tx.send(SourceMsg::Tuned { center_hz: hz }).is_err() {
                    break Ok(());
                }
            }
        }
        match rx.next_block(Some(Duration::from_secs(2))).wait() {
            Ok(Some(block)) => {
                let msg = SourceMsg::Data { source, bytes: block.raw_bytes().to_vec(), dropped: block.dropped_samples(), at: Instant::now() };
                if tx.send(msg).is_err() {
                    break Ok(());
                }
            }
            Ok(None) => break Err("USB stream timed out".to_string()),
            Err(e) => break Err(format!("USB: {e}")),
        }
    };
    let _ = rx.close().wait();
    let _ = dev.shutdown().wait();
    result
}
