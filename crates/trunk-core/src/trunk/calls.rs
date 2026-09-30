//! Trunked call lifecycle — Trunk Recorder's monitor_systems.cc
//! (handle_call_grant / handle_call_update / manage_calls), one system.
//! Several systems share one [`CallIds`] so call ids are unique across them.
//!
//! A call is (talkgroup, frequency, TDMA slot). GRANTs (and UPDATEs, with
//! `new_call_from_update`) create calls; UPDATEs refresh them. A RECORDING
//! call ends when the control channel has not mentioned it for
//! `call_timeout_s` AND its recorder has written no audio for as long; a
//! MONITORING call on the first condition alone. Time is the sample clock.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use super::message::{Message, MessageType};
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

struct PatchGroup {
    members: HashSet<u32>,
    last_s: f64,
}

#[derive(Default)]
pub struct CallManager {
    pub calls: Vec<Call>,
    pub talkgroups: Talkgroups,
    pub cfg: CallConfig,
    /// Stamped on every call made here.
    pub system: u16,
    ids: CallIds,
    patches: HashMap<u32, PatchGroup>,
}

impl CallManager {
    pub fn new(cfg: CallConfig, talkgroups: Talkgroups) -> Self {
        CallManager { calls: Vec::new(), talkgroups, cfg, system: 0, ids: CallIds::default(), patches: HashMap::new() }
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
                        let g = self.patches.entry(p.sg).or_insert(PatchGroup { members: HashSet::new(), last_s: m.time_s });
                        g.members.extend(p.ga.iter().copied().filter(|&x| x != 0));
                        g.last_s = m.time_s;
                    }
                }
                MessageType::PatchDelete => {
                    if let Some(p) = m.patch {
                        self.patches.remove(&p.sg);
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
        self.patches.retain(|_, p| now_s - p.last_s <= 60.0);
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

    fn refresh(c: &mut Call, m: &Message) -> bool {
        c.last_update_s = m.time_s;
        let mut changed = false;
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
            if Self::refresh(c, m) {
                ev.push(CallEvent::Update(c.clone()));
            }
        }
    }

    fn grant(&mut self, m: &Message, unit_to_unit: bool, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        if m.freq_hz == 0 {
            return; // channel not resolvable yet (no IDEN seen)
        }
        if let Some(c) = self.calls.iter_mut().find(|c| Self::matches(c, m)) {
            if Self::refresh(c, m) {
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
        };
        // Trunk Recorder's start_recorder() gates, in its order.
        let patched_known = tg.is_none()
            && self.patches.iter().any(|(&sg, p)| {
                (sg == m.talkgroup || p.members.contains(&m.talkgroup))
                    && (self.talkgroups.contains_key(&sg) || p.members.iter().any(|g| self.talkgroups.contains_key(g)))
            });
        if tg.is_none() && !self.cfg.record_unknown && !patched_known && !self.talkgroups.is_empty() {
            c.reason = Some(Reason::UnknownTg);
        } else if c.encrypted && !self.cfg.record_encrypted {
            // No audio to record, but its terminators' link control is in
            // the clear: who spoke, and their talker aliases.
            c.reason = Some(Reason::Encrypted);
            host.follow(&c);
        } else {
            match host.start_recording(&c) {
                Ok(()) => c.recording = true,
                Err(r) => c.reason = Some(r),
            }
        }
        ev.push(CallEvent::Start(c.clone()));
        self.calls.push(c);
    }
}
