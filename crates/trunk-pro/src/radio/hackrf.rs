//! HackRF One over USB (sdr-hackrf: pure Rust over nusb, no libhackrf or
//! libusb to install). SoapySDR's `driver=hackrf` stays for what this
//! doesn't open (a rad1o, a HackRF through SoapyRemote) — and on Windows,
//! where sdr-hackrf doesn't build (its control transfers are the device's,
//! which nusb offers only through an interface there).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use sdr_core::TuneRequest;

use crate::sdr::{Control, Gain, Radio, Sink, Stage, Stages};

#[derive(Clone, Debug)]
pub struct HackrfConfig {
    /// Serial number ("" = the first).
    pub serial: String,
    pub center_hz: f64,
    pub rate_hz: f64,
    /// The IF LNA ([`Stage::Lna`], 0–40 dB in 8 dB steps), the baseband VGA
    /// ([`Stage::Vga`], 0–62 dB in 2 dB steps), the +14 dB RF amp ([`Stage::Amp`],
    /// on when above 0: off near strong signals, it overloads easily).
    pub gain: Stages,
    /// Power an LNA over the antenna port.
    pub bias_tee: bool,
    pub ppm: f64,
}

/// The oldest firmware that answers what a change while streaming needs (its
/// byte count, sdr-hackrf's `m0_state`): 2021.03.
/// The stages as the HackRF takes them: LNA and VGA dB, the amp on or off.
#[cfg_attr(windows, allow(dead_code))]
fn levels(s: &Stages) -> (u16, u16, bool) {
    let db = |stage| s.get(stage).unwrap_or(0.0).max(0.0) as u16;
    (db(Stage::Lna), db(Stage::Vga), s.get(Stage::Amp).is_some_and(|a| a > 0.0))
}

#[cfg_attr(windows, allow(dead_code))]
const FIRMWARE_MIN: (u32, u32) = (2021, 3);

/// "2024.02.1" → (2024, 2); None for a build that doesn't say ("git-…").
#[cfg_attr(windows, allow(dead_code))]
fn release(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?))
}

/// Attached HackRFs, for the browser: [{serial, label}].
#[cfg(windows)]
pub fn devices() -> Vec<serde_json::Value> {
    Vec::new()
}

/// Attached HackRFs, for the browser: [{serial, label}].
#[cfg(not(windows))]
pub fn devices() -> Vec<serde_json::Value> {
    sdr_hackrf::blocking::list()
        .into_iter()
        .map(|d| {
            let serial = d.spec.split_once("serial=").map(|(_, s)| s.to_string()).unwrap_or_default();
            serde_json::json!({ "serial": serial, "label": d.label })
        })
        .collect()
}

impl HackrfConfig {
    fn name(&self) -> String {
        format!("HackRF {}", if self.serial.is_empty() { "(first)" } else { &self.serial })
    }
    /// What the radio is asked for: the frequency corrected by `ppm`.
    fn tune_request(&self, hz: f64) -> TuneRequest {
        TuneRequest::auto(super::ppm_corrected(hz, self.ppm))
    }
}

impl Radio for HackrfConfig {
    const BACKOFF: Duration = Duration::from_secs(2);
    fn rate_hz(&self) -> f64 {
        self.rate_hz
    }
    fn retune_to(&mut self, hz: f64) {
        self.center_hz = hz;
    }
    fn gain_to(&mut self, g: Gain) {
        if let Gain::Stages(s) = g {
            self.gain = s;
        }
    }
    fn stream_once(&self, sink: &mut Sink, stop: &Arc<AtomicBool>, ctl: Option<&Control>) -> Result<(), String> {
        stream_once(self, sink, stop, ctl)
    }
}

#[cfg(windows)]
fn stream_once(cfg: &HackrfConfig, _: &mut Sink, _: &AtomicBool, _: Option<&Control>) -> Result<(), String> {
    Err(format!("{}: the built-in HackRF driver isn't available on Windows yet — add it as a SoapySDR source with device arguments driver=hackrf instead", cfg.name()))
}

#[cfg(not(windows))]
use native::stream_once;

#[cfg(not(windows))]
mod native {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use sdr_core::Iq;
    use sdr_hackrf::blocking::{HackRf, Stream};

    use super::{release, HackrfConfig, FIRMWARE_MIN};
    use crate::radio::Deadline;
    use crate::sdr::{Applied, Control, Gain, Retuned, Sink, StreamCheck, Tunable, Wire};

    /// A HackRF streaming, as [`Control`]'s requests change it. It keeps
    /// sampling through a change (placed by the firmware's own byte count): no gap.
    struct Live<'a> {
        stream: &'a Stream,
        cfg: &'a HackrfConfig,
    }

    impl Tunable for Live<'_> {
        fn tune(&mut self, hz: f64) -> Result<Retuned, String> {
            self.stream.retune(&self.cfg.tune_request(hz)).map_err(|e| format!("tune {:.4} MHz: {e}", hz / 1e6))?;
            Ok(Retuned { hz, gap: 0 })
        }
        fn set_gain(&mut self, g: Gain) -> Result<u64, String> {
            let r = match g {
                Gain::Stages(s) => {
                    let (lna, vga, amp) = super::levels(&s);
                    self.stream.reconfigure(async |h| {
                        h.set_lna_gain(lna).await?;
                        h.set_vga_gain(vga).await?;
                        h.set_amp(amp).await
                    })
                }
                // (The survey's one number, split across the stages by the driver.)
                Gain::Db(db) => self.stream.set_gain(db as f64).map(|b| ((), b)),
                Gain::Agc | Gain::Preset(..) => return Ok(0),
            };
            r.map(|_| 0).map_err(|e| format!("gain {g}: {e}"))
        }
    }

    pub(super) fn stream_once(cfg: &HackrfConfig, sink: &mut Sink, stop: &AtomicBool, ctl: Option<&Control>) -> Result<(), String> {
        let name = cfg.name();
        let spec = if cfg.serial.is_empty() { String::new() } else { format!("serial={}", cfg.serial) };
        let what = name.clone();
        let mut h = crate::radio::open(&name, Deadline::Quick, true, move || HackRf::open(&spec).map_err(|e| format!("open {what}: {e}")))?;
        let version = h.version_string().unwrap_or_default();
        if release(&version).is_some_and(|r| r < FIRMWARE_MIN) {
            return Err(format!(
                "{name}'s firmware ({version}) is older than {}.{:02}, which streaming needs: update it (hackrf_spiflash; see the HackRF documentation's \"Updating Firmware\")",
                FIRMWARE_MIN.0, FIRMWARE_MIN.1
            ));
        }
        let (lna, vga, amp) = super::levels(&cfg.gain);
        let gain = |h: &mut HackRf| -> sdr_core::Result<()> {
            h.set_lna_gain(lna)?;
            h.set_vga_gain(vga)?;
            h.set_amp(amp)?;
            h.set_bias_tee(cfg.bias_tee)
        };
        h.set_rate(cfg.rate_hz).and_then(|()| h.set_freq(&cfg.tune_request(cfg.center_hz)).map(drop)).and_then(|()| gain(&mut h)).map_err(|e| format!("{name}: {e}"))?;
        log::info!("{name} ({}, firmware {version}): {:.4} MHz, {:.3} MSPS, {}", h.label(), cfg.center_hz / 1e6, cfg.rate_hz / 1e6, cfg.gain);
        // (The driver's default transfers: 32 × 64 KiB in flight.)
        let mut stream = h.into_stream(0, 0).map_err(|e| format!("{name}: {}", e.error))?;
        stream.start().map_err(|e| format!("{name}: {e}"))?;
        let mut check = StreamCheck::new(cfg.rate_hz);
        let mut iq = vec![Iq::default(); stream.max_num_samps().max(16_384)];
        let mut bytes = Vec::with_capacity(iq.len() * 2);
        let mut timeouts = 0;
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            if let Some(c) = ctl {
                match c.apply(&mut Live { stream: &stream, cfg }, cfg.center_hz, sink) {
                    Err(e) => break Err(e),
                    Ok(Applied::Closed) => break Ok(()),
                    Ok(_) => {}
                }
            }
            let info = match stream.recv_into(&mut iq, 1.0) {
                Ok(i) => i,
                Err(e) => break Err(format!("{name} USB: {e}")),
            };
            if info.timed_out && info.n_samples == 0 {
                timeouts += 1;
                if timeouts >= 2 {
                    break Err(format!("{name}: no samples for 2 s"));
                }
                continue;
            }
            timeouts = 0;
            let at = Instant::now();
            // Back to the radio's own signed bytes (the driver widened them: `<< 8`).
            bytes.clear();
            bytes.extend(iq[..info.n_samples].iter().flat_map(|s| [(s.i >> 8) as u8, (s.q >> 8) as u8]));
            if let Err(e) = check.block(&bytes, at) {
                break Err(format!("{name} {e}; reopening it. If this keeps happening, unplug it and plug it back in"));
            }
            let dropped = info.link.map_or(0, |l| l.inferred_lost_samples);
            if !sink.bytes(Wire::Signed, &bytes, dropped, at) {
                break Ok(());
            }
        };
        if let Ok(f) = stream.finish() {
            log::debug!("{name}: stream finished: {:?}", f.report);
        }
        result
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_releases() {
        assert_eq!(release("2024.02.1"), Some((2024, 2)));
        assert_eq!(release("2021.03"), Some((2021, 3)));
        assert_eq!(release("git-5b8ad4c"), None);
        assert!(release("2018.01.1").is_some_and(|r| r < FIRMWARE_MIN));
        assert!(!release("2021.03.1").is_some_and(|r| r < FIRMWARE_MIN));
    }
}
