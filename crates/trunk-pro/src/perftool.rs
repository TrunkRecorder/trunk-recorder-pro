//! `trunk-pro tool perf <rtl|hackrf|airspy> <capture.cu8> [--rate Hz]
//! [--passes N]` — one radio's read path, on this thread, over a capture:
//! the bytes in blocks as its driver hands them over, through the driver's
//! own per-block code (the radio module's `take`) and the [`Sink`], and out
//! of the [`Inbox`] its decoding thread reads. Nothing else runs, so an
//! instruction count of the whole process is this path's cost (perf/gate.sh).
//!
//! `--passes N` goes over the capture N times; the difference between two
//! pass counts is one pass's cost alone, without the start-up and the
//! capture's loading.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::radio::{hackrf, rtl};
use crate::sdr::{Inbox, Sink, StreamCheck};
use crate::{die, Args};

pub fn run(a: &Args) {
    let stage = a.positional.get(1).map(String::as_str).unwrap_or_else(|| die("tool perf rtl|hackrf|airspy <capture.cu8>"));
    let path = a.positional.get(2).unwrap_or_else(|| die("tool perf: no capture"));
    let data = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let rate = a.num("rate", 2_400_000.0);
    let passes = a.num("passes", 1.0) as usize;
    // (A queue as deep as a radio's: drained after every block, so it never fills.)
    let (tx, rx) = mpsc::sync_channel(256);
    let mut inbox = Inbox::new(rx);
    let mut drain = || while inbox.try_recv().is_ok() {};
    let t0 = Instant::now();
    // When block k came, at the radio's own rate.
    let at = |k: usize, len: usize| t0 + Duration::from_secs_f64((k * len) as f64 / 2.0 / rate);
    match stage {
        "rtl" => {
            // The dongle's 16 KiB transfers.
            for _ in 0..passes {
                let (mut check, mut sink) = (StreamCheck::new(rate), Sink::new(0, tx.clone(), rate, true));
                for (k, b) in data.chunks_exact(16_384).enumerate() {
                    rtl::take(&mut check, &mut sink, b, 0, at(k, b.len())).unwrap_or_else(|e| die(&e));
                    drain();
                }
                drop(sink);
                drain();
            }
        }
        "hackrf" => {
            // As sdr-hackrf hands them over: signed bytes widened (`<< 8`), 16 Ki samples at a time.
            let iq: Vec<sdr_core::Iq> = data.chunks_exact(2).map(|p| sdr_core::Iq { i: ((p[0] ^ 0x80) as i8 as i16) << 8, q: ((p[1] ^ 0x80) as i8 as i16) << 8 }).collect();
            let mut bytes = Vec::new();
            for _ in 0..passes {
                let (mut check, mut sink) = (StreamCheck::new(rate), Sink::new(0, tx.clone(), rate, true));
                for (k, b) in iq.chunks_exact(16_384).enumerate() {
                    hackrf::take(&mut check, &mut sink, &mut bytes, b, 0, at(k, 2 * b.len())).unwrap_or_else(|e| die(&e));
                    drain();
                }
                drop(sink);
                drain();
            }
        }
        "airspy" => {
            // libairspy's INT16_REAL transfers (65,536 12-bit samples), made I/Q in the inbox.
            let real: Vec<i16> = data.iter().map(|&b| (b as i16 - 128) << 4).collect();
            for _ in 0..passes {
                let mut sink = Sink::new(0, tx.clone(), rate, true);
                for (k, b) in real.chunks_exact(65_536).enumerate() {
                    sink.real16(b, 0, at(k, b.len()));
                    drain();
                }
                drop(sink);
                drain();
            }
        }
        _ => die(&format!("tool perf {stage}: rtl, hackrf or airspy")),
    }
}
