//! Node health reports: telling a call-collecting service how well this recorder hears what it
//! uploads (`docs/api/node-health.md`, version 1).
//!
//! The recorder sends the [`health`](crate::topic::HEALTH) topic every 10 s: running totals for
//! each site, conventional channel and radio ([`Health`]). A plugin keeps them in a [`History`]
//! and a [`Reporter`] for each node it reports on, which says when that node's next report is
//! due — at a random point in each interval, its own, so a service's nodes don't all report at
//! once — and makes the [`Report`] from the totals: what changed since its last delivered one.
//!
//! ```no_run
//! # use trunk_recorder_plugin::{health::*, Health};
//! # fn post(_: &Report) -> Result<Answer, Failure> { unimplemented!() }
//! # let mut history = History::default();
//! # let mut node = Reporter::new(Select::system("dcfd"), "1234");
//! # let health = Health::default();
//! history.push(health.clone());
//! if node.due(&health) {
//!     if let Some(r) = node.report(&history, serde_json::json!({ "systemId": 1234 }), Recorder::new("my-recorder", "1.0")) {
//!         match post(&r) {
//!             Ok(answer) => node.delivered(&r, answer.next_s),
//!             Err(f) => node.failed(&health, f),
//!         }
//!     }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{CallTotals, ChannelHealth, FrameTotals, Health, SiteHealth, SourceHealth, VoiceTotals};

/// This format's version.
pub const VERSION: u32 = 1;
/// Seconds between a node's reports, unless the service says otherwise.
pub const INTERVAL_S: f64 = 300.0;
/// Each send moves by up to this much either way, s.
pub const JITTER_S: f64 = 15.0;
/// No report before this long after recording starts, s.
pub const FIRST_AFTER_S: f64 = 60.0;
/// A report covers at most this long, s: older time not delivered is dropped (`gap`).
pub const MAX_WINDOW_S: f64 = 900.0;
/// The longest a service can make the interval (`nextS`), or backing off can, s.
pub const MAX_INTERVAL_S: f64 = 3600.0;
/// The shortest a service can make it, s.
pub const MIN_INTERVAL_S: f64 = 60.0;

// ─── The report (as the spec has it: camelCase) ──────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub v: u32,
    /// The service's fields naming the node (its binding).
    pub node: Value,
    pub recorder: Recorder,
    pub window: Window,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub radios: Vec<RadioReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<Clock>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recorder {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_s: Option<f64>,
}

impl Recorder {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Recorder {
        Recorder { name: name.into(), version: version.into(), up_s: None }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub start: f64,
    pub end: f64,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gap: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub r#final: bool,
}

impl Window {
    pub fn seconds(&self) -> f64 {
        self.end - self.start
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteReport {
    pub name: String,
    pub protocol: String,
    /// Which receiver, when the recorder decodes the site with more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub id: BTreeMap<String, u32>,
    pub cc: CcReport,
    pub calls: CallsReport,
    #[serde(default, skip_serializing_if = "VoiceReport::is_empty")]
    pub voice: VoiceReport,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<u64>,
    pub locked_s: f64,
    pub good: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_loss_sym: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_sym: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snr_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mer_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_err_deg: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallsReport {
    pub started: u64,
    pub recorded: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub not_recorded: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imbe: Option<FrameTotals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambe: Option<FrameTotals>,
}

impl VoiceReport {
    pub fn is_empty(&self) -> bool {
        self.imbe.is_none() && self.ambe.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelReport {
    pub hz: u64,
    pub mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Which receiver, when the recorder hears the channel on more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// The code it records by: `CTCSS 151.4`, `DCS 023N`, `NAC 293`, `CC 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    pub heard_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise_db: Option<f64>,
    pub opens: u64,
    pub open_s: f64,
    pub calls: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by_other: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snr_db: Option<f64>,
    #[serde(default, skip_serializing_if = "VoiceReport::is_empty")]
    pub voice: VoiceReport,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioReport {
    pub name: String,
    pub kind: String,
    pub seconds: f64,
    pub dropped_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppm_error: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppm_applied: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_offset_ms: Option<f64>,
}

/// A service's answer to a report: all optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Answer {
    /// Send the next report this many seconds after this one.
    pub next_s: Option<f64>,
    /// Something to show the user.
    pub message: Option<String>,
    pub role: Option<Value>,
}

/// Why a report wasn't delivered ([`Reporter::failed`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    /// Not reached, or a server error: the next report covers this one's window too.
    Unreachable,
    /// `429` / `503`: wait this long if the service said (`Retry-After`), else back off.
    Busy(Option<f64>),
    /// `401` / `403`: the node's credentials are wrong; no more reports for it.
    Refused,
    /// `400`: the report was wrong. Logged; the next goes as usual.
    Rejected,
}

// ─── Which sites and channels a node is ──────────────────────────────────────

/// The part of the recorder a node reports on: a system, all its sites and channels but some,
/// or some only (a site a service takes as a node of its own).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Select {
    pub short_name: String,
    /// Only these sites (by name); none: all.
    pub only: Option<BTreeSet<String>>,
    /// Not these sites.
    pub except: BTreeSet<String>,
}

impl Select {
    pub fn system(short_name: &str) -> Select {
        Select { short_name: short_name.into(), ..Default::default() }
    }
    pub fn site(short_name: &str, site: &str) -> Select {
        Select { short_name: short_name.into(), only: Some([site.to_string()].into()), ..Default::default() }
    }
    pub fn except(mut self, site: &str) -> Select {
        self.except.insert(site.to_string());
        self
    }
    fn site_in(&self, s: &SiteHealth) -> bool {
        s.short_name == self.short_name && self.only.as_ref().is_none_or(|o| o.contains(&s.site_name)) && !self.except.contains(&s.site_name)
    }
    fn channel_in(&self, c: &ChannelHealth) -> bool {
        c.short_name == self.short_name && self.only.is_none()
    }
}

// ─── Making a report from two snapshots ──────────────────────────────────────

fn mean(total: f64, s: f64) -> Option<f64> {
    (s >= 1.0).then(|| (total / s * 10.0).round() / 10.0)
}

fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn frames(a: FrameTotals, b: Option<&FrameTotals>) -> Option<FrameTotals> {
    let b = b.copied().unwrap_or_default();
    let d = FrameTotals {
        frames: a.frames.saturating_sub(b.frames),
        bad: a.bad.saturating_sub(b.bad),
        errors: a.errors.saturating_sub(b.errors),
        missing: a.missing.map(|m| m.saturating_sub(b.missing.unwrap_or(0))),
    };
    (d.frames > 0).then_some(d)
}

fn voice(a: &VoiceTotals, b: Option<&VoiceTotals>) -> VoiceReport {
    VoiceReport { imbe: frames(a.imbe, b.map(|b| &b.imbe)), ambe: frames(a.ambe, b.map(|b| &b.ambe)) }
}

fn calls(a: &CallTotals, b: Option<&CallTotals>) -> CallsReport {
    let started = a.started.saturating_sub(b.map_or(0, |b| b.started));
    let recorded = a.recorded.saturating_sub(b.map_or(0, |b| b.recorded));
    let not_recorded = a
        .not_recorded
        .iter()
        .map(|(k, &n)| (camel(k), n.saturating_sub(b.and_then(|b| b.not_recorded.get(k)).copied().unwrap_or(0))))
        .filter(|&(_, n)| n > 0)
        .collect();
    CallsReport { started, recorded, not_recorded }
}

/// `no_source` → `noSource`.
fn camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn site(a: &SiteHealth, b: Option<&SiteHealth>) -> SiteReport {
    let d = |f: fn(&SiteHealth) -> f64| f(a) - b.map_or(0.0, f);
    let id = [("wacn", a.wacn), ("sysId", a.sys_id), ("rfss", a.rfss), ("site", a.site), ("nac", a.nac)]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
        .collect();
    SiteReport {
        name: a.site_name.clone(),
        protocol: a.protocol.clone(),
        receiver: None,
        id,
        cc: CcReport {
            hz: a.control_channel_hz,
            locked_s: r1(d(|s| s.locked_s)),
            good: a.good.saturating_sub(b.map_or(0, |b| b.good)),
            bad: Some(a.bad.saturating_sub(b.map_or(0, |b| b.bad))),
            sync_loss_sym: a.sync_loss_sym.map(|n| n.saturating_sub(b.and_then(|b| b.sync_loss_sym).unwrap_or(0))),
            air_sym: a.air_sym.map(|n| n.saturating_sub(b.and_then(|b| b.air_sym).unwrap_or(0))),
            symbol_rate: a.symbol_rate,
            snr_db: mean(d(|s| s.snr_db_total), d(|s| s.snr_s)),
            mer_db: None,
            phase_err_deg: mean(d(|s| s.phase_err_deg_total), d(|s| s.phase_err_s)),
        },
        calls: calls(&a.calls, b.map(|b| &b.calls)),
        voice: voice(&a.voice, b.map(|b| &b.voice)),
    }
}

fn channel(a: &ChannelHealth, b: Option<&ChannelHealth>) -> ChannelReport {
    let d = |f: fn(&ChannelHealth) -> f64| f(a) - b.map_or(0.0, f);
    let c = calls(&a.calls, b.map(|b| &b.calls));
    let skipped = a.skipped.saturating_sub(b.map_or(0, |b| b.skipped));
    ChannelReport {
        hz: a.freq_hz,
        mode: a.mode.clone(),
        name: a.names.join(", "),
        receiver: None,
        access: a.access.clone(),
        heard_s: r1(d(|c| c.heard_s)),
        noise_db: mean(d(|c| c.closed_db_total), d(|c| c.closed_s)),
        opens: c.started + skipped,
        open_s: r1(d(|c| c.open_s)),
        calls: c.recorded,
        opened_by_other: Some(skipped),
        signal_db: mean(d(|c| c.signal_db_total), d(|c| c.open_s)),
        snr_db: mean(d(|c| c.snr_db_total), d(|c| c.open_s)),
        voice: voice(&a.voice, b.map(|b| &b.voice)),
    }
}

fn radio(a: &SourceHealth, b: Option<&SourceHealth>) -> RadioReport {
    let d = |f: fn(&SourceHealth) -> f64| f(a) - b.map_or(0.0, f);
    let secs = d(|s| s.seconds);
    RadioReport {
        name: a.name.clone(),
        kind: a.driver.clone(),
        seconds: r1(secs),
        dropped_s: r1(d(|s| s.dropped_s)),
        clip_pct: (secs >= 1.0).then(|| (d(|s| s.clip_pct_total) / secs * 1000.0).round() / 1000.0),
        ppm_error: a.ppm_error.map(|p| (p * 100.0).round() / 100.0),
        ppm_applied: a.ppm_applied.map(|p| (p * 100.0).round() / 100.0),
    }
}

/// The report of what changed from `base` to `now` for the node `sel` names (`base` none, or
/// from before a restart of recording: from when recording started). None when nothing of the
/// recorder is that node's.
pub fn report(base: Option<&Health>, now: &Health, sel: &Select, node: Value, recorder: Recorder, seq: u64) -> Option<Report> {
    let base = base.filter(|b| b.since == now.since && b.time <= now.time);
    let sites: Vec<SiteReport> = now
        .sites
        .iter()
        .filter(|s| sel.site_in(s))
        .map(|a| site(a, base.and_then(|b| b.sites.iter().find(|s| s.short_name == a.short_name && s.site_name == a.site_name))))
        .collect();
    let channels: Vec<ChannelReport> = now
        .channels
        .iter()
        .filter(|c| sel.channel_in(c))
        .map(|a| channel(a, base.and_then(|b| b.channels.iter().find(|c| c.short_name == a.short_name && c.freq_hz == a.freq_hz))))
        .collect();
    if sites.is_empty() && channels.is_empty() {
        return None;
    }
    let used: BTreeSet<usize> = now.sites.iter().filter(|s| sel.site_in(s)).filter_map(|s| s.source).chain(now.channels.iter().filter(|c| sel.channel_in(c)).filter_map(|c| c.source)).collect();
    let radios = now.sources.iter().filter(|s| used.contains(&s.index)).map(|a| radio(a, base.and_then(|b| b.sources.iter().find(|s| s.name == a.name)))).collect();
    let start = base.map_or(now.since, |b| b.time);
    Some(Report {
        v: VERSION,
        node,
        recorder: Recorder { up_s: Some(r1(now.time - now.since)), ..recorder },
        window: Window { start: r1(start), end: r1(now.time), seq, gap: false, r#final: false },
        sites,
        channels,
        radios,
        clock: (now.clock_sync.is_some() || now.system_offset_ms.is_some())
            .then(|| Clock { sync: now.clock_sync.clone(), system_offset_ms: now.system_offset_ms.map(f64::round) }),
    })
}

// ─── Keeping the totals, and when to send ────────────────────────────────────

/// The recorder's recent totals: enough to report a node's window after failures, up to
/// [`MAX_WINDOW_S`].
#[derive(Clone, Debug, Default)]
pub struct History {
    snaps: VecDeque<Health>,
}

impl History {
    pub fn push(&mut self, h: Health) {
        if self.snaps.back().is_some_and(|b| b.since != h.since) {
            self.snaps.clear();
        }
        let keep = h.time - MAX_WINDOW_S - 2.0 * INTERVAL_S;
        while self.snaps.front().is_some_and(|f| f.time < keep) {
            self.snaps.pop_front();
        }
        self.snaps.push_back(h);
    }

    pub fn latest(&self) -> Option<&Health> {
        self.snaps.back()
    }

    /// The earliest totals at or after `t`.
    fn at_or_after(&self, t: f64) -> Option<&Health> {
        self.snaps.iter().find(|h| h.time >= t)
    }
}

/// splitmix64: enough randomness for spreading reports out, without a dependency.
fn next_rand(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

fn seed(key: &str) -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    // FNV-1a of the node's key, so two nodes started in the same instant still differ.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ t ^ ((std::process::id() as u64) << 32)
}

/// One node's reports: when the next is due, and what it covers.
#[derive(Clone, Debug)]
pub struct Reporter {
    pub select: Select,
    rng: u64,
    /// The interval, and the slot the next report is for: reports go once a slot, each
    /// moved by the jitter.
    interval: f64,
    slot: Option<f64>,
    next_at: Option<f64>,
    /// While backing off (`Busy`): the interval to use, doubling.
    backoff: Option<f64>,
    /// The end of the last report delivered (its totals' `time`), and its recording's `since`.
    delivered_to: Option<(f64, f64)>,
    /// The totals the last report was made from (`time`, `since`), exactly: the report's own
    /// figures are rounded, and the next window must start where this one ended.
    built: Option<(f64, f64)>,
    seq: u64,
    /// The credentials were refused: no more reports.
    pub stopped: bool,
}

impl Reporter {
    /// `key`: something of the node's own (its ID), to spread nodes started together.
    pub fn new(select: Select, key: &str) -> Reporter {
        Reporter { select, rng: seed(key), interval: INTERVAL_S, slot: None, next_at: None, backoff: None, delivered_to: None, built: None, seq: 0, stopped: false }
    }

    /// For tests: a fixed seed.
    pub fn seeded(select: Select, seed: u64) -> Reporter {
        Reporter { rng: seed, ..Reporter::new(select, "") }
    }

    fn jitter(&mut self) -> f64 {
        (next_rand(&mut self.rng) * 2.0 - 1.0) * JITTER_S
    }

    /// Report for `slot`, jittered, but not before `now`.
    fn aim(&mut self, slot: f64, now: f64) {
        self.slot = Some(slot);
        self.next_at = Some((slot + self.jitter()).max(now));
    }

    /// On to the slot after this one (or, if that's long past, the first still to come).
    fn advance(&mut self, now: f64) {
        let interval = self.backoff.unwrap_or(self.interval);
        let mut slot = self.slot.unwrap_or(now) + interval;
        while slot + JITTER_S < now {
            slot += interval;
        }
        self.aim(slot, now);
    }

    /// When the next report is due, Unix s (none: not yet known — no totals seen).
    pub fn next_at(&self) -> Option<f64> {
        self.next_at
    }

    /// Whether a report is due now, given the latest totals.
    pub fn due(&mut self, now: &Health) -> bool {
        if self.stopped {
            return false;
        }
        // Recording restarted: reports start over.
        if self.delivered_to.is_some_and(|(_, since)| since != now.since) {
            self.delivered_to = None;
            self.seq = 0;
        }
        if self.slot.is_none() {
            // The node's phase: a random point in the interval, its first slot the first at
            // that point FIRST_AFTER_S or more after recording started.
            let phase = next_rand(&mut self.rng) * self.interval;
            let mut slot = now.since + phase;
            while slot < now.since + FIRST_AFTER_S {
                slot += self.interval;
            }
            self.aim(slot, now.since + FIRST_AFTER_S);
        }
        self.next_at.is_some_and(|t| now.time >= t)
    }

    /// The report due: from the end of the last one delivered (or, after more than
    /// [`MAX_WINDOW_S`] undelivered, the window's latest start, with `gap`) to the latest totals.
    pub fn report(&mut self, history: &History, node: Value, recorder: Recorder) -> Option<Report> {
        let now = history.latest()?;
        let from = self.delivered_to.filter(|&(_, since)| since == now.since).map(|(t, _)| t);
        let oldest = now.time - MAX_WINDOW_S;
        let (base, gap) = match from {
            Some(t) if t >= oldest => (history.at_or_after(t), false),
            Some(_) => (history.at_or_after(oldest), true),
            // Never delivered: from the start of recording, unless that's too long ago.
            None if now.time - now.since > MAX_WINDOW_S => (history.at_or_after(oldest), true),
            None => (None, false),
        };
        let mut r = report(base, now, &self.select, node, recorder, self.seq)?;
        r.window.gap = gap;
        self.built = Some((now.time, now.since));
        Some(r)
    }

    /// `r` was delivered; the service may have set the interval (`next_s`).
    pub fn delivered(&mut self, r: &Report, next_s: Option<f64>) {
        self.seq += 1;
        // (A report this Reporter didn't make: its own, rounded, figures.)
        let made = self.built.take().unwrap_or((r.window.end, r.window.end - r.recorder.up_s.unwrap_or(0.0)));
        self.delivered_to = Some(made);
        self.backoff = None;
        match next_s.filter(|n| n.is_finite()) {
            Some(n) => {
                self.interval = n.clamp(MIN_INTERVAL_S, MAX_INTERVAL_S);
                self.aim(r.window.end + self.interval, r.window.end + 1.0);
            }
            None => self.advance(r.window.end + 1.0),
        }
    }

    /// The report due at `now` wasn't delivered.
    pub fn failed(&mut self, now: &Health, why: Failure) {
        match why {
            Failure::Refused => self.stopped = true,
            Failure::Busy(Some(after)) => self.next_at = Some(now.time + after.clamp(1.0, MAX_INTERVAL_S)),
            Failure::Busy(None) => {
                self.backoff = Some((self.backoff.unwrap_or(self.interval) * 2.0).min(MAX_INTERVAL_S));
                self.advance(now.time + 1.0);
            }
            // Wrong: counted as sent, so the next doesn't carry it again.
            Failure::Rejected => {
                self.seq += 1;
                self.delivered_to = Some((now.time, now.since));
                self.advance(now.time + 1.0);
            }
            Failure::Unreachable => self.advance(now.time + 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(time: f64, good: u64, frames: u64, bad: u64) -> Health {
        let mut calls = CallTotals { started: good / 10, recorded: good / 12, ..Default::default() };
        calls.not_recorded.insert("no_source".into(), good / 100);
        Health {
            time,
            since: 1000.0,
            sites: vec![
                SiteHealth {
                    short_name: "dcfd".into(),
                    site_name: "north".into(),
                    protocol: "p25".into(),
                    control_channel_hz: Some(851_000_000),
                    wacn: Some(0xBEE00),
                    sys_id: Some(0x3AB),
                    source: Some(0),
                    locked_s: time - 1000.0,
                    good,
                    bad: good / 20,
                    air_sym: Some(good * 100),
                    symbol_rate: Some(4800),
                    snr_db_total: 15.0 * (time - 1000.0),
                    snr_s: time - 1000.0,
                    calls,
                    voice: VoiceTotals { imbe: FrameTotals { frames, bad, errors: bad * 3, missing: None }, ..Default::default() },
                    ..Default::default()
                },
                SiteHealth { short_name: "dcfd".into(), site_name: "south".into(), protocol: "p25".into(), ..Default::default() },
            ],
            channels: vec![ChannelHealth { short_name: "fire".into(), freq_hz: 154_430_000, mode: "fm".into(), source: Some(1), heard_s: time - 1000.0, ..Default::default() }],
            sources: vec![
                SourceHealth { index: 0, name: "800".into(), driver: "airspy".into(), seconds: time - 1000.0, clip_pct_total: 0.1 * (time - 1000.0), ppm_error: Some(1.134), ppm_applied: Some(1.1), ..Default::default() },
                SourceHealth { index: 1, name: "vhf".into(), driver: "rtlsdr".into(), seconds: time - 1000.0, ..Default::default() },
            ],
            system_offset_ms: Some(12.4),
            clock_sync: Some("ntp".into()),
        }
    }

    #[test]
    fn a_report_is_what_changed_for_its_node_alone() {
        let a = totals(1300.0, 1000, 500, 50);
        let b = totals(1600.0, 2500, 900, 70);
        let r = report(Some(&a), &b, &Select::system("dcfd").except("south"), serde_json::json!({ "systemId": 1 }), Recorder::new("x", "1"), 3).unwrap();
        assert_eq!((r.window.start, r.window.end, r.window.seq), (1300.0, 1600.0, 3));
        assert_eq!(r.sites.len(), 1);
        let s = &r.sites[0];
        assert_eq!((s.cc.locked_s, s.cc.good, s.cc.bad, s.cc.air_sym), (300.0, 1500, Some(75), Some(150_000)));
        assert_eq!(s.cc.snr_db, Some(15.0));
        assert_eq!(s.id["wacn"], 0xBEE00);
        assert_eq!(s.calls.started, 150);
        assert_eq!(s.calls.not_recorded["noSource"], 15);
        assert_eq!(s.voice.imbe, Some(FrameTotals { frames: 400, bad: 20, errors: 60, missing: None }));
        assert_eq!(s.voice.ambe, None, "no frames: left out, not 0");
        assert!(r.channels.is_empty(), "another system's");
        assert_eq!(r.radios.len(), 1, "only the radio its sites are on");
        assert_eq!(r.radios[0].clip_pct, Some(0.1));
        assert_eq!((r.radios[0].ppm_error, r.radios[0].ppm_applied), (Some(1.13), Some(1.1)), "the error, and what AutoTune corrects of it");
        let json = serde_json::to_value(&r.radios[0]).unwrap();
        assert_eq!((json["ppmError"].as_f64(), json["ppmApplied"].as_f64()), (Some(1.13), Some(1.1)));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["sites"][0]["cc"]["lockedS"], 300.0);
        assert_eq!(v["recorder"]["upS"], 600.0);
        assert_eq!(v["clock"]["systemOffsetMs"], 12.0);
        assert_eq!(v["clock"]["sync"], "ntp");
        assert!(v["window"].get("gap").is_none());
        assert!(report(None, &b, &Select::system("nope"), Value::Null, Recorder::default(), 0).is_none());
    }

    #[test]
    fn after_a_restart_of_recording_it_counts_from_the_start() {
        let a = totals(1300.0, 1000, 500, 50);
        let mut b = totals(1100.0, 300, 0, 0);
        b.since = 1050.0;
        let r = report(Some(&a), &b, &Select::site("dcfd", "north"), Value::Null, Recorder::default(), 0).unwrap();
        assert_eq!(r.window.start, 1050.0);
        assert_eq!(r.sites[0].cc.good, 300);
    }

    #[test]
    fn nodes_report_at_their_own_random_point_in_each_interval() {
        let mut starts = Vec::new();
        for seed in 0..50 {
            let mut n = Reporter::seeded(Select::system("dcfd"), seed);
            let mut h = totals(1000.0, 0, 0, 0);
            assert!(!n.due(&h));
            let first = n.next_at().unwrap();
            assert!(first >= 1000.0 + FIRST_AFTER_S && first <= 1000.0 + FIRST_AFTER_S + INTERVAL_S + JITTER_S, "{first}");
            starts.push(first);
            // The next ones keep to the phase, give or take the jitter.
            h.time = first;
            assert!(n.due(&h));
            let mut hist = History::default();
            hist.push(h.clone());
            let r = n.report(&hist, Value::Null, Recorder::default()).unwrap();
            n.delivered(&r, None);
            let second = n.next_at().unwrap();
            assert!((second - first - INTERVAL_S).abs() <= 2.0 * JITTER_S + 1.0, "{first} {second}");
        }
        // Spread over the interval, not bunched.
        starts.sort_by(f64::total_cmp);
        assert!(starts[49] - starts[0] > INTERVAL_S * 0.7, "{starts:?}");
    }

    #[test]
    fn delivered_windows_follow_on_at_times_that_are_not_round() {
        // Recording started, and totals come, at times a report rounds (to 0.1 s).
        let since = 1000.0437;
        let mut n = Reporter::seeded(Select::system("dcfd"), 5);
        let mut hist = History::default();
        let (mut seqs, mut good, mut last_end) = (Vec::new(), 0, None);
        let mut t = since;
        while t < since + 2000.0 {
            let mut h = totals(t, ((t - since) / 10.0) as u64 * 37, 0, 0);
            h.since = since;
            hist.push(h.clone());
            if n.due(&h) {
                let r = n.report(&hist, Value::Null, Recorder::default()).unwrap();
                if let Some(end) = last_end {
                    assert_eq!(r.window.start, end, "each window starts where the last ended");
                }
                seqs.push(r.window.seq);
                good += r.sites.iter().map(|s| s.cc.good).sum::<u64>();
                last_end = Some(r.window.end);
                n.delivered(&r, None);
            }
            t += 10.0037;
        }
        assert!(seqs.len() >= 5, "{seqs:?}");
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>(), "numbered on, not started over");
        // Every block counted once: the windows' sum is the total at the last one's end.
        let at_end = hist.at_or_after(n.delivered_to.unwrap().0).unwrap();
        assert_eq!(good, at_end.sites.iter().filter(|s| Select::system("dcfd").site_in(s)).map(|s| s.good).sum::<u64>());
    }

    #[test]
    fn a_failed_report_is_carried_by_the_next_up_to_the_longest_window() {
        let mut n = Reporter::seeded(Select::system("dcfd"), 7);
        let mut hist = History::default();
        let mut t = 1000.0;
        let mut first = None;
        while t <= 3000.0 {
            let h = totals(t, (t - 1000.0) as u64 * 10, 0, 0);
            hist.push(h.clone());
            if n.due(&h) {
                let r = n.report(&hist, Value::Null, Recorder::default()).unwrap();
                match first {
                    None => {
                        first = Some(r.window.end);
                        n.delivered(&r, None);
                    }
                    // Undelivered for long: the window is the longest, and says what's missing.
                    Some(end) if t - end > MAX_WINDOW_S => {
                        assert!(r.window.gap);
                        assert!((r.window.seconds() - MAX_WINDOW_S).abs() <= 10.0, "{:?}", r.window);
                        break;
                    }
                    Some(end) => {
                        assert_eq!(r.window.start, end, "carried from the last delivered");
                        assert!(!r.window.gap);
                        n.failed(&h, Failure::Unreachable);
                    }
                }
            }
            t += 10.0;
        }
        assert!(t <= 3000.0, "reached the longest window");
    }

    #[test]
    fn a_busy_service_is_backed_off_and_a_refusal_stops_the_node() {
        let mut n = Reporter::seeded(Select::system("dcfd"), 3);
        let h = totals(5000.0, 0, 0, 0);
        n.due(&h);
        n.failed(&h, Failure::Busy(Some(120.0)));
        assert_eq!(n.next_at(), Some(5120.0));
        n.failed(&h, Failure::Busy(None));
        assert!(n.next_at().unwrap() - 5000.0 <= 2.0 * INTERVAL_S + JITTER_S);
        n.failed(&h, Failure::Refused);
        assert!(!n.due(&totals(99_999.0, 0, 0, 0)));
    }

    #[test]
    fn the_service_can_set_the_interval() {
        let mut n = Reporter::seeded(Select::system("dcfd"), 9);
        let h = totals(2000.0, 0, 0, 0);
        n.due(&h);
        let mut hist = History::default();
        hist.push(h);
        let r = n.report(&hist, Value::Null, Recorder::default()).unwrap();
        n.delivered(&r, Some(1200.0));
        let next = n.next_at().unwrap();
        assert!((next - 2000.0 - 1200.0).abs() <= JITTER_S + 1.0, "{next}");
        n.delivered(&r, Some(5.0));
        assert!(n.next_at().unwrap() - 2000.0 >= MIN_INTERVAL_S - JITTER_S);
    }

    #[test]
    fn snake_case_reasons_become_camel_case() {
        assert_eq!(camel("no_recorder"), "noRecorder");
        assert_eq!(camel("encrypted"), "encrypted");
    }
}
