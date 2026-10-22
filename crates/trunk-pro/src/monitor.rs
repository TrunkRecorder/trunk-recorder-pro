//! The host's own watch, recording or not: every [`EVERY`] the computer
//! ([`crate::platform`]) and the plugins report into one aggregator; each
//! minute's rollup goes to the history, and each tick a `host` message to
//! the browsers: `{type:"host", t, values, platform}`.
//!
//! The RAM spool is watched here too (a disk row, `plat/disk/spool/*`, events
//! as it fills), and what's left in it too long moved out; and on macOS,
//! whether Spotlight indexes the recordings folder (`platform.spotlight`).

use trunk_app::protocol::{body, ToClient};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trunk_app::stats::{Aggregator, MonitorEvent};
use trunk_core::metrics::Sink;

use crate::platform::{Check, Platform, Probes, Watched};
use crate::runtime::{publish, Ctx};

/// Every connection hears from the recorder this often, recording or not: the
/// `host` message is the connections' heartbeat, so it goes to all of them,
/// never behind a topic or access. A browser that heard nothing for far longer
/// wasn't running (transport.ts's `FROZEN_MS`); one whose `host` statuses lag
/// the `t` stamped on them more and more is taking in a backlog (`BEHIND_MS`).
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
pub fn message(t: f64, agg: &Aggregator, platform: serde_json::Value) -> ToClient {
    ToClient::Host(body(json!({ "t": (t * 10.0).round() / 10.0, "values": agg.values_json(), "platform": platform })))
}

pub fn start(ctx: Arc<Ctx>) {
    let _ = std::thread::Builder::new().name("monitor".into()).spawn(move || run(ctx));
}

fn run(ctx: Arc<Ctx>) {
    let (checks, capture) = {
        let c = ctx.config.lock().unwrap();
        let mut checks: Vec<Check> = c.monitor.probe_hosts.iter().cloned().map(Check::Connect).collect();
        checks.extend(Some(c.monitor.time_server.trim()).filter(|s| !s.is_empty()).map(|s| Check::Time(s.to_string())));
        (checks, c.recording.capture_dir.clone())
    };
    let c2 = ctx.clone();
    let probes = Probes::start(checks, move |(target, up, down_s)| {
        c2.event(if up { MonitorEvent::LinkUp { target, down_s: down_s.round() } } else { MonitorEvent::LinkDown { target } });
    });
    let watched = vec![Watched { name: "recordings", path: capture.into() }, Watched { name: "data", path: crate::paths::data_dir() }];
    run_with(ctx, Platform::new(watched, probes));
}

fn run_with(ctx: Arc<Ctx>, mut platform: Platform) {
    let mut agg = Aggregator::default();
    let mut low: std::collections::HashMap<String, bool> = Default::default();
    let mut spool = SpoolWatch::default();
    // The clock's level as last told: 0 fine, 1 off, 2 badly off.
    let mut clock_level = 0u8;
    let spotlight: Arc<Mutex<Option<Value>>> = Default::default();
    let mut spotlight_for: Option<(String, Instant)> = None;
    loop {
        let t = crate::clock::unix_s();
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
        let problem = &p["clock"]["problem"];
        let level = match problem["level"].as_str() {
            Some("bad") => 2,
            Some(_) => 1,
            None => 0,
        };
        if level > clock_level {
            ctx.event(MonitorEvent::ClockOff { why: problem["why"].as_str().unwrap_or_default().to_string(), bad: level == 2 });
        } else if level == 0 && clock_level > 0 {
            ctx.event(MonitorEvent::ClockOk {});
        }
        clock_level = level;
        let msg = message(t, &agg, p);
        *ctx.host_last.lock().unwrap() = Some(msg.clone());
        publish(&ctx.hub, msg);
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
            // (A call held back from a plugin waits that much longer.)
            let n = s.sweep(capture, crate::spool::STALE + ctx.plugins.longest_delay());
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
