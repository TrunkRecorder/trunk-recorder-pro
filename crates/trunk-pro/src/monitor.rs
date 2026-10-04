//! The host's own watch, recording or not: every [`EVERY`] the computer
//! ([`crate::platform`]) and the plugins report into one aggregator; each
//! minute's rollup goes to the history, and each tick a `host` message to
//! the browsers: `{type:"host", t, values, platform}`.
//!
//! The RAM spool is watched here too (a disk row, `plat/disk/spool/*`, events
//! as it fills), and what's left in it too long moved out; and on macOS,
//! whether Spotlight indexes the recordings folder (`platform.spotlight`).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use trunk_app::stats::{Aggregator, MonitorEvent};
use trunk_core::metrics::Sink;

use crate::platform::{Platform, Probes, Watched};
use crate::runtime::{publish, Ctx};

pub const EVERY: Duration = Duration::from_secs(2);
/// A watched disk below this much free, %, is an event.
const DISK_LOW_PCT: f64 = 5.0;
/// The spool below this much free, %: uploads aren't keeping up; and nearly full.
const SPOOL_LOW_PCT: f64 = 25.0;
const SPOOL_CRITICAL_PCT: f64 = 10.0;
/// How often the spool is swept of files nothing settled.
const SWEEP_EVERY: Duration = Duration::from_secs(600);
/// How often Spotlight is asked again (and when the recordings folder changes).
const SPOTLIGHT_EVERY: Duration = Duration::from_secs(24 * 3600);

/// The `host` message: the series' values this tick, and the platform's own figures.
pub fn message(t: f64, agg: &Aggregator, platform: serde_json::Value) -> serde_json::Value {
    json!({ "type": "host", "t": (t * 10.0).round() / 10.0, "values": agg.values_json(), "platform": platform })
}

pub fn start(ctx: Arc<Ctx>) {
    let _ = std::thread::Builder::new().name("monitor".into()).spawn(move || run(ctx));
}

fn run(ctx: Arc<Ctx>) {
    let (targets, capture) = {
        let c = ctx.config.lock().unwrap();
        (c.monitor.probe_hosts.clone(), c.recording.capture_dir.clone())
    };
    let c2 = ctx.clone();
    let probes = Probes::start(targets, move |(target, up, down_s)| {
        c2.event(if up { MonitorEvent::LinkUp { target, down_s: down_s.round() } } else { MonitorEvent::LinkDown { target } });
    });
    let watched = vec![Watched { name: "recordings", path: capture.into() }, Watched { name: "data", path: crate::paths::data_dir() }];
    run_with(ctx, Platform::new(watched, probes));
}

fn run_with(ctx: Arc<Ctx>, mut platform: Platform) {
    let mut agg = Aggregator::default();
    let mut low: std::collections::HashMap<String, bool> = Default::default();
    let mut spool = SpoolWatch::default();
    let spotlight: Arc<Mutex<Option<Value>>> = Default::default();
    let mut spotlight_for: Option<(String, Instant)> = None;
    loop {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        if let Some(r) = agg.begin(t) {
            ctx.store.add(r);
        }
        let detail = ctx.topics.lock().unwrap().contains_key("platform");
        let mut p = platform.sample(&mut agg, detail);
        ctx.plugins.report(&mut agg);
        let capture = ctx.config.lock().unwrap().recording.capture_dir.clone();
        if let Some(s) = ctx.spool.lock().unwrap().clone() {
            spool.watch(&ctx, &s, Path::new(&capture), &mut agg, &mut p);
        }
        if spotlight_for.as_ref().is_none_or(|(d, at)| *d != capture || at.elapsed() >= SPOTLIGHT_EVERY) {
            spotlight_for = Some((capture.clone(), Instant::now()));
            let out = spotlight.clone();
            let _ = std::thread::Builder::new().name("spotlight".into()).spawn(move || *out.lock().unwrap() = crate::spotlight::check(Path::new(&capture)));
        }
        p["spotlight"] = spotlight.lock().unwrap().clone().unwrap_or(Value::Null);
        for d in p["disks"].as_array().into_iter().flatten() {
            let (Some(name), Some(total), Some(free)) = (d["name"].as_str(), d["totalBytes"].as_f64(), d["freeBytes"].as_f64()) else { continue };
            if name == "spool" {
                continue;
            }
            let pct = 100.0 * free / total.max(1.0);
            let is_low = pct < DISK_LOW_PCT;
            if low.insert(name.to_string(), is_low) != Some(is_low) && is_low {
                ctx.event(MonitorEvent::DiskLow { path: name.to_string(), free_pct: (pct * 10.0).round() / 10.0 });
            }
        }
        let msg = message(t, &agg, p);
        publish(&ctx.hub, msg.clone());
        *ctx.host_last.lock().unwrap() = Some(msg);
        std::thread::sleep(EVERY);
    }
}

/// The spool, from one tick to the next.
#[derive(Default)]
struct SpoolWatch {
    /// 0 fine, 1 low, 2 nearly full: as last told.
    level: u8,
    overflowed: u64,
    swept: Option<Instant>,
}

impl SpoolWatch {
    /// Its disk row (into the platform's `disks`) and figures; events as it fills and empties.
    fn watch(&mut self, ctx: &Ctx, s: &crate::spool::Spool, capture: &Path, sink: &mut dyn Sink, p: &mut Value) {
        if self.swept.is_none_or(|t| t.elapsed() >= SWEEP_EVERY) {
            self.swept = Some(Instant::now());
            let n = s.sweep(capture, crate::spool::STALE);
            if n > 0 {
                log::warn!("{n} file(s) left in the RAM spool by calls no plugin finished: moved to {}", capture.display());
            }
        }
        let n = s.overflowed();
        if n > self.overflowed {
            ctx.event(MonitorEvent::SpoolFull { calls: n - self.overflowed });
            self.overflowed = n;
        }
        let Some((total, free)) = s.usage().filter(|u| u.0 > 0) else { return };
        let pct = 100.0 * free as f64 / total as f64;
        sink.gauge("plat/disk/spool/free", free as f64);
        sink.gauge("plat/disk/spool/freePct", pct);
        let row = json!({ "name": "spool", "path": s.dir.display().to_string(), "mount": s.dir.display().to_string(), "kind": s.kind, "totalBytes": total, "freeBytes": free, "overflowed": n });
        if let Some(d) = p["disks"].as_array_mut() {
            d.push(row);
        }
        let level = if pct < SPOOL_CRITICAL_PCT { 2 } else if pct < SPOOL_LOW_PCT { 1 } else { 0 };
        let free_pct = (pct * 10.0).round() / 10.0;
        if level > self.level {
            ctx.event(MonitorEvent::SpoolLow { free_pct, critical: level == 2 });
        } else if level == 0 && self.level > 0 {
            ctx.event(MonitorEvent::SpoolRecovered { free_pct });
        }
        self.level = level;
    }
}
