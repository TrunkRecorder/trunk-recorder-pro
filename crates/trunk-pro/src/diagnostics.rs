//! A capture's `diagnostics.json`: the machine and the recorder's state, for
//! whoever helps with a problem (research/iq-corpus.md). It's published with
//! the capture, so it's built field by field: what isn't named here stays
//! out, and no free text (a log line can hold a path or an address).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sysinfo::{Disks, System};
use trunk_app::config::{Config, NameLists};

use crate::iq_capture::{keep, Activity, Live, QUEUE, RADIO_KEYS, RECORDING_KEYS};

/// Of the recent events, the ones and fields that say how the radios and the
/// clock did. A source's name (it can hold a serial number) becomes its number.
const EVENT_KINDS: &[&str] = &["controlLost", "controlRegained", "sourceDrops", "sourceClipping", "sourceRecovered", "clockOff", "clockOk", "clockStep", "diskLow", "linkDown", "linkUp", "spoolLow", "spoolFull", "spoolRecovered", "spoolRemoved"];
const EVENT_KEYS: &[&str] = &["type", "t", "perS", "pct", "freePct", "critical", "calls", "bad", "byMs", "remade", "downS"];

fn event(e: &Value, labels: &[String]) -> Option<Value> {
    let kind = e["type"].as_str()?;
    if !EVENT_KINDS.contains(&kind) {
        return None;
    }
    let mut v = keep(e, EVENT_KEYS);
    if let Some(i) = e["source"].as_str().and_then(|s| labels.iter().position(|l| l == s)) {
        v["source"] = json!(i);
    }
    Some(v)
}

/// What the config is, by counts and kinds: no names, frequencies or paths.
fn config_shape(cfg: &Config, names: &NameLists) -> Value {
    let cfg_json = serde_json::to_value(cfg).unwrap_or_default();
    let systems: Vec<Value> = cfg
        .systems
        .iter()
        .map(|s| {
            json!({
                "type": serde_json::to_value(s.kind).unwrap_or_default(),
                "enabled": s.enabled,
                "sites": s.sites.len(),
                "controlChannels": s.sites.iter().map(|x| x.control_channels_hz.len()).sum::<usize>(),
                "modulation": s.sites.first().map(|x| serde_json::to_value(x.modulation).unwrap_or_default()),
                "talkgroups": names.get(&s.short_name).map_or(0, |n| n.talkgroups.len()),
                "plugins": s.plugins.len(),
            })
        })
        .collect();
    let conventional: Vec<Value> = cfg.conventional.iter().map(|c| json!({ "enabled": c.enabled, "channels": c.channels.len() })).collect();
    json!({
        "systems": systems,
        "conventional": conventional,
        "plugins": cfg.plugins.len(),
        "storage": cfg_json["storage"]["backend"],
        "recording": keep(&cfg_json["recording"], RECORDING_KEYS),
    })
}

fn host() -> Value {
    let sys = System::new_with_specifics(sysinfo::RefreshKind::nothing().with_cpu(sysinfo::CpuRefreshKind::nothing()).with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()));
    json!({
        "os": System::long_os_version().or_else(System::name).unwrap_or_else(|| std::env::consts::OS.to_string()),
        "arch": System::cpu_arch(),
        "cpu": sys.cpus().first().map(|c| c.brand().to_string()),
        "cores": sys.cpus().len(),
        "physicalCores": System::physical_core_count(),
        "memoryBytes": sys.total_memory(),
        "loadAverage": System::load_average().one,
        "uptimeS": System::uptime(),
        "container": crate::platform::container(),
    })
}

/// The disk `path` is on, among those known.
fn disk_of<'a>(disks: &'a Disks, path: &Path) -> Option<&'a sysinfo::Disk> {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    disks.list().iter().filter(|d| p.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())
}

/// How long a device has been busy, ms, from Linux's `/proc/diskstats`.
fn busy_ms(diskstats: &str, device: &str) -> Option<u64> {
    diskstats.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).find(|f| f.get(2) == Some(&device))?.get(12)?.parse().ok()
}

fn device_busy_ms(device: &str) -> Option<u64> {
    busy_ms(&std::fs::read_to_string("/proc/diskstats").ok()?, device)
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Watch a capture until `stop`: second by second, each radio's lost samples beside how
/// full its queue got and its slowest write, and what the disk it's going to was doing.
pub fn watch(activity: &[Arc<Activity>], capture_dir: &Path, recordings: &Path, stop: &Arc<AtomicBool>) -> Value {
    let mut disks = Disks::new_with_refreshed_list();
    let (device, disk) = match disk_of(&disks, capture_dir) {
        // (Which kind of disk it is, and whether it's the recordings': no names or paths.)
        Some(d) => (Some(d.name().to_string_lossy().trim_start_matches("/dev/").to_string()), json!({ "kind": d.kind().to_string(), "filesystem": d.file_system().to_string_lossy(), "same_as_recordings": disk_of(&disks, recordings).is_some_and(|r| r.mount_point() == d.mount_point()) })),
        None => (None, Value::Null),
    };
    let started = Instant::now();
    let mut busy_at = device.as_deref().and_then(device_busy_ms);
    let mut timeline = Vec::new();
    let (mut total_lost, mut peak, mut slowest) = (vec![0u64; activity.len()], vec![0usize; activity.len()], vec![0f64; activity.len()]);
    loop {
        let tick = Instant::now();
        while tick.elapsed() < Duration::from_secs(1) && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let took = tick.elapsed().as_secs_f64();
        let now: Vec<_> = activity.iter().map(|a| a.take()).collect();
        for (i, n) in now.iter().enumerate() {
            total_lost[i] += n.0;
            peak[i] = peak[i].max(n.1);
            slowest[i] = slowest[i].max(n.2);
        }
        disks.refresh(true);
        let busy = device.as_deref().and_then(device_busy_ms);
        let busy_pct = busy.zip(busy_at).map(|(b, a)| round(b.saturating_sub(a) as f64 / (took * 10.0)));
        busy_at = busy;
        timeline.push(json!({
            "t": round(started.elapsed().as_secs_f64()),
            "lost_samples": now.iter().map(|n| n.0).collect::<Vec<_>>(),
            "queue_peak": now.iter().map(|n| n.1).collect::<Vec<_>>(),
            "slowest_write_ms": now.iter().map(|n| round(n.2)).collect::<Vec<_>>(),
            "written_mb": round(now.iter().map(|n| n.3).sum::<u64>() as f64 / 1e6),
            "disk_written_mb": disk_of(&disks, capture_dir).map(|d| round(d.usage().written_bytes as f64 / 1e6)),
            "disk_busy_pct": busy_pct,
        }));
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    json!({
        "lost_samples": total_lost,
        "queue_peak": peak,
        "queue_capacity": QUEUE,
        "slowest_write_ms": slowest.iter().map(|v| round(*v)).collect::<Vec<_>>(),
        "disk": disk,
        "timeline": timeline,
    })
}

/// The radios: how each is set, and how it has done this run. `labels` are the config's names for them.
fn radios(cfg: &Config, live: &Live) -> Vec<Value> {
    cfg.sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut v = keep(&serde_json::to_value(s).unwrap_or_default(), RADIO_KEYS);
            v["index"] = json!(i);
            if let Some(c) = live.sources.get(i) {
                v["samples"] = json!(c.samples);
                v["dropped"] = json!(c.dropped);
                v["errors"] = json!(c.errors);
                v["measuredRateHz"] = json!(c.rate_measured);
            }
            if let Some(r) = live.status.sources.get(i) {
                v["frequencyErrorPpm"] = json!(r.error_ppm);
                v["appliedPpm"] = json!(r.applied_ppm);
                v["voiceChannels"] = json!(r.voice_channels);
            }
            v
        })
        .collect()
}

/// `diagnostics.json`, from what's known now.
pub fn build(cfg: &Config, names: &NameLists, live: &Live, recent: &[Value]) -> Value {
    let labels: Vec<String> = cfg.sources.iter().map(|s| s.label()).collect();
    json!({
        "version": 1,
        "app": { "name": "Trunk Recorder Pro", "version": env!("CARGO_PKG_VERSION"), "target": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH) },
        "host": host(),
        "radios": radios(cfg, live),
        "config": config_shape(cfg, names),
        "events": recent.iter().filter_map(|e| event(e, &labels)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_app::config::SystemNames;

    #[test]
    fn nothing_private_reaches_the_diagnostics() {
        let cfg = Config::from_value(json!({
            "sources": [{ "type": "usrp", "args": "addr=SECRET-ADDRESS,serial=SECRET-SERIAL", "gainDb": 30, "centerHz": 858000000, "rateHz": 8000000, "name": "SECRET-RADIO-NAME" }],
            "recording": { "captureDir": "/SECRET-PATH" },
            "keys": { "a": { "file": "/SECRET-KEY-FILE" } },
            "systems": [{
                "shortName": "SECRET-SHORT", "name": "SECRET-TITLE", "type": "p25", "keys": "a",
                "plugins": { "openmhz": { "settings": { "apiKey": "SECRET-API-KEY" } } },
                "sites": [{ "name": "SECRET-SITE", "controlChannelsHz": [857987500.0] }]
            }],
            "conventional": [{ "shortName": "SECRET-CONV", "channelFile": "/SECRET-CHANNELS.csv", "channels": [{ "freqHz": 155000000.0, "name": "SECRET-CHANNEL" }] }],
            "plugins": { "secret-plugin": { "settings": { "apiKey": "SECRET-GLOBAL-KEY" } } },
            "storage": { "backend": "postgres", "url": "postgres://SECRET-DB" }
        }))
        .unwrap();
        let mut names = NameLists::new();
        names.insert("SECRET-SHORT".into(), SystemNames { talkgroups: vec![trunk_core::trunk::TalkgroupRow { tg: trunk_core::trunk::Talkgroup { number: 7, alpha_tag: "SECRET-TG".into(), ..Default::default() }, ..Default::default() }], ..Default::default() });
        let label = cfg.sources[0].label();
        let recent = [
            json!({ "type": "sourceDrops", "t": 1.0, "source": label, "perS": 12.5 }),
            json!({ "type": "diskLow", "t": 2.0, "path": "/SECRET-DISK", "freePct": 3.0 }),
            json!({ "type": "linkDown", "t": 3.0, "target": "SECRET.example.com" }),
            json!({ "type": "clockOff", "t": 4.0, "why": "SECRET-WHY", "bad": true }),
            json!({ "type": "tgFirstSeen", "t": 5.0, "alphaTag": "SECRET-FIRST" }),
        ];
        let live = Live { status: Default::default(), bandplans: vec![], sources: vec![Default::default()] };
        let v = build(&cfg, &names, &live, &recent);
        let text = v.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(v["radios"][0]["type"], "usrp");
        assert_eq!(v["radios"][0]["gainDb"], 30.0, "how the radio was set is kept");
        assert_eq!(v["config"]["systems"][0]["talkgroups"], 1, "how many, not which");
        assert_eq!(v["config"]["storage"], "postgres", "which backend is no secret");
        let events = v["events"].as_array().unwrap();
        assert_eq!(events.len(), 4, "the talkgroup event isn't one of the kinds kept");
        assert_eq!(events[0]["source"], 0, "a radio by its number");
        assert_eq!(events[0]["perS"], 12.5);
    }

    #[test]
    fn a_device_busy_time_comes_from_diskstats() {
        let stats = "   8       0 sda 100 0 800 50 200 0 1600 90 0 12345 140\n   8       1 sda1 10 0 80 5 20 0 160 9 0 777 14\n";
        assert_eq!((busy_ms(stats, "sda"), busy_ms(stats, "sda1"), busy_ms(stats, "sdb")), (Some(12345), Some(777), None));
    }

    #[test]
    fn watching_a_capture_says_what_happened_and_no_paths() {
        let dir = std::env::temp_dir();
        let a: Arc<Activity> = Arc::default();
        let stop = Arc::new(AtomicBool::new(true));
        let v = watch(&[a], &dir, &dir, &stop);
        assert_eq!(v["queue_capacity"], QUEUE);
        assert_eq!(v["lost_samples"], json!([0]));
        assert_eq!(v["timeline"].as_array().unwrap().len(), 1);
        assert!(!v.to_string().contains(dir.to_str().unwrap().trim_end_matches('/')), "{v}");
    }

    #[test]
    fn it_says_what_machine_this_is() {
        let v = host();
        assert!(v["cores"].as_u64().unwrap() > 0 && v["memoryBytes"].as_u64().unwrap() > 0);
        assert!(v.get("host").is_none() && v.get("hostname").is_none());
    }
}
