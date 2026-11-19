//! The aggregator: what [`Instrumented`](trunk_core::metrics::Instrumented)
//! parts report each tick becomes a value per series — a gauge as it is, a
//! running total as its rate per second — and, each wall-clock minute, a
//! [`Rollup`] (average, lowest, highest of those values over the minute) for
//! the history — and each series' running [`Total`] since it began, for
//! plugins ([`Aggregator::totals`]). It knows nothing about which series
//! exist or what they mean.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use trunk_core::metrics::Sink;
use trunk_recorder_plugin::Total;

/// A tick this long after the last one isn't made a rate of (the session
/// stalled, the machine slept): the totals are only remembered.
const MAX_TICK_S: f64 = 5.0;

#[derive(Clone, Copy, Debug)]
struct Acc {
    sum: f64,
    n: u32,
    min: f32,
    max: f32,
}

impl Default for Acc {
    fn default() -> Self {
        Acc { sum: 0.0, n: 0, min: f32::INFINITY, max: f32::NEG_INFINITY }
    }
}

impl Acc {
    fn add(&mut self, v: f32) {
        self.sum += v as f64;
        self.n += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }
}

#[derive(Default)]
struct Series {
    last_total: Option<u64>,
    now: Option<f32>,
    minute: Acc,
}

/// One minute of every series: `[average, lowest, highest]` of its values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rollup {
    /// The minute's start, Unix s.
    pub t: i64,
    pub values: BTreeMap<String, [f32; 3]>,
}

impl Rollup {
    /// One line of the stats files: `{"t":…,"s":{"key":[avg,min,max],…}}`.
    pub fn to_json(&self) -> Value {
        let s: Map<String, Value> = self.values.iter().map(|(k, v)| (k.clone(), json!([round(v[0]), round(v[1]), round(v[2])]))).collect();
        json!({ "t": self.t, "s": s })
    }

    pub fn from_json(v: &Value) -> Option<Rollup> {
        let t = v["t"].as_i64()?;
        let values = v["s"]
            .as_object()?
            .iter()
            .filter_map(|(k, a)| {
                let a = a.as_array()?;
                let f = |i: usize| a.get(i).and_then(Value::as_f64).map(|x| x as f32);
                Some((k.clone(), [f(0)?, f(1)?, f(2)?]))
            })
            .collect();
        Some(Rollup { t, values })
    }
}

/// Four significant figures: plenty for a chart, a third of the bytes.
pub fn round(v: f32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return 0.0;
    }
    let mag = 10f64.powi(3 - (v.abs() as f64).log10().floor() as i32);
    ((v as f64) * mag).round() / mag
}

#[derive(Default)]
pub struct Aggregator {
    series: BTreeMap<String, Series>,
    /// Since the first tick (kept when a series stops reporting).
    totals: BTreeMap<String, Total>,
    t: Option<f64>,
    dt: f64,
    minute: Option<i64>,
}

impl Aggregator {
    /// Start a tick at Unix time `t` (s); reports follow. Returns the last
    /// minute's rollup when this tick is in a new minute.
    pub fn begin(&mut self, t: f64) -> Option<Rollup> {
        self.dt = self.t.map_or(0.0, |t0| t - t0);
        self.t = Some(t);
        let m = (t / 60.0).floor() as i64;
        let r = match self.minute.replace(m) {
            Some(prev) if prev != m => Some(self.rollup(prev)),
            _ => None,
        };
        for s in self.series.values_mut() {
            s.now = None;
        }
        r
    }

    /// The minute so far as a rollup (and start the next).
    fn rollup(&mut self, minute: i64) -> Rollup {
        let mut values = BTreeMap::new();
        for (k, s) in self.series.iter_mut() {
            let a = std::mem::take(&mut s.minute);
            if a.n > 0 {
                values.insert(k.clone(), [(a.sum / a.n as f64) as f32, a.min, a.max]);
            }
        }
        // Series that stopped reporting (a system taken out) are let go.
        self.series.retain(|_, s| s.now.is_some() || s.last_total.is_some());
        Rollup { t: minute * 60, values }
    }

    /// Each series' value this tick (gauges as they are, totals as rates per second).
    pub fn values(&self) -> impl Iterator<Item = (&str, f32)> {
        self.series.iter().filter_map(|(k, s)| s.now.map(|v| (k.as_str(), v)))
    }

    pub fn get(&self, key: &str) -> Option<f32> {
        self.series.get(key).and_then(|s| s.now)
    }

    /// Every series' running total: a counter's count (across restarts of the part
    /// counting), a gauge's value times the seconds it held it; with the latest tick's value.
    pub fn totals(&self) -> BTreeMap<String, Total> {
        self.totals.iter().map(|(k, t)| (k.clone(), Total { now: self.series.get(k).and_then(|s| s.now).map(|v| v as f64), ..*t })).collect()
    }

    /// This tick's values as a JSON object, rounded.
    pub fn values_json(&self) -> Map<String, Value> {
        self.values().map(|(k, v)| (k.to_string(), json!(round(v)))).collect()
    }

    fn set(&mut self, name: &str, v: f32) {
        let s = self.series.entry(name.to_string()).or_default();
        s.now = Some(v);
        s.minute.add(v);
    }
}

impl Sink for Aggregator {
    fn counter(&mut self, name: &str, total: u64) {
        let dt = self.dt;
        // A tick (not the first) that measures time: not after a stall.
        let timed = dt > 0.0 && dt <= MAX_TICK_S;
        let s = self.series.entry(name.to_string()).or_default();
        // A series that appears after the first tick counts from 0 (what it counted came since the last).
        let last = s.last_total.replace(total).or((self.t.is_some() && dt > 0.0).then_some(0));
        // Fewer than last time: the part was replaced and counts from 0 again.
        let d = last.map_or(total, |l| if total >= l { total - l } else { total });
        let t = self.totals.entry(name.to_string()).or_default();
        t.sum += d as f64;
        if timed && d > 0 {
            t.s += dt;
        }
        if timed && last.is_some() {
            self.set(name, (d as f64 / dt) as f32);
        }
    }

    fn gauge(&mut self, name: &str, value: f64) {
        if value.is_finite() {
            if self.dt > 0.0 && self.dt <= MAX_TICK_S {
                let t = self.totals.entry(name.to_string()).or_default();
                t.sum += value * self.dt;
                t.s += self.dt;
            }
            self.set(name, value as f32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_become_rates_and_minutes_roll_up() {
        let mut a = Aggregator::default();
        assert!(a.begin(60.0).is_none());
        a.counter("good", 100);
        a.gauge("noise", -80.0);
        assert_eq!(a.get("good"), None, "no rate from the first total");
        assert_eq!(a.get("noise"), Some(-80.0));
        a.begin(61.0);
        a.counter("good", 140);
        a.gauge("noise", -70.0);
        assert_eq!(a.get("good"), Some(40.0));
        a.begin(62.0);
        a.counter("good", 5); // retuned: counts from 0
        assert_eq!(a.get("good"), Some(5.0));
        assert_eq!(a.get("noise"), None, "not reported this tick");
        let r = a.begin(120.0).expect("a new minute");
        assert_eq!(r.t, 60);
        assert_eq!(r.values["good"], [22.5, 5.0, 40.0]);
        assert_eq!(r.values["noise"], [-75.0, -80.0, -70.0]);
        let back = Rollup::from_json(&r.to_json()).unwrap();
        assert_eq!(back, r);
    }

    /// Since the first tick: a counter's every count, across a restart of the part counting
    /// and a series that appears late; a gauge's value over the seconds it held it.
    #[test]
    fn totals_keep_every_count_and_each_gauges_seconds() {
        let mut a = Aggregator::default();
        a.begin(60.0);
        a.counter("good", 100);
        a.gauge("locked", 1.0);
        a.begin(61.0);
        a.counter("good", 140);
        a.gauge("locked", 1.0);
        a.begin(62.0);
        a.counter("good", 5);
        a.counter("why/noRecorder", 2);
        a.gauge("locked", 0.0);
        assert_eq!(a.get("why/noRecorder"), Some(2.0), "new: counted from 0");
        a.begin(92.0); // a stall: counted, but not as time
        a.counter("good", 7);
        a.gauge("locked", 1.0);
        let t = a.totals();
        assert_eq!((t["good"].sum, t["good"].s), (147.0, 2.0));
        assert_eq!(t["why/noRecorder"].sum, 2.0);
        assert_eq!((t["locked"].sum, t["locked"].s, t["locked"].now), (1.0, 2.0, Some(1.0)));
    }

    #[test]
    fn rounding_keeps_four_figures() {
        assert_eq!(round(123.456_7), 123.5);
        assert_eq!(round(-0.012_345), -0.01235);
        assert_eq!(round(f32::NAN), 0.0);
    }
}
