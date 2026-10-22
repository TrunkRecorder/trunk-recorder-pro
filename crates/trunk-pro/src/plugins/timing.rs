//! Delivery timing (#61): how long each finished call took to reach a
//! plugin's service, and where the time went. The same for every plugin that
//! takes calls (Broadcastify Calls, OpenMHz, Rdio Scanner, a script): the
//! [`super::Archive`], which follows each finished call to every plugin it
//! goes to, sees it end, the hold give it late and the plugin's result, so no
//! plugin measures anything but its own work.
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

use trunk_core::metrics::{key_part, Sink};

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

impl Totals {
    /// A call delivered `age_ms` after its end, `held_ms` of it held back,
    /// and `took_ms` of it the plugin's own work (when it said).
    pub fn delivered(&mut self, age_ms: u64, held_ms: u64, took_ms: Option<u64>) {
        self.delivered += 1;
        self.age_ms += age_ms;
        self.held_ms += held_ms;
        self.tick_age = Some(self.tick_age.map_or(age_ms, |a| a.max(age_ms)));
        if let Some(u) = took_ms {
            self.uploads += 1;
            self.upload_ms += u;
            self.wait_ms += age_ms.saturating_sub(held_ms + u);
            self.tick_upload = Some(self.tick_upload.map_or(u, |a| a.max(u)));
        }
    }

    /// Plugin `id`'s, into `sink` as `plg/<id>/…`: the totals (`delivered`,
    /// `ageMs`, `heldMs`, `uploads`, `uploadMs`, `waitMs`) and the tick's
    /// longest (`age`, `upload`, ms; only in a tick with deliveries).
    pub fn report(&mut self, id: &str, sink: &mut dyn Sink) {
        let k = format!("plg/{}", key_part(id));
        sink.counter(&format!("{k}/delivered"), self.delivered);
        sink.counter(&format!("{k}/ageMs"), self.age_ms);
        sink.counter(&format!("{k}/heldMs"), self.held_ms);
        sink.counter(&format!("{k}/uploads"), self.uploads);
        sink.counter(&format!("{k}/uploadMs"), self.upload_ms);
        sink.counter(&format!("{k}/waitMs"), self.wait_ms);
        if let Some(a) = self.tick_age.take() {
            sink.gauge(&format!("{k}/age"), a as f64);
        }
        if let Some(u) = self.tick_upload.take() {
            sink.gauge(&format!("{k}/upload"), u as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
        let mut t = Totals::default();
        // Straight away, 2.5 s after the end, 400 ms of it uploading.
        t.delivered(2500, 0, Some(400));
        // Held 60 s by a delay rule, delivered 61.2 s after the end, 300 ms uploading.
        t.delivered(61_200, 60_000, Some(300));
        // A plugin that doesn't say its time on it: the age alone.
        t.delivered(5000, 0, None);
        assert_eq!((t.delivered, t.age_ms, t.held_ms, t.uploads, t.upload_ms, t.wait_ms), (3, 68_700, 60_000, 2, 700, 3000));
    }

    #[test]
    fn reported_as_totals_and_each_ticks_longest() {
        let mut t = Totals::default();
        t.delivered(2000, 0, Some(300));
        t.delivered(5000, 0, Some(900));
        let mut g = Got::default();
        t.report("broadcastify", &mut g);
        assert_eq!(g.0["plg/broadcastify/delivered"], 2.0);
        assert_eq!(g.0["plg/broadcastify/ageMs"], 7000.0);
        assert_eq!(g.0["plg/broadcastify/uploadMs"], 1200.0);
        assert_eq!(g.0["plg/broadcastify/age"], 5000.0);
        assert_eq!(g.0["plg/broadcastify/upload"], 900.0);
        let mut next = Got::default();
        t.report("broadcastify", &mut next);
        assert!(!next.0.contains_key("plg/broadcastify/age"), "no deliveries this tick: no longest");
        assert_eq!(next.0["plg/broadcastify/delivered"], 2.0, "the totals stay");
    }
}
