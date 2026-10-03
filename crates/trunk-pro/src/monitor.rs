//! The host's own watch, recording or not: every [`EVERY`] the computer
//! ([`crate::platform`]) and the plugins report into one aggregator; each
//! minute's rollup goes to the history, and each tick a `host` message to
//! the browsers: `{type:"host", t, values, platform}`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use trunk_app::stats::{Aggregator, MonitorEvent};

use crate::platform::{Platform, Probes, Watched};
use crate::runtime::{publish, Ctx};

pub const EVERY: Duration = Duration::from_secs(2);
/// A watched disk below this much free, %, is an event.
const DISK_LOW_PCT: f64 = 5.0;

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
    loop {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        if let Some(r) = agg.begin(t) {
            ctx.store.add(r);
        }
        let detail = ctx.topics.lock().unwrap().contains_key("platform");
        let p = platform.sample(&mut agg, detail);
        ctx.plugins.report(&mut agg);
        for d in p["disks"].as_array().into_iter().flatten() {
            let (Some(name), Some(total), Some(free)) = (d["name"].as_str(), d["totalBytes"].as_f64(), d["freeBytes"].as_f64()) else { continue };
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
