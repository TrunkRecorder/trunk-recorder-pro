//! Profiling a source's roll-off for Setup (`profileSource`): open it alone
//! for a moment, look at its band ([`Profiler`]), close it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use trunk_app::profile::{self, Request};
use trunk_core::dsp::rolloff::Profiler;

use crate::config::{Config, Source};
use crate::sdr::{Control, SourceMsg};

/// Give up on a source that sends nothing for this long.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The `sourceProfile` answer (or its error).
pub fn run(cfg: &Config, req: &Request) -> Value {
    match measure(cfg, req) {
        Ok(v) => v,
        Err(e) => profile::error_json(req.source, &e),
    }
}

fn measure(cfg: &Config, req: &Request) -> Result<Value, String> {
    let src = cfg.sources.get(req.source).ok_or("No such source.")?;
    let center = match src {
        Source::File { .. } => cfg.resolved_centers()[req.source],
        _ if req.center_hz > 0.0 => req.center_hz,
        _ => return Err("Choose a frequency to look at.".into()),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<SourceMsg>(256);
    let th = crate::survey::spawn_source(src, req.source, center, tx, stop.clone(), Arc::new(Control::default()))?;
    let mut p = Profiler::new(src.rate_hz(), profile::SECONDS);
    let mut last = Instant::now();
    let mut err = None;
    while !p.done() {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(SourceMsg::Data { bytes, .. }) => {
                p.push_u8(&bytes);
                last = Instant::now();
            }
            Ok(SourceMsg::Iq { samples, .. }) => {
                p.push_iq(&samples);
                last = Instant::now();
            }
            Ok(SourceMsg::Error { error, .. }) => {
                err = Some(error);
                break;
            }
            Ok(SourceMsg::End { .. }) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(SourceMsg::Tuned { .. }) => {}
            Err(RecvTimeoutError::Timeout) if last.elapsed() > TIMEOUT => {
                err = Some("the source sent nothing".into());
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = th.join();
    match (p.result(), err) {
        (_, Some(e)) => Err(e),
        (None, None) => Err("The source ended before anything could be seen.".into()),
        (Some(r), None) => Ok(profile::json(req.source, center, &r)),
    }
}

/// `trunk-pro rolloff`: profile a dongle (or a capture) and print the result
/// (`--full`: with its spectra and waterfall).
pub fn cli(a: &crate::Args) {
    let rate = a.num("rate", 2_400_000.0);
    let center = a.num("center", 0.0);
    let guard_hz = crate::config::DEFAULT_GUARD_HZ;
    let src = match a.positional.first() {
        Some(path) => Source::File { path: path.clone(), center_hz: center, rate_hz: rate, realtime: false, format: Some(crate::sample_format(path, a.get("format"))), auto_tune: false, guard_hz },
        None => Source::Rtlsdr {
            serial: a.get("serial").unwrap_or("").into(),
            center_hz: center,
            rate_hz: rate,
            gain_db: a.get("gain").and_then(|g| g.parse().ok()).unwrap_or(crate::config::RTL_DEFAULT_GAIN_DB),
            agc: false,
            ppm: 0,
            auto_tune: false,
            guard_hz,
        },
    };
    let cfg = Config { sources: vec![src], ..Default::default() };
    let mut v = run(&cfg, &Request { source: 0, center_hz: center });
    if let Some(e) = v["error"].as_str() {
        crate::die(e);
    }
    if !a.flag("full") {
        for k in ["spectrum", "floor", "rows"] {
            v.as_object_mut().unwrap().remove(k);
        }
    }
    println!("{v}");
}
