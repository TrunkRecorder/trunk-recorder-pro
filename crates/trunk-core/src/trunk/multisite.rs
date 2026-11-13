//! Multi-site: a call heard on several sites of one system is saved once.
//!
//! The config says which sites are one system ([`super::engine::SystemConfig::sites`]).
//! Copies of a call are the same talkgroup (or the same radio called, unit
//! to unit) granted on different sites of a system within [`TWIN_WINDOW_S`]
//! of each other.
//!
//! Unlike Trunk Recorder (record the first grant, ignore the rest), every
//! copy is recorded and the best kept once the last one ends — so a site
//! that fades mid-call, or whose voice channel falls outside every source,
//! doesn't cost the call. Best: the most cleanly decoded audio; a site the
//! talkgroup prefers (its row's, else its system's) wins while its copy holds
//! [`PREFERRED_SHARE`] of that.
//!
//! Live audio comes from one site at a time, for each talkgroup of a system
//! ([`MultiSite::audio_lead`]), however the sites happen to divide it into
//! calls: one may keep a channel through a conversation while another gives
//! each transmission its own.
//!
//! However many copies there are, it's one call to the world: [`MultiSite`]
//! tells its start, changes and end, and nothing else does.
//!
//! A copy is dropped only when one saved holds all of it ([`Kept::holds`]);
//! anything else is saved too: better a duplicate than a transmission lost.
//! With conversation mode off ([`super::calls::CallConfig::conversation`]) a
//! call is one transmission, so that's decided transmission by transmission:
//! sites that keep a channel through a conversation and sites that give
//! each transmission its own are matched alike.

use std::collections::HashMap;

use super::calls::{Call, CallId};
use super::engine::Event;
use super::recorded::Recorded;


/// Grants for one call reach each site's control channel at nearly the same time.
pub const TWIN_WINDOW_S: f64 = 3.0;
/// A preferred site's copy is kept while it has this share of the best copy's clean audio.
pub const PREFERRED_SHARE: f64 = 0.9;
/// Live audio stays with one site until it has been quiet this long.
const LEAD_HOLD_S: f64 = 0.5;

/// What live audio is one site at a time for: a system's talkgroup, or the radio called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LiveKey {
    system: u16,
    talkgroup: u32,
    unit_to_unit: bool,
}

impl LiveKey {
    pub fn of(c: &Call) -> LiveKey {
        LiveKey { system: c.system, talkgroup: c.talkgroup, unit_to_unit: c.unit_to_unit }
    }
}

/// The site a talkgroup's live audio comes from, and when it last gave some.
#[derive(Clone, Copy, Debug)]
struct Lead {
    site: u16,
    last_s: f64,
}

impl Lead {
    /// Is it `site`'s to give at `now_s`: its own, or free, the lead having gone quiet?
    fn lets(self, site: u16, now_s: f64) -> bool {
        self.site == site || now_s - self.last_s > LEAD_HOLD_S
    }
}

/// When a copy was heard: from its grant to its last audio or update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start_s: f64,
    pub end_s: f64,
}

impl Span {
    pub fn of(c: &Call) -> Span {
        Span { start_s: c.start_s, end_s: c.last_update_s.max(c.last_audio_s).max(c.start_s) }
    }

    /// Begun at nearly the same time: grants reach each site at once.
    pub fn starts_with(self, o: Span) -> bool {
        (self.start_s - o.start_s).abs() <= TWIN_WINDOW_S
    }

    /// Takes in all of `o` (give or take [`TWIN_WINDOW_S`] at either end).
    pub fn holds(self, o: Span) -> bool {
        o.start_s >= self.start_s - TWIN_WINDOW_S && o.end_s <= self.end_s + TWIN_WINDOW_S
    }
}

/// A copy saved, as the others are weighed against it.
pub struct Kept {
    pub call: Call,
    clean_s: f64,
}

impl Kept {
    pub fn of(h: &Held) -> Kept {
        Kept { call: h.call.clone(), clean_s: h.clean_s() }
    }

    /// `h` is a duplicate of this: another site's copy of the same time, the
    /// same talkers (those known on both), decoded no better (but for
    /// [`PREFERRED_SHARE`], as [`pick`] has it). A copy of what this missed
    /// (a transmission it didn't have, a talker it didn't hear, a part it
    /// lost to a fade) isn't.
    pub fn holds(&self, h: &Held) -> bool {
        let talkers = |c: &Call| c.sources.iter().map(|s| s.src).filter(|&s| s != 0).collect::<Vec<_>>();
        let (mine, theirs) = (talkers(&self.call), talkers(&h.call));
        self.call.site != h.call.site
            && Span::of(&self.call).holds(Span::of(&h.call))
            && (mine.is_empty() || theirs.iter().all(|t| mine.contains(t)))
            && self.clean_s >= h.clean_s() * PREFERRED_SHARE
    }
}

/// A copy's recording, held until its twins end.
#[derive(Clone, Debug)]
pub struct Held {
    pub call: Call,
    pub rec: Recorded,
    /// Where it was recorded.
    pub on: super::record::RecordedOn,
}

impl Held {
    /// Leave out what the saved call won't have ([`Recorded::trim`]).
    pub fn trim(&mut self, min_tx_s: f64) {
        self.rec.trim(&mut self.call.sources, min_tx_s);
    }

    /// Seconds of audio decoded cleanly ([`Recorded::clean_s`]).
    pub fn clean_s(&self) -> f64 {
        self.rec.clean_s()
    }
}

/// A copy of a call: as last heard, and whether it's still going.
#[derive(Clone, Debug)]
struct Member {
    call: Call,
    open: bool,
}

/// One call, however many sites it's heard on.
struct Conversation {
    /// What the world knows it by: its first copy's id.
    id: CallId,
    /// Every copy, ended ones too, the first first.
    members: Vec<Member>,
    held: Vec<Held>,
    /// What's been told of it.
    told: Call,
}

/// The same news, whenever it was heard.
fn same_news(a: &Call, b: &Call) -> bool {
    let bare = |c: &Call| Call { last_update_s: 0.0, heard_s: 0.0, last_audio_s: 0.0, ..c.clone() };
    bare(a) == bare(b)
}

impl Conversation {
    fn new(c: &Call) -> Conversation {
        Conversation { id: c.id, members: vec![Member { call: c.clone(), open: true }], held: Vec::new(), told: c.clone() }
    }

    /// `c`, just begun, is a copy of this: another site's, of the same
    /// talkgroup (or radio called), begun at nearly the same time as a copy
    /// still going. One copy from each site, so that a talkgroup's copies
    /// never chain one into the next.
    fn takes(&self, c: &Call) -> bool {
        self.members.iter().all(|m| m.call.site != c.site)
            && self.members.iter().any(|m| m.open && m.call.system == c.system && m.call.talkgroup == c.talkgroup && m.call.unit_to_unit == c.unit_to_unit && Span::of(&m.call).starts_with(Span::of(c)))
    }

    /// The copy that speaks for the call: the first still going that's
    /// recorded, else the first still going, else the first recorded.
    fn teller(&self) -> &Call {
        let recorded = |m: &&Member| m.call.recording;
        let going = || self.members.iter().filter(|m| m.open);
        let m = going().find(recorded).or_else(|| going().next()).or_else(|| self.members.iter().find(recorded)).unwrap_or(&self.members[0]);
        &m.call
    }

    /// The call as the world is told of it: its teller's, from the first
    /// copy's start to the last one's end, with what any copy heard.
    fn view(&self) -> Call {
        let mut v = self.teller().clone();
        v.id = self.id;
        for c in self.members.iter().map(|m| &m.call) {
            v.start_s = v.start_s.min(c.start_s);
            v.last_update_s = v.last_update_s.max(c.last_update_s);
            v.heard_s = v.heard_s.max(c.heard_s);
            v.last_audio_s = v.last_audio_s.max(c.last_audio_s);
            v.recording |= c.recording;
            v.encrypted |= c.encrypted;
            v.decrypted |= c.decrypted;
            v.emergency |= c.emergency;
            for s in &c.sources {
                if !v.sources.iter().any(|o| o.src == s.src) {
                    v.sources.push(s.clone());
                }
            }
        }
        if v.recording {
            v.reason = None;
        }
        v.sources.sort_by(|a, b| a.time_s.total_cmp(&b.time_s));
        v
    }

    /// What the world hasn't been told yet, if anything.
    fn news(&mut self) -> Option<Call> {
        let v = self.view();
        (!same_news(&v, &self.told)).then(|| {
            self.told = v.clone();
            v
        })
    }
}

/// Every call in progress and what's known of its copies. Each call goes
/// through here, and only here is it told: [`Event::CallStart`] when its
/// first copy begins, [`Event::CallEnd`] when its last one ends, under the
/// first copy's id. A site's own copies are [`Event::CopyStart`] and
/// [`Event::CopyEnd`].
#[derive(Default)]
pub struct MultiSite {
    /// By the call's id.
    convs: HashMap<CallId, Conversation>,
    /// A copy's call.
    of: HashMap<CallId, CallId>,
    leads: HashMap<LiveKey, Lead>,
}

impl MultiSite {
    /// Copy `c` began: a call of its own, or (`link`: a multi-site call may
    /// be saved once) another site's copy of one begun with it. What that tells.
    pub fn start(&mut self, c: &Call, link: bool) -> Option<Event> {
        let of = self.convs.values().filter(|v| link && v.takes(c)).map(|v| v.id).min();
        let Some(id) = of else {
            self.of.insert(c.id, c.id);
            let v = Conversation::new(c);
            let told = v.told.clone();
            self.convs.insert(c.id, v);
            return Some(Event::CallStart(told));
        };
        self.of.insert(c.id, id);
        let v = self.convs.get_mut(&id)?;
        v.members.push(Member { call: c.clone(), open: true });
        v.news().map(Event::CallUpdate)
    }

    /// Copy `c` changed. What that tells.
    pub fn update(&mut self, c: &Call) -> Option<Event> {
        let v = self.convs.get_mut(self.of.get(&c.id)?)?;
        v.members.iter_mut().find(|m| m.call.id == c.id)?.call = c.clone();
        v.news().map(Event::CallUpdate)
    }

    /// The other copies of `call` (ended ones too, while it's going): (call, site).
    pub fn twins(&self, call: CallId) -> Vec<(CallId, u16)> {
        self.of.get(&call).and_then(|i| self.convs.get(i)).map_or_else(Vec::new, |v| v.members.iter().filter(|m| m.call.id != call).filter_map(|m| Some((m.call.id, m.call.site?))).collect())
    }

    /// The calls in progress, one each, as told. `live`: their copies as
    /// they are now (a grant repeated is news to nobody, but it's still air time).
    pub fn calls(&self, live: &[Call]) -> Vec<Call> {
        let of = |v: &Conversation, c: &Call| self.of.get(&c.id) == Some(&v.id);
        self.convs.values().map(|v| Call { last_update_s: live.iter().filter(|c| of(v, c)).fold(v.view().last_update_s, |t, c| t.max(c.last_update_s)), ..v.view() }).collect()
    }

    /// Whether `site` is where `key`'s live audio comes from at `now_s` (or nowhere is).
    pub fn leads(&self, key: LiveKey, site: u16, now_s: f64) -> bool {
        self.leads.get(&key).is_none_or(|l| l.lets(site, now_s))
    }

    /// Should `site`'s live audio for `key` at `now_s` be passed on? One
    /// site at a time: the one already playing, until it has been quiet a moment.
    pub fn audio_lead(&mut self, key: LiveKey, site: u16, now_s: f64) -> bool {
        let take = self.leads(key, site, now_s);
        if take {
            self.leads.insert(key, Lead { site, last_s: now_s });
        }
        take
    }

    /// Copy `c` ended, with its recording (None: it wasn't recorded). The
    /// recordings, once the last copy has, and what's told.
    fn end(&mut self, c: &Call, held: Option<Held>) -> (Vec<Held>, Option<Event>) {
        let Some(&id) = self.of.get(&c.id) else { return (held.into_iter().collect(), None) };
        let Some(v) = self.convs.get_mut(&id) else { return (held.into_iter().collect(), None) };
        if let Some(m) = v.members.iter_mut().find(|m| m.call.id == c.id) {
            m.call = c.clone();
            m.open = false;
        }
        v.held.extend(held);
        if v.members.iter().any(|m| m.open) {
            return (Vec::new(), v.news().map(Event::CallUpdate));
        }
        let Some(v) = self.convs.remove(&id) else { return (Vec::new(), None) };
        for m in &v.members {
            self.of.remove(&m.call.id);
        }
        let told = v.view();
        (v.held, Some(Event::CallEnd(told)))
    }

    /// Copy `c` ended (`held`: its recording, None when it wasn't
    /// recorded). When it was the last: all of them, the one to save first
    /// ([`pick`]), then the others, best first — the next to try when one
    /// isn't kept (too short for its site's rules) — and what's told.
    /// `prefers(call)`: whether the copy's site is the one its talkgroup is
    /// wanted from.
    pub fn conclude(&mut self, c: &Call, held: Option<Held>, prefers: impl Fn(&Call) -> bool) -> (Vec<Held>, Option<Event>) {
        let (mut copies, told) = self.end(c, held);
        let preferred: Vec<bool> = copies.iter().map(|h| prefers(&h.call)).collect();
        let Some(k) = pick(&copies, &preferred) else { return (copies, told) };
        let first = copies.remove(k);
        copies.sort_by(|a, b| b.clean_s().total_cmp(&a.clean_s()));
        copies.insert(0, first);
        (copies, told)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.convs.is_empty() && self.of.is_empty()
    }
}

/// Which copy to keep: a preferred one (`preferred[i]`) holding
/// [`PREFERRED_SHARE`] of the best clean audio, else the most clean audio;
/// ties to the fewest FEC errors, then the earliest start.
pub fn pick(copies: &[Held], preferred: &[bool]) -> Option<usize> {
    let best = copies.iter().map(Held::clean_s).fold(0.0, f64::max);
    let rank = |i: &usize| {
        let h = &copies[*i];
        (h.clean_s(), std::cmp::Reverse(h.rec.frames.errors.total_errors()), -h.call.start_s)
    };
    let cmp = |a: &usize, b: &usize| rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal);
    let pref = (0..copies.len()).filter(|&i| preferred.get(i) == Some(&true) && copies[i].clean_s() >= best * PREFERRED_SHARE).max_by(cmp);
    pref.or_else(|| (0..copies.len()).max_by(cmp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::calls::CallSource;
    use crate::mbe::{Kind, FRAME_SAMPLES, SAMPLE_RATE};
    use crate::trunk::frames::{Codec, VoiceFrame};
    use crate::trunk::inband::InBandSpec;

    fn call(id: CallId, site: u16, start_s: f64) -> Call {
        Call { id, site: Some(site), start_s, ..Default::default() }
    }

    /// `good` clean frames then `bad` repeated ones, `errs` bit errors each.
    fn held(id: CallId, site: u16, good: usize, bad: usize, errs: u32) -> Held {
        let mut rec = Recorded::new(false, InBandSpec::default());
        for k in 0..good + bad {
            let kind = if k < good { Kind::Voice } else { Kind::Repeat };
            rec.frames.push(&VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs, erased: false, kind, tone: None });
        }
        rec.audio = vec![0.1; (good + bad) * FRAME_SAMPLES];
        Held { call: call(id, site, id as f64 * 0.1), rec, on: Default::default() }
    }

    /// Copy `c` begins, linked.
    fn begin(m: &mut MultiSite, c: Call) -> Option<Event> {
        m.start(&c, true)
    }

    #[test]
    fn copies_are_the_same_talkgroup_granted_close_together() {
        let tg = |id, site, talkgroup, start_s| Call { talkgroup, ..call(id, site, start_s) };
        let mut m = MultiSite::default();
        for c in [tg(1, 0, 101, 8.0), tg(2, 0, 102, 10.0), tg(3, 0, 101, 5.0)] {
            begin(&mut m, c);
        }
        begin(&mut m, tg(5, 1, 101, 10.0));
        assert_eq!(m.twins(5), [(1, 0)], "another talkgroup, or granted too long before, isn't a copy");
    }

    /// One copy from each site: a talkgroup's next transmission on a site
    /// isn't drawn into the last one's conversation through another site's
    /// copy begun in between (copies never chain one into the next).
    #[test]
    fn a_conversation_has_one_copy_from_each_site() {
        let tg = |id, site, start_s| Call { talkgroup: 145, ..call(id, site, start_s) };
        let mut m = MultiSite::default();
        begin(&mut m, tg(1, 0, 0.0));
        begin(&mut m, tg(2, 1, 0.2));
        // Site 1's next transmission, 1.5 s on: site 0's copy already has a site 1 copy.
        begin(&mut m, tg(3, 1, 1.5));
        assert!(m.twins(3).is_empty());
        // Site 0's next, 2.5 s on, goes with it, not with the first.
        begin(&mut m, tg(4, 0, 2.5));
        assert_eq!((m.twins(4), m.twins(1)), (vec![(3, 1)], vec![(2, 1)]));
    }

    /// A copy is a duplicate of one saved only when that holds all of it:
    /// anything it missed is saved too.
    #[test]
    fn a_copy_is_a_duplicate_only_of_one_that_holds_it() {
        let copy = |id, site: u16, start_s: f64, until_s: f64, talkers: &[u32], good: usize| {
            let mut h = held(id, site, good, 0, 0);
            h.call = Call { start_s, last_audio_s: until_s, sources: talkers.iter().map(|&src| CallSource { src, ..Default::default() }).collect(), ..h.call };
            h
        };
        let kept = Kept::of(&copy(1, 0, 0.0, 5.0, &[7], 100));
        assert!(kept.holds(&copy(2, 1, 0.3, 5.1, &[7], 100)), "the same transmission, decoded as well");
        assert!(kept.holds(&copy(3, 1, 0.3, 5.1, &[], 100)), "no talker known on it");
        assert!(!kept.holds(&copy(4, 0, 0.3, 5.1, &[7], 100)), "the same site: another transmission");
        assert!(!kept.holds(&copy(5, 1, 0.3, 12.0, &[7], 100)), "it went on after this one ended");
        assert!(!kept.holds(&copy(6, 1, 0.3, 5.1, &[9], 100)), "another talker");
        assert!(!kept.holds(&copy(7, 1, 0.3, 5.1, &[7], 120)), "it decoded more: a part this lost");
    }

    #[test]
    fn a_call_to_a_radio_is_no_copy_of_a_talkgroup_call_of_its_number() {
        let tg = |id, site, u2u| Call { talkgroup: 1234, unit_to_unit: u2u, ..call(id, site, 0.0) };
        let mut m = MultiSite::default();
        begin(&mut m, tg(1, 0, false));
        begin(&mut m, tg(2, 1, true));
        assert!(m.twins(2).is_empty());
        begin(&mut m, tg(3, 0, true));
        assert_eq!(m.twins(3), [(2, 1)], "the same radio called on two sites is one call");
    }

    #[test]
    fn the_last_copy_to_end_hands_over_every_recording_and_ends_the_call() {
        let mut m = MultiSite::default();
        let (c1, c2, c3) = (call(1, 0, 0.0), call(2, 1, 0.1), call(3, 2, 0.2));
        assert!(matches!(begin(&mut m, c1.clone()), Some(Event::CallStart(c)) if c.id == 1));
        assert!(begin(&mut m, c2.clone()).is_none(), "the same call");
        begin(&mut m, c3.clone());
        assert_eq!(m.twins(1), vec![(2, 1), (3, 2)]);
        let none = |_: &Call| false;
        let (v, told) = m.conclude(&c1, Some(held(1, 0, 10, 0, 0)), none);
        assert!(v.is_empty() && !matches!(told, Some(Event::CallEnd(_))), "two copies go on");
        let (v, told) = m.conclude(&c3, None, none);
        assert!(v.is_empty() && !matches!(told, Some(Event::CallEnd(_))), "one goes on");
        let (v, told) = m.conclude(&c2, Some(held(2, 1, 12, 0, 0)), none);
        assert_eq!(v.iter().map(|h| h.call.id).collect::<Vec<_>>(), [2, 1]);
        assert!(matches!(told, Some(Event::CallEnd(c)) if c.id == 1), "by the id it was told by");
        assert!(m.is_empty());
    }

    /// The call's told by a copy being recorded, wherever that is, from
    /// the first copy's start to the last one's end, with what any copy heard.
    #[test]
    fn a_call_is_told_as_its_copies_together_have_it() {
        let src = |src, time_s| CallSource { src, time_s, ..Default::default() };
        let mut m = MultiSite::default();
        let first = Call { reason: Some(crate::trunk::calls::Reason::NoSource), sources: vec![src(7, 0.0)], ..call(1, 0, 0.0) };
        begin(&mut m, first.clone());
        let second = Call { recording: true, freq_hz: 851_000_000, sources: vec![src(9, 0.5)], ..call(2, 1, 0.4) };
        let Some(Event::CallUpdate(c)) = begin(&mut m, second.clone()) else { panic!("it's being recorded after all") };
        assert_eq!((c.id, c.recording, c.reason, c.site, c.freq_hz, c.start_s), (1, true, None, Some(1), 851_000_000, 0.0));
        assert_eq!(c.sources.iter().map(|s| s.src).collect::<Vec<_>>(), [7, 9]);
        assert!(m.update(&Call { last_update_s: 3.0, ..second.clone() }).is_none(), "time passing isn't news");
        let none = |_: &Call| false;
        m.conclude(&first, None, none);
        let (_, told) = m.conclude(&Call { last_update_s: 9.0, ..second }, None, none);
        assert!(matches!(told, Some(Event::CallEnd(c)) if c.id == 1 && c.recording && c.last_update_s == 9.0));
    }

    fn key(system: u16, talkgroup: u32) -> LiveKey {
        LiveKey { system, talkgroup, unit_to_unit: false }
    }

    #[test]
    fn live_audio_stays_with_one_site_until_it_goes_quiet() {
        let mut m = MultiSite::default();
        let tg = key(0, 101);
        assert!(m.audio_lead(tg, 0, 0.0));
        assert!(!m.audio_lead(tg, 1, 0.1));
        assert!(m.audio_lead(tg, 0, 0.3));
        assert!(m.audio_lead(tg, 1, 0.9), "site 0 went quiet");
        assert!(!m.audio_lead(tg, 0, 1.0));
        assert!(m.audio_lead(key(0, 102), 0, 1.0), "another talkgroup");
        assert!(m.audio_lead(key(1, 101), 0, 1.0), "another system");
        assert!(m.audio_lead(LiveKey { unit_to_unit: true, ..tg }, 0, 1.0), "a call to a radio of that number");
    }

    /// Issue 151: one site keeps one long call while another ends its call and starts a new one.
    /// Nothing links the second site's new call to the long one, and only one site is still heard.
    #[test]
    fn live_audio_is_one_site_however_the_sites_divide_it_into_calls() {
        let mut m = MultiSite::default();
        let tg = key(0, 101);
        begin(&mut m, Call { talkgroup: 101, ..call(1, 0, 0.0) });
        begin(&mut m, Call { talkgroup: 101, ..call(2, 1, 20.0) });
        assert!(m.twins(2).is_empty(), "begun too far apart to be copies");
        assert!(m.audio_lead(tg, 0, 20.0));
        assert!(!m.audio_lead(tg, 1, 20.1), "the same air, from another site");
        assert!(m.leads(tg, 0, 20.2) && !m.leads(tg, 1, 20.2));
        assert!(m.leads(tg, 1, 21.0), "free once it's quiet");
    }

    #[test]
    fn keeps_the_cleanest_copy_unless_a_preferred_one_is_nearly_as_good() {
        let copies = [held(1, 0, 100, 20, 0), held(2, 1, 110, 0, 5), held(3, 2, 110, 0, 1)];
        assert_eq!(pick(&copies, &[false; 3]), Some(2), "ties go to fewer FEC errors");
        assert_eq!(pick(&copies, &[true, false, false]), Some(0), "100 of 110 clean is enough");
        let copies = [held(1, 0, 50, 70, 0), held(2, 1, 110, 0, 0)];
        assert_eq!(pick(&copies, &[true, false]), Some(1), "a preferred site that faded loses");
        let mut silent = held(1, 0, 0, 0, 0);
        silent.call.encrypted = true;
        assert_eq!(pick(&[silent.clone(), held(2, 0, 0, 0, 0)], &[false, false]), Some(0), "nothing to choose by: the first to start");
        assert_eq!(pick(&[], &[]), None);
    }

    #[test]
    fn copies_are_ranked_by_the_audio_they_keep() {
        let frame_s = FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
        // The good frames are in a transmission left out (encrypted, no key).
        let mut h = held(1, 0, 0, 0, 0);
        for k in 0..100 {
            let kind = if k < 50 { Kind::Repeat } else { Kind::Voice };
            h.rec.frames.push(&VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind, tone: None });
        }
        h.rec.audio = vec![0.1; 100 * FRAME_SAMPLES];
        h.rec.tx.note(0, 0.0);
        h.rec.tx.note(50 * FRAME_SAMPLES, 5.0);
        h.rec.tx.mark_encrypted(5.0);
        h.trim(0.0);
        assert_eq!(h.clean_s(), 0.0);
        // Decrypted from its second transmission only: half of it, which loses to a whole clear copy even preferred.
        let mut late = held(1, 0, 100, 0, 0);
        late.rec.tx.note(0, 0.0);
        late.rec.tx.mark_encrypted(0.0);
        late.rec.tx.note(50 * FRAME_SAMPLES, 5.0);
        late.rec.tx.mark_encrypted(5.0);
        late.rec.tx.mark_decrypted(5.0);
        late.trim(0.0);
        assert!((late.clean_s() - 50.0 * frame_s).abs() < 1e-9);
        assert_eq!(pick(&[late.clone(), held(2, 1, 100, 0, 0)], &[true, false]), Some(1));
        // A burst under the site's minimum isn't counted.
        let mut burst = held(1, 0, 10, 50, 0);
        burst.rec.tx.note(0, 0.0);
        burst.rec.tx.note(10 * FRAME_SAMPLES, 5.0);
        burst.trim(20.0 * frame_s);
        assert_eq!(burst.clean_s(), 0.0);
        assert_eq!(burst.rec.audio.len(), 50 * FRAME_SAMPLES);
        // Trimmed once: saving it again changes nothing.
        let before = burst.rec.audio.len();
        burst.trim(20.0 * frame_s);
        assert_eq!(burst.rec.audio.len(), before);
    }

    #[test]
    fn sources_are_placed_by_where_they_are_in_the_audio() {
        use crate::trunk::calls::CallSource;
        // Three transmissions with dead air between: 2 s at 0 s, a 0.4 s key-up at 10 s, 3 s at 20 s.
        let mut h = held(1, 0, 0, 0, 0);
        for (t0, secs) in [(0.0, 2.0), (10.0, 0.4), (20.0, 3.0)] {
            let mut t = t0;
            let end = h.rec.audio.len() + (secs * SAMPLE_RATE as f64) as usize;
            while h.rec.audio.len() < end {
                h.rec.tx.note(h.rec.audio.len(), t);
                h.rec.audio.extend(std::iter::repeat_n(0.1, FRAME_SAMPLES * 9));
                t += 0.18;
            }
        }
        // Granted at -0.5 s; one source heard mid-way through the first, one at the key-up, one in the gap before the last.
        let src = |src, time_s| CallSource { src, time_s, ..Default::default() };
        h.call.start_s = -0.5;
        h.call.sources = vec![src(7, -0.5), src(8, 1.0), src(9, 10.0), src(10, 15.0)];
        let mut all = h.clone();
        all.trim(0.0);
        let at: Vec<_> = all.call.sources.iter().map(|s| s.audio_pos.unwrap()).collect();
        // (Audio comes 9 frames, 1440 samples, at a time: the first transmission is 12 of those, the key-up 3.)
        let chunk = 9 * FRAME_SAMPLES;
        assert_eq!(at, [0, 6 * chunk, 12 * chunk, 15 * chunk], "the key-up starts where the first transmission ends, not 10.5 s in");
        // The key-up left out: who keyed it starts where the next kept audio does.
        h.trim(1.0);
        let at: Vec<_> = h.call.sources.iter().map(|s| s.audio_pos.unwrap()).collect();
        assert_eq!(at, [0, 6 * chunk, 12 * chunk, 12 * chunk]);
        assert_eq!(h.rec.audio.len(), 12 * chunk + 17 * chunk);
    }
}
