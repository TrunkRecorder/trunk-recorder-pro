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
    /// The engine's totals as last seen (good, bad, no frequency): they start again from 0
    /// when a control channel's decoder is replaced.
    last: Option<(u64, u64, u64)>,
    /// None: its protocol doesn't measure it.
    pub air_sym: Option<f64>,
    /// Symbols running without frame sync (None: its protocol doesn't measure it).
    pub sync_loss_sym: Option<f64>,
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

/// A site's running totals as the engine keeps them ([`trunk_core::trunk::SiteStatus`]).
#[derive(Clone, Debug, Default)]
pub struct SiteCounts {
    pub system: u16,
    pub site: u16,
    /// Its stats key (`sys/<key>/cc/...`).
    pub key: String,
    pub good: u64,
    pub bad: u64,
    /// Calls granted on a channel never turned into a frequency.
    pub no_frequency: u64,
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
    /// Sites' calls going on, as counted when they started: a call's outcome can change
    /// before it ends (one skipped as an unknown talkgroup gets another look when patched
    /// to a known one), and is counted again, the change, when it ends.
    open: BTreeMap<trunk_core::trunk::CallId, ((u16, u16), Outcome)>,
}

/// What became of a call, as counted: recorded, or the reason it wasn't (None: neither).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    Recorded,
    Not(&'static str),
    Unknown,
}

fn outcome(c: &Call) -> Outcome {
    if c.recording {
        Outcome::Recorded
    } else if let Some(r) = c.reason {
        Outcome::Not(r.as_str())
    } else {
        Outcome::Unknown
    }
}

/// Adds a call's outcome to the totals (`add`), or takes it away.
fn tally(calls: &mut CallTotals, o: Outcome, add: bool) {
    let step = |n: &mut u64| *n = if add { *n + 1 } else { n.saturating_sub(1) };
    match o {
        Outcome::Recorded => step(&mut calls.recorded),
        Outcome::Not(r) => step(calls.not_recorded.entry(r.to_string()).or_default()),
        Outcome::Unknown => {}
    }
}

fn count(calls: &mut CallTotals, c: &Call) {
    calls.started += 1;
    tally(calls, outcome(c), true);
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
                    self.open.insert(c.id, ((c.system, site), outcome(c)));
                }
            }
            Event::CopyEnd(c) => {
                if let Some((key, then)) = self.open.remove(&c.id) {
                    let now = outcome(c);
                    if now != then {
                        let calls = &mut self.sites.entry(key).or_default().calls;
                        tally(calls, then, false);
                        tally(calls, now, true);
                    }
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
    pub fn tick(&mut self, dt_s: f64, agg: &Aggregator, sites: &[SiteCounts], channels: &[ChannelSnapshot], sources: &[String]) {
        for s in sites {
            let (key, good, bad) = (&s.key, s.good, s.bad);
            let t = self.sites.entry((s.system, s.site)).or_default();
            // The engine's totals, a fall in them a new decoder counting from 0.
            let since = |now: u64, then: u64| if now >= then { now - then } else { now };
            let (g0, b0, n0) = t.last.unwrap_or((0, 0, 0));
            t.good += since(good, g0);
            t.bad += since(bad, b0);
            // Calls missed for want of a frequency: started, and not recorded.
            let missed = since(s.no_frequency, n0);
            if missed > 0 {
                t.calls.started += missed;
                *t.calls.not_recorded.entry("no_frequency".to_string()).or_default() += missed;
            }
            t.last = Some((good, bad, s.no_frequency));
            if !(dt_s > 0.0 && dt_s <= MAX_TICK_S) {
                continue;
            }
            let get = |k: &str| agg.get(&format!("{key}/cc/{k}")).map(|v| v as f64).filter(|v| v.is_finite());
            if let Some(rate) = get("airSym") {
                *t.air_sym.get_or_insert(0.0) += rate * dt_s;
            }
            if let Some(rate) = get("syncLossSym") {
                *t.sync_loss_sym.get_or_insert(0.0) += rate * dt_s;
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

    /// A call skipped as an unknown talkgroup gets another look when it's patched to a
    /// known one: counted by what finally became of it, recorded or why not.
    #[test]
    fn a_call_counted_one_way_at_its_start_is_moved_when_it_ends_another() {
        let mut t = Totals::new(0);
        let unknown = |id| Call { id, ..call(0, Some(1), false, Some(Reason::UnknownTg)) };
        t.observe(&Event::CopyStart(unknown(1)));
        t.observe(&Event::CopyStart(unknown(2)));
        t.observe(&Event::CopyStart(unknown(3)));
        // Patched: recorded after all; refused (every recorder busy); still unknown.
        t.observe(&Event::CopyEnd(Call { id: 1, ..call(0, Some(1), true, None) }));
        t.observe(&Event::CopyEnd(Call { id: 2, ..call(0, Some(1), false, Some(Reason::NoRecorder)) }));
        t.observe(&Event::CopyEnd(unknown(3)));
        let c = &t.sites[&(0, 1)].calls;
        assert_eq!(c.started, 3);
        assert_eq!(c.recorded, 1);
        assert_eq!(c.not_recorded.get("no_recorder"), Some(&1));
        assert_eq!(c.not_recorded.get("unknown_tg"), Some(&1));
        assert!(t.open.is_empty(), "nothing kept once they end");
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
            let no_frequency = if i < 5 { i } else { i - 5 };
            t.tick(1.0, &agg, &[SiteCounts { key: key.clone(), good, bad: 2, no_frequency, ..Default::default() }], &[], &["a".into()]);
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
        t.tick(30.0, &agg, &[SiteCounts { key, good, bad: 2, no_frequency: 4, ..Default::default() }], &[], &["a".into()]);
        assert_eq!(t.sources[0].seconds, 10.0);
    }

    #[test]
    fn calls_on_a_channel_never_resolved_are_started_and_missed() {
        let mut t = Totals::new(0);
        let agg = Aggregator::default();
        let tick = |t: &mut Totals, n: u64| t.tick(1.0, &agg, &[SiteCounts { key: "sys/x".into(), no_frequency: n, ..Default::default() }], &[], &[]);
        tick(&mut t, 0);
        tick(&mut t, 2);
        tick(&mut t, 3);
        // The site's call manager replaced: it counts from 0 again.
        tick(&mut t, 1);
        let c = &t.sites[&(0, 0)].calls;
        assert_eq!(c.started, 4);
        assert_eq!(c.recorded, 0);
        assert_eq!(c.not_recorded.get("no_frequency"), Some(&4));
    }
}
