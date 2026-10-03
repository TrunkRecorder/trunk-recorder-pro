//! The minute history: every series' [`Rollup`]s on a grid of minutes, kept
//! in memory for a week (about 120 kB a series), so a chart's query never
//! reads a file. The desktop app fills it from the stats files at startup and
//! from the live rollups; the browser build from this session's only.

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
    /// points: `{key: {t0, stepS, v: [avg|null…], lo: [min…], hi: [max…], n: [minutes…]}}`
    /// (`n`: the minutes each average is of — a rate × n × 60 is the amount).
    pub fn query(&self, patterns: &[String], from: i64, to: i64, points: usize) -> Map<String, Value> {
        let points = if points == 0 { DEFAULT_POINTS } else { points.min(2000) };
        let (m0, m1) = (from.div_euclid(60), to.div_euclid(60).max(from.div_euclid(60)));
        let span = (m1 - m0 + 1) as usize;
        let step = span.div_ceil(points).max(1) as i64;
        let m0 = m0.div_euclid(step) * step;
        let n = ((m1 - m0) / step + 1) as usize;
        let mut out = Map::new();
        for (k, g) in &self.series {
            if !patterns.iter().any(|p| matches(p, k)) {
                continue;
            }
            let (mut v, mut lo, mut hi, mut mins) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
            let mut any = false;
            for b in 0..n as i64 {
                let (a, z) = (m0 + b * step, m0 + (b + 1) * step);
                let (mut sum, mut cnt, mut mn, mut mx) = (0.0f64, 0u32, f32::INFINITY, f32::NEG_INFINITY);
                for m in a.max(g.first)..z.min(g.first + g.vals.len() as i64) {
                    let p = g.vals[(m - g.first) as usize];
                    if p[0].is_finite() {
                        sum += p[0] as f64;
                        cnt += 1;
                        mn = mn.min(p[1]);
                        mx = mx.max(p[2]);
                    }
                }
                mins.push(cnt);
                if cnt > 0 {
                    any = true;
                    v.push(json!(round((sum / cnt as f64) as f32)));
                    lo.push(json!(round(mn)));
                    hi.push(json!(round(mx)));
                } else {
                    v.push(Value::Null);
                    lo.push(Value::Null);
                    hi.push(Value::Null);
                }
            }
            if any {
                out.insert(k.clone(), json!({ "t0": m0 * 60, "stepS": step * 60, "v": v, "lo": lo, "hi": hi, "n": mins }));
            }
        }
        out
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
