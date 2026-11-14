//! Running totals for plugins' node health reports (the `health` topic): how well each
//! trunked site's control channel is decoded, the calls it announced and what was recorded of
//! them, the voice frames of what was, and each conventional channel's and radio's time. Fed
//! by the session's events and, each tick, by what the dashboard's series measured that second.

use std::collections::BTreeMap;

use trunk_core::trunk::{Call, ChannelKind, ChannelSnapshot, Event, Voice};
use trunk_recorder_plugin::{CallTotals, FrameTotals, VoiceTotals};

use super::Aggregator;

/// A tick longer than this isn't counted as time (the session stalled, the machine slept).
const MAX_TICK_S: f64 = 5.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteTotals {
    pub locked_s: f64,
    pub good: u64,
    pub bad: u64,
    /// The engine's totals as last seen (they start again from 0 when a control channel's
    /// decoder is replaced).
    last: Option<(u64, u64)>,
    /// None: its protocol doesn't measure it.
    pub air_sym: Option<f64>,
    pub snr_db_total: f64,
    pub snr_s: f64,
    pub phase_err_deg_total: f64,
    pub phase_err_s: f64,
    pub calls: CallTotals,
    pub voice: VoiceTotals,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelTotals {
    pub heard_s: f64,
    pub open_s: f64,
    pub signal_db_total: f64,
    pub snr_db_total: f64,
    pub closed_db_total: f64,
    pub closed_s: f64,
    pub calls: CallTotals,
    pub skipped: u64,
    pub voice: VoiceTotals,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceTotals {
    pub seconds: f64,
    pub dropped_s: f64,
    pub clip_pct_total: f64,
}

/// Since recording started.
#[derive(Clone, Debug, Default)]
pub struct Totals {
    /// By (system, site): the numbers calls carry.
    pub sites: BTreeMap<(u16, u16), SiteTotals>,
    /// By (system, Hz).
    pub channels: BTreeMap<(u16, u64), ChannelTotals>,
    /// By source.
    pub sources: Vec<SourceTotals>,
}

fn count(calls: &mut CallTotals, c: &Call) {
    calls.started += 1;
    if c.recording {
        calls.recorded += 1;
    } else if let Some(r) = c.reason {
        *calls.not_recorded.entry(r.as_str().to_string()).or_default() += 1;
    }
}

/// The codec of a call's voice: IMBE or AMBE+2 (none: analog).
fn codec(v: Voice) -> Option<bool> {
    match v {
        Voice::P25 | Voice::ProVoice => Some(false),
        Voice::P25Tdma { .. } | Voice::Dmr { .. } | Voice::Nxdn(_) => Some(true),
        Voice::Fm | Voice::Am => None,
    }
}

fn add_frames(v: &mut VoiceTotals, ambe: bool, rec: &trunk_recorder_plugin::CallRecord) {
    let t: &mut FrameTotals = if ambe { &mut v.ambe } else { &mut v.imbe };
    for i in &rec.error_list {
        t.frames += i.frames;
        t.bad += i.bad_frames;
        t.errors += i.error_count;
    }
}

impl Totals {
    pub fn new(sources: usize) -> Totals {
        Totals { sources: vec![SourceTotals::default(); sources], ..Default::default() }
    }

    /// One of the session's events.
    pub fn observe(&mut self, ev: &Event) {
        match ev {
            // A trunked site's own copy of a call (a call heard on two sites is two copies).
            Event::CopyStart(c) => {
                if let Some(site) = c.site {
                    count(&mut self.sites.entry((c.system, site)).or_default().calls, c);
                }
            }
            Event::CallStart(c) if c.site.is_none() => count(&mut self.channels.entry((c.system, c.freq_hz)).or_default().calls, c),
            Event::ConvSkipped { freq_hz, .. } => {
                for (_, t) in self.channels.iter_mut().filter(|((_, hz), _)| hz == freq_hz) {
                    t.skipped += 1;
                }
            }
            Event::Concluded(k) => {
                let Some(ambe) = codec(k.call.voice) else { return };
                let voice = match k.call.site {
                    Some(site) => &mut self.sites.entry((k.call.system, site)).or_default().voice,
                    None => &mut self.channels.entry((k.call.system, k.call.freq_hz)).or_default().voice,
                };
                add_frames(voice, ambe, &k.record);
            }
            _ => {}
        }
    }

    /// A tick of `dt_s` seconds, the series measured in it in `agg`: each site
    /// (system, site, its series' key, the engine's good and bad totals), each conventional
    /// channel, each source (its series' key).
    pub fn tick(&mut self, dt_s: f64, agg: &Aggregator, sites: &[(u16, u16, String, u64, u64)], channels: &[ChannelSnapshot], sources: &[String]) {
        for &(system, site, ref key, good, bad) in sites {
            let t = self.sites.entry((system, site)).or_default();
            // Blocks: the engine's totals, a fall in them a new decoder counting from 0.
            let (g0, b0) = t.last.unwrap_or((0, 0));
            t.good += if good >= g0 { good - g0 } else { good };
            t.bad += if bad >= b0 { bad - b0 } else { bad };
            t.last = Some((good, bad));
            if !(dt_s > 0.0 && dt_s <= MAX_TICK_S) {
                continue;
            }
            let get = |k: &str| agg.get(&format!("{key}/cc/{k}")).map(|v| v as f64).filter(|v| v.is_finite());
            if let Some(rate) = get("airSym") {
                *t.air_sym.get_or_insert(0.0) += rate * dt_s;
            }
            if get("locked").is_some_and(|l| l > 0.5) {
                t.locked_s += dt_s;
                if let Some(v) = get("snr") {
                    t.snr_db_total += v * dt_s;
                    t.snr_s += dt_s;
                }
                if let Some(v) = get("phaseErr") {
                    t.phase_err_deg_total += v * dt_s;
                    t.phase_err_s += dt_s;
                }
            }
        }
        if !(dt_s > 0.0 && dt_s <= MAX_TICK_S) {
            return;
        }
        let dropping: Vec<bool> = sources.iter().map(|n| agg.get(&format!("src/{n}/dropped")).is_some_and(|d| d > 0.0)).collect();
        for (i, n) in sources.iter().enumerate() {
            if i >= self.sources.len() {
                self.sources.resize(i + 1, SourceTotals::default());
            }
            let s = &mut self.sources[i];
            s.seconds += dt_s;
            if dropping[i] {
                s.dropped_s += dt_s;
            }
            if let Some(c) = agg.get(&format!("src/{n}/clipPct")) {
                s.clip_pct_total += c as f64 * dt_s;
            }
        }
        for c in channels.iter().filter(|c| matches!(c.kind, ChannelKind::Conventional(_))) {
            let t = self.channels.entry((c.system, c.freq_hz)).or_default();
            if !dropping.get(c.source).copied().unwrap_or(false) {
                t.heard_s += dt_s;
            }
            if !c.power_db.is_finite() {
                continue;
            }
            if c.open {
                t.open_s += dt_s;
                t.signal_db_total += c.power_db * dt_s;
                if c.noise_db.is_finite() {
                    t.snr_db_total += (c.power_db - c.noise_db) * dt_s;
                }
            } else {
                t.closed_db_total += c.power_db * dt_s;
                t.closed_s += dt_s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_core::metrics::Sink;
    use trunk_core::trunk::calls::Reason;

    fn call(system: u16, site: Option<u16>, recording: bool, reason: Option<Reason>) -> Call {
        Call { system, site, recording, reason, freq_hz: 851_000_000, ..Default::default() }
    }

    #[test]
    fn calls_count_by_site_and_why_not() {
        let mut t = Totals::new(1);
        t.observe(&Event::CopyStart(call(0, Some(1), true, None)));
        t.observe(&Event::CopyStart(call(0, Some(1), false, Some(Reason::NoRecorder))));
        t.observe(&Event::CopyStart(call(0, Some(2), true, None)));
        // A call's start (once, however many sites) isn't a site's.
        t.observe(&Event::CallStart(call(0, Some(1), true, None)));
        let s = &t.sites[&(0, 1)].calls;
        assert_eq!((s.started, s.recorded, s.not_recorded["no_recorder"]), (2, 1, 1));
        assert_eq!(t.sites[&(0, 2)].calls.started, 1);
        // Conventional: by frequency, with what no row took.
        t.observe(&Event::CallStart(call(3, None, true, None)));
        t.observe(&Event::ConvSkipped { freq_hz: 851_000_000, code: "D023N".into() });
        let c = &t.channels[&(3, 851_000_000)];
        assert_eq!((c.calls.started, c.calls.recorded, c.skipped), (1, 1, 1));
    }

    #[test]
    fn ticks_add_up_time_locked_and_blocks() {
        let mut t = Totals::new(1);
        let mut agg = Aggregator::default();
        let key = "sys/dcfd".to_string();
        let mut good = 0;
        for i in 0..10 {
            agg.begin(1000.0 + i as f64);
            agg.gauge(&format!("{key}/cc/locked"), if i < 6 { 1.0 } else { 0.0 });
            agg.gauge(&format!("{key}/cc/snr"), 12.0);
            agg.counter(&format!("{key}/cc/airSym"), i * 4000);
            agg.gauge("src/a/clipPct", 0.5);
            // The decoder is replaced half way: its count starts again.
            good = if i == 5 { 10 } else { good + 40 };
            t.tick(1.0, &agg, &[(0, 0, key.clone(), good, 2)], &[], &["a".into()]);
        }
        let s = &t.sites[&(0, 0)];
        assert_eq!(s.locked_s, 6.0);
        // 40 a tick to 200, then from 10 to 170.
        assert_eq!(s.good, 200 + 10 + 160);
        assert_eq!(s.bad, 2);
        assert_eq!((s.snr_db_total / s.snr_s), 12.0);
        // No rate from the first total: nine seconds of 4000.
        assert_eq!(s.air_sym, Some(36_000.0));
        assert_eq!(t.sources[0].seconds, 10.0);
        assert!((t.sources[0].clip_pct_total - 5.0).abs() < 1e-9);
        // A stalled tick isn't time.
        t.tick(30.0, &agg, &[(0, 0, key, good, 2)], &[], &["a".into()]);
        assert_eq!(t.sources[0].seconds, 10.0);
    }
}
