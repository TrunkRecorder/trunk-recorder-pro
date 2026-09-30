//! Trunked call lifecycle — Trunk Recorder's monitor_systems.cc
//! (handle_call_grant / handle_call_update / manage_calls), one system.
//! Several systems share one [`CallIds`] so call ids are unique across them.
//!
//! A call is (talkgroup, frequency, TDMA slot). GRANTs (and UPDATEs, with
//! `new_call_from_update`) create calls; UPDATEs refresh them. A RECORDING
//! call ends when the control channel has not mentioned it for
//! `call_timeout_s` AND its recorder has written no audio for as long; a
//! MONITORING call on the first condition alone. Time is the sample clock.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::message::{Message, MessageType};
use super::patches::Patches;
use super::talkgroups::{Talkgroup, Talkgroups};

pub type CallId = u32;

/// `Call::system` of a conventional channel's call.
pub const CONVENTIONAL: u16 = u16::MAX;

/// Hands out call ids; clones share the sequence (one per engine).
#[derive(Clone, Debug)]
pub struct CallIds(Arc<AtomicU32>);

impl Default for CallIds {
    fn default() -> Self {
        CallIds(Arc::new(AtomicU32::new(1)))
    }
}

impl CallIds {
    pub fn next(&self) -> CallId {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    UnknownTg,
    Encrypted,
    NoSource,
    NoRecorder,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::UnknownTg => "unknown_tg",
            Reason::Encrypted => "encrypted",
            Reason::NoSource => "no_source",
            Reason::NoRecorder => "no_recorder",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CallSource {
    pub src: u32,
    /// Sample-clock seconds this source was first heard on the call.
    pub time_s: f64,
    pub emergency: bool,
}

#[derive(Clone, Debug)]
pub struct Call {
    pub id: CallId,
    /// The system (index in the engine's list) that made it; [`CONVENTIONAL`] for a conventional channel.
    pub system: u16,
    pub talkgroup: u32,
    pub freq_hz: u64,
    pub phase2_tdma: bool,
    pub tdma_slot: u8,
    pub unit_to_unit: bool,
    pub recording: bool,
    pub reason: Option<Reason>,
    pub encrypted: bool,
    pub emergency: bool,
    pub priority: u8,
    pub duplex: bool,
    pub mode: bool,
    /// Analog voice (a conventional FM channel).
    pub analog: bool,
    pub start_s: f64,
    pub last_update_s: f64,
    pub last_audio_s: f64,
    pub sources: Vec<CallSource>,
    pub talkgroup_info: Option<Talkgroup>,
    /// Every talkgroup patched with this one while the call lasted, its own
    /// included, ascending (TR's patched_talkgroups); empty when never patched.
    pub patched_talkgroups: Vec<u32>,
    /// DMR: the colour code the voice came with (a DMR call's slot is `tdma_slot`).
    pub color_code: Option<u8>,
}

impl Call {
    /// Fold in the talkgroups patched with it now (`members`); true when that added any.
    fn note_patch(&mut self, members: Vec<u32>) -> bool {
        let n = self.patched_talkgroups.len();
        for tg in members {
            if let Err(i) = self.patched_talkgroups.binary_search(&tg) {
                self.patched_talkgroups.insert(i, tg);
            }
        }
        self.patched_talkgroups.len() != n
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CallConfig {
    pub call_timeout_s: f64,
    pub record_unknown: bool,
    /// Record calls flagged encrypted (you get silence; TR's monitorEncrypted).
    pub record_encrypted: bool,
    pub record_unit_to_unit: bool,
    pub new_call_from_update: bool,
}

impl Default for CallConfig {
    fn default() -> Self {
        CallConfig { call_timeout_s: 3.0, record_unknown: true, record_encrypted: false, record_unit_to_unit: true, new_call_from_update: true }
    }
}

/// What the manager asks of whoever owns the radio.
pub trait RecorderHost {
    /// Start recording; `Err` when out of band / no recorder free / unsupported.
    fn start_recording(&mut self, call: &Call) -> Result<(), Reason>;
    /// Follow a call that isn't recorded — its voice channel's link control
    /// only (who talks, talker aliases); true when a channel was opened.
    fn follow(&mut self, _call: &Call) -> bool {
        false
    }
    /// Stop recording (or following) `call`; nothing when it has no channel.
    fn stop_recording(&mut self, call: &Call);
}

/// What happened, for the owner to act on and report.
#[derive(Clone, Debug)]
pub enum CallEvent {
    Start(Call),
    Update(Call),
    End(Call),
}

#[derive(Default)]
pub struct CallManager {
    pub calls: Vec<Call>,
    pub talkgroups: Talkgroups,
    pub cfg: CallConfig,
    /// Stamped on every call made here.
    pub system: u16,
    ids: CallIds,
    /// The system's standing patches.
    pub patches: Patches,
}

impl CallManager {
    pub fn new(cfg: CallConfig, talkgroups: Talkgroups) -> Self {
        CallManager { calls: Vec::new(), talkgroups, cfg, system: 0, ids: CallIds::default(), patches: Patches::default() }
    }

    /// For system `system`, drawing ids from `ids` (shared with the other systems).
    pub fn with_ids(cfg: CallConfig, talkgroups: Talkgroups, system: u16, ids: CallIds) -> Self {
        CallManager { system, ids, ..Self::new(cfg, talkgroups) }
    }

    pub fn handle(&mut self, msgs: &[Message], host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        for m in msgs {
            match m.kind {
                MessageType::Grant => self.grant(m, false, host, ev),
                MessageType::Update if self.cfg.new_call_from_update => self.grant(m, false, host, ev),
                MessageType::Update => self.update(m, ev),
                MessageType::UuVGrant if self.cfg.record_unit_to_unit => self.grant(m, true, host, ev),
                MessageType::UuVUpdate if self.cfg.record_unit_to_unit => self.update(m, ev),
                MessageType::PatchAdd => {
                    if let Some(p) = m.patch {
                        if self.patches.add(&p, m.time_s) {
                            // Calls already up on a talkgroup now patched. The
                            // grant can come first (WMATA's does): one skipped
                            // as unknown gets another look.
                            for c in self.calls.iter_mut() {
                                if c.note_patch(self.patches.members_of(c.talkgroup)) {
                                    if c.reason == Some(Reason::UnknownTg) {
                                        Self::admit(c, &self.cfg, &self.talkgroups, host);
                                    }
                                    ev.push(CallEvent::Update(c.clone()));
                                }
                            }
                        }
                    }
                }
                MessageType::PatchDelete => {
                    if let Some(p) = m.patch {
                        self.patches.delete(&p);
                    }
                }
                _ => {}
            }
        }
    }

    /// A fresh call id (conventional channels make their own calls).
    pub fn allocate_id(&mut self) -> CallId {
        self.ids.next()
    }

    pub fn call_mut(&mut self, id: CallId) -> Option<&mut Call> {
        self.calls.iter_mut().find(|c| c.id == id)
    }

    /// The recorder delivered audio for this call.
    pub fn note_audio(&mut self, id: CallId, time_s: f64) {
        if let Some(c) = self.call_mut(id) {
            c.last_audio_s = c.last_audio_s.max(time_s);
        }
    }

    /// A voice channel named a source radio (returns true if it was new).
    pub fn note_source(c: &mut Call, src: u32, time_s: f64, emergency: bool) -> bool {
        if src == 0 || c.sources.last().is_some_and(|s| s.src == src) {
            return false;
        }
        c.sources.push(CallSource { src, time_s, emergency });
        true
    }

    /// Advance the clock: end calls that have gone quiet.
    pub fn tick(&mut self, now_s: f64, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        let t = self.cfg.call_timeout_s;
        let mut i = self.calls.len();
        while i > 0 {
            i -= 1;
            let c = &self.calls[i];
            let quiet_cc = now_s - c.last_update_s > t;
            let quiet_audio = now_s - c.last_audio_s > t;
            if quiet_cc && (!c.recording || quiet_audio) {
                let c = self.calls.remove(i);
                host.stop_recording(&c);
                ev.push(CallEvent::End(c));
            }
        }
        self.patches.expire(now_s);
    }

    /// End everything (source stopped).
    pub fn end_all(&mut self, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        for c in std::mem::take(&mut self.calls) {
            host.stop_recording(&c);
            ev.push(CallEvent::End(c));
        }
    }

    fn matches(c: &Call, m: &Message) -> bool {
        c.talkgroup == m.talkgroup && c.freq_hz == m.freq_hz && c.tdma_slot == m.tdma_slot && c.phase2_tdma == m.phase2_tdma
    }

    fn refresh(c: &mut Call, m: &Message, patches: &Patches) -> bool {
        c.last_update_s = m.time_s;
        let mut changed = c.note_patch(patches.members_of(c.talkgroup));
        if m.encrypted && !c.encrypted {
            c.encrypted = true;
            changed = true;
        }
        if m.emergency && !c.emergency {
            c.emergency = true;
            changed = true;
        }
        if m.source > 0 {
            changed |= Self::note_source(c, m.source as u32, m.time_s, m.emergency);
        }
        changed
    }

    fn update(&mut self, m: &Message, ev: &mut Vec<CallEvent>) {
        for c in self.calls.iter_mut().filter(|c| Self::matches(c, m)) {
            if Self::refresh(c, m, &self.patches) {
                ev.push(CallEvent::Update(c.clone()));
            }
        }
    }

    fn grant(&mut self, m: &Message, unit_to_unit: bool, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        if m.freq_hz == 0 {
            return; // channel not resolvable yet (no IDEN seen)
        }
        if let Some(c) = self.calls.iter_mut().find(|c| Self::matches(c, m)) {
            if Self::refresh(c, m, &self.patches) {
                ev.push(CallEvent::Update(c.clone()));
            }
            return;
        }
        let tg = self.talkgroups.get(&m.talkgroup).cloned();
        let mut c = Call {
            id: self.ids.next(),
            system: self.system,
            talkgroup: m.talkgroup,
            freq_hz: m.freq_hz,
            phase2_tdma: m.phase2_tdma,
            tdma_slot: m.tdma_slot,
            unit_to_unit,
            recording: false,
            reason: None,
            encrypted: m.encrypted || tg.as_ref().is_some_and(|t| t.encrypted_mode()),
            emergency: m.emergency,
            priority: m.priority,
            duplex: m.duplex,
            mode: m.mode,
            // The control channel says (SmartNet), or the talkgroup file does (TR's mode "A").
            analog: m.analog || tg.as_ref().is_some_and(|t| t.mode.starts_with('A')),
            start_s: m.time_s,
            last_update_s: m.time_s,
            last_audio_s: m.time_s,
            sources: if m.source > 0 { vec![CallSource { src: m.source as u32, time_s: m.time_s, emergency: m.emergency }] } else { vec![] },
            talkgroup_info: tg.clone(),
            patched_talkgroups: self.patches.members_of(m.talkgroup),
            color_code: None,
        };
        Self::admit(&mut c, &self.cfg, &self.talkgroups, host);
        ev.push(CallEvent::Start(c.clone()));
        self.calls.push(c);
    }

    /// Record, follow or skip a call: Trunk Recorder's start_recorder()
    /// gates, in its order. A talkgroup not in the file is recorded while
    /// it's patched with one that is.
    fn admit(c: &mut Call, cfg: &CallConfig, talkgroups: &Talkgroups, host: &mut dyn RecorderHost) {
        c.reason = None;
        let known = c.talkgroup_info.is_some() || c.patched_talkgroups.iter().any(|g| talkgroups.contains_key(g));
        if !known && !cfg.record_unknown && !talkgroups.is_empty() {
            c.reason = Some(Reason::UnknownTg);
        } else if c.encrypted && !cfg.record_encrypted {
            // No audio to record, but its terminators' link control is in
            // the clear: who spoke, and their talker aliases.
            c.reason = Some(Reason::Encrypted);
            host.follow(c);
        } else {
            match host.start_recording(c) {
                Ok(()) => c.recording = true,
                Err(r) => c.reason = Some(r),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::message::Patch;
    use crate::trunk::record::{call_record, ConcludeInfo};

    struct Host;
    impl RecorderHost for Host {
        fn start_recording(&mut self, _: &Call) -> Result<(), Reason> {
            Ok(())
        }
        fn stop_recording(&mut self, _: &Call) {}
    }

    fn msg(kind: MessageType, t: f64, tg: u32) -> Message {
        Message { kind, time_s: t, talkgroup: tg, freq_hz: 851_000_000, source: 7, ..Default::default() }
    }
    fn patch(t: f64, sg: u32, ga: [u32; 3]) -> Message {
        Message { kind: MessageType::PatchAdd, time_s: t, patch: Some(Patch { sg, ga }), ..Default::default() }
    }
    /// Talkgroup 101 is in the file; supergroups aren't.
    fn manager() -> CallManager {
        let tgs = [(101, Talkgroup { number: 101, alpha_tag: "Fire Disp".into(), ..Default::default() })].into_iter().collect();
        CallManager::new(CallConfig { record_unknown: false, ..Default::default() }, tgs)
    }

    #[test]
    fn an_unknown_supergroup_is_recorded_while_a_known_talkgroup_is_patched_in() {
        let mut m = manager();
        let mut ev = Vec::new();
        m.handle(&[patch(0.0, 65001, [101, 202, 0]), msg(MessageType::Grant, 0.5, 65001)], &mut Host, &mut ev);
        assert!(m.calls[0].recording);
        assert_eq!(m.calls[0].patched_talkgroups, vec![101, 202, 65001]);

        // Patch gone (no repeats): a new call on the supergroup isn't recorded.
        m.tick(20.0, &mut Host, &mut ev);
        m.handle(&[msg(MessageType::Grant, 20.0, 65001)], &mut Host, &mut ev);
        assert_eq!(m.calls[0].reason, Some(Reason::UnknownTg));
        assert!(m.calls[0].patched_talkgroups.is_empty());
    }

    #[test]
    fn a_supergroup_granted_before_its_patch_is_heard_starts_recording_when_it_is() {
        let mut m = manager();
        let mut ev = Vec::new();
        m.handle(&[msg(MessageType::Grant, 0.0, 32816)], &mut Host, &mut ev);
        assert_eq!(m.calls[0].reason, Some(Reason::UnknownTg));
        ev.clear();
        m.handle(&[patch(0.1, 32816, [101, 0, 0])], &mut Host, &mut ev);
        assert!(m.calls[0].recording && m.calls[0].reason.is_none());
        assert!(matches!(&ev[..], [CallEvent::Update(c)] if c.recording));
    }

    #[test]
    fn a_patch_heard_after_the_grant_joins_the_call_and_outlasts_it() {
        let mut m = manager();
        let mut ev = Vec::new();
        m.handle(&[msg(MessageType::Grant, 0.0, 101)], &mut Host, &mut ev);
        ev.clear();
        m.handle(&[patch(1.0, 65001, [101, 0, 0])], &mut Host, &mut ev);
        assert!(matches!(&ev[..], [CallEvent::Update(c)] if c.patched_talkgroups == [101, 65001]));
        // A repeat changes nothing.
        ev.clear();
        m.handle(&[patch(2.0, 65001, [101, 0, 0])], &mut Host, &mut ev);
        assert!(ev.is_empty());
        // The patch ends before the call does: the call still says it was patched.
        m.handle(&[Message { kind: MessageType::PatchDelete, time_s: 2.5, patch: Some(Patch { sg: 65001, ga: [0; 3] }), ..Default::default() }], &mut Host, &mut ev);
        m.handle(&[msg(MessageType::Update, 2.5, 101)], &mut Host, &mut ev);
        assert_eq!(m.calls[0].patched_talkgroups, vec![101, 65001]);
    }

    #[test]
    fn the_call_json_lists_the_patch() {
        let mut m = manager();
        let mut ev = Vec::new();
        m.handle(&[patch(0.0, 65001, [101, 0, 0]), msg(MessageType::Grant, 0.5, 65001), msg(MessageType::Grant, 0.5, 303)], &mut Host, &mut ev);
        let errors = Default::default();
        let info = ConcludeInfo { short_name: "s", epoch_ms_at_zero: 0.0, audio_seconds: 1.0, errors: &errors, recorder_num: 0, end_s: 1.0, units: None };
        assert!(call_record(&m.calls[0], &info).0.contains("\"short_name\":\"s\",\"patched_talkgroups\":[101,65001],\"freqList\""));
        assert!(!call_record(&m.calls[1], &info).0.contains("patched"));
    }
}
