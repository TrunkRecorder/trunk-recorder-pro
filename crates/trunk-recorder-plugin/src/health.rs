//! Node health reports: telling a call-collecting service how well this recorder hears what it
//! uploads (`docs/api/node-health.md`, version 1).
//!
//! The recorder sends the [`totals`](crate::topic::TOTALS) topic every 10 s: every series it
//! measures as a running total, and which sites, conventional channels and radios they're of
//! ([`Totals`]). A plugin keeps them in a [`History`]
//! and a [`Reporter`] for each node it reports on, which says when that node's next report is
//! due — at a random point in each interval, its own, so a service's nodes don't all report at
//! once — and makes the [`Report`] from the totals: what changed since its last delivered one.
//!
//! ```no_run
//! # use trunk_recorder_plugin::{health::*, Totals};
//! # fn post(_: &Report) -> Result<Answer, Failure> { unimplemented!() }
//! # let mut history = History::default();
//! # let mut node = Reporter::new(Select::system("dcfd"), "1234");
//! # let totals = Totals::default();
//! history.push(totals.clone());
//! if node.due(&totals) {
//!     if let Some(r) = node.report(&history, serde_json::json!({ "systemId": 1234 }), Recorder::new("my-recorder", "1.0")) {
//!         match post(&r) {
//!             Ok(answer) => node.delivered(&r, answer.next_s),
//!             Err(f) => node.failed(&totals, f),
//!         }
//!     }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{ChannelScope, Codec, SiteScope, SourceScope, Total, Totals};

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

/// Vocoder frames of recorded calls, of one codec.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameTotals {
    pub frames: u64,
    /// Too damaged to use.
    pub bad: u64,
    /// Bit errors FEC corrected.
    pub errors: u64,
    /// Frames the calls' lengths say should have come but never did (not in `frames`), when the
    /// recorder can tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<u64>,
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
    fn site_in(&self, s: &SiteScope) -> bool {
        s.short_name == self.short_name && self.only.as_ref().is_none_or(|o| o.contains(&s.site_name)) && !self.except.contains(&s.site_name)
    }
    fn channel_in(&self, c: &ChannelScope) -> bool {
        c.short_name == self.short_name && self.only.is_none()
    }
}

// ─── Making a report from two snapshots ──────────────────────────────────────

fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// A window's series ([`Totals::since`]) under one key.
struct Scope<'a> {
    w: &'a Totals,
    key: &'a str,
}

impl Scope<'_> {
    fn get(&self, name: &str) -> Option<&Total> {
        self.w.get(&format!("{}/{name}", self.key))
    }
    fn count(&self, name: &str) -> Option<u64> {
        self.get(name).map(Total::count)
    }
    /// A gauge's seconds-weighted sum: the seconds it was 1 (`locked`, `open`).
    fn seconds(&self, name: &str) -> f64 {
        r1(self.get(name).map_or(0.0, |t| t.sum))
    }
    fn mean(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(Total::mean).map(r1)
    }
    fn calls(&self) -> CallsReport {
        let why = format!("{}/calls/why/", self.key);
        let not_recorded = self.w.series.range(why.clone()..).take_while(|(k, _)| k.starts_with(&why)).map(|(k, t)| (k[why.len()..].to_string(), t.count())).filter(|&(_, n)| n > 0).collect();
        CallsReport { started: self.count("calls/started").unwrap_or(0), recorded: self.count("calls/recorded").unwrap_or(0), not_recorded }
    }
    fn voice(&self) -> VoiceReport {
        let codec = |c: Codec| {
            let n = |k: &str| self.count(&format!("voice/{}/{k}", c.key())).unwrap_or(0);
            Some(FrameTotals { frames: n("frames"), bad: n("bad"), errors: n("errors"), missing: None }).filter(|f| f.frames > 0)
        };
        VoiceReport { imbe: codec(Codec::Imbe), ambe: codec(Codec::Ambe) }
    }
}

/// A site's `id` as the spec names it: its [`SiteIdentity`](crate::protocol::SiteIdentity)'s own
/// names, an NXDN system's `system`.
fn site_id(s: &SiteScope) -> BTreeMap<String, u32> {
    let Ok(Value::Object(m)) = serde_json::to_value(s.identity) else { return BTreeMap::new() };
    m.into_iter()
        .filter_map(|(k, v)| Some((if k == "sysId" && s.protocol == "nxdn" { "system".to_string() } else { k }, v.as_u64()? as u32)))
        .collect()
}

fn site(w: &Totals, s: &SiteScope) -> SiteReport {
    let k = Scope { w, key: &s.key };
    let id = site_id(s);
    let air_sym = k.count("cc/airSym");
    SiteReport {
        name: s.site_name.clone(),
        protocol: s.protocol.clone(),
        receiver: None,
        id,
        cc: CcReport {
            hz: s.control_channel_hz,
            locked_s: k.seconds("cc/locked"),
            good: k.count("cc/good").unwrap_or(0),
            bad: k.count("cc/bad"),
            sync_loss_sym: k.count("cc/syncLossSym"),
            air_sym,
            symbol_rate: s.symbol_rate.filter(|_| air_sym.is_some()),
            snr_db: k.mean("cc/snr"),
            mer_db: None,
            phase_err_deg: k.mean("cc/phaseErr"),
        },
        calls: k.calls(),
        voice: k.voice(),
    }
}

/// Seconds source `src` delivered samples, and of those, seconds it lost some in.
fn source_seconds(w: &Totals, src: &SourceScope) -> (f64, f64) {
    let k = Scope { w, key: &src.key };
    (k.get("samples").map_or(0.0, |t| t.s), k.get("dropped").map_or(0.0, |t| t.s))
}

fn channel(w: &Totals, c: &ChannelScope) -> ChannelReport {
    let k = Scope { w, key: &c.key };
    let calls = k.calls();
    let skipped = k.count("calls/skipped").unwrap_or(0);
    let heard_s = w.sources.iter().find(|s| s.index == c.source).map_or(0.0, |s| {
        let (secs, dropped) = source_seconds(w, s);
        secs - dropped
    });
    ChannelReport {
        hz: c.freq_hz,
        mode: c.mode.clone(),
        name: c.names.join(", "),
        receiver: None,
        access: c.access.clone(),
        heard_s: r1(heard_s),
        noise_db: k.mean("closed"),
        opens: calls.started + skipped,
        open_s: k.seconds("open"),
        calls: calls.recorded,
        opened_by_other: Some(skipped),
        signal_db: k.mean("signal"),
        snr_db: k.mean("snr"),
        voice: k.voice(),
    }
}

fn radio(w: &Totals, s: &SourceScope) -> RadioReport {
    let k = Scope { w, key: &s.key };
    let (secs, dropped) = source_seconds(w, s);
    let r2 = |p: f64| (p * 100.0).round() / 100.0;
    RadioReport {
        name: s.name.clone(),
        kind: s.driver.clone(),
        seconds: r1(secs),
        dropped_s: r1(dropped),
        clip_pct: k.get("clipPct").and_then(Total::mean).map(|p| (p * 1000.0).round() / 1000.0),
        ppm_error: k.get("ppm").and_then(|t| t.now).map(r2),
        ppm_applied: k.get("tune").and_then(|t| t.now).filter(|_| s.auto_tune).map(r2),
    }
}

/// The report of what changed from `base` to `now` for the node `sel` names (`base` none, or
/// from before a restart of recording: from when recording started). None when nothing of the
/// recorder is that node's.
pub fn report(base: Option<&Totals>, now: &Totals, sel: &Select, node: Value, recorder: Recorder, seq: u64) -> Option<Report> {
    let base = base.filter(|b| b.since == now.since && b.time <= now.time);
    let w = now.since(base);
    let mut sites: Vec<SiteReport> = now.sites.iter().filter(|s| sel.site_in(s)).map(|s| site(&w, s)).collect();
    // Sites that say they're the same one (each its own control channel decoder, on its own
    // radio): receivers of one site, each entry named by its radio.
    let scopes: Vec<&SiteScope> = now.sites.iter().filter(|s| sel.site_in(s)).collect();
    let ids: Vec<BTreeMap<String, u32>> = sites.iter().map(|r| r.id.clone()).collect();
    for (r, sc) in sites.iter_mut().zip(&scopes) {
        if !r.id.is_empty() && ids.iter().filter(|&i| *i == r.id).count() > 1 {
            r.receiver = Some(now.sources.iter().find(|x| Some(x.index) == sc.source).map_or_else(|| sc.site_name.clone(), |x| x.name.clone()));
        }
    }
    let channels: Vec<ChannelReport> = now.channels.iter().filter(|c| sel.channel_in(c)).map(|c| channel(&w, c)).collect();
    if sites.is_empty() && channels.is_empty() {
        return None;
    }
    let used: BTreeSet<usize> = now.sites.iter().filter(|s| sel.site_in(s)).filter_map(|s| s.source).chain(now.channels.iter().filter(|c| sel.channel_in(c)).map(|c| c.source)).collect();
    let radios = now.sources.iter().filter(|s| used.contains(&s.index)).map(|s| radio(&w, s)).collect();
    let start = base.map_or(now.since, |b| b.time);
    Some(Report {
        v: VERSION,
        node,
        recorder: Recorder { up_s: Some(r1(now.time - now.since)), ..recorder },
        window: Window { start: r1(start), end: r1(now.time), seq, gap: false, r#final: false },
        sites,
        channels,
        radios,
        clock: now.clock.as_ref().filter(|c| c.sync.is_some() || c.system_offset_ms.is_some()).map(|c| Clock { sync: c.sync.clone(), system_offset_ms: c.system_offset_ms.map(f64::round) }),
    })
}

// ─── Keeping the totals, and when to send ────────────────────────────────────

/// The recorder's recent totals: enough to report a node's window after failures, up to
/// [`MAX_WINDOW_S`].
#[derive(Clone, Debug, Default)]
pub struct History {
    snaps: VecDeque<Totals>,
}

impl History {
    pub fn push(&mut self, h: Totals) {
        if self.snaps.back().is_some_and(|b| b.since != h.since) {
            self.snaps.clear();
        }
        let keep = h.time - MAX_WINDOW_S - 2.0 * INTERVAL_S;
        while self.snaps.front().is_some_and(|f| f.time < keep) {
            self.snaps.pop_front();
        }
        self.snaps.push_back(h);
    }

    pub fn latest(&self) -> Option<&Totals> {
        self.snaps.back()
    }

    /// The earliest totals at or after `t`.
    fn at_or_after(&self, t: f64) -> Option<&Totals> {
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
    pub fn due(&mut self, now: &Totals) -> bool {
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
    pub fn failed(&mut self, now: &Totals, why: Failure) {
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
    use crate::protocol::SiteIdentity;

    /// A counter's total.
    fn n(v: u64) -> Total {
        Total { sum: v as f64, s: 0.0, now: None }
    }
    /// A gauge held at `v` for `s` seconds.
    fn g(v: f64, s: f64) -> Total {
        Total { sum: v * s, s, now: Some(v) }
    }

    fn totals(time: f64, good: u64, frames: u64, bad: u64) -> Totals {
        let up = time - 1000.0;
        let north = "sys/dcfd/site/north";
        let series = [
            (format!("{north}/cc/good"), n(good)),
            (format!("{north}/cc/bad"), n(good / 20)),
            (format!("{north}/cc/airSym"), n(good * 100)),
            (format!("{north}/cc/locked"), g(1.0, up)),
            (format!("{north}/cc/snr"), g(15.0, up)),
            (format!("{north}/calls/started"), n(good / 10)),
            (format!("{north}/calls/recorded"), n(good / 12)),
            (format!("{north}/calls/why/noSource"), n(good / 100)),
            (format!("{north}/voice/imbe/frames"), n(frames)),
            (format!("{north}/voice/imbe/bad"), n(bad)),
            (format!("{north}/voice/imbe/errors"), n(bad * 3)),
            ("src/800/samples".to_string(), Total { sum: 1e9, s: up, now: None }),
            ("src/800/clipPct".to_string(), g(0.1, up)),
            ("src/800/ppm".to_string(), g(1.134, up)),
            ("src/800/tune".to_string(), g(1.1, up)),
            ("src/vhf/samples".to_string(), Total { sum: 1e9, s: up, now: None }),
        ];
        let site = |name: &str, key: &str| SiteScope { short_name: "dcfd".into(), site_name: name.into(), key: key.into(), protocol: "p25".into(), ..Default::default() };
        Totals {
            time,
            since: 1000.0,
            series: series.into_iter().collect(),
            sites: vec![
                SiteScope { control_channel_hz: Some(851_000_000), identity: SiteIdentity { wacn: Some(0xBEE00), sys_id: Some(0x3AB), ..Default::default() }, source: Some(0), symbol_rate: Some(4800), ..site("north", north) },
                site("south", "sys/dcfd/site/south"),
            ],
            channels: vec![ChannelScope { key: "sys/fire/ch/154430000".into(), short_name: "fire".into(), freq_hz: 154_430_000, mode: "fm".into(), source: 1, ..Default::default() }],
            sources: vec![
                SourceScope { key: "src/800".into(), index: 0, name: "800".into(), driver: "airspy".into(), auto_tune: true },
                SourceScope { key: "src/vhf".into(), index: 1, name: "vhf".into(), driver: "rtlsdr".into(), auto_tune: false },
            ],
            clock: Some(crate::protocol::ClockState { sync: Some("ntp".into()), system_offset_ms: Some(12.4) }),
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

    /// Two of the recorder's sites saying they're one (the same identity, each on its own radio)
    /// are receivers of it; an NXDN system's number is its `system`, a DMR site's colour code
    /// its `colorCode`.
    #[test]
    fn sites_saying_they_are_one_are_its_receivers_and_ids_take_the_specs_names() {
        let mut t = totals(1300.0, 1000, 0, 0);
        let same = t.sites[0].identity;
        t.sites[1].identity = same;
        t.sites[1].source = Some(1);
        let r = report(None, &t, &Select::system("dcfd"), Value::Null, Recorder::default(), 0).unwrap();
        assert_eq!(r.sites.iter().map(|s| s.receiver.as_deref()).collect::<Vec<_>>(), [Some("800"), Some("vhf")]);
        assert_eq!(r.sites[0].id, r.sites[1].id);
        // Told apart by what they say: not receivers.
        t.sites[1].identity.site = Some(9);
        let r = report(None, &t, &Select::system("dcfd"), Value::Null, Recorder::default(), 0).unwrap();
        assert!(r.sites.iter().all(|s| s.receiver.is_none()));
        let nxdn = SiteScope { protocol: "nxdn".into(), identity: SiteIdentity { sys_id: Some(123), site: Some(2), ran: Some(5), ..Default::default() }, ..Default::default() };
        assert_eq!(site_id(&nxdn), [("ran".to_string(), 5), ("site".into(), 2), ("system".into(), 123)].into());
        let dmr = SiteScope { protocol: "dmr".into(), identity: SiteIdentity { color_code: Some(1), ..Default::default() }, ..Default::default() };
        assert_eq!(site_id(&dmr), [("colorCode".to_string(), 1)].into());
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
        assert_eq!(good, at_end.get("sys/dcfd/site/north/cc/good").unwrap().count());
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
}
