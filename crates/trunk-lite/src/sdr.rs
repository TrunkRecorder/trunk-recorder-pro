//! RTL-SDR input over USB (rtlsdr-nusb: pure Rust, no libusb/librtlsdr).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

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

pub fn list() -> Vec<String> {
    match Device::list().wait() {
        Ok(v) => v.iter().map(|d| format!("{d:?}")).collect(),
        Err(e) => vec![format!("error: {e}")],
    }
}

/// What a source thread reports.
pub enum SourceMsg {
    /// Raw u8 IQ, and samples the driver knows were dropped just before it.
    Data { source: usize, bytes: Vec<u8>, dropped: u64 },
    Error { source: usize, error: String },
}

/// Stream a dongle until `stop` is set; blocks go to `tx`. Reopens the device
/// after USB errors (a stall, a sleep/wake) with a short back-off.
pub fn run(source: usize, cfg: RtlConfig, tx: SyncSender<SourceMsg>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = stream_once(source, &cfg, &tx, &stop) {
            let _ = tx.send(SourceMsg::Error { source, error: e });
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

fn stream_once(source: usize, cfg: &RtlConfig, tx: &SyncSender<SourceMsg>, stop: &AtomicBool) -> Result<(), String> {
    let mut b = Device::builder().raw_iq().frequency_hz(cfg.center_hz).sample_rate_hz(cfg.rate_hz).correction_ppm(cfg.ppm);
    b = b.gain(cfg.gain_db.map_or(GainConfig::Auto, GainConfig::Manual));
    if !cfg.serial.is_empty() {
        b = b.serial(cfg.serial.clone());
    }
    let mut dev = b.open().wait().map_err(|e| format!("open RTL-SDR {}: {e}", if cfg.serial.is_empty() { "(first)" } else { &cfg.serial }))?;
    let mut rx = dev.rx_stream().map_err(|e| e.to_string())?;
    rx.start().wait().map_err(|e| e.to_string())?;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match rx.next_block(Some(Duration::from_secs(2))).wait() {
            Ok(Some(block)) => {
                let msg = SourceMsg::Data { source, bytes: block.raw_bytes().to_vec(), dropped: block.dropped_samples() };
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
