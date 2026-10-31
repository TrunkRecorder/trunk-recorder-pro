//! RTL-SDR over USB (rtlsdr-nusb: pure Rust, no libusb/librtlsdr).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use rtlsdr_nusb::{Device, GainConfig, MaybeFuture, RawIq, RxBuffers, RxStream};

use super::Deadline;
use crate::sdr::{Applied, Control, Gain, Radio, Retuned, Sink, StreamCheck, Tunable, Wire};

#[derive(Clone, Debug)]
pub struct RtlConfig {
    /// Serial number ("" = first dongle).
    pub serial: String,
    pub center_hz: u64,
    pub rate_hz: u32,
    /// None = tuner AGC.
    pub gain_db: Option<f32>,
    pub ppm: i32,
    /// Bytes per USB transfer, 0 = the driver's default (Trunk Recorder's `buflen`).
    pub buf_len: u32,
    /// USB transfers in flight, 0 = enough for about 2 MiB.
    pub buffers: u32,
}

/// About this much is kept in flight when only the transfer size is set:
/// what the driver's 128 × 16 KiB default holds, about 440 ms at 2.4 MS/s.
const RTL_IN_FLIGHT: usize = 2 << 20;

impl RtlConfig {
    /// The USB transfers to ask for: as set, else the driver's; a size alone
    /// gets as many as keep [`RTL_IN_FLIGHT`] in flight.
    pub fn rx_buffers(&self) -> RxBuffers {
        let d = RxBuffers::default();
        let len = if self.buf_len == 0 { d.len } else { self.buf_len as usize };
        let count = if self.buffers != 0 { self.buffers as usize } else if self.buf_len == 0 { d.count } else { (RTL_IN_FLIGHT / len).clamp(8, 256) };
        RxBuffers { len, count }
    }

    fn name(&self) -> String {
        format!("RTL-SDR {}", if self.serial.is_empty() { "(first)" } else { &self.serial })
    }
}

/// Opening a dongle whose tuner rtlsdr-nusb doesn't drive (only R820T / R828D).
pub const UNSUPPORTED_TUNER: &str = "its tuner isn't supported natively (only R820T / R828D; not E4000, FC0012, FC0013 or FC2580) — add it as a SoapySDR source with device arguments driver=rtlsdr instead (needs SoapySDR's RTL-SDR module)";

/// Serials of the dongles this program has open: a search for dongles
/// reports them as recording rather than open them to ask.
static OPEN: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Attached dongles, for the browser: [{serial, product, index, busy, bus}].
/// `busy`: why it can't be opened now (another program has it, this one is
/// recording from it), or null.
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
                    let ours = d.serial.as_ref().is_some_and(|sn| OPEN.lock().unwrap().contains(sn));
                    serde_json::json!({
                        "index": d.index,
                        "serial": d.serial.clone().unwrap_or_default(),
                        "product": d.product.clone().unwrap_or_else(|| "RTL-SDR".into()),
                        "busy": if ours { Some("recording".to_string()) } else { info.and_then(busy) },
                        "bus": info.map(|u| u.bus_id().to_string()),
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
    let name = info.serial_number().map_or_else(|| format!("RTL-SDR on bus {} address {}", info.bus_id(), info.device_address()), |s| format!("RTL-SDR {s}"));
    let info = info.clone();
    // (Under the opening lock: this claim could make a radio opening now look busy.)
    super::open(&name, Deadline::Quick, true, move || {
        let dev = info.open().wait().map_err(why)?;
        dev.detach_and_claim_interface(0).wait().map(drop).map_err(why)
    })
    .err()
}

impl Radio for RtlConfig {
    const BACKOFF: Duration = Duration::from_secs(2);
    fn rate_hz(&self) -> f64 {
        self.rate_hz as f64
    }
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz as u64;
    }
    fn gain_to(&mut self, g: Gain) {
        match g {
            Gain::Agc => self.gain_db = None,
            Gain::Db(db) => self.gain_db = Some(db),
            Gain::Airspy(_) | Gain::Hackrf(_) => {}
        }
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

/// An open RTL-SDR and its stream, for [`Control::apply`]. A change is made
/// with the chip's output stopped: nothing it writes costs samples unseen
/// (each control request served while samples flow costs ~900, sdr-rtl
/// findings 16), the chip's own dropped count is read meanwhile, and the
/// quiet is reported as the change's gap.
struct Rtl<'a> {
    dev: &'a mut Device<RawIq>,
    rx: &'a mut RxStream<RawIq>,
    rate: f64,
}

impl Rtl<'_> {
    /// `change` with the output stopped; the samples' worth of quiet it took.
    fn halted(&mut self, change: impl FnOnce(&mut Device<RawIq>) -> Result<(), String>) -> Result<u64, String> {
        let t = Instant::now();
        self.rx.stop().wait().map_err(|e| format!("USB: {e}"))?;
        let changed = change(self.dev);
        // (Restarted even when the change failed: the device keeps its last good settings.)
        self.rx.start().wait().map_err(|e| format!("USB: {e}"))?;
        changed?;
        Ok((t.elapsed().as_secs_f64() * self.rate) as u64)
    }
}

impl Tunable for Rtl<'_> {
    fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
        let gap = self.halted(|dev| dev.set_frequency_hz(hz.round() as u64).wait().map_err(|e| format!("tune {:.4} MHz: {e}", hz / 1e6)))?;
        Ok(Retuned { hz: self.dev.actual_frequency_hz() as f64, gap })
    }
    fn set_gain(&mut self, g: Gain) -> Result<u64, String> {
        let gain = match g {
            Gain::Agc => GainConfig::Auto,
            Gain::Db(db) => GainConfig::Manual(db),
            Gain::Airspy(_) | Gain::Hackrf(_) => return Ok(0),
        };
        self.halted(|dev| dev.set_gain(gain).wait().map_err(|e| format!("gain {g}: {e}")))
    }
}

/// Keeps a dongle in [`OPEN`] while it streams.
struct Using(Option<String>);

impl Drop for Using {
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            OPEN.lock().unwrap().remove(s);
        }
    }
}

fn stream_once(cfg: &RtlConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
    let mut b = Device::builder().raw_iq().frequency_hz(cfg.center_hz).sample_rate_hz(cfg.rate_hz).correction_ppm(cfg.ppm);
    b = b.gain(cfg.gain_db.map_or(GainConfig::Auto, GainConfig::Manual));
    if !cfg.serial.is_empty() {
        b = b.serial(cfg.serial.clone());
    }
    let name = cfg.name();
    let mut dev = {
        let name = name.clone();
        super::open(&name.clone(), Deadline::Quick, true, move || {
            b.open().wait().map_err(|e| {
                if matches!(e, rtlsdr_nusb::Error::UnsupportedTuner) {
                    format!("open {name}: {UNSUPPORTED_TUNER}")
                } else {
                    format!("open {name}: {e}")
                }
            })
        })?
    };
    let serial = dev.info().descriptor.serial.clone();
    if let Some(s) = &serial {
        OPEN.lock().unwrap().insert(s.clone());
    }
    let _using = Using(serial);
    let mut rx = dev.rx_stream_with(cfg.rx_buffers()).map_err(|e| format!("{name} USB transfers: {e}"))?;
    rx.start().wait().map_err(|e| format!("{name}: {e}"))?;
    let mut check = StreamCheck::new(cfg.rate_hz as f64);
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        if let Some(c) = ctl {
            match c.apply(&mut Rtl { dev: &mut dev, rx: &mut rx, rate: cfg.rate_hz as f64 }, cfg.center_hz as f64, sink) {
                Err(e) => break Err(e),
                Ok(Applied::Closed) => break Ok(()),
                Ok(_) => {}
            }
        }
        match rx.next_block(Some(Duration::from_secs(2))).wait() {
            Ok(Some(block)) => {
                let at = Instant::now();
                if let Err(e) = check.block(block.raw_bytes(), at) {
                    break Err(format!("{name} {e}; reopening it. If this keeps happening, unplug it and plug it back in"));
                }
                if !sink.bytes(Wire::Offset, block.raw_bytes(), block.dropped_samples(), at) {
                    break Ok(());
                }
            }
            Ok(None) => break Err(format!("{name}: no samples for 2 s")),
            Err(e) => break Err(format!("{name} USB: {e}")),
        }
    };
    let stats = rx.close().wait().unwrap_or_else(|_| rx_stats_unknown());
    let link = dev.link_stats();
    if stats.chip_dropped_samples > 0 || stats.stall_recoveries > 0 || link.stall_retries > 0 {
        log::info!(
            "{name}: the chip dropped {} samples; {} USB stall(s) cleared, {} refused request(s) retried",
            stats.chip_dropped_samples,
            stats.stall_recoveries,
            link.stall_retries
        );
    }
    let _ = dev.shutdown().wait();
    result
}

fn rx_stats_unknown() -> rtlsdr_nusb::StreamingStats {
    rtlsdr_nusb::StreamingStats::default()
}
