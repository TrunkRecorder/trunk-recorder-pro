//! Delivery timing (#61): how long each finished call took to reach a
//! plugin's service, and where the time went. The same for every plugin that
//! takes calls (Broadcastify Calls, OpenMHz, Rdio Scanner, a script): the
//! recorder sees each call end, the hold give it late, and the plugin's
//! result, so no plugin measures anything but its own work.
//!
//! | Measure | What it is |
//! |---|---|
//! | Age at delivery | the call's end (its stop time) to the plugin's Ok |
//! | Held | its delay rules' wait ([`super::hold`]) |
//! | Upload | the plugin's own time on it, its last try (`took_ms`; plugins on SDK 0.6 and later) |
//! | Waiting | the rest: its files being finished, the plugin's queue, failed tries |
//!
//! All by this computer's clock, which stamps a call's end too: it's local,
//! not anything a service reports. Only calls a plugin delivered (Ok) are
//! timed; failures are counted where they always were.
//!
//! Reported per plugin (its copies together) as running totals, which the
//! dashboard makes averages of over any span (ms over calls), and as the
//! longest of each tick, whose history keeps its highest.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trunk_core::metrics::{key_part, Sink};
use trunk_recorder_plugin::Outcome;

/// A call's end is forgotten after this (a plugin never answered, or the
/// recorder restarted between): longer than any sensible delay rule.
const FORGET: Duration = Duration::from_secs(26 * 3600);
/// And at most this many are remembered.
const MAX_CALLS: usize = 100_000;

/// One plugin's totals since the recorder started.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    /// Calls delivered and timed, and the sums of their age and time held, ms.
    pub delivered: u64,
    pub age_ms: u64,
    pub held_ms: u64,
    /// Of those, the ones the plugin said its time on, and the sums of that and the waiting, ms.
    pub uploads: u64,
    pub upload_ms: u64,
    pub wait_ms: u64,
    /// The longest age and upload since the last report.
    tick_age: Option<u64>,
    tick_upload: Option<u64>,
}

#[derive(Default)]
struct Inner {
    /// Finished calls handed to the plugins, by path: when each ended (Unix
    /// ms), how many plugins are yet to report on it, and when it came.
    ended: HashMap<String, (u64, usize, Instant)>,
    /// How long a call was held back from a plugin, ms, by (plugin label, path).
    held: HashMap<(String, String), u64>,
    /// By plugin id.
    totals: BTreeMap<String, Totals>,
}

#[derive(Default)]
pub struct Deliveries {
    inner: Mutex<Inner>,
}

impl Deliveries {
    /// Call `rel`, which ended at `ended_ms` (Unix ms), goes to `takers`
    /// plugins (as the archive counts them), each of which will report on it.
    pub fn ended(&self, rel: &str, ended_ms: u64, takers: usize) {
        if takers == 0 {
            return;
        }
        let mut x = self.inner.lock().unwrap();
        if x.ended.len() >= MAX_CALLS {
            x.ended.retain(|_, (_, _, at)| at.elapsed() < FORGET);
            if x.ended.len() >= MAX_CALLS {
                return;
            }
        }
        x.ended.insert(rel.to_string(), (ended_ms, takers, Instant::now()));
    }

    /// Call `rel` was given to plugin `label` after its delay rules held it `held_ms`.
    pub fn held(&self, label: &str, rel: &str, held_ms: u64) {
        let mut x = self.inner.lock().unwrap();
        if x.ended.contains_key(rel) {
            x.held.insert((label.to_string(), rel.to_string()), held_ms);
        }
    }

    /// Plugin `label` (a copy's: `openmhz#2`) reported on call `rel` at
    /// `now_ms`, its own time on it `took_ms` when it said.
    pub fn result(&self, label: &str, rel: &str, outcome: Outcome, took_ms: Option<u64>, now_ms: u64) {
        let mut x = self.inner.lock().unwrap();
        let held = x.held.remove(&(label.to_string(), rel.to_string())).unwrap_or(0);
        let Some((ended, left, _)) = x.ended.get_mut(rel) else { return };
        let ended = *ended;
        // (Every plugin has said: forgotten.)
        *left = left.saturating_sub(1);
        if *left == 0 {
            x.ended.remove(rel);
        }
        if outcome != Outcome::Ok {
            return;
        }
        let age = now_ms.saturating_sub(ended);
        let t = x.totals.entry(super::parse_label(label).0.to_string()).or_default();
        t.delivered += 1;
        t.age_ms += age;
        t.held_ms += held;
        t.tick_age = Some(t.tick_age.map_or(age, |a| a.max(age)));
        if let Some(u) = took_ms {
            t.uploads += 1;
            t.upload_ms += u;
            t.wait_ms += age.saturating_sub(held + u);
            t.tick_upload = Some(t.tick_upload.map_or(u, |a| a.max(u)));
        }
    }

    /// Into `sink`, per plugin `plg/<id>/…`: the totals (`delivered`,
    /// `ageMs`, `heldMs`, `uploads`, `uploadMs`, `waitMs`) and the tick's
    /// longest (`age`, `upload`, ms; only in a tick with deliveries).
    pub fn report(&self, sink: &mut dyn Sink) {
        let mut x = self.inner.lock().unwrap();
        for (id, t) in x.totals.iter_mut() {
            let k = format!("plg/{}", key_part(id));
            sink.counter(&format!("{k}/delivered"), t.delivered);
            sink.counter(&format!("{k}/ageMs"), t.age_ms);
            sink.counter(&format!("{k}/heldMs"), t.held_ms);
            sink.counter(&format!("{k}/uploads"), t.uploads);
            sink.counter(&format!("{k}/uploadMs"), t.upload_ms);
            sink.counter(&format!("{k}/waitMs"), t.wait_ms);
            if let Some(a) = t.tick_age.take() {
                sink.gauge(&format!("{k}/age"), a as f64);
            }
            if let Some(u) = t.tick_upload.take() {
                sink.gauge(&format!("{k}/upload"), u as f64);
            }
        }
    }

    #[cfg(test)]
    pub fn totals(&self, id: &str) -> Totals {
        self.inner.lock().unwrap().totals.get(id).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Got(BTreeMap<String, f64>);
    impl Sink for Got {
        fn counter(&mut self, n: &str, v: u64) {
            self.0.insert(n.into(), v as f64);
        }
        fn gauge(&mut self, n: &str, v: f64) {
            self.0.insert(n.into(), v);
        }
    }

    #[test]
    fn age_held_upload_and_the_waiting_left() {
        let d = Deliveries::default();
        let end = 1_761_134_400_000;
        d.ended("dcfd/a", end, 2);
        // Broadcastify: straight away, 2.5 s after the end, 400 ms of it uploading.
        d.result("broadcastify", "dcfd/a", Outcome::Ok, Some(400), end + 2500);
        // OpenMHz: held 60 s by a delay rule, delivered 61.2 s after the end, 300 ms uploading.
        d.held("openmhz", "dcfd/a", 60_000);
        d.result("openmhz", "dcfd/a", Outcome::Ok, Some(300), end + 61_200);
        let b = d.totals("broadcastify");
        assert_eq!((b.delivered, b.age_ms, b.held_ms, b.uploads, b.upload_ms, b.wait_ms), (1, 2500, 0, 1, 400, 2100));
        let o = d.totals("openmhz");
        assert_eq!((o.age_ms, o.held_ms, o.upload_ms, o.wait_ms), (61_200, 60_000, 300, 900));
    }

    #[test]
    fn copies_count_as_their_plugin_and_an_older_plugin_gives_the_age_alone() {
        let d = Deliveries::default();
        d.ended("dcfd/a", 1000, 1);
        d.ended("dcfd/b", 2000, 1);
        d.result("openmhz", "dcfd/a", Outcome::Ok, Some(100), 3000);
        d.result("openmhz#2", "dcfd/b", Outcome::Ok, None, 5000);
        let t = d.totals("openmhz");
        assert_eq!((t.delivered, t.age_ms, t.uploads, t.upload_ms, t.wait_ms), (2, 5000, 1, 100, 1900));
    }

    #[test]
    fn only_deliveries_are_timed() {
        let d = Deliveries::default();
        d.ended("dcfd/a", 1000, 2);
        d.held("openmhz", "dcfd/a", 500);
        d.result("openmhz", "dcfd/a", Outcome::Failed, Some(9000), 20_000);
        d.result("broadcastify", "dcfd/a", Outcome::Skipped, Some(5), 2000);
        d.result("rdioscanner", "dcfd/unknown", Outcome::Ok, Some(5), 2000);
        assert_eq!(d.totals("openmhz"), Totals::default());
        assert_eq!(d.totals("broadcastify"), Totals::default());
        assert_eq!(d.totals("rdioscanner"), Totals::default(), "a call it never saw end (a restart between)");
        assert!(d.inner.lock().unwrap().held.is_empty(), "a result forgets its hold");
        assert!(d.inner.lock().unwrap().ended.is_empty(), "both plugins said: the call is forgotten");
    }

    #[test]
    fn reported_as_totals_and_each_ticks_longest() {
        let d = Deliveries::default();
        for (i, (age, up)) in [(2000, 300), (5000, 900)].into_iter().enumerate() {
            let rel = format!("dcfd/{i}");
            d.ended(&rel, 0, 1);
            d.result("broadcastify", &rel, Outcome::Ok, Some(up), age);
        }
        let mut g = Got::default();
        d.report(&mut g);
        assert_eq!(g.0["plg/broadcastify/delivered"], 2.0);
        assert_eq!(g.0["plg/broadcastify/ageMs"], 7000.0);
        assert_eq!(g.0["plg/broadcastify/uploadMs"], 1200.0);
        assert_eq!(g.0["plg/broadcastify/age"], 5000.0);
        assert_eq!(g.0["plg/broadcastify/upload"], 900.0);
        let mut next = Got::default();
        d.report(&mut next);
        assert!(!next.0.contains_key("plg/broadcastify/age"), "no deliveries this tick: no longest");
        assert_eq!(next.0["plg/broadcastify/delivered"], 2.0, "the totals stay");
    }
}
