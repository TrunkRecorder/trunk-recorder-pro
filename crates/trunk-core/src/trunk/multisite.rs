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
use super::frames::{CallFrames, FrameErrors};
use super::talkgroups::Talkgroup;
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
    /// The call JSON's `recorder_num` ([`super::record::ConcludeInfo::recorder_num`]).
    pub recorder_num: u32,
    /// Where each transmission starts in `audio`.
    pub tx: super::record::Transmissions,
    /// How far off its channel the voice came in, Hz.
    pub freq_error_hz: Option<f64>,
    /// How strong it came in.
    pub reception: super::record::Reception,
}

impl Held {
    /// The frame errors of the audio the saved call keeps: all but its
    /// transmissions that were encrypted and not decrypted (what a vocoder
    /// makes of ciphertext can look clean) and those shorter than the
    /// site's `min_tx_s`, as `save_call` leaves them out.
    fn kept_errors(&self, min_tx_s: f64) -> std::borrow::Cow<'_, FrameErrors> {
        let len = self.audio.len();
        self.tx
            .kept(len, min_tx_s, SAMPLE_RATE)
            .and_then(|parts| self.frames.errors_within(&parts, len))
            .map_or(std::borrow::Cow::Borrowed(&self.frames.errors), std::borrow::Cow::Owned)
    }

    /// Seconds of audio decoded cleanly and kept: the vocoder frames not
    /// repeated, muted or lost (analog: all of it), less the transmissions
    /// the saved call leaves out (see `kept_errors`). Encrypted, no key: none.
    pub fn clean_s(&self, min_tx_s: f64) -> f64 {
        if self.audio.is_empty() {
            return 0.0;
        }
        let len = self.audio.len();
        let parts = self.tx.kept(len, min_tx_s, SAMPLE_RATE);
        let kept = parts.as_ref().and_then(|p| self.frames.errors_within(p, len));
        let iv = &kept.as_ref().unwrap_or(&self.frames.errors).intervals;
        if kept.is_none() && iv.is_empty() {
            // No frames to go by (analog): what's kept is all clean.
            let n = parts.map_or(len, |p| p.iter().map(|r| r.len()).sum());
            return n as f64 / SAMPLE_RATE as f64;
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

impl MultiSite {
    /// Link call `c`, just granted on site `key`, with the copies of it
    /// already going on other sites of its system — the same talkgroup,
    /// granted within [`TWIN_WINDOW_S`]. `sites`: every other site's key
    /// (None: not known yet) and its calls.
    pub fn link_new<'a>(&mut self, c: &Call, key: &SiteKey, sites: impl IntoIterator<Item = (Option<SiteKey>, &'a [Call])>) {
        let twins: Vec<(CallId, u16)> = sites
            .into_iter()
            .filter(|(k, _)| k.as_ref().is_some_and(|k| k.twin(key)))
            .flat_map(|(_, calls)| calls.iter())
            .filter(|o| o.talkgroup == c.talkgroup && (o.start_s - c.start_s).abs() <= TWIN_WINDOW_S)
            .map(|o| (o.id, o.system))
            .collect();
        self.link(c.id, c.system, &twins);
    }

    /// Call `id` ended (`held`: its recording, None when it wasn't
    /// recorded). When it was the last of its copies: all of them, the one
    /// to save first ([`pick`]), then the others, best first — the next to
    /// try when one isn't kept (too short for its site's rules).
    /// `min_tx_s(system)`: the shortest transmission that site's rules keep.
    /// `prefers(system, tg)`: whether `system`'s site is the one the
    /// talkgroup file wants `tg` from.
    pub fn conclude(&mut self, id: CallId, held: Option<Held>, prefers: impl Fn(u16, &Talkgroup) -> bool, min_tx_s: impl Fn(u16) -> f64) -> Option<Vec<Held>> {
        let mut copies = match self.end(id, held) {
            Ended::Alone(None) | Ended::Waiting => return None,
            Ended::Alone(Some(h)) => vec![h],
            Ended::Done(v) => v,
        };
        // The talkgroup's preferred site, by any site's talkgroup file.
        let prefs: Vec<&Talkgroup> = copies.iter().filter_map(|h| h.call.talkgroup_info.as_ref()).collect();
        let preferred: Vec<bool> = copies.iter().map(|h| prefs.iter().any(|tg| prefers(h.call.system, tg))).collect();
        let min_tx: Vec<f64> = copies.iter().map(|h| min_tx_s(h.call.system)).collect();
        let k = pick(&copies, &preferred, &min_tx)?;
        let first = copies.remove(k);
        let mut rest: Vec<(f64, Held)> = copies.into_iter().map(|h| (h.clean_s(min_tx_s(h.call.system)), h)).collect();
        rest.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut copies = vec![first];
        copies.extend(rest.into_iter().map(|(_, h)| h));
        Some(copies)
    }
}

/// Which copy to keep: a preferred one (`preferred[i]`) holding
/// [`PREFERRED_SHARE`] of the best clean audio, else the most clean audio;
/// ties to the fewest FEC errors, then the earliest start.
pub fn pick(copies: &[Held], preferred: &[bool], min_tx: &[f64]) -> Option<usize> {
    // (A missing entry is no minimum.)
    let clean = |i: usize| copies[i].clean_s(min_tx.get(i).copied().unwrap_or(0.0));
    let best = (0..copies.len()).map(clean).fold(0.0, f64::max);
    let rank = |i: &usize| {
        let h = &copies[*i];
        (clean(*i), std::cmp::Reverse(h.kept_errors(min_tx.get(*i).copied().unwrap_or(0.0)).total_errors()), -h.call.start_s)
    };
    let cmp = |a: &usize, b: &usize| rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal);
    let pref = (0..copies.len()).filter(|&i| preferred.get(i) == Some(&true) && clean(i) >= best * PREFERRED_SHARE).max_by(cmp);
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
        Held { call: call(id, system, id as f64 * 0.1), audio: vec![0.1; (good + bad) * FRAME_SAMPLES], frames, recorder_num: 0, tx: Default::default(), freq_error_hz: None, reception: Default::default() }
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
        assert_eq!(pick(&copies, &[false; 3], &[0.0; 3]), Some(2), "ties go to fewer FEC errors");
        assert_eq!(pick(&copies, &[true, false, false], &[0.0; 3]), Some(0), "100 of 110 clean is enough");
        let copies = [held(1, 0, 50, 70, 0), held(2, 1, 110, 0, 0)];
        assert_eq!(pick(&copies, &[true, false], &[0.0; 2]), Some(1), "a preferred site that faded loses");
        let mut silent = held(1, 0, 0, 0, 0);
        silent.call.encrypted = true;
        assert_eq!(pick(&[silent.clone(), held(2, 0, 0, 0, 0)], &[false, false], &[0.0; 2]), Some(0), "nothing to choose by: the first to start");
        assert_eq!(pick(&[], &[], &[]), None);
    }

    #[test]
    fn clean_frames_are_counted_in_the_kept_transmissions_only() {
        // The kept transmission is all bad frames, the one left out all good ones.
        let mut h = held(1, 0, 0, 0, 0);
        for k in 0..100 {
            let kind = if k < 50 { Kind::Repeat } else { Kind::Voice };
            h.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind });
        }
        h.audio = vec![0.1; 100 * FRAME_SAMPLES];
        h.tx.note(0, 0.0);
        h.tx.note(50 * FRAME_SAMPLES, 5.0);
        h.tx.mark_encrypted(5.0);
        assert_eq!(h.clean_s(0.0), 0.0, "the good frames are in the part that is left out");
    }

    #[test]
    fn audio_left_out_as_undecrypted_does_not_count_as_clean() {
        // One site decrypted its whole call; the other heard the second
        // transmission encrypted and without its key (noise that decodes "clean").
        let mut whole = held(1, 0, 100, 0, 0);
        whole.tx.note(0, 0.0);
        whole.tx.mark_encrypted(0.0);
        whole.tx.mark_decrypted(0.0);
        whole.tx.note(50 * FRAME_SAMPLES, 5.0);
        whole.tx.mark_encrypted(5.0);
        whole.tx.mark_decrypted(5.0);
        let mut cut = held(2, 1, 100, 0, 0);
        cut.tx.note(0, 0.0);
        cut.tx.note(50 * FRAME_SAMPLES, 5.0);
        cut.tx.mark_encrypted(5.0);
        assert!(whole.clean_s(0.0) > cut.clean_s(0.0));
        assert!((cut.clean_s(0.0) - whole.clean_s(0.0) / 2.0).abs() < 1e-9, "half of it is left out");
        assert_eq!(pick(&[cut.clone(), whole.clone()], &[false; 2], &[0.0; 2]), Some(1));
        // The site the talkgroup file prefers doesn't keep a call that lost half its audio.
        assert_eq!(pick(&[cut, whole], &[true, false], &[0.0; 2]), Some(1));
    }

    #[test]
    fn transmissions_shorter_than_the_sites_minimum_are_not_counted() {
        // Frames: a 10-frame burst of clean ones, then 50 bad (repeated) ones in a second transmission.
        let mut h = held(1, 0, 0, 0, 0);
        for k in 0..60 {
            let kind = if k < 10 { Kind::Voice } else { Kind::Repeat };
            h.frames.push(VoiceFrame { codec: Codec::Imbe, bits: vec![0; 88], e0: 0, errs: 0, erased: false, kind });
        }
        h.audio = vec![0.1; 60 * FRAME_SAMPLES];
        h.tx.note(0, 0.0);
        h.tx.note(10 * FRAME_SAMPLES, 5.0);
        let burst_s = 10.0 * FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
        assert!((h.clean_s(0.0) - burst_s).abs() < 1e-9, "short burst counts with no minimum");
        assert_eq!(h.clean_s(burst_s * 2.0), 0.0, "and is left out by a site that drops it");
        let other = held(2, 1, 20, 0, 0);
        assert_eq!(pick(&[h, other], &[false; 2], &[burst_s * 2.0, 0.0]), Some(1));
    }

    #[test]
    fn a_call_decrypted_from_its_second_transmission_keeps_only_that_part() {
        // This site missed the key for the first transmission, then had it.
        let mut late = held(1, 0, 100, 0, 0);
        late.tx.note(0, 0.0);
        late.tx.mark_encrypted(0.0);
        late.tx.note(50 * FRAME_SAMPLES, 5.0);
        late.tx.mark_encrypted(5.0);
        late.tx.mark_decrypted(5.0);
        let frame_s = FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
        assert!((late.clean_s(0.0) - 50.0 * frame_s).abs() < 1e-9, "only the decrypted second half is kept");
        let clear = held(2, 1, 100, 0, 0);
        assert_eq!(pick(&[late.clone(), clear.clone()], &[false; 2], &[0.0; 2]), Some(1));
        assert_eq!(pick(&[late.clone(), clear], &[true, false], &[0.0; 2]), Some(1), "preferred, but half the audio");
        // Equally usable: the preferred site's copy wins over the one that started earlier.
        let other = held(3, 1, 50, 0, 0);
        assert_eq!(pick(&[late.clone(), other.clone()], &[false; 2], &[0.0; 2]), Some(0), "earliest start without a preference");
        assert_eq!(pick(&[late, other], &[false, true], &[0.0; 2]), Some(1));
    }
}
