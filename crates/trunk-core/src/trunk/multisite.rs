//! Multi-site: a call heard on several sites of one system is saved once.
//!
//! Sites belong to one system when their control channels say so — P25 by
//! WACN and System ID, SmartNet by System ID — or when the config names
//! them into one group ([`super::engine::SystemConfig::site_group`]: DMR,
//! ISSI-linked systems). Copies of a call are the same talkgroup granted on
//! different sites of a group within [`TWIN_WINDOW_S`] of each other; two
//! configs following the same site are not copies of each other (as in
//! Trunk Recorder).
//!
//! Unlike Trunk Recorder (record the first grant, ignore the rest), every
//! copy is recorded and the best kept once the last one ends — so a site
//! that fades mid-call, or whose voice channel falls outside every source,
//! doesn't cost the call. Best: the most cleanly decoded audio; a site the
//! talkgroup prefers (talkgroup file) wins while its copy holds
//! [`PREFERRED_SHARE`] of that. Live audio comes from one copy at a time.

use std::collections::HashMap;

use super::calls::{Call, CallId};
use super::frames::CallFrames;
use crate::mbe::{FRAME_SAMPLES, SAMPLE_RATE};

/// Grants for one call reach each site's control channel at nearly the same time.
pub const TWIN_WINDOW_S: f64 = 3.0;
/// A preferred site's copy is kept while it has this share of the best copy's clean audio.
pub const PREFERRED_SHARE: f64 = 0.9;
/// Live audio stays with one copy until it has been quiet this long.
const LEAD_HOLD_S: f64 = 0.5;

/// Which system a site belongs to, and which site it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteKey {
    /// "p25:<wacn>.<sysid>", "smartnet:<sysid>", or "group:<name>" from the config.
    pub group: String,
    /// (RFSS, site), once the control channel has said.
    pub site: Option<(u32, u32)>,
    /// The control channel it follows (before the site is heard, one
    /// control channel is one site).
    pub cc_hz: Option<u64>,
}

impl SiteKey {
    /// Another site of the same system (not the same site followed twice).
    pub fn twin(&self, o: &SiteKey) -> bool {
        fn same<T: PartialEq>(a: Option<T>, b: Option<T>) -> bool {
            a.is_some() && a == b
        }
        self.group == o.group && !same(self.site, o.site) && !same(self.cc_hz, o.cc_hz)
    }
}

/// A copy's recording, held until its twins end.
#[derive(Clone, Debug)]
pub struct Held {
    pub call: Call,
    pub audio: Vec<f32>,
    pub frames: CallFrames,
    pub recorder_num: u32,
}

impl Held {
    /// Seconds of audio decoded cleanly: the vocoder frames not repeated,
    /// muted or lost (analog: all of it). Encrypted: none.
    pub fn clean_s(&self) -> f64 {
        if self.call.encrypted || self.audio.is_empty() {
            return 0.0;
        }
        let iv = &self.frames.errors.intervals;
        if iv.is_empty() {
            return self.audio.len() as f64 / SAMPLE_RATE as f64;
        }
        let good: u32 = iv.iter().map(|i| i.frames - i.bad_frames).sum();
        good as f64 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64
    }
}

#[derive(Default)]
struct Conversation {
    /// Every copy: (call, its system); `open` those not ended yet.
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
    /// `call` (on `system`) is a copy of `twins` (calls still going, with their systems).
    pub fn link(&mut self, call: CallId, system: u16, twins: &[(CallId, u16)]) {
        if twins.is_empty() {
            return;
        }
        // Join the twins' conversation(s) — merging, should they be in different ones.
        let mut ids: Vec<u32> = twins.iter().filter_map(|t| self.of.get(&t.0).copied()).collect();
        ids.sort_unstable();
        ids.dedup();
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
        for &(c, sys) in twins.iter().chain([&(call, system)]) {
            if !conv.members.iter().any(|m| m.0 == c) {
                conv.members.push((c, sys));
                conv.open.push(c);
                self.of.insert(c, id);
            }
        }
    }

    /// The other copies of `call` (ended ones too, while it's going): (call, system).
    pub fn twins(&self, call: CallId) -> Vec<(CallId, u16)> {
        self.of.get(&call).and_then(|i| self.convs.get(i)).map_or_else(Vec::new, |c| c.members.iter().copied().filter(|m| m.0 != call).collect())
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

/// Which copy to keep: a preferred one (`preferred[i]`) holding
/// [`PREFERRED_SHARE`] of the best clean audio, else the most clean audio;
/// ties to the fewest FEC errors, then the earliest start.
pub fn pick(copies: &[Held], preferred: &[bool]) -> Option<usize> {
    let best = copies.iter().map(Held::clean_s).fold(0.0, f64::max);
    let rank = |i: &usize| {
        let h = &copies[*i];
        (h.clean_s(), std::cmp::Reverse(h.frames.errors.total_errors()), -h.call.start_s)
    };
    let cmp = |a: &usize, b: &usize| rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal);
    let pref = (0..copies.len()).filter(|&i| preferred.get(i) == Some(&true) && copies[i].clean_s() >= best * PREFERRED_SHARE).max_by(cmp);
    pref.or_else(|| (0..copies.len()).max_by(cmp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mbe::Kind;
    use crate::trunk::frames::{Codec, VoiceFrame};

    fn key(group: &str, site: Option<(u32, u32)>) -> SiteKey {
        SiteKey { group: group.into(), site, cc_hz: None }
    }

    fn call(id: CallId, system: u16, start_s: f64) -> Call {
        Call { id, system, start_s, ..Default::default() }
    }

    /// `good` clean frames then `bad` repeated ones, `errs` bit errors each.
    fn held(id: CallId, system: u16, good: usize, bad: usize, errs: u32) -> Held {
        let mut frames = CallFrames::new(false);
        for k in 0..good + bad {
            let kind = if k < good { Kind::Voice } else { Kind::Repeat };
            frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs, erased: false, kind });
        }
        Held { call: call(id, system, id as f64 * 0.1), audio: vec![0.1; (good + bad) * FRAME_SAMPLES], frames, recorder_num: 0 }
    }

    #[test]
    fn sites_of_one_system_are_twins_and_one_site_twice_is_not() {
        let a = key("p25:bee00.1a2", Some((1, 3)));
        assert!(a.twin(&key("p25:bee00.1a2", Some((1, 4)))));
        assert!(a.twin(&key("p25:bee00.1a2", None)), "site not heard yet");
        assert!(!a.twin(&key("p25:bee00.1a2", Some((1, 3)))));
        let cc = |hz| SiteKey { cc_hz: Some(hz), ..key("smartnet:2011", None) };
        assert!(cc(856_000_000).twin(&cc(857_000_000)), "no site number: other control channels are other sites");
        assert!(!cc(856_000_000).twin(&cc(856_000_000)));
        assert!(!a.twin(&key("p25:bee00.1a3", Some((1, 4)))));
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
}
