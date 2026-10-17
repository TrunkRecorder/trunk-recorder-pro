//! Opt-in test of a physically attached receiver. Never run by ordinary CI.
#![cfg(not(target_arch = "wasm32"))]
use rtlsdr_nusb::{Complex32, Device, Error, GainConfig, MaybeFuture};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires exclusive access to an attached RTL-SDR; bias power remains off"]
fn capture_retune_restart_and_final_owner_cleanup() -> rtlsdr_nusb::Result<()> {
    let serial = std::env::var("RTLSDR_SERIAL").ok();
    let builder = Device::builder().sample_rate_hz(2_048_000);
    let mut device = if let Some(serial) = &serial {
        builder.serial(serial)
    } else {
        builder
    }
    .open()
    .wait()?;
    println!("{:?}", device.info());
    let identity = device.info().descriptor.serial.clone();
    let mut rx = device.rx_stream()?;
    assert!(matches!(device.shutdown().wait(), Err(Error::Busy)));
    rx.start().wait()?;
    let mut samples = vec![Complex32::default(); 65536];
    let start = Instant::now();
    let mut count = 0;
    let mut power = 0.0f64;
    while start.elapsed() < Duration::from_secs(2) {
        let n = rx.read(&mut samples, Some(Duration::from_secs(2))).wait()?;
        assert!(n > 0, "capture timed out");
        count += n;
        for sample in &samples[..n] {
            assert!(sample.re.is_finite() && sample.im.is_finite());
            power += f64::from(sample.norm_sqr());
        }
    }
    println!(
        "{count} complex samples in {:?}; mean power {:.6}",
        start.elapsed(),
        power / count as f64
    );
    assert!(count >= 2_048_000);
    assert!(
        power / count as f64 > 0.0001,
        "unexpected constant center-code data"
    );
    for (hz, rate) in [
        (433_920_000, 1_024_000),
        (1_090_000_000, 2_400_000),
        (100_000_000, 2_048_000),
    ] {
        rx.stop().wait()?;
        device.set_frequency_hz(hz).wait()?;
        device.set_sample_rate_hz(rate).wait()?;
        device.set_gain(GainConfig::Manual(28.0)).wait()?;
        device.set_correction_ppm(1).wait()?;
        rx.start().wait()?;
        let n = rx.read(&mut samples, Some(Duration::from_secs(2))).wait()?;
        assert!(n > 0);
        println!(
            "retune: {} Hz, {} samples/s, {n} samples",
            device.actual_frequency_hz(),
            device.actual_sample_rate_hz()
        );
    }
    println!("{:?}", rx.close().wait()?);
    device.shutdown().wait()?;
    device.shutdown().wait()?;
    drop(device); // release USB interface before reopening

    let builder = Device::builder().raw_iq();
    let mut device = if let Some(serial) = &identity {
        builder.serial(serial)
    } else {
        builder
    }
    .open()
    .wait()?;
    let mut rx = device.rx_stream()?;
    rx.start().wait()?;
    let block = rx
        .next_block(Some(Duration::from_secs(2)))
        .wait()?
        .expect("raw capture timed out");
    println!(
        "raw: {} samples, {} bytes",
        block.sample_count(),
        block.raw_bytes().len()
    );
    assert_eq!(block.raw_bytes().len(), 2 * block.sample_count());
    assert!(block.raw_bytes().iter().any(|&b| b != block.raw_bytes()[0]));
    rx.stop().wait()?;
    assert!(matches!(device.shutdown().wait(), Err(Error::Busy)));
    drop(device);
    rx.start().wait()?; // stopped stream owns the hardware after device drop
    assert!(
        rx.next_block(Some(Duration::from_secs(2)))
            .wait()?
            .is_some()
    );
    drop(rx.close()); // unpolled close triggers final-owner cleanup

    let builder = Device::builder();
    let mut device = if let Some(serial) = &identity {
        builder.serial(serial)
    } else {
        builder
    }
    .open()
    .wait()?;
    device.shutdown().wait()?;
    Ok(())
}
