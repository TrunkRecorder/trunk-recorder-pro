//! An EDACS site, watched whole: every frequency it may use at once (its
//! control channels and its channel list), so the control channel is found
//! wherever it is, and the channel list is checked — and learned — from the
//! air.
//!
//! Each carrier runs the control channel receiver and a carrier detector.
//! The one whose frames keep coming is the control channel; only its
//! messages are followed. When it assigns a call a channel (an LCN), the
//! carrier that comes up just then is that channel: after a few calls agree,
//! the LCN is that frequency, whatever the list said. A system being moved
//! to another (frequencies taken away, channels renumbered) stays followed.
//!
//! ```text
//! carrier IQ ─┬─ 9600 baud 2FSK → framer → (the control channel's) → Parser → Message
//!             └─ power → keyed / not, and when it keyed ─┐
//! Parser: "LCN 4 assigned at t" ─────────────────────────┴─→ votes → LCN 4 = 860.1125 MHz
//! ```

use std::collections::BTreeMap;

use num_complex::Complex32;

use super::framer::Framer;
use super::parser::{EdacsConfig, Parser};
use crate::smartnet::{Bit, Fsk2};
use crate::trunk::Message;

/// A carrier keyed this long after (or before) an assignment is its channel.
const ONSET_BEFORE_S: f64 = 0.3;
const ONSET_AFTER_S: f64 = 1.0;
/// An assignment is judged this long after it was heard (the carriers have caught up).
const JUDGE_AFTER_S: f64 = 1.5;
/// Votes an LCN needs for a frequency, and how far ahead of any other
/// (a call keying several carriers at once gives each a share).
const LEARN_VOTES: f32 = 3.0;
/// Raster power is judged over this long.
const SCAN_WINDOW_S: f64 = 0.05;
/// Carrier power above the floor that keys it / unkeys it.
const KEY_RATIO: f32 = 10.0;
const UNKEY_RATIO: f32 = 4.0;
/// Frames in a row a carrier needs to be taken for the control channel.
const CC_RUN: u32 = 5;
/// The control channel heard nothing for this long: another may take over.
const CC_LOST_S: f64 = 1.5;
/// Same frequency, Hz.
const SAME_HZ: u64 = 1000;
/// A raster frequency whose floor is this far above the raster's is a carrier that's always on.
const CONTINUOUS_RATIO: f32 = 4.0;

struct Carrier {
    hz: u64,
    rx: Fsk2,
    /// The rate its receiver was made for.
    rate: f64,
    framer: Framer,
    bits: Vec<Bit>,
    /// Power: the floor (slowly following the quietest), keyed or not, and when it last keyed.
    floor: f32,
    keyed: bool,
    keyed_at: Option<f64>,
    unkeyed_at: f64,
    /// The last time a frame decoded here.
    good_at: f64,
    /// Its air time now.
    t: f64,
}

/// A raster frequency's power, measured from the source's spectrum (no receiver).
#[derive(Default)]
struct Slot {
    sum: f32,
    n: u32,
    since: f64,
    floor: f32,
    keyed: bool,
    keyed_at: Option<f64>,
    /// Its power when it keyed (of neighbours keying together, the strongest is the one).
    level: f32,
}

pub struct Site {
    pub parser: Parser,
    carriers: Vec<Carrier>,
    /// The channel raster (first, step, Hz) the site's frequencies are on, and each raster frequency's power.
    raster: Option<(f64, f64)>,
    slots: BTreeMap<u64, Slot>,
    scan_t: f64,
    /// The carrier the control channel is on.
    cc: Option<usize>,
    /// Assignments waiting to be judged: (LCN, when).
    pending: Vec<(u8, f64)>,
    /// (LCN, frequency) → its share of the assignments it keyed for.
    votes: BTreeMap<(u8, u64), f32>,
    /// The channel list as configured, and what the air taught.
    configured: BTreeMap<u8, u64>,
    learned: BTreeMap<u8, u64>,
    notes: Vec<(bool, String)>,
}

impl Site {
    /// `carriers`: every frequency the site may use, Hz.
    pub fn new(cfg: EdacsConfig, carriers: &[u64], rate: f64) -> Self {
        let mut hz: Vec<u64> = carriers.to_vec();
        hz.extend(cfg.lcns.values().copied());
        hz.sort_unstable();
        hz.dedup_by(|a, b| a.abs_diff(*b) < SAME_HZ);
        let configured = cfg.lcns.clone();
        // The raster: 25 kHz when every frequency is on one (800 MHz), else 12.5 kHz.
        let raster = hz.first().and_then(|&first| {
            [25_000u64, 12_500].into_iter().find(|&step| hz.iter().all(|&f| (f - first) % step < SAME_HZ || step - (f - first) % step < SAME_HZ)).map(|step| (first as f64, step as f64))
        });
        Site {
            raster,
            slots: BTreeMap::new(),
            scan_t: f64::NEG_INFINITY,
            parser: Parser::new(cfg),
            carriers: hz
                .into_iter()
                .map(|hz| Carrier { hz, rx: super::receiver(rate), rate, framer: Framer::new(), bits: Vec::new(), floor: f32::MAX, keyed: false, keyed_at: None, unkeyed_at: 0.0, good_at: f64::NEG_INFINITY, t: 0.0 })
                .collect(),
            cc: None,
            pending: Vec::new(),
            votes: BTreeMap::new(),
            configured,
            learned: BTreeMap::new(),
            notes: Vec::new(),
        }
    }

    /// The frequencies watched, Hz.
    pub fn carriers(&self) -> Vec<f64> {
        self.carriers.iter().map(|c| c.hz as f64).collect()
    }

    /// The raster to measure (see [`Site::scan`]).
    pub fn scan_raster(&self) -> Option<(f64, f64)> {
        self.raster
    }

    /// Each raster frequency's power in the block that ended at `t`: when
    /// an LCN's frequency isn't among the site's, the carrier that keys for
    /// its calls is found here.
    pub fn scan(&mut self, t: f64, powers: &[(u64, f32)]) {
        self.scan_t = self.scan_t.max(t);
        for &(hz, p) in powers {
            let s = self.slots.entry(hz).or_insert_with(|| Slot { since: t, floor: f32::MAX, ..Default::default() });
            s.sum += p;
            s.n += 1;
            if t - s.since < SCAN_WINDOW_S {
                continue;
            }
            let p = s.sum / s.n as f32;
            let start = s.since;
            (s.sum, s.n, s.since) = (0.0, 0, t);
            s.floor = if p < s.floor { p } else { s.floor * 1.0005 }.max(1e-30);
            if !s.keyed && p > s.floor * KEY_RATIO {
                (s.keyed, s.keyed_at, s.level) = (true, Some(start), p);
            } else if s.keyed && p < s.floor * UNKEY_RATIO {
                s.keyed = false;
            }
        }
    }

    /// The control channel's frequency, once found.
    pub fn control_hz(&self) -> Option<u64> {
        self.cc.map(|i| self.carriers[i].hz)
    }

    pub fn counts(&self) -> (u64, u64) {
        self.cc.map_or((0, 0), |i| (self.carriers[i].framer.good, self.carriers[i].framer.bad))
    }

    pub fn offset_hz(&self) -> Option<f32> {
        self.cc.map(|i| self.carriers[i].rx.offset_hz())
    }

    pub fn take_notes(&mut self) -> Vec<(bool, String)> {
        let mut n = std::mem::take(&mut self.notes);
        n.extend(self.parser.take_notes());
        n
    }

    /// IQ of carrier `i` at `rate`, whose first sample was at air time `t0`
    /// (the receiver counts samples from it).
    pub fn push(&mut self, i: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Message>) {
        let Some(c) = self.carriers.get_mut(i) else { return };
        if c.rate != rate {
            // On a source of another rate than the site was made for.
            (c.rx, c.rate) = (super::receiver(rate), rate);
        }
        // Carrier power over this block.
        if !iq.is_empty() {
            let p = iq.iter().map(|x| x.norm_sqr()).sum::<f32>() / iq.len() as f32;
            let start = c.t;
            c.floor = if p < c.floor { p } else { c.floor * 1.0005 }.max(1e-20);
            if !c.keyed && p > c.floor * KEY_RATIO {
                c.keyed = true;
                c.keyed_at = Some(start);
            } else if c.keyed && p < c.floor * UNKEY_RATIO {
                c.keyed = false;
                c.unkeyed_at = start;
            }
        }
        c.bits.clear();
        c.rx.push(iq, &mut c.bits);
        let mut frames = Vec::new();
        for b in &c.bits {
            let t = t0 + b.sample / rate;
            if let Some(f) = c.framer.push(b.soft, t) {
                frames.push(f);
            }
        }
        if let Some(f) = frames.iter().rev().find(|f| f.msgs.iter().any(|m| m.is_some())) {
            c.good_at = f.t;
        }
        c.t = t0 + c.rx.samples() as f64 / rate;
        // The control channel: the carrier whose frames keep coming.
        let now = c.t;
        let lost = self.cc.is_none_or(|k| now - self.carriers[k].good_at > CC_LOST_S);
        if lost && self.cc != Some(i) && self.carriers[i].framer.run >= CC_RUN && now - self.carriers[i].good_at < 0.5 {
            let was = self.cc.map(|k| self.carriers[k].hz);
            self.cc = Some(i);
            let hz = self.carriers[i].hz;
            self.parser.cc_hz = Some(hz);
            if let Some(w) = was {
                self.notes.push((false, format!("Control channel moved from {:.4} to {:.4} MHz", w as f64 / 1e6, hz as f64 / 1e6)));
            }
        }
        if self.cc == Some(i) {
            for f in frames {
                self.parser.frame(f.msgs[0], f.msgs[1], f.t, out);
            }
            let granted = std::mem::take(&mut self.parser.granted);
            for g in granted {
                // An assignment is sent several times: once is enough.
                if !self.pending.iter().any(|&(l, t)| l == g.0 && (g.1 - t).abs() < 1.0) {
                    self.pending.push(g);
                }
            }
        }
        self.judge();
    }

    /// Assignments old enough to judge: which carriers keyed for each.
    fn judge(&mut self) {
        let now = self.carriers.iter().map(|c| c.t).fold(self.scan_t, f64::max);
        let (ready, wait): (Vec<_>, Vec<_>) = self.pending.iter().partition(|&&(_, t)| now - t > JUDGE_AFTER_S);
        self.pending = wait;
        let cc = self.control_hz();
        for (lcn, t) in ready {
            let near = |at: Option<f64>| at.is_some_and(|at| at >= t - ONSET_BEFORE_S && at <= t + ONSET_AFTER_S);
            // The site's own frequencies, then the raster's others; the strongest of neighbours keying together.
            let mut keyed: Vec<(u64, f32)> = self.carriers.iter().filter(|c| near(c.keyed_at)).map(|c| (c.hz, f32::MAX)).collect();
            let own: Vec<u64> = self.carriers.iter().map(|c| c.hz).collect();
            let step = self.raster.map_or(25_000, |r| r.1 as u64);
            // Frequencies in no list only once the site has identified itself: a control
            // channel heard badly (or another site's, faintly) mustn't teach them.
            // (Or the config says to: a site that decodes well but never sends its ID.)
            let known = self.parser.site_known() || self.parser.cfg.learn_unlisted;
            // Never another site's frequency (a networked call keys those too), nor a carrier
            // that's always on (another system's control channel: its floor is its own power).
            let others = &self.parser.cfg.others;
            let quiet = self.quiet_floor();
            let mut raster: Vec<(u64, f32)> = self
                .slots
                .iter()
                .filter(|&(&hz, s)| {
                    known
                        && near(s.keyed_at)
                        && !own.iter().chain(others).any(|&o| o.abs_diff(hz) < step / 2)
                        && quiet.is_some_and(|q| s.floor <= q * CONTINUOUS_RATIO)
                })
                .map(|(&hz, s)| (hz, s.level))
                .collect();
            raster.sort_unstable_by_key(|r| r.0);
            for r in raster {
                match keyed.last_mut().filter(|k| k.0.abs_diff(r.0) <= step && k.1 != f32::MAX) {
                    Some(k) if r.1 > k.1 => *k = r,
                    Some(_) => {}
                    None => keyed.push(r),
                }
            }
            // Not the control channel, nor a frequency already learned as another LCN.
            keyed.retain(|&(hz, _)| cc.is_none_or(|c| c.abs_diff(hz) >= SAME_HZ) && !self.learned.iter().any(|(&l, &f)| l != lcn && f.abs_diff(hz) < SAME_HZ));
            if keyed.is_empty() {
                continue;
            }
            let share = 1.0 / keyed.len() as f32;
            for (hz, _) in keyed {
                *self.votes.entry((lcn, hz)).or_default() += share;
            }
            self.decide(lcn);
        }
    }

    /// The raster's noise floor: the median of its frequencies' floors (most are empty).
    fn quiet_floor(&self) -> Option<f32> {
        let mut f: Vec<f32> = self.slots.values().map(|s| s.floor).filter(|f| f.is_finite() && *f < f32::MAX).collect();
        if f.is_empty() {
            return None;
        }
        f.sort_unstable_by(|a, b| a.total_cmp(b));
        Some(f[f.len() / 2])
    }

    /// Enough votes for LCN `lcn`: its frequency, as the air says.
    fn decide(&mut self, lcn: u8) {
        let mut v: Vec<(f32, u64)> = self.votes.iter().filter(|&(&(l, _), _)| l == lcn).map(|(&(_, hz), &n)| (n, hz)).collect();
        v.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let Some(&(best, hz)) = v.first() else { return };
        let second = v.get(1).map_or(0.0, |x| x.0);
        if best < LEARN_VOTES || best < 2.0 * second {
            return;
        }
        if self.learned.get(&lcn) == Some(&hz) {
            return;
        }
        let mhz = |f: u64| format!("{:.4} MHz", f as f64 / 1e6);
        let list = self.configured.get(&lcn).copied();
        let text = match list {
            Some(f) if f.abs_diff(hz) < SAME_HZ => format!("LCN {lcn} = {} confirmed from the air", mhz(hz)),
            Some(f) => format!("LCN {lcn} is {} on the air, not {} as the channel list has it: following the air", mhz(hz), mhz(f)),
            None => format!("LCN {lcn} is {} on the air (not in the channel list): following it", mhz(hz)),
        };
        let warn = list.is_none_or(|f| f.abs_diff(hz) >= SAME_HZ);
        self.notes.push((warn, text));
        self.learned.insert(lcn, hz);
        // Another LCN the list had on this frequency isn't.
        self.parser.cfg.lcns.retain(|&l, f| l == lcn || f.abs_diff(hz) >= SAME_HZ || self.learned.contains_key(&l));
        self.parser.cfg.lcns.insert(lcn, hz);
    }

    /// What was learned, to keep between runs: "lcn=hz" lines.
    pub fn map_to_string(&self) -> String {
        self.learned.iter().map(|(l, f)| format!("{l}={f}\n")).collect()
    }

    pub fn map_from_str(&mut self, s: &str) {
        for line in s.lines() {
            let Some((l, f)) = line.trim().split_once('=') else { continue };
            let (Ok(l), Ok(f)) = (l.trim().parse::<u8>(), f.trim().parse::<u64>()) else { continue };
            // Never another site's frequency (an older version could learn one).
            if (1..=25).contains(&l) && f > 0 && !self.parser.cfg.others.iter().any(|&o| o.abs_diff(f) < SAME_HZ) {
                self.learned.insert(l, f);
                self.parser.cfg.lcns.retain(|&k, g| k == l || g.abs_diff(f) >= SAME_HZ);
                self.parser.cfg.lcns.insert(l, f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{framer, parser, tests::modulate, tests::noise};
    use super::*;
    use crate::trunk::MessageType;
    use std::f64::consts::PI;

    /// A site on four frequencies: the control channel on 857.0, voice on
    /// 857.025 (LCN 2), 857.05 (LCN 3) and 857.075 (LCN 4) — and a channel
    /// list that has LCN 3 and 4 swapped. Calls on LCN 3 key 857.05 each time.
    #[test]
    fn learns_the_channels_from_the_air() {
        let (fs, center) = (240_000.0, 857_040_000.0);
        let cfg = EdacsConfig { lcns: [(1, 857_000_000), (2, 857_025_000), (3, 857_075_000), (4, 857_050_000)].into_iter().collect(), esk: false, ..Default::default() };
        // 10 s of control channel: the site, and a call on LCN 3 every 2 s.
        let mut bits = Vec::new();
        let mut keyed = Vec::new();
        for k in 0..340 {
            let t = k as f64 * 0.03;
            let (m1, m2) = if k % 66 == 20 {
                keyed.push(t);
                parser::group_grant(457, 1234, 3, false, false)
            } else {
                parser::site_id(2, 1)
            };
            bits.extend(framer::encode(m1, m2));
        }
        let cc = modulate(&bits, fs, 2600.0, 857_000_000.0 - center);
        let mut ph = 0.0f64;
        let mut iq: Vec<Complex32> = (0..cc.len())
            .map(|i| {
                let t = i as f64 / fs;
                // The voice channel keys 0.2 s after each assignment, for 1 s.
                let on = keyed.iter().any(|&k| t >= k + 0.2 && t < k + 1.2);
                ph = (ph + 2.0 * PI * (857_050_000.0 - center + 2500.0 * (2.0 * PI * 1000.0 * t).sin()) / fs) % (2.0 * PI);
                cc[i] * 0.3 + if on { Complex32::from_polar(0.3, ph as f32) } else { Complex32::default() }
            })
            .collect();
        noise(&mut iq, 0.02, 5);
        // Each carrier mixed down and decimated to 30 kHz (as the channelizer's heads).
        let mut site = Site::new(cfg, &[857_000_000], 30_000.0);
        let carriers = site.carriers();
        let heads: Vec<Vec<Complex32>> = carriers
            .iter()
            .map(|&hz| {
                let w = 2.0 * PI * (hz - center) / fs;
                let mixed: Vec<Complex32> = iq.iter().enumerate().map(|(n, x)| x * Complex32::from_polar(1.0, (-w * n as f64 % (2.0 * PI)) as f32)).collect();
                // Boxcar over 8 samples (about ±15 kHz), then every 8th.
                mixed.chunks(8).map(|c| c.iter().sum::<Complex32>() / 8.0).collect()
            })
            .collect();
        let mut out = Vec::new();
        for chunk in 0..heads[0].len() / 1500 {
            for (k, h) in heads.iter().enumerate() {
                site.push(k, &h[chunk * 1500..(chunk + 1) * 1500], 0.0, 30_000.0, &mut out);
            }
        }
        let notes = site.take_notes();
        assert_eq!(site.control_hz(), Some(857_000_000), "{notes:?}");
        assert!(notes.iter().any(|n| n.1.contains("LCN 3 is 857.0500 MHz on the air")), "{notes:?}");
        assert_eq!(site.parser.cfg.freq(3), Some(857_050_000));
        assert_eq!(site.parser.cfg.freq(4), None, "LCN 4 was listed on LCN 3's frequency");
        // The later grants follow the air.
        let grants: Vec<u64> = out.iter().filter(|m| m.kind == MessageType::Grant).map(|m| m.freq_hz).collect();
        assert_eq!(grants.first(), Some(&857_075_000), "the first: the list's");
        assert_eq!(grants.last(), Some(&857_050_000), "the last: the air's");
        assert_eq!(site.map_to_string(), "3=857050000\n");
    }
}
