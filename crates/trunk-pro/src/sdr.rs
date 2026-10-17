//! RTL-SDR input over USB (rtlsdr-nusb: pure Rust, no libusb/librtlsdr).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rtlsdr_nusb::{Device, GainConfig, MaybeFuture, RxBuffers};

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

/// About this much is kept in flight when the number of transfers isn't set:
/// what the driver's 8 × 256 KiB default holds, about 440 ms at 2.4 MS/s.
const RTL_IN_FLIGHT: usize = 2 << 20;

impl RtlConfig {
    /// The USB transfers to ask for: smaller ones, if `buf_len` is set, but
    /// more of them, so as much is still in flight.
    pub fn rx_buffers(&self) -> RxBuffers {
        let d = RxBuffers::default();
        let len = if self.buf_len == 0 { d.len } else { self.buf_len as usize };
        let count = if self.buffers != 0 { self.buffers as usize } else if self.buf_len == 0 { d.count } else { (RTL_IN_FLIGHT / len).clamp(8, 64) };
        RxBuffers { len, count }
    }
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
                        "bus": info.map(|u| u.bus_id().to_string()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The most RTL-SDRs plugged into one USB bus (host controller), None if
/// the USB devices can't be listed.
pub fn rtl_most_on_a_bus() -> Option<usize> {
    let usb: Vec<nusb::DeviceInfo> = nusb::list_devices().wait().ok()?.collect();
    let mut per_bus = std::collections::HashMap::<&str, usize>::new();
    for u in usb.iter().filter(|u| u.vendor_id() == 0x0bda && matches!(u.product_id(), 0x2832 | 0x2838)) {
        *per_bus.entry(u.bus_id()).or_default() += 1;
    }
    Some(per_bus.into_values().max().unwrap_or(0))
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

/// A buffer that goes back to its pool when dropped: a driver's thread
/// (which must keep up with its device) reuses buffers instead of
/// allocating one per transfer.
pub struct Pooled<T> {
    v: Vec<T>,
    pool: Arc<Mutex<Vec<Vec<T>>>>,
}

/// Buffers a pool keeps (more are freed): a few in flight plus a queue's worth beyond the first.
const POOL_KEEP: usize = 64;

/// Buffers for a driver to fill ([`Pooled`]).
#[derive(Clone, Default)]
pub struct Pool<T> {
    free: Arc<Mutex<Vec<Vec<T>>>>,
}

impl<T: Copy> Pool<T> {
    /// A buffer holding a copy of `from` (one returned earlier when there is one).
    pub fn copy(&self, from: &[T]) -> Pooled<T> {
        let mut v = self.free.lock().unwrap().pop().unwrap_or_default();
        v.clear();
        v.extend_from_slice(from);
        Pooled { v, pool: self.free.clone() }
    }
}

impl<T> std::ops::Deref for Pooled<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.v
    }
}

impl<T> Drop for Pooled<T> {
    fn drop(&mut self) {
        let mut free = self.pool.lock().unwrap();
        if free.len() < POOL_KEEP {
            free.push(std::mem::take(&mut self.v));
        }
    }
}

/// Turns each source's [`SourceMsg::Real16`] into [`SourceMsg::Iq`] on the
/// thread that takes the messages; the rest pass through.
#[derive(Default)]
pub struct IqFrom {
    conv: Vec<Option<trunk_core::dsp::real_iq::RealToIq>>,
}

impl IqFrom {
    pub fn iq(&mut self, m: SourceMsg) -> SourceMsg {
        let SourceMsg::Real16 { source, samples, dropped, at } = m else { return m };
        if self.conv.len() <= source {
            self.conv.resize_with(source + 1, || None);
        }
        let c = self.conv[source].get_or_insert_with(trunk_core::dsp::real_iq::RealToIq::new);
        let mut iq = Vec::with_capacity(samples.len() / 2 + 64);
        c.push(&samples, &mut iq);
        SourceMsg::Iq { source, samples: iq, dropped, at }
    }
}

/// What a source thread reports.
pub enum SourceMsg {
    /// Raw u8 IQ, samples the driver knows were dropped just before it, and
    /// when the driver handed it over (the source's clock is measured by it).
    /// (A [`Pooled`] buffer: the driver's thread reuses them, allocating none per transfer.)
    Data { source: usize, bytes: Pooled<u8>, dropped: u64, at: Instant },
    /// Float IQ (USRP, float captures).
    Iq { source: usize, samples: Vec<num_complex::Complex32>, dropped: u64, at: Instant },
    /// Real samples at twice the IQ rate (an Airspy's ADC, libairspy's INT16_REAL): turned into IQ on the
    /// thread that takes them ([`IqFrom`]), not the driver's. `dropped` in IQ samples.
    Real16 { source: usize, samples: Pooled<i16>, dropped: u64, at: Instant },
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
    let mut rx = dev.rx_stream_with(cfg.rx_buffers()).map_err(|e| format!("RTL-SDR USB transfers: {e}"))?;
    rx.start().wait().map_err(|e| e.to_string())?;
    // (Buffers to copy each transfer into, reused: this thread must keep up with the dongle.)
    let pool = Pool::default();
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
                let msg = SourceMsg::Data { source, bytes: pool.copy(block.raw_bytes()), dropped: block.dropped_samples(), at: Instant::now() };
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

#[cfg(test)]
mod pool_tests {
    use super::*;

    /// A buffer goes back to its pool when dropped, and the next copy reuses it.
    #[test]
    fn buffers_are_reused() {
        let pool: Pool<i16> = Pool::default();
        let a = pool.copy(&[1, 2, 3]);
        let p = a.as_ptr();
        assert_eq!(&*a, &[1, 2, 3]);
        drop(a);
        let b = pool.copy(&[4, 5]);
        assert_eq!((&*b, b.as_ptr()), (&[4i16, 5][..], p), "the same memory, refilled");
    }

    /// Real samples become IQ at half their count, per source.
    #[test]
    fn real_samples_become_iq() {
        let pool: Pool<i16> = Pool::default();
        let mut iqf = IqFrom::default();
        let m = SourceMsg::Real16 { source: 1, samples: pool.copy(&vec![0i16; 4096]), dropped: 7, at: Instant::now() };
        match iqf.iq(m) {
            // (Half as many, less the filter's look-ahead held for the next push.)
            SourceMsg::Iq { source, samples, dropped, .. } => assert_eq!((source, dropped, samples.len()), (1, 7, 2048 - 19)),
            _ => panic!("not IQ"),
        }
    }
}
