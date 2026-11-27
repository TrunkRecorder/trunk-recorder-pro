//! An LTR site, watched whole: every repeater listed, each with a
//! [`Receiver`] of its own, and what their words say becomes [`Message`]s.
//!
//! ```text
//! each repeater: Receiver → words (each read by several slicers: once)
//!   → a word heard again within REPEAT_S (status words repeat while a
//!     repeater is up; a 7-bit checksum alone lets noise through)
//!   → Call  { LCN, talkgroup }  → Grant / Update on that LCN's frequency
//!     End / Idle                → the call there is over
//!     Idle  { LCN }             → this repeater is that LCN (learned)
//!     LTR-Net: unique IDs (Registration), site ID, neighbours
//!     Passport: the radio talking (MIN), the site
//! ```
//!
//! A word names the LCN its call is on, which needn't be the repeater it is
//! heard on (a home repeater tells its idle radios where their group went).
//! Each repeater's LCN comes from the config's table (`lcnTableHz`), or is
//! learned: the LCN its words name most — an idle word names its own
//! repeater, so it counts for more.
//!
//! LTR and Passport words start with the same sync, so a Passport word can
//! pass as an LTR word now and then (and the other way): once one kind is
//! plainly the site's, the other is dropped.

use std::collections::{BTreeMap, BTreeSet};

use num_complex::Complex32;

use super::{ltr_label, Flavor, Receiver, Status, Word};
use crate::trunk::engine::NoteLevel;
use crate::trunk::identity::{IdField, Identity};
use crate::trunk::message::{ChannelEntry, ChannelTable, LastCall, Message, MessageType};
use crate::trunk::Voice;

/// One word read by several slicers ends within this of the others, s.
const SAME_WORD_S: f64 = 0.02;
/// A word is acted on when heard again within this, s.
const REPEAT_S: f64 = 1.0;
/// A call on an LCN is refreshed (an Update) at most this often, s.
const CALL_REFRESH_S: f64 = 1.0;
/// A repeater counts as heard this long after its last word, s.
const HEARD_HOLD_S: f64 = 2.0;
/// Votes an LCN needs to be a repeater's, and how far ahead of any other.
const LEARN_VOTES: f32 = 3.0;
/// An idle word's vote (it names its own repeater), and a call's.
const IDLE_VOTE: f32 = 3.0;
const CALL_VOTE: f32 = 1.0;
/// Words of one kind (LTR or Passport) that make it the site's, and how many times the other's.
const KIND_WORDS: u32 = 5;
const KIND_LEAD: u32 = 4;

#[derive(Clone, Debug, Default)]
pub struct LtrConfig {
    /// LCN → frequency (the site's channel list); wins over what is learned.
    pub lcn_table: BTreeMap<u32, u64>,
}

/// An LTR site, for the dashboard ([`Site::status`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteStatus {
    pub flavor: Option<Flavor>,
    /// LTR-Net's site ID, Passport's site number.
    pub site: Option<u32>,
    /// LCN → frequency, configured or learned.
    pub channels: Vec<ChannelEntry>,
    /// LCNs named whose frequency isn't known.
    pub unknown: Vec<u32>,
    pub carriers: Vec<CarrierStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarrierStatus {
    pub hz: u64,
    /// Its words heard lately.
    pub heard: bool,
    /// The LCN it is.
    pub lcn: Option<u32>,
    /// Its call now: (talkgroup, talker; 0 = not named).
    pub call: Option<(u32, u32)>,
}

struct Carrier {
    hz: u64,
    rx: Receiver,
    rate: f64,
    /// Words read lately, and when each was last read.
    recent: Vec<(Word, f64)>,
    /// Passport: the radio talking on it.
    talker: u32,
    heard_s: f64,
    /// Samples given it (at `rate`).
    samples: u64,
}

pub struct Site {
    carriers: Vec<Carrier>,
    table: ChannelTable,
    /// (LCN, carrier) → the words heard there naming it.
    votes: BTreeMap<(u32, usize), f32>,
    /// Each LCN's call, as last reported.
    calls: BTreeMap<u32, LastCall>,
    unknown: BTreeSet<u32>,
    /// Words read (and acted on) of each kind: LTR, Passport.
    kinds: [u32; 2],
    net: bool,
    flavor: Option<Flavor>,
    site: Option<u32>,
    good: u64,
    notes: Vec<(NoteLevel, String)>,
    now_s: f64,
    identity: Identity,
}

fn mhz(hz: u64) -> String {
    format!("{:.5} MHz", hz as f64 / 1e6)
}

impl Site {
    /// `repeaters`: every frequency of the site, Hz.
    pub fn new(repeaters: &[f64], rate: f64, cfg: LtrConfig) -> Self {
        let mut seen = BTreeSet::new();
        let carriers = repeaters
            .iter()
            .chain(cfg.lcn_table.values().map(|&f| f as f64).collect::<Vec<_>>().iter())
            .map(|f| f.round() as u64)
            .filter(|&hz| hz > 0 && seen.insert(hz))
            .map(|hz| Carrier { hz, rx: Receiver::new(rate), rate, recent: Vec::new(), talker: 0, heard_s: f64::NEG_INFINITY, samples: 0 })
            .collect();
        Site {
            carriers,
            table: ChannelTable::new(cfg.lcn_table),
            votes: BTreeMap::new(),
            calls: BTreeMap::new(),
            unknown: BTreeSet::new(),
            kinds: [0; 2],
            net: false,
            flavor: None,
            site: None,
            good: 0,
            notes: Vec::new(),
            now_s: 0.0,
            identity: Identity::default(),
        }
    }

    /// The frequencies watched, Hz.
    pub fn carriers_hz(&self) -> Vec<f64> {
        self.carriers.iter().map(|c| c.hz as f64).collect()
    }

    pub fn flavor(&self) -> Option<Flavor> {
        self.flavor
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Words decoded (each once, when acted on); none are counted lost: a repeater
    /// sends only while it is up, so there's no stream of them to miss one of.
    pub fn counts(&self) -> (u64, u64) {
        (self.good, 0)
    }

    /// The repeater heard last, while it is heard: with no control channel, the one that
    /// shows the site is alive.
    pub fn control_hz(&self) -> Option<u64> {
        self.carriers.iter().filter(|c| self.now_s - c.heard_s < HEARD_HOLD_S).max_by(|a, b| a.heard_s.total_cmp(&b.heard_s)).map(|c| c.hz)
    }

    pub fn take_notes(&mut self) -> Vec<(NoteLevel, String)> {
        std::mem::take(&mut self.notes)
    }

    /// The learned table as `lcn=hz` lines.
    pub fn map_to_string(&self) -> String {
        self.table.saved()
    }
    pub fn map_from_str(&mut self, s: &str) {
        self.table.load(s);
    }

    /// The LCN carrier `i` is, as far as known.
    fn lcn_of(&self, i: usize) -> Option<u32> {
        self.table.entries().into_iter().find(|e| e.hz == self.carriers[i].hz).map(|e| e.number)
    }

    pub fn status(&self) -> SiteStatus {
        let carriers = (0..self.carriers.len())
            .map(|i| {
                let c = &self.carriers[i];
                let lcn = self.lcn_of(i);
                CarrierStatus { hz: c.hz, heard: self.now_s - c.heard_s < HEARD_HOLD_S, lcn, call: lcn.and_then(|l| self.calls.get(&l)).and_then(|l| l.shown(self.now_s)) }
            })
            .collect();
        SiteStatus {
            flavor: self.flavor,
            site: self.site,
            channels: self.table.entries(),
            unknown: self.unknown.iter().copied().filter(|&l| self.table.resolve(l).is_none()).collect(),
            carriers,
        }
    }

    /// Carrier `i`'s channel IQ at `rate`; `t0` is the air time of the first sample it was
    /// ever given (it counts them from there).
    pub fn push(&mut self, i: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Message>) {
        let Some(c) = self.carriers.get_mut(i) else { return };
        if c.rate != rate {
            (c.rx, c.rate, c.samples) = (Receiver::new(rate), rate, 0);
        }
        let mut read = Vec::new();
        let start = t0 + c.samples as f64 / rate;
        c.rx.push(iq, start, &mut read);
        c.samples += iq.len() as u64;
        self.now_s = self.now_s.max(t0 + c.samples as f64 / rate);
        for (t, w) in read {
            let c = &mut self.carriers[i];
            c.recent.retain(|&(_, at)| t - at < REPEAT_S);
            match c.recent.iter_mut().find(|(r, _)| *r == w) {
                // Another slicer's copy of a word just read.
                Some((_, at)) if t - *at < SAME_WORD_S => {}
                // Heard again: it's real.
                Some((_, at)) => {
                    *at = t;
                    c.heard_s = t;
                    self.word(i, w, t, out);
                }
                None => c.recent.push((w, t)),
            }
        }
    }

    /// The kind of word the site sends, once plain: its words alone are taken.
    fn kind_ok(&mut self, w: &Word) -> bool {
        let k = matches!(w, Word::Passport(_)) as usize;
        let [l, p] = self.kinds;
        let plain = |a: u32, b: u32| a >= KIND_WORDS && a >= KIND_LEAD * b;
        if plain([l, p][1 - k], [l, p][k]) {
            return false;
        }
        self.kinds[k] += 1;
        self.net |= w.flavor() == Flavor::Net;
        let flavor = match self.kinds {
            [_, p] if plain(p, l) => Some(Flavor::Passport),
            [l, _] if plain(l, p) => Some(if self.net { Flavor::Net } else { Flavor::Standard }),
            _ => None,
        };
        if let Some(f) = flavor.filter(|_| flavor != self.flavor) {
            self.flavor = flavor;
            self.notes.push((NoteLevel::Info, format!("{} site", f.name())));
        }
        true
    }

    fn base(&self, kind: MessageType, t: f64) -> Message {
        Message { kind, time_s: t, voice: Voice::Fm, site: self.site.unwrap_or(0), ..Default::default() }
    }

    fn word(&mut self, i: usize, w: Word, t: f64, out: &mut Vec<Message>) {
        if !self.kind_ok(&w) {
            return;
        }
        self.good += 1;
        let label = |tg: u32| if matches!(w, Word::Ltr(_)) { ltr_label(tg) } else { tg.to_string() };
        match w.status() {
            Status::Call { lcn, tg, data } => {
                let lcn = lcn as u32;
                self.vote(lcn, i, CALL_VOTE);
                let Some(hz) = self.table.resolve(lcn) else {
                    if self.unknown.insert(lcn) {
                        self.notes.push((NoteLevel::Warning, format!("Talkgroup {} is on LCN {lcn}, whose frequency isn't known yet: not followed until it is (learned from its repeater's words, or set in the site's LCN table)", label(tg))));
                    }
                    return;
                };
                let talker = self.carriers.iter().find(|c| c.hz == hz).map_or(0, |c| c.talker);
                let Some(repeat) = self.calls.entry(lcn).or_default().heard(tg, talker, t, CALL_REFRESH_S) else { return };
                let mut m = self.base(if data { MessageType::DataGrant } else { MessageType::voice(true, repeat) }, t);
                m.freq_hz = hz;
                m.talkgroup = tg;
                m.source = if talker != 0 { talker as i64 } else { -1 };
                m.meta = format!("{} talkgroup {} on LCN {lcn} ({})", if data { "Data" } else { "Voice" }, label(tg), mhz(hz));
                out.push(m);
            }
            Status::End { tg } => {
                // The call it names, wherever it is, and this repeater's.
                let here = self.lcn_of(i);
                let now = self.now_s;
                for (&l, c) in self.calls.iter_mut() {
                    if Some(l) == here || tg.is_some_and(|tg| c.shown(now).is_some_and(|(g, _)| g == tg)) {
                        c.clear();
                    }
                }
                self.carriers[i].talker = 0;
            }
            Status::Idle { lcn, site } => {
                if lcn != 0 {
                    self.vote(lcn as u32, i, IDLE_VOTE);
                }
                if let Some(l) = self.lcn_of(i) {
                    self.calls.entry(l).or_default().clear();
                }
                self.carriers[i].talker = 0;
                if let Some(s) = site {
                    self.set_site(s);
                }
            }
            Status::Talker { unit } => self.carriers[i].talker = unit,
            Status::Registered { unit } => {
                let mut m = self.base(MessageType::Registration, t);
                m.source = if unit != 0 { unit as i64 } else { -1 };
                m.meta = if unit != 0 { format!("Registration: unique ID {unit}") } else { "Registration".into() };
                out.push(m);
            }
            Status::Site { site } => self.set_site(site),
            Status::Neighbour { rank, site } => {
                let mut m = self.base(MessageType::Adjacent, t);
                m.site = site;
                m.meta = format!("Neighbour site {site} (rank {rank})");
                out.push(m);
            }
            Status::Other => {}
        }
    }

    fn set_site(&mut self, site: u32) {
        if self.site != Some(site) {
            self.site = Some(site);
            self.identity.set(IdField::Site, site);
            self.notes.push((NoteLevel::Info, format!("Site {site}")));
        }
    }

    /// A word on carrier `i` names LCN `lcn`: once it plainly names one most, that's its LCN.
    fn vote(&mut self, lcn: u32, i: usize, weight: f32) {
        *self.votes.entry((lcn, i)).or_default() += weight;
        let mut v: Vec<(f32, u32)> = self.votes.iter().filter(|&(&(_, c), _)| c == i).map(|(&(l, _), &n)| (n, l)).collect();
        v.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let (best, l) = v[0];
        if best >= LEARN_VOTES && best >= 2.0 * v.get(1).map_or(0.0, |x| x.0) && self.table.resolve(l) != Some(self.carriers[i].hz) {
            self.learn(l, self.carriers[i].hz, "its repeater's words say");
        }
    }

    fn learn(&mut self, lcn: u32, hz: u64, why: &str) {
        if self.table.learn(lcn, hz) {
            self.notes.push((NoteLevel::Info, format!("LCN {lcn} is {} ({why})", mhz(hz))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{bits, modulate};
    use super::super::{LtrWord, PassportWord};
    use super::*;

    const RATE: f64 = 25_000.0;

    /// The site on `hz` (each a repeater) fed `secs` of air: repeater `k` sends `words[k]` (None: down).
    fn run(site: &mut Site, words: &[Option<Vec<Word>>], secs: f64, out: &mut Vec<Message>) {
        let chunk = 0.02;
        let mut ph = vec![0.0; words.len()];
        let mut t = 0.0;
        while t < secs {
            for (k, w) in words.iter().enumerate() {
                let b = w.as_ref().map(|w| bits(w));
                let iq = modulate(b.as_deref(), chunk, RATE, 1000.0, 0.0, &mut ph[k], t);
                site.push(k, &iq, 0.0, RATE, out);
            }
            t += chunk;
        }
    }

    fn ltr(channel: u8, home: u8, group: u8, free: u8) -> Word {
        Word::Ltr(LtrWord { area: false, channel, home, group, free })
    }

    /// A call on a repeater whose LCN isn't configured: its idle words name it, then its
    /// call is granted on it; its end ends it there.
    #[test]
    fn learns_each_repeaters_lcn_and_follows_its_calls() {
        let mut site = Site::new(&[851_012_500.0, 851_512_500.0], RATE, LtrConfig::default());
        let mut out = Vec::new();
        run(&mut site, &[None, Some(vec![ltr(4, 4, 255, 4)])], 1.5, &mut out);
        assert_eq!(site.table.resolve(4), Some(851_512_500), "{:?}", site.take_notes());
        // Talkgroup 0-04-012 on LCN 4, its home repeater.
        run(&mut site, &[None, Some(vec![ltr(4, 4, 12, 2)])], 2.5, &mut out);
        let grants: Vec<(MessageType, u64, u32)> = out.iter().map(|m| (m.kind, m.freq_hz, m.talkgroup)).collect();
        assert_eq!(grants.first(), Some(&(MessageType::Grant, 851_512_500, 4012)), "{grants:?}");
        assert!(grants[1..].iter().all(|g| *g == (MessageType::Update, 851_512_500, 4012)), "{grants:?}");
        assert!(site.status().carriers[1].call.is_some());
        assert_eq!(site.flavor(), Some(Flavor::Standard));
        run(&mut site, &[None, Some(vec![ltr(31, 4, 12, 4)])], 0.6, &mut out);
        assert_eq!(site.status().carriers[1].call, None, "over");
        assert_eq!(site.control_hz(), Some(851_512_500));
    }

    /// A configured LCN table stands; a call named on another repeater (a home repeater
    /// sending its group elsewhere) is granted on the LCN it names, not where it was heard.
    #[test]
    fn a_call_goes_where_its_word_says() {
        let cfg = LtrConfig { lcn_table: [(1, 851_012_500), (2, 851_512_500)].into_iter().collect() };
        let mut site = Site::new(&[], RATE, cfg);
        assert_eq!(site.carriers_hz().len(), 2);
        let mut out = Vec::new();
        run(&mut site, &[Some(vec![ltr(2, 1, 77, 0)]), None], 1.0, &mut out);
        let g = out.iter().find(|m| m.kind == MessageType::Grant).expect("a grant");
        assert_eq!((g.freq_hz, g.talkgroup), (851_512_500, 1077));
        assert_eq!(site.table.resolve(2), Some(851_512_500), "the config's");
    }

    /// LTR-Net: the site's ID and a unique ID's registration, among its idle words.
    #[test]
    fn ltr_net_says_its_site_and_unique_ids() {
        let mut site = Site::new(&[851_012_500.0], RATE, LtrConfig::default());
        let mut out = Vec::new();
        let words = vec![ltr(18, 24, 0b10, 3), ltr(17, 24, 3, 10), ltr(1, 1, 255, 1)];
        run(&mut site, &[Some(words)], 4.0, &mut out);
        let notes = site.take_notes();
        assert_eq!(site.flavor(), Some(Flavor::Net), "{notes:?}");
        assert_eq!(site.status().site, Some(0b10_00011));
        assert_eq!(site.table.resolve(1), Some(851_012_500), "{notes:?}");
        assert!(out.iter().any(|m| m.kind == MessageType::Registration && m.source == 106), "{out:?}");
    }

    /// The engine end to end: two repeaters in one band, an idle burst on LCN 2's, then a call
    /// of 1-02-033 on it: recorded as an analog call there, its voice without the data under it.
    #[test]
    fn engine_records_an_ltr_call() {
        use crate::trunk::{Engine, EngineConfig, Event, SiteConfig, SourceConfig, SystemConfig};
        use std::f64::consts::PI;
        let (fs, center) = (480_000.0, 851_262_500.0);
        let (r1, r2) = (851_162_500.0, 851_362_500.0);
        let idle = bits(&[Word::Ltr(LtrWord { area: true, channel: 2, home: 2, group: 255, free: 2 })]);
        let call = bits(&[Word::Ltr(LtrWord { area: true, channel: 2, home: 2, group: 33, free: 1 })]);
        let n = (4.0 * fs) as usize;
        let (mut ph, mut x) = (0.0f64, 9u32);
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                // Idle 0.2–1.0 s, the call 1.5–3.5 s (a 1 kHz tone for voice).
                let (b, voice) = match t {
                    _ if (0.2..1.0).contains(&t) => (Some(&idle), 0.0),
                    _ if (1.5..3.5).contains(&t) => (Some(&call), 2500.0 * (2.0 * PI * 1000.0 * t).sin()),
                    _ => (None, 0.0),
                };
                let mut u = || {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as f32 / u32::MAX as f32 - 0.5
                };
                let noise = Complex32::new(u(), u()) * 0.02;
                let Some(b) = b else { return noise };
                let d = if b[(t * 300.0) as usize % b.len()] { 1000.0 } else { -1000.0 };
                ph = (ph + 2.0 * PI * (r2 - center + voice + d) / fs) % (2.0 * PI);
                Complex32::from_polar(0.3, ph as f32) + noise
            })
            .collect();
        let site = SiteConfig { control_channels: vec![r1, r2], protocol: crate::trunk::Protocol::Ltr(LtrConfig::default()), ..Default::default() };
        let ecfg = EngineConfig {
            systems: vec![SystemConfig::trunked("ltr", vec![site])],
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ, audio: false }],
            ..Default::default()
        };
        let e = Engine::new(ecfg).unwrap();
        for chunk in iq.chunks(4096) {
            e.push_iq(0, chunk);
        }
        e.finish();
        let events = e.drain_events();
        let done: Vec<_> = events.iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some(k) } else { None }).collect();
        assert_eq!(done.len(), 1, "{:?}", events.iter().filter(|e| !matches!(e, Event::Audio { .. })).collect::<Vec<_>>());
        let k = done[0];
        assert_eq!((k.call.talkgroup, k.call.freq_hz, k.call.voice), (102_033, r2 as u64, Voice::Fm));
        let secs = k.audio.len() as f64 / 8000.0;
        assert!(secs > 1.2, "{secs:.2} s of audio");
    }

    /// Passport: calls by 16-bit group, the radio talking named.
    #[test]
    fn passport_calls_name_their_talker() {
        let cfg = LtrConfig { lcn_table: [(357, 452_462_500)].into_iter().collect() };
        let mut site = Site::new(&[], RATE, cfg);
        let mut out = Vec::new();
        let call = Word::Passport(PassportWord { dcc: 1, lcn: 357, site: 9, group: 4321, kind: 0, free: 360 });
        let who = Word::Passport(PassportWord { dcc: 1, lcn: 357, site: 0, group: 777, kind: 6, free: 360 });
        run(&mut site, &[Some(vec![who, call])], 3.0, &mut out);
        assert_eq!(site.flavor(), Some(Flavor::Passport));
        let g: Vec<&Message> = out.iter().filter(|m| m.talkgroup == 4321).collect();
        assert!(g.iter().any(|m| m.kind == MessageType::Grant && m.freq_hz == 452_462_500), "{out:?}");
        assert!(g.iter().any(|m| m.source == 777), "{out:?}");
    }
}
