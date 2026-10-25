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
//! talkgroup prefers (talkgroup file) wins while its copy holds
//! [`PREFERRED_SHARE`] of that. Live audio comes from one copy at a time.
//!
//! A copy is dropped only when one saved holds all of it ([`Kept::holds`]);
//! anything else is saved too: better a duplicate than a transmission lost.
//! With conversation mode off ([`super::calls::CallConfig::conversation`]) a
//! call is one transmission, so that's decided transmission by transmission:
//! sites that keep a channel through a conversation and sites that give
//! each transmission its own are matched alike.

use std::collections::HashMap;

use super::calls::{Call, CallId};
use super::recorded::Recorded;
use super::talkgroups::Talkgroup;


/// Grants for one call reach each site's control channel at nearly the same time.
pub const TWIN_WINDOW_S: f64 = 3.0;
/// A preferred site's copy is kept while it has this share of the best copy's clean audio.
pub const PREFERRED_SHARE: f64 = 0.9;
/// Live audio stays with one copy until it has been quiet this long.
const LEAD_HOLD_S: f64 = 0.5;

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
    /// The call JSON's `recorder_num` ([`super::record::ConcludeInfo::recorder_num`]).
    pub recorder_num: u32,
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

#[derive(Default)]
struct Conversation {
    /// Every copy: (call, its site); `open` those not ended yet.
    members: Vec<(CallId, u16)>,
    open: Vec<CallId>,
    held: Vec<Held>,
    /// The copy live audio comes from, and when it last gave some.
    lead: Option<(CallId, f64)>,
}

/// What ending a copy left to do.
#[derive(Debug)]
pub enum Ended {
    /// Not a copy of anything: save it as usual.
    Alone(Option<Held>),
    /// Its twins are still going: held.
    Waiting,
    /// The last copy ended: every recording the conversation made.
    Done(Vec<Held>),
}

#[derive(Default)]
pub struct MultiSite {
    convs: HashMap<u32, Conversation>,
    of: HashMap<CallId, u32>,
    next: u32,
}

impl MultiSite {
    /// `call` (on `site`) is a copy of `twins` (calls still going, with their
    /// sites). A conversation has one copy from each site: a twin whose
    /// conversation has one from a site already taken is another
    /// transmission's (or conversation's), left out — so a talkgroup's
    /// copies never chain one into the next.
    pub fn link(&mut self, call: CallId, site: u16, twins: &[(CallId, u16)]) {
        // The twins' conversations (or the twins themselves, in none yet) taken, each with sites not taken yet.
        let (mut sites, mut ids, mut alone) = (vec![site], Vec::<u32>::new(), Vec::<(CallId, u16)>::new());
        for &t in twins {
            let conv = self.of.get(&t.0).copied();
            if conv.is_some_and(|id| ids.contains(&id)) {
                continue;
            }
            let theirs: Vec<u16> = conv.map_or(vec![t.1], |id| self.convs[&id].members.iter().map(|m| m.1).collect());
            if theirs.iter().any(|s| sites.contains(s)) {
                continue;
            }
            sites.extend(theirs);
            match conv {
                Some(id) => ids.push(id),
                None => alone.push(t),
            }
        }
        if ids.is_empty() && alone.is_empty() {
            return;
        }
        // Join the twins' conversation(s) — merging, should they be in different ones.
        ids.sort_unstable();
        let id = match ids.first() {
            Some(&id) => id,
            None => {
                self.next += 1;
                self.convs.insert(self.next, Conversation::default());
                self.next
            }
        };
        for other in ids.iter().skip(1) {
            let c = self.convs.remove(other).unwrap_or_default();
            for m in &c.members {
                self.of.insert(m.0, id);
            }
            let conv = self.convs.get_mut(&id).unwrap();
            conv.members.extend(c.members);
            conv.open.extend(c.open);
            conv.held.extend(c.held);
        }
        let conv = self.convs.get_mut(&id).unwrap();
        for &(c, at) in alone.iter().chain([&(call, site)]) {
            if !conv.members.iter().any(|m| m.0 == c) {
                conv.members.push((c, at));
                conv.open.push(c);
                self.of.insert(c, id);
            }
        }
    }

    /// The other copies of `call` (ended ones too, while it's going): (call, site).
    pub fn twins(&self, call: CallId) -> Vec<(CallId, u16)> {
        self.of.get(&call).and_then(|i| self.convs.get(i)).map_or_else(Vec::new, |c| c.members.iter().copied().filter(|m| m.0 != call).collect())
    }

    /// Whether `call` is the copy live audio comes from now (or the only one).
    pub fn leads(&self, call: CallId) -> bool {
        self.of.get(&call).and_then(|i| self.convs.get(i)).is_none_or(|c| c.lead.is_none_or(|(lead, _)| lead == call))
    }

    /// Should `call`'s live audio at `now_s` be passed on? One copy at a
    /// time: the one already playing, until it has been quiet a moment.
    pub fn audio_lead(&mut self, call: CallId, now_s: f64) -> bool {
        let Some(c) = self.of.get(&call).and_then(|i| self.convs.get_mut(i)) else { return true };
        let take = match c.lead {
            Some((lead, last)) => lead == call || !c.open.contains(&lead) || now_s - last > LEAD_HOLD_S,
            None => true,
        };
        if take {
            c.lead = Some((call, now_s));
        }
        take
    }

    /// A copy ended, with its recording (None: it wasn't recorded).
    pub fn end(&mut self, call: CallId, held: Option<Held>) -> Ended {
        let Some(&id) = self.of.get(&call) else { return Ended::Alone(held) };
        let conv = self.convs.get_mut(&id).unwrap();
        conv.open.retain(|&c| c != call);
        conv.held.extend(held);
        if !conv.open.is_empty() {
            return Ended::Waiting;
        }
        let conv = self.convs.remove(&id).unwrap();
        for m in &conv.members {
            self.of.remove(&m.0);
        }
        Ended::Done(conv.held)
    }
}

impl MultiSite {
    /// Link call `c`, just begun on `site`, with the copies of it already
    /// going on the other sites of its system (`others`: their calls) — the
    /// same talkgroup, begun within [`TWIN_WINDOW_S`] ([`Span::starts_with`]).
    /// (A unit-to-unit call's "talkgroup" is the radio called: never a copy
    /// of a talkgroup's call.)
    pub fn link_new<'a>(&mut self, c: &Call, site: u16, others: impl IntoIterator<Item = &'a [Call]>) {
        let twins: Vec<(CallId, u16)> = others
            .into_iter()
            .flatten()
            .filter(|o| o.talkgroup == c.talkgroup && o.unit_to_unit == c.unit_to_unit && Span::of(o).starts_with(Span::of(c)))
            .filter_map(|o| Some((o.id, o.site?)))
            .collect();
        self.link(c.id, site, &twins);
    }

    /// Call `id` ended (`held`: its recording, None when it wasn't
    /// recorded). When it was the last of its copies: all of them, the one
    /// to save first ([`pick`]), then the others, best first — the next to
    /// try when one isn't kept (too short for its site's rules).
    /// `prefers(site, tg)`: whether `site` is the one the talkgroup file
    /// wants `tg` from.
    pub fn conclude(&mut self, id: CallId, held: Option<Held>, prefers: impl Fn(Option<u16>, &Talkgroup) -> bool) -> Option<Vec<Held>> {
        let mut copies = match self.end(id, held) {
            Ended::Alone(None) | Ended::Waiting => return None,
            Ended::Alone(Some(h)) => vec![h],
            Ended::Done(v) => v,
        };
        // The talkgroup's preferred site, by any site's talkgroup file.
        let prefs: Vec<&Talkgroup> = copies.iter().filter_map(|h| h.call.talkgroup_info.as_ref()).collect();
        let preferred: Vec<bool> = copies.iter().map(|h| prefs.iter().any(|tg| prefers(h.call.site, tg))).collect();
        let k = pick(&copies, &preferred)?;
        let first = copies.remove(k);
        copies.sort_by(|a, b| b.clean_s().total_cmp(&a.clean_s()));
        copies.insert(0, first);
        Some(copies)
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
            rec.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs, erased: false, kind, tone: None });
        }
        rec.audio = vec![0.1; (good + bad) * FRAME_SAMPLES];
        Held { call: call(id, site, id as f64 * 0.1), rec, recorder_num: 0 }
    }

    #[test]
    fn copies_are_the_same_talkgroup_granted_close_together() {
        let tg = |id, site, talkgroup, start_s, u2u| Call { talkgroup, unit_to_unit: u2u, ..call(id, site, start_s) };
        let mut m = MultiSite::default();
        m.link_new(&tg(5, 1, 101, 10.0, false), 1, [&[tg(1, 0, 101, 8.0, false), tg(2, 0, 102, 10.0, false), tg(3, 0, 101, 5.0, false)][..]]);
        assert_eq!(m.twins(5), [(1, 0)], "another talkgroup, or granted too long before, isn't a copy");
        // A call to a radio is no copy of a talkgroup call of its number; the same radio called on two sites is.
        m.link_new(&tg(7, 1, 1234, 0.0, true), 1, [&[tg(6, 0, 1234, 0.0, false)][..]]);
        assert!(m.twins(7).is_empty());
        m.link_new(&tg(9, 1, 1234, 0.0, true), 1, [&[tg(8, 0, 1234, 0.0, true)][..]]);
        assert_eq!(m.twins(9), [(8, 0)]);
    }

    /// One copy from each site: a talkgroup's next transmission on a site
    /// isn't drawn into the last one's conversation through another site's
    /// copy begun in between (copies never chain one into the next).
    #[test]
    fn a_conversation_has_one_copy_from_each_site() {
        let tg = |id, site, start_s| Call { talkgroup: 145, ..call(id, site, start_s) };
        let mut m = MultiSite::default();
        m.link_new(&tg(2, 1, 0.2), 1, [&[tg(1, 0, 0.0)][..]]);
        // Site 1's next transmission, 1.5 s on: site 0's copy already has a site 1 copy.
        m.link_new(&tg(3, 1, 1.5), 1, [&[tg(1, 0, 0.0)][..]]);
        assert!(m.twins(3).is_empty());
        // Site 0's next, 2.5 s on, goes with it, not with the first.
        m.link_new(&tg(4, 0, 2.5), 0, [&[tg(2, 1, 0.2), tg(3, 1, 1.5)][..]]);
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
        m.link_new(&tg(2, 1, true), 1, [&[tg(1, 0, false)][..]]);
        assert!(m.twins(2).is_empty());
        m.link_new(&tg(4, 1, true), 1, [&[tg(3, 0, true)][..]]);
        assert_eq!(m.twins(4), [(3, 0)], "the same radio called on two sites is one call");
    }

    #[test]
    fn the_last_copy_to_end_hands_over_every_recording() {
        let mut m = MultiSite::default();
        m.link(2, 1, &[(1, 0)]);
        m.link(3, 2, &[(2, 1)]);
        assert_eq!(m.twins(1), vec![(2, 1), (3, 2)]);
        assert!(matches!(m.end(1, Some(held(1, 0, 10, 0, 0))), Ended::Waiting));
        assert!(matches!(m.end(3, None), Ended::Waiting));
        let Ended::Done(v) = m.end(2, Some(held(2, 1, 12, 0, 0))) else { panic!() };
        assert_eq!(v.iter().map(|h| h.call.id).collect::<Vec<_>>(), [1, 2]);
        assert!(m.twins(2).is_empty());
        assert!(matches!(m.end(9, None), Ended::Alone(None)));
    }

    #[test]
    fn conversations_merge_when_a_copy_links_two() {
        let mut m = MultiSite::default();
        m.link(2, 1, &[(1, 0)]);
        m.link(4, 3, &[(3, 2)]);
        m.link(5, 4, &[(1, 0), (3, 2)]);
        assert_eq!(m.twins(5).len(), 4);
        for c in [1, 2, 3, 4] {
            assert!(matches!(m.end(c, None), Ended::Waiting));
        }
        assert!(matches!(m.end(5, None), Ended::Done(v) if v.is_empty()));
    }

    #[test]
    fn live_audio_stays_with_one_copy_until_it_goes_quiet() {
        let mut m = MultiSite::default();
        m.link(2, 1, &[(1, 0)]);
        assert!(m.audio_lead(1, 0.0));
        assert!(!m.audio_lead(2, 0.1));
        assert!(m.audio_lead(1, 0.3));
        assert!(m.audio_lead(2, 0.9), "1 went quiet");
        assert!(!m.audio_lead(1, 1.0));
        assert!(m.audio_lead(7, 0.0), "not a copy");
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
            h.rec.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind, tone: None });
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
