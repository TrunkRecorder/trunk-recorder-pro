//! The minute history: every series' [`Rollup`]s on a grid of minutes, kept
//! in memory for the recent span (12 bytes a minute a series), so a chart's
//! query of it never reads a file. With no store (the browser build, or
//! `files`) that's a week (about 120 kB a series). The desktop app fills it
//! from its store (or the stats files) at startup and from the live rollups;
//! the browser build from this session's only.
//!
//! With a store, memory keeps only a day and a bit, and the store keeps each
//! series' hours ([`HourRow`], made here from the minutes): a longer chart is
//! the store's hours and memory's minutes added into the same [`Span`]'s buckets.

use std::collections::{HashMap, VecDeque};

use serde_json::{json, Map, Value};

use super::aggregate::{round, Rollup};

/// A week of minutes.
pub const WEEK_MINUTES: usize = 7 * 24 * 60;
/// Points a query returns by default.
const DEFAULT_POINTS: usize = 360;

#[derive(Default)]
struct Grid {
    /// Minute index (Unix s / 60) of `vals[0]`.
    first: i64,
    vals: VecDeque<[f32; 3]>,
}

const GAP: [f32; 3] = [f32::NAN; 3];

/// A series' hour, as a store keeps it past the week of minutes: the
/// average of its minutes' averages, their lowest and highest, and how many
/// minutes were measured.
#[derive(Clone, Debug, PartialEq)]
pub struct HourRow {
    pub series: String,
    /// The hour's start, Unix s.
    pub t: i64,
    pub avg: f32,
    pub lo: f32,
    pub hi: f32,
    pub n: u32,
}

/// One point of a chart: the minutes added into it.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    /// Σ (average × minutes).
    pub sum: f64,
    /// Minutes measured.
    pub n: u32,
    pub lo: f32,
    pub hi: f32,
}

impl Default for Bucket {
    fn default() -> Self {
        Bucket { sum: 0.0, n: 0, lo: f32::INFINITY, hi: f32::NEG_INFINITY }
    }
}

impl Bucket {
    /// `n` minutes averaging `avg` (a minute, an hour, or a store's sum of them: `sum` = avg × n).
    pub fn add(&mut self, sum: f64, n: u32, lo: f32, hi: f32) {
        self.sum += sum;
        self.n += n;
        self.lo = self.lo.min(lo);
        self.hi = self.hi.max(hi);
    }
}

/// Each series' buckets, by name.
pub type Buckets = HashMap<String, Vec<Bucket>>;

/// A query's grid: buckets of `step` minutes from minute `m0`, `n` of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub m0: i64,
    pub step: i64,
    pub n: usize,
}

impl Span {
    /// `from` to `to` (Unix s) in about `points` points (0: the default),
    /// aligned to their size. `unit` 1: any number of minutes each; 60
    /// (a store's hours): whole hours, of a size a chart reads (1, 2, 3, 6,
    /// 12 hours, a day, two, a week), so a long chart's points are days.
    pub fn of(from: i64, to: i64, points: usize, unit: i64) -> Span {
        const HOURS: [usize; 8] = [60, 120, 180, 360, 720, 1440, 2880, 10_080];
        let points = if points == 0 { DEFAULT_POINTS } else { points.min(2000) };
        let (m0, m1) = (from.div_euclid(60), to.div_euclid(60).max(from.div_euclid(60)));
        let span = (m1 - m0 + 1) as usize;
        let need = span.div_ceil(points).max(1);
        let step = if unit <= 1 {
            need
        } else {
            // (The nearest that isn't much finer: 90 days and a minute in 90 points is a day each.)
            HOURS.into_iter().find(|s| *s * 10 >= need * 9).unwrap_or(need.div_ceil(10_080) * 10_080)
        } as i64;
        let m0 = m0.div_euclid(step) * step;
        Span { m0, step, n: ((m1 - m0) / step + 1) as usize }
    }

    /// The bucket minute `m` falls in.
    pub fn bucket(&self, m: i64) -> Option<usize> {
        let b = (m - self.m0).div_euclid(self.step);
        (0..self.n as i64).contains(&b).then_some(b as usize)
    }

    /// The answer: `{key: {t0, stepS, v: [avg|null…], lo: [min…], hi: [max…], n: [minutes…]}}`
    /// for each series with something measured (`n`: the minutes each
    /// average is of — a rate × n × 60 is the amount).
    pub fn answer(&self, buckets: Buckets) -> Map<String, Value> {
        let mut out = Map::new();
        for (k, bs) in buckets {
            if bs.iter().all(|b| b.n == 0) {
                continue;
            }
            let (mut v, mut lo, mut hi, mut mins) = (Vec::with_capacity(self.n), Vec::with_capacity(self.n), Vec::with_capacity(self.n), Vec::with_capacity(self.n));
            for b in &bs {
                mins.push(b.n);
                if b.n > 0 {
                    v.push(json!(round((b.sum / b.n as f64) as f32)));
                    lo.push(json!(round(b.lo)));
                    hi.push(json!(round(b.hi)));
                } else {
                    v.push(Value::Null);
                    lo.push(Value::Null);
                    hi.push(Value::Null);
                }
            }
            out.insert(k, json!({ "t0": self.m0 * 60, "stepS": self.step * 60, "v": v, "lo": lo, "hi": hi, "n": mins }));
        }
        out
    }
}

pub struct History {
    series: HashMap<String, Grid>,
    cap: usize,
    /// Still reading the files: queries say so.
    pub loading: bool,
}

impl Default for History {
    fn default() -> Self {
        History::new(WEEK_MINUTES)
    }
}

impl History {
    pub fn new(cap_minutes: usize) -> Self {
        History { series: HashMap::new(), cap: cap_minutes.max(2), loading: false }
    }

    /// Add a minute (merging with what that minute already has: rollups of
    /// one minute come from several places — the engine, the platform, the plugins).
    pub fn insert(&mut self, r: &Rollup) {
        let m = r.t.div_euclid(60);
        for (k, v) in &r.values {
            let g = self.series.entry(k.clone()).or_default();
            if g.vals.is_empty() {
                g.first = m;
                g.vals.push_back(*v);
                continue;
            }
            if m < g.first {
                // Older than anything kept (the files read in after live minutes came).
                if g.first - m > self.cap as i64 {
                    continue;
                }
                for _ in m + 1..g.first {
                    g.vals.push_front(GAP);
                }
                g.vals.push_front(*v);
                g.first = m;
            } else {
                let i = (m - g.first) as usize;
                if i >= g.vals.len() {
                    let pad = i - g.vals.len();
                    if pad >= self.cap {
                        g.vals.clear();
                        g.first = m;
                    } else {
                        g.vals.extend(std::iter::repeat_n(GAP, pad));
                    }
                    g.vals.push_back(*v);
                } else {
                    g.vals[i] = *v;
                }
            }
            while g.vals.len() > self.cap {
                g.vals.pop_front();
                g.first += 1;
            }
        }
    }

    /// The series names kept.
    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self.series.keys().cloned().collect();
        k.sort();
        k
    }

    /// The series matching `patterns` (a name, or a prefix ending in `*`)
    /// from `from` to `to` (Unix s), each averaged down to about `points`
    /// points ([`Span::answer`]).
    pub fn query(&self, patterns: &[String], from: i64, to: i64, points: usize) -> Map<String, Value> {
        let span = Span::of(from, to, points, 1);
        let mut b = Buckets::new();
        self.fill(patterns, &span, i64::MIN, &mut b);
        span.answer(b)
    }

    /// Add the minutes from minute `since` on of the series matching
    /// `patterns` into `span`'s buckets.
    pub fn fill(&self, patterns: &[String], span: &Span, since: i64, out: &mut Buckets) {
        let (a, z) = (span.m0.max(since), span.m0 + span.step * span.n as i64);
        for (k, g) in &self.series {
            if !patterns.iter().any(|p| matches(p, k)) {
                continue;
            }
            let bs = out.entry(k.clone()).or_insert_with(|| vec![Bucket::default(); span.n]);
            for m in a.max(g.first)..z.min(g.first + g.vals.len() as i64) {
                let p = g.vals[(m - g.first) as usize];
                if let (true, Some(b)) = (p[0].is_finite(), span.bucket(m)) {
                    bs[b].add(p[0] as f64, 1, p[1], p[2]);
                }
            }
        }
    }

    /// Every series' hour starting at `t` (Unix s, on the hour) that has a
    /// minute measured: for a store to keep past the week.
    pub fn hour_rows(&self, t: i64) -> Vec<HourRow> {
        let m0 = t.div_euclid(60);
        let mut out: Vec<HourRow> = self
            .series
            .iter()
            .filter_map(|(k, g)| {
                let mut b = Bucket::default();
                for m in m0.max(g.first)..(m0 + 60).min(g.first + g.vals.len() as i64) {
                    let p = g.vals[(m - g.first) as usize];
                    if p[0].is_finite() {
                        b.add(p[0] as f64, 1, p[1], p[2]);
                    }
                }
                (b.n > 0).then(|| HourRow { series: k.clone(), t: m0 * 60, avg: (b.sum / b.n as f64) as f32, lo: b.lo, hi: b.hi, n: b.n })
            })
            .collect();
        out.sort_by(|a, b| a.series.cmp(&b.series));
        out
    }

    /// How many minutes of each series it keeps.
    pub fn cap_minutes(&self) -> usize {
        self.cap
    }

    /// The first minute kept (Unix s), of any series.
    pub fn first(&self) -> Option<i64> {
        self.series.values().filter(|g| !g.vals.is_empty()).map(|g| g.first * 60).min()
    }
}

/// `pattern` is `key` exactly, or ends in `*` and is a prefix of it.
pub fn matches(pattern: &str, key: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => key.starts_with(prefix),
        None => pattern == key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn r(t: i64, k: &str, v: f32) -> Rollup {
        Rollup { t, values: BTreeMap::from([(k.to_string(), [v, v - 1.0, v + 1.0])]) }
    }

    #[test]
    fn minutes_land_on_their_grid_in_any_order() {
        let mut h = History::new(10);
        h.insert(&r(600, "a", 1.0));
        h.insert(&r(720, "a", 3.0)); // one minute missing
        h.insert(&r(480, "a", 0.5)); // older, read from a file later
        let q = h.query(&["a".into()], 480, 720, 100);
        assert_eq!(q["a"]["t0"], 480);
        assert_eq!(q["a"]["v"], json!([0.5, null, 1.0, null, 3.0]));
        // Down to about two points: buckets of three minutes, aligned to the step (6–8, 9–11, 12–14).
        let q = h.query(&["a*".into()], 480, 720, 2);
        assert_eq!(q["a"]["stepS"], 180);
        assert_eq!(q["a"]["t0"], 360);
        assert_eq!(q["a"]["v"], json!([0.5, 1.0, 3.0]));
        assert_eq!(q["a"]["n"], json!([1, 1, 1]), "minutes measured in each step");
        assert_eq!(q["a"]["lo"], json!([-0.5, 0.0, 2.0]));
        assert!(h.query(&["b".into()], 480, 720, 10).is_empty());
    }

    /// An hour's row is its minutes; a store's hours and memory's minutes
    /// add into one grid of whole hours.
    #[test]
    fn hours_from_minutes_and_one_grid_for_both() {
        let mut h = History::new(WEEK_MINUTES);
        for m in 0..90 {
            h.insert(&r(7200 + m * 60, "a", if m < 60 { 2.0 } else { 4.0 }));
        }
        assert_eq!(h.hour_rows(7200), [HourRow { series: "a".into(), t: 7200, avg: 2.0, lo: 1.0, hi: 3.0, n: 60 }]);
        assert_eq!(h.hour_rows(10_800)[0].n, 30);
        assert!(h.hour_rows(0).is_empty());
        assert_eq!(h.first(), Some(7200));
        // Hours buckets: about three points over a day are a size a chart reads (12 h).
        let span = Span::of(0, 86_399, 3, 60);
        assert_eq!((span.m0, span.step, span.n), (0, 720, 2));
        assert_eq!(Span::of(0, 90 * 86_400, 90, 60).step, 1440, "90 days and a minute: a day each");
        let mut b = Buckets::new();
        b.entry("a".into()).or_insert_with(|| vec![Bucket::default(); span.n])[0].add(5.0 * 60.0, 60, 4.0, 6.0); // an hour from a store
        // (Memory's minutes from 3:00 on: the store had the hours before.)
        h.fill(&["a".into()], &span, 3 * 60, &mut b);
        let q = span.answer(b);
        assert_eq!(q["a"]["n"], json!([90, 0]));
        assert_eq!(q["a"]["v"], json!([4.667, null]), "(5 × 60 + 4 × 30) / 90");
        assert_eq!((q["a"]["lo"][0].clone(), q["a"]["hi"][0].clone()), (json!(3.0), json!(6.0)));
    }

    #[test]
    fn old_minutes_fall_off() {
        let mut h = History::new(3);
        for m in 0..6 {
            h.insert(&r(m * 60, "a", m as f32));
        }
        assert_eq!(h.query(&["a".into()], 180, 300, 10)["a"]["v"], json!([3.0, 4.0, 5.0]));
        assert_eq!(h.query(&["a".into()], 0, 300, 10)["a"]["v"], json!([null, null, null, 3.0, 4.0, 5.0]));
        // A jump of more than the whole span starts afresh.
        h.insert(&r(6000, "a", 9.0));
        assert!(h.query(&["a".into()], 0, 300, 10).is_empty());
        assert_eq!(h.query(&["a".into()], 6000, 6000, 10)["a"]["v"], json!([9.0]));
    }
}
