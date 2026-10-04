//! Notable things, as they happen: the hook alerting will hang on. A
//! [`Monitor`] hands every [`MonitorEvent`] to its [`Watcher`]s (none yet —
//! a rules engine will be one) and keeps the recent ones for the dashboard's
//! event feed. Edge-triggered events (a control channel lost, a talkgroup
//! never heard before) are always made; per-call ones (a talkgroup active, a
//! radio keying up) only when a watcher wants them, as they are frequent.

use serde::Serialize;
use serde_json::{json, Value};

use super::ring::Ring;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MonitorEvent {
    /// A talkgroup heard for the first time.
    #[serde(rename_all = "camelCase")]
    TgFirstSeen { system: String, talkgroup: u32, alpha_tag: String },
    /// A radio heard for the first time (only once the registry has a baseline).
    #[serde(rename_all = "camelCase")]
    UnitFirstSeen { system: String, unit: u32, alias: String },
    /// A call began on a talkgroup (per call: watchers only).
    #[serde(rename_all = "camelCase")]
    TgActive { system: String, talkgroup: u32, units: Vec<u32> },
    /// A radio transmitted (per call: watchers only).
    #[serde(rename_all = "camelCase")]
    UnitActive { system: String, unit: u32, talkgroup: u32 },
    /// A radio affiliated with a talkgroup (watchers only).
    #[serde(rename_all = "camelCase")]
    UnitAffiliated { system: String, unit: u32, talkgroup: u32 },
    /// Calls in the last minute (once a minute: watchers only).
    #[serde(rename_all = "camelCase")]
    CallVolume { system: String, per_min: u32 },
    #[serde(rename_all = "camelCase")]
    ControlLost { system: String, freq_hz: Option<u64> },
    #[serde(rename_all = "camelCase")]
    ControlRegained { system: String, freq_hz: Option<u64> },
    /// A source dropped samples after a quiet spell.
    #[serde(rename_all = "camelCase")]
    SourceDrops { source: String, per_s: f64 },
    /// A source's samples hit the rails (gain too high?).
    #[serde(rename_all = "camelCase")]
    SourceClipping { source: String, pct: f64 },
    #[serde(rename_all = "camelCase")]
    SourceRecovered { source: String },
    // ── the desktop host's own ──
    #[serde(rename_all = "camelCase")]
    PluginHealth { plugin: String, state: String, message: String },
    #[serde(rename_all = "camelCase")]
    LinkDown { target: String },
    #[serde(rename_all = "camelCase")]
    LinkUp { target: String, down_s: f64 },
    #[serde(rename_all = "camelCase")]
    DiskLow { path: String, free_pct: f64 },
    /// The RAM spool is filling (uploads not keeping up); `critical`: nearly full.
    #[serde(rename_all = "camelCase")]
    SpoolLow { free_pct: f64, critical: bool },
    /// It had no room: calls went to the recordings folder.
    #[serde(rename_all = "camelCase")]
    SpoolFull { calls: u64 },
    #[serde(rename_all = "camelCase")]
    SpoolRecovered { free_pct: f64 },
}

impl MonitorEvent {
    /// Made on every call or more often: built only for watchers.
    pub fn frequent(&self) -> bool {
        matches!(self, MonitorEvent::TgActive { .. } | MonitorEvent::UnitActive { .. } | MonitorEvent::UnitAffiliated { .. } | MonitorEvent::CallVolume { .. })
    }

    /// "info" | "warn" | "bad" | "ok": how the feed colours it.
    pub fn level(&self) -> &'static str {
        match self {
            MonitorEvent::ControlLost { .. } | MonitorEvent::LinkDown { .. } | MonitorEvent::DiskLow { .. } | MonitorEvent::SpoolFull { .. } => "bad",
            MonitorEvent::SpoolLow { critical, .. } => if *critical { "bad" } else { "warn" },
            MonitorEvent::SourceDrops { .. } | MonitorEvent::SourceClipping { .. } => "warn",
            MonitorEvent::PluginHealth { state, .. } => match state.as_str() {
                "error" => "bad",
                "warning" => "warn",
                _ => "ok",
            },
            MonitorEvent::ControlRegained { .. } | MonitorEvent::LinkUp { .. } | MonitorEvent::SourceRecovered { .. } | MonitorEvent::SpoolRecovered { .. } => "ok",
            _ => "info",
        }
    }

    /// The `monitorEvent` message: `{type, t, level, kind, …}`.
    pub fn to_json(&self, t: f64) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(json!({}));
        v["type"] = json!("monitorEvent");
        v["t"] = json!((t * 10.0).round() / 10.0);
        v["level"] = json!(self.level());
        v
    }
}

/// Something that acts on events (an alert rule; nothing yet).
pub trait Watcher: Send {
    /// `t`: Unix seconds.
    fn event(&mut self, t: f64, e: &MonitorEvent);
}

/// Recent events kept for a dashboard that connects later.
pub const RECENT: usize = 200;

pub struct Monitor {
    watchers: Vec<Box<dyn Watcher>>,
    recent: Ring<Value>,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor { watchers: Vec::new(), recent: Ring::new(RECENT) }
    }
}

impl Monitor {
    pub fn add_watcher(&mut self, w: Box<dyn Watcher>) {
        self.watchers.push(w);
    }

    /// Someone acts on per-call events (build them).
    pub fn wants_frequent(&self) -> bool {
        !self.watchers.is_empty()
    }

    /// Hand `e` to the watchers; returns its message when it is one for
    /// the feed (frequent ones aren't).
    pub fn emit(&mut self, t: f64, e: MonitorEvent) -> Option<Value> {
        for w in self.watchers.iter_mut() {
            w.event(t, &e);
        }
        if e.frequent() {
            return None;
        }
        let v = e.to_json(t);
        self.recent.push(v.clone());
        Some(v)
    }

    /// Newest last.
    pub fn recent(&self) -> Vec<Value> {
        self.recent.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Count(Arc<Mutex<Vec<String>>>);
    impl Watcher for Count {
        fn event(&mut self, _: f64, e: &MonitorEvent) {
            self.0.lock().unwrap().push(format!("{e:?}"));
        }
    }

    #[test]
    fn watchers_see_everything_the_feed_only_edges() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut m = Monitor::default();
        assert!(!m.wants_frequent());
        m.add_watcher(Box::new(Count(seen.clone())));
        assert!(m.wants_frequent());
        let v = m.emit(100.0, MonitorEvent::ControlLost { system: "dcfd".into(), freq_hz: Some(851_000_000) }).unwrap();
        assert_eq!(v["kind"], "controlLost");
        assert_eq!(v["level"], "bad");
        assert_eq!(v["freqHz"], 851_000_000);
        assert!(m.emit(101.0, MonitorEvent::TgActive { system: "dcfd".into(), talkgroup: 1, units: vec![] }).is_none());
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(m.recent().len(), 1);
    }
}
