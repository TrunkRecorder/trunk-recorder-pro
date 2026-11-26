//! Device-gated integration tests — the M6 hardware-validation suite.
//!
//! Every test here talks to a real Airspy over USB and is therefore
//! `#[ignore]`d so CI (and any hardware-free machine) stays green.
//! With an Airspy connected, run them single-threaded — the tests
//! share one physical device and each opens it exclusively:
//!
//! ```sh
//! cargo test -p libairspy-rs --test device -- --ignored --test-threads=1
//! ```
//!
//! The bias tee is deliberately never enabled: these tests cannot
//! know what is attached to the antenna port, and 4.5 V on the wrong
//! hardware is destructive.

use std::time::{Duration, Instant};

use libairspy_rs::commands::{BoardId, SampleType};
use libairspy_rs::reader::SampleBlock;
use libairspy_rs::{Device, list_devices};

/// A tuning target for the streaming tests. The content of the
/// spectrum does not matter (no antenna may be attached); 100 MHz is
/// simply a valid, mid-band frequency.
const TEST_FREQ_HZ: u32 = 100_000_000;

/// Blocks to pull for the short per-type streams — enough to cross
/// several USB transfers without dragging the suite out.
const SHORT_STREAM_BLOCKS: usize = 5;

/// `airspy_set_lna_gain`'s clamp bound (`value > 14` in
/// `airspyone_host` `libairspy/src/airspy.c`).
const LNA_GAIN_MAX: u8 = 14;
/// `airspy_set_mixer_gain`'s clamp bound (`value > 15` in
/// `airspy.c`).
const MIXER_GAIN_MAX: u8 = 15;
/// `airspy_set_vga_gain`'s clamp bound (`value > 15` in `airspy.c`).
const VGA_GAIN_MAX: u8 = 15;
/// The last index of `airspy.c`'s 22-entry
/// `airspy_linearity_*_gains` / `airspy_sensitivity_*_gains` tables.
const COMPOSITE_GAIN_MAX: u8 = 21;

/// One past the largest 12-bit ADC value — the sample width of
/// `airspy.c`'s `unpack_samples` bitstream. Unpacked RAW words stay
/// below this (top nibble of every 16-bit word zero); the packed
/// 12-bit stream uses every nibble.
const ADC_WORD_LIMIT: u16 = 1 << 12;

/// How long a capture may run before the suite declares the device
/// stalled. The library's reader tolerates bulk timeouts exactly as
/// C's event loop does, so a silent device would otherwise block a
/// blocking iterator read forever.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Run a device-borrowing capture on a worker thread, failing the
/// test if it does not finish within [`STALL_TIMEOUT`] — a silent
/// device keeps the reader polling (C event-loop semantics), which
/// would otherwise hang a blocking iterator read forever. The device
/// moves through the worker and back so a timed-out capture cannot
/// race a reopened handle.
fn with_stall_guard<T: Send + 'static>(
    device: Device,
    capture: impl FnOnce(&mut Device) -> T + Send + 'static,
) -> (Device, T) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut device = device;
        // A panicking capture must surface as itself, not as a
        // 30-second "stalled" timeout: catch it and re-raise on the
        // test thread immediately.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| capture(&mut device)));
        // A dropped receiver (timed-out test) makes this send a no-op.
        let _ = tx.send(result.map(|value| (device, value)));
    });
    // On a true stall the worker stays blocked in the USB read and
    // keeps the exclusive handle — nothing outside the kernel can
    // cancel that read, so the honest behavior is a prompt, explicit
    // failure. Later opens in the same run may then fail too; with
    // hardware that wedged, replug the Airspy before rerunning.
    let outcome = rx.recv_timeout(STALL_TIMEOUT).expect(
        "device stalled: capture did not finish within STALL_TIMEOUT \
         (the worker thread still holds the USB handle; replug the \
         Airspy before rerunning)",
    );
    match outcome {
        Ok(pair) => pair,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Pull a handful of transfers in the device's current configuration,
/// asserting each reports the expected latched sample type.
fn capture_blocks(
    device: Device,
    expected_type: SampleType,
    count: usize,
) -> (Device, Vec<SampleBlock>) {
    with_stall_guard(device, move |device| {
        let mut blocks = Vec::new();
        {
            let iter = device.rx_blocks().expect("rx_blocks");
            for transfer in iter.take(count) {
                assert_eq!(
                    transfer.sample_type, expected_type,
                    "transfer reports the wrong sample type"
                );
                blocks.push(transfer.samples);
            }
        }
        assert!(!device.is_streaming(), "stream still live after drop");
        blocks
    })
}

/// Fraction of 16-bit words in a RAW block whose top nibble is used.
fn high_nibble_ratio(bytes: &[u8]) -> f64 {
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let high = words.iter().filter(|w| **w >= ADC_WORD_LIMIT).count();
    #[allow(clippy::cast_precision_loss)]
    let ratio = high as f64 / words.len().max(1) as f64;
    ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SERIAL_NUMBER_UNUSED` in `airspyone_host`'s `airspy.c` — serial 0
    /// is the "no filter" sentinel `airspy_open_sn` treats as "open the
    /// first device", so a real device's serial must never read as 0.
    const SERIAL_NUMBER_UNUSED: u64 = 0;

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn enumerates_at_least_one_device() {
        let serials = list_devices().expect("list_devices");
        assert!(!serials.is_empty(), "no Airspy enumerated");
        for serial in &serials {
            println!("enumerated serial: 0x{serial:016X}");
        }
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn discovered_serial_round_trips_through_open_serial() {
        // Exercises the serial string-descriptor path end to end (issue
        // #75, finding #1): a failed descriptor read or parse would
        // silently drop the device from list_devices AND make open_serial
        // return NotFound. Assert every discovered serial is non-zero and
        // reopens the device by that serial.
        let serials = list_devices().expect("list_devices");
        assert!(!serials.is_empty(), "no Airspy enumerated");
        for serial in serials {
            assert_ne!(
                serial, SERIAL_NUMBER_UNUSED,
                "serial descriptor read/parsed as zero"
            );
            println!("reopening discovered serial: 0x{serial:016X}");
            let device = Device::open_serial(serial)
                .expect("open_serial with a serial that discovery just read");
            drop(device);
        }
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn identity_reads_match_an_airspy() {
        let device = Device::open().expect("open");
        let board_id = device.board_id().expect("board_id");
        println!("board id: {board_id}");
        assert_eq!(
            BoardId::from_u8(board_id),
            Some(BoardId::ProtoAirspy),
            "unexpected board id {board_id}"
        );

        let version = device.version_string().expect("version_string");
        println!("firmware: {version}");
        assert!(!version.is_empty());
        assert!(version.contains("AirSpy"), "unexpected version: {version}");

        let ids = device.partid_serialno().expect("partid_serialno");
        println!(
            "part id: 0x{:08X} 0x{:08X}, serial: 0x{:08X}{:08X}",
            ids.part_id[0], ids.part_id[1], ids.serial_no[2], ids.serial_no[3]
        );
        assert_ne!(ids.serial_no, [0; 4], "serial reads as all zero");
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn samplerate_table_is_populated() {
        let device = Device::open().expect("open");
        let rates = device.samplerates();
        println!("supported IQ rates: {rates:?}");
        assert!(!rates.is_empty());
        // Every known Airspy firmware advertises 10 MSPS; the R2 also
        // lists 2.5 MSPS.
        assert!(rates.contains(&10_000_000), "10 MSPS missing: {rates:?}");
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn control_surface_accepts_the_full_range() {
        let mut device = Device::open().expect("open");
        device.set_samplerate(0).expect("set_samplerate index 0");
        device.set_freq(TEST_FREQ_HZ).expect("set_freq");

        // Gain endpoints (clamping covered by unit tests; here the wire
        // must accept the extremes).
        device.set_lna_gain(0).expect("lna 0");
        device.set_lna_gain(LNA_GAIN_MAX).expect("lna max");
        device.set_mixer_gain(0).expect("mixer 0");
        device.set_mixer_gain(MIXER_GAIN_MAX).expect("mixer max");
        device.set_vga_gain(0).expect("vga 0");
        device.set_vga_gain(VGA_GAIN_MAX).expect("vga max");
        device.set_linearity_gain(0).expect("linearity 0");
        device
            .set_linearity_gain(COMPOSITE_GAIN_MAX)
            .expect("linearity max");
        device.set_sensitivity_gain(0).expect("sensitivity 0");
        device
            .set_sensitivity_gain(COMPOSITE_GAIN_MAX)
            .expect("sensitivity max");
        device.set_lna_agc(false).expect("lna agc off");
        device.set_mixer_agc(false).expect("mixer agc off");
        device.set_packing(false).expect("packing off");
        // NOTE: set_rf_bias(true) is intentionally absent — see the
        // module docs. Turning it off is safe regardless.
        device.set_rf_bias(false).expect("bias off");
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn streams_every_sample_type() {
        let mut device = Device::open().expect("open");
        device.set_samplerate(0).expect("rate");
        device.set_freq(TEST_FREQ_HZ).expect("freq");

        for sample_type in [
            SampleType::Float32Iq,
            SampleType::Float32Real,
            SampleType::Int16Iq,
            SampleType::Int16Real,
            SampleType::Uint16Real,
            SampleType::Raw,
        ] {
            device.set_sample_type(sample_type).expect("set type");
            let (returned, blocks) = capture_blocks(device, sample_type, SHORT_STREAM_BLOCKS);
            device = returned;
            assert_eq!(blocks.len(), SHORT_STREAM_BLOCKS, "{sample_type:?}");
            for block in &blocks {
                // Each sample type must arrive in its own block
                // variant — the conversion dispatch, not just the
                // latched metadata.
                let (len, finite) = match (sample_type, block) {
                    (SampleType::Float32Iq | SampleType::Float32Real, SampleBlock::Float32(s)) => {
                        (s.len(), s.iter().all(|v| v.is_finite()))
                    }
                    (SampleType::Int16Iq | SampleType::Int16Real, SampleBlock::Int16(s)) => {
                        (s.len(), true)
                    }
                    (SampleType::Uint16Real, SampleBlock::Uint16(s)) => (s.len(), true),
                    (SampleType::Raw, SampleBlock::Raw(s)) => (s.len(), true),
                    (_, other) => {
                        unreachable!("{sample_type:?} delivered the wrong variant: {other:?}")
                    }
                };
                assert!(len > 0, "{sample_type:?}: empty block");
                assert!(finite, "{sample_type:?}: non-finite samples");
            }
            println!("{sample_type:?}: {} blocks OK", blocks.len());
        }
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn packed_raw_capture_streams() {
        let mut device = Device::open().expect("open");
        device.set_samplerate(0).expect("rate");
        device.set_freq(TEST_FREQ_HZ).expect("freq");
        device.set_sample_type(SampleType::Raw).expect("type");

        // Unpacked RAW: 12-bit ADC values in 16-bit words — every word
        // below ADC_WORD_LIMIT, top nibbles all zero.
        device.set_packing(false).expect("packing off");
        let (returned, unpacked) = capture_blocks(device, SampleType::Raw, SHORT_STREAM_BLOCKS);
        device = returned;
        assert_eq!(unpacked.len(), SHORT_STREAM_BLOCKS);
        for block in &unpacked {
            let SampleBlock::Raw(bytes) = block else {
                unreachable!("expected raw block, got {block:?}");
            };
            assert!(!bytes.is_empty());
            assert!(
                high_nibble_ratio(bytes) == 0.0,
                "unpacked RAW words exceed 12 bits"
            );
        }

        // Packed RAW: a continuous 12-bit stream uses every nibble, so a
        // substantial share of word-aligned reads land above the 12-bit
        // limit — this is what proves set_packing(true) took effect on
        // the wire (block byte counts are identical in both modes).
        device.set_packing(true).expect("packing on");
        let (returned, packed) = capture_blocks(device, SampleType::Raw, SHORT_STREAM_BLOCKS);
        device = returned;
        assert_eq!(packed.len(), SHORT_STREAM_BLOCKS);
        for block in &packed {
            let SampleBlock::Raw(bytes) = block else {
                unreachable!("expected raw block, got {block:?}");
            };
            assert!(!bytes.is_empty());
            assert!(
                high_nibble_ratio(bytes) > 0.05,
                "packed RAW stream looks unpacked (top nibbles unused)"
            );
        }
        device.set_packing(false).expect("packing off again");
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn sustained_capture_has_no_drops() {
        const CAPTURE_SECONDS: u64 = 3;

        let mut device = Device::open().expect("open");
        device.set_samplerate(0).expect("rate (index 0 = fastest)");
        device.set_freq(TEST_FREQ_HZ).expect("freq");
        device
            .set_sample_type(SampleType::Int16Iq)
            .expect("int16 iq");

        let rates = device.samplerates();
        let nominal = rates.first().copied().expect("a rate");

        let started = Instant::now();
        let (device, (dropped, frames)) = with_stall_guard(device, move |device| {
            let mut dropped = 0u64;
            let mut frames = 0u64;
            let iter = device.rx_blocks().expect("rx_blocks");
            for transfer in iter {
                dropped += transfer.dropped_samples;
                if let SampleBlock::Int16(samples) = &transfer.samples {
                    frames += samples.len() as u64 / 2;
                }
                if started.elapsed() >= Duration::from_secs(CAPTURE_SECONDS) {
                    break;
                }
            }
            (dropped, frames)
        });
        drop(device);
        let elapsed = started.elapsed().as_secs_f64();
        // Frame counts over a few seconds sit far below 2^52.
        #[allow(clippy::cast_precision_loss)]
        let rate = frames as f64 / elapsed;
        println!(
            "sustained: {frames} IQ frames in {elapsed:.2}s = {:.3} MSPS (nominal {:.1}), dropped {dropped}",
            rate / 1e6,
            f64::from(nominal) / 1e6
        );
        assert_eq!(dropped, 0, "dropped samples during sustained capture");
        // Within 10% of nominal proves the pipeline keeps up.
        assert!(
            rate > f64::from(nominal) * 0.9,
            "throughput {rate} too far below nominal {nominal}"
        );
    }

    #[test]
    #[ignore = "requires Airspy hardware"]
    fn stream_restarts_cleanly() {
        let mut device = Device::open().expect("open");
        device.set_samplerate(0).expect("rate");
        device.set_freq(TEST_FREQ_HZ).expect("freq");
        device.set_sample_type(SampleType::Float32Iq).expect("type");
        for round in 0..3 {
            let (returned, blocks) = capture_blocks(device, SampleType::Float32Iq, 2);
            device = returned;
            assert_eq!(blocks.len(), 2, "round {round}");
        }
    }
}
