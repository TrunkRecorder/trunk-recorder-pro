//! Opt-in test of an attached E4000 dongle. Never run by ordinary CI.
//!
//! RTLSDR_SERIAL=<serial> cargo test -p rtlsdr-nusb --test hardware_e4000 -- --ignored --nocapture --test-threads=1
#![cfg(not(target_arch = "wasm32"))]
use rtlsdr_nusb::{Complex32, Device, F32Iq, GainConfig, MaybeFuture, RxStream, TunerKind};
use std::time::Duration;

fn open() -> rtlsdr_nusb::Result<Device> {
    let builder = Device::builder().sample_rate_hz(2_400_000).frequency_hz(851_000_000);
    match std::env::var("RTLSDR_SERIAL") {
        Ok(serial) => builder.serial(serial),
        Err(_) => builder,
    }
    .open()
    .wait()
}

/// (mean power, DC offset magnitude, Q/I power ratio) over ~0.1 s.
fn measure(rx: &mut RxStream<F32Iq>) -> rtlsdr_nusb::Result<(f64, f64, f64)> {
    let mut samples = vec![Complex32::default(); 262_144];
    // Drop what predates the change.
    rx.read(&mut samples, Some(Duration::from_secs(2))).wait()?;
    let n = rx.read(&mut samples, Some(Duration::from_secs(2))).wait()?;
    assert!(n > 0, "capture timed out");
    let s = &samples[..n];
    let (mut pi, mut pq, mut di, mut dq) = (0f64, 0f64, 0f64, 0f64);
    for c in s {
        pi += f64::from(c.re * c.re);
        pq += f64::from(c.im * c.im);
        di += f64::from(c.re);
        dq += f64::from(c.im);
    }
    let n = n as f64;
    Ok(((pi + pq) / n, ((di / n).powi(2) + (dq / n).powi(2)).sqrt(), pq / pi.max(1e-12)))
}

#[test]
#[ignore = "requires exclusive access to an attached E4000 RTL-SDR"]
fn e4000_p25_bands_gain_and_lock_range() -> rtlsdr_nusb::Result<()> {
    let mut device = open()?;
    println!("{:?}", device.info());
    assert_eq!(device.info().tuner, TunerKind::E4000, "this dongle is not an E4000");

    let mut rx = device.rx_stream()?;
    rx.start().wait()?;
    println!("{:>12} {:>10} {:>10} {:>8} {:>6}", "Hz", "power", "DC", "Q/I", "gain");
    for hz in [155_000_000u64, 460_000_000, 770_000_000, 851_012_500, 860_000_000] {
        for gain in [GainConfig::Auto, GainConfig::Manual(0.0), GainConfig::Manual(29.0), GainConfig::Manual(42.0)] {
            rx.stop().wait()?;
            device.set_frequency_hz(hz).wait()?;
            device.set_gain(gain).wait()?;
            rx.start().wait()?;
            let (power, dc, ratio) = measure(&mut rx)?;
            println!("{:>12} {power:>10.6} {dc:>10.6} {ratio:>8.3} {gain:?}", device.actual_frequency_hz());
            assert!(device.actual_frequency_hz().abs_diff(hz) < 500);
            assert!(power > 1e-6, "{hz} Hz: no signal at all (constant samples)");
            // Zero IF: both ADCs carry signal. A dead Q (or I) branch shows here.
            assert!((0.2..5.0).contains(&ratio), "{hz} Hz: I/Q imbalance {ratio:.3}");
        }
    }
    // Gain must move the noise floor: 42 dB well above 0 dB at the same frequency.
    let mut floor = |gain| -> rtlsdr_nusb::Result<f64> {
        rx.stop().wait()?;
        device.set_gain(gain).wait()?;
        rx.start().wait()?;
        Ok(measure(&mut rx)?.0)
    };
    let (low, high) = (floor(GainConfig::Manual(0.0))?, floor(GainConfig::Manual(42.0))?);
    println!("noise power at 0 dB (-1 dB step) {low:.6}, at 42 dB {high:.6}");
    assert!(high > low * 2.0, "manual gain had no effect");
    rx.close().wait()?;

    // Where the PLL locks: report, don't fail (it varies between dongles).
    let mut unlocked = Vec::new();
    for mhz in (40..=1766).step_by(10) {
        if device.set_frequency_hz(mhz * 1_000_000).wait().is_err() {
            unlocked.push(mhz);
        }
    }
    println!("no lock at (MHz): {unlocked:?}");
    for mhz in [136, 174, 380, 520, 764, 870] {
        assert!(!unlocked.contains(&(mhz / 10 * 10)), "{mhz} MHz should lock");
    }
    device.set_frequency_hz(851_000_000).wait()?;
    device.shutdown().wait()?;
    Ok(())
}
