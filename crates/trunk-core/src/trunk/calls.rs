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
use super::voice::{CipherId, VoiceKind};
use crate::dsp::tones::{Tone, ToneHeard};
use crate::position::Position;

pub type CallId = u32;

/// What a call's voice is on: the kind of voice channel, and where on it.
/// Each control channel's parser says so in its grants
/// ([`Message::voice`]); a conventional channel says so by its mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Voice {
    /// P25 Phase 1 (FDMA): IMBE.
    #[default]
    P25,
    /// P25 Phase 2 (TDMA) on slot `slot` (0 / 1): AMBE+2.
    P25Tdma { slot: u8 },
    /// DMR on slot `slot` (0 / 1), with its colour code once known.
    Dmr { slot: u8, color_code: Option<u8> },
    /// NXDN at its rate.
    Nxdn(crate::nxdn::Rate),
    /// Analog FM (SmartNet's analog grants, conventional FM channels).
    Fm,
    /// Analog AM (conventional channels).
    Am,
    /// EDACS ProVoice (9600 baud 2FSK, IMBE 7100).
    ProVoice,
}

impl Voice {
    /// The kind of voice channel it needs.
    pub fn kind(self) -> VoiceKind {
        match self {
            Voice::P25 => VoiceKind::Fdma,
            Voice::P25Tdma { .. } => VoiceKind::Tdma,
            Voice::Dmr { .. } => VoiceKind::Dmr,
            Voice::Nxdn(r) => VoiceKind::Nxdn(r),
            Voice::Fm => VoiceKind::Analog,
            Voice::Am => VoiceKind::Am,
            Voice::ProVoice => VoiceKind::ProVoice,
        }
    }

    /// Its slot (0 / 1) on a two-slot channel: P25 Phase 2 or DMR.
    pub fn slot(self) -> Option<u8> {
        match self {
            Voice::P25Tdma { slot } | Voice::Dmr { slot, .. } => Some(slot),
            _ => None,
        }
    }

    pub fn analog(self) -> bool {
        matches!(self, Voice::Fm | Voice::Am)
    }

    /// P25 Phase 2 (Trunk Recorder's `phase2_tdma`).
    pub fn tdma(self) -> bool {
        matches!(self, Voice::P25Tdma { .. })
    }

    /// Its voice is of a kind that can be decrypted (P25 Phase 1 or 2).
    pub fn decryptable(self) -> bool {
        matches!(self, Voice::P25 | Voice::P25Tdma { .. })
    }

    /// DMR: its colour code.
    pub fn color_code(self) -> Option<u8> {
        match self {
            Voice::Dmr { color_code, .. } => color_code,
            _ => None,
        }
    }

    /// Its name in the call JSON's `voice_mode`: a conventional row's `mode`
    /// (`p25`, `dmr`, `nxdn48`, `nxdn96`, `fm`, `am`), `p25_phase2` or `provoice`.
    pub fn mode_name(self) -> &'static str {
        match self {
            Voice::P25 => "p25",
            Voice::P25Tdma { .. } => "p25_phase2",
            Voice::Dmr { .. } => "dmr",
            Voice::Nxdn(r) => r.name(),
            Voice::Fm => "fm",
            Voice::Am => "am",
            Voice::ProVoice => "provoice",
        }
    }

    /// NXDN: its rate.
    pub fn nxdn(self) -> Option<crate::nxdn::Rate> {
        match self {
            Voice::Nxdn(r) => Some(r),
            _ => None,
        }
    }

    /// Two grants for one channel's same place: P25 Phase 2 or not, the same slot.
    fn same_place(self, other: Voice) -> bool {
        self.tdma() == other.tdma() && self.slot().unwrap_or(0) == other.slot().unwrap_or(0)
    }
}

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
    /// Marked Ignore in the talkgroup file.
    Ignored,
    UnknownTg,
    Encrypted,
    NoSource,
    NoRecorder,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Ignored => "ignored",
            Reason::UnknownTg => "unknown_tg",
            Reason::Encrypted => "encrypted",
            Reason::NoSource => "no_source",
            Reason::NoRecorder => "no_recorder",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallSource {
    pub src: u32,
    /// Sample-clock seconds this source was first heard on the call.
    pub time_s: f64,
    /// Where in the saved audio it starts talking (sample index), once the
    /// call's audio is put together ([`super::multisite::Held::trim`]).
    pub audio_pos: Option<usize>,
    /// What its transmission said of itself (the grant's, then the voice
    /// channel's words while it talked).
    pub flags: Flags,
    /// Where the radio said it was (GPS) while it talked: latitude, longitude.
    pub position: Option<(f64, f64)>,
}

/// What a radio's transmission said of itself: P25's service options
/// (AABC-D §2.3.24: in grants, link control and Phase 2's voice channel user
/// messages), or the emergency and encryption flags other protocols have.
/// Kept per source: one talker's emergency isn't the next one's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub emergency: bool,
    /// Encrypted (P25: protected).
    pub encrypted: bool,
    /// Full duplex (half otherwise).
    pub duplex: bool,
    /// Packet mode (circuit otherwise).
    pub packet: bool,
    /// P25's priority level: 1 lowest, 4 the default, 7 highest; 0 not given.
    pub priority: u8,
}

impl Flags {
    /// A control channel message's, for the radio it names.
    pub fn of(m: &Message) -> Self {
        Flags { emergency: m.emergency, encrypted: m.encrypted, duplex: m.duplex, packet: m.mode, priority: m.priority }
    }
    pub fn emergency(emergency: bool) -> Self {
        Flags { emergency, ..Default::default() }
    }
    /// Another word about the same transmission: a flag either set is set;
    /// the priority is the latest one given.
    pub fn merge(self, o: Flags) -> Flags {
        Flags {
            emergency: self.emergency || o.emergency,
            encrypted: self.encrypted || o.encrypted,
            duplex: self.duplex || o.duplex,
            packet: self.packet || o.packet,
            priority: if o.priority != 0 { o.priority } else { self.priority },
        }
    }
}

impl From<crate::p25::osp::ServiceOptions> for Flags {
    fn from(o: crate::p25::osp::ServiceOptions) -> Self {
        Flags { emergency: o.emergency(), encrypted: o.protected(), duplex: o.duplex(), packet: o.packet(), priority: o.priority() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Call {
    pub id: CallId,
    /// The site that made it (its number among the engine's sites,
    /// [`super::engine::sites`]); none for a conventional call.
    pub site: Option<u16>,
    /// Its system's number (its place in the engine's systems).
    pub system: u16,
    /// Multi-site: a copy of a call another site of its system had first —
    /// one call, which plugins and stats count by its first copy.
    pub twin: bool,
    pub talkgroup: u32,
    /// Conventional digital: the talkgroup the air named, which its row may
    /// file under another `talkgroup` (None: analog, nothing named yet, or
    /// trunked — the grant's is the air's).
    pub talkgroup_ota: Option<u32>,
    pub freq_hz: u64,
    /// What its voice is on.
    pub voice: Voice,
    pub unit_to_unit: bool,
    pub recording: bool,
    /// Followed without a recorder (its voice channel's link control only:
    /// encrypted, not recorded).
    pub followed: bool,
    pub reason: Option<Reason>,
    pub encrypted: bool,
    /// The cipher its voice's encryption sync named (the latest, if more than one).
    pub cipher: Option<CipherId>,
    /// Some of its encrypted voice was decrypted (its system had the key).
    pub decrypted: bool,
    pub emergency: bool,
    pub priority: u8,
    pub duplex: bool,
    pub mode: bool,
    pub start_s: f64,
    pub last_update_s: f64,
    /// When the recorder took in that update, by its own clock: the call
    /// timeout counts from here, as Trunk Recorder's does. A control
    /// channel's messages come out some 0.4 s after they were on the air
    /// (P25's diversity bank holds them 0.35 s), so counting from
    /// `last_update_s` would end every call that much early.
    pub heard_s: f64,
    pub last_audio_s: f64,
    pub sources: Vec<CallSource>,
    pub talkgroup_info: Option<Talkgroup>,
    /// Every talkgroup patched with this one while the call lasted, its own
    /// included, ascending (TR's patched_talkgroups); empty when never patched.
    pub patched_talkgroups: Vec<u32>,
    /// NXDN: the RAN its frames carried.
    pub ran: Option<u8>,
    /// P25: the NAC its frames carried (conventional), or its site's (trunked).
    pub nac: Option<u16>,
    /// Conventional FM: the CTCSS tone or DCS code it carried.
    pub tone: Option<ToneHeard>,
    /// Conventional FM: the tone its channel row records (None: any).
    pub tone_set: Option<Tone>,
}

impl Call {
    /// Audio of it was heard at `time_s`.
    pub fn note_audio(&mut self, time_s: f64) {
        self.last_audio_s = self.last_audio_s.max(time_s);
    }

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
    /// A recorded call this long is saved and a new one carries on from
    /// where it ended (Trunk Recorder's maxDuration); 0 = no limit.
    pub max_call_s: f64,
}

impl Default for CallConfig {
    fn default() -> Self {
        CallConfig { call_timeout_s: 3.0, record_unknown: true, record_encrypted: false, record_unit_to_unit: true, new_call_from_update: true, max_call_s: 0.0 }
    }
}

/// The longest a call goes on with no `max_call_s` set: one longer (a stuck
/// carrier, a channel left keyed) is saved in parts this long, s.
pub const STUCK_CALL_S: f64 = 600.0;

/// What the manager asks of whoever owns the radio: a call's voice channel
/// opened to record it ([`record`](Self::record)) or only to follow it
/// ([`follow`](Self::follow)), and closed again ([`release`](Self::release)).
pub trait RecorderHost {
    /// Record `call`: a recorder of the pool, its voice channel opened; `Err`
    /// when out of band / no recorder free / unsupported.
    fn record(&mut self, call: &Call) -> Result<(), Reason>;
    /// Follow a call that isn't recorded — its voice channel's link control
    /// only (who talks, talker aliases); true when a channel was opened.
    fn follow(&mut self, _call: &Call) -> bool {
        false
    }
    /// `call` is over, recorded or followed: its place on the voice channel
    /// is given up (the channel closes with its last call); nothing when it
    /// has no channel.
    fn release(&mut self, call: &Call);
    /// A long call goes on as `new`: record it on `old`'s channel. `old`'s
    /// recorder is freed as it ends, so this takes none more from the pool.
    fn record_continued(&mut self, _old: &Call, new: &Call) -> Result<(), Reason> {
        self.record(new)
    }
}

/// What happened, for the owner to act on and report.
#[derive(Clone, Debug)]
pub enum CallEvent {
    Start(Call),
    Update(Call),
    End(Call),
}

/// What a system decides about its calls that can change while recording,
/// as its sites' call managers see it when the control channel speaks.
#[derive(Clone, Copy, Debug)]
pub struct Policy<'a> {
    pub talkgroups: &'a Talkgroups,
    /// The system has encryption keys: encrypted calls it may be able to
    /// decrypt are recorded, to find out (the key ID comes with the voice).
    pub can_decrypt: bool,
}

#[derive(Default)]
pub struct CallManager {
    pub calls: Vec<Call>,
    pub cfg: CallConfig,
    /// Stamped on every call made here: its site, and its site's system.
    pub site: u16,
    pub system: u16,
    ids: CallIds,
    /// The site's standing patches.
    pub patches: Patches,
}

impl CallManager {
    pub fn new(cfg: CallConfig) -> Self {
        CallManager { calls: Vec::new(), cfg, site: 0, system: 0, ids: CallIds::default(), patches: Patches::default() }
    }

    /// For site `site` of system `system`, drawing ids from `ids` (shared with the other sites).
    pub fn with_ids(cfg: CallConfig, site: u16, system: u16, ids: CallIds) -> Self {
        CallManager { site, system, ids, ..Self::new(cfg) }
    }

    pub fn handle(&mut self, msgs: &[Message], now_s: f64, policy: Policy, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        for m in msgs {
            match m.kind {
                MessageType::Grant => self.grant(m, now_s, false, policy, host, ev),
                MessageType::Update if self.cfg.new_call_from_update => self.grant(m, now_s, false, policy, host, ev),
                MessageType::Update => self.update(m, now_s, ev),
                // Individual calls: unit-to-unit, and a radio's telephone call.
                MessageType::UuVGrant | MessageType::PhoneGrant if self.cfg.record_unit_to_unit => self.grant(m, now_s, true, policy, host, ev),
                MessageType::UuVUpdate | MessageType::PhoneUpdate if self.cfg.record_unit_to_unit => self.update(m, now_s, ev),
                MessageType::PatchAdd => {
                    if let Some(p) = m.patch {
                        if self.patches.add(&p, m.time_s) {
                            // Calls already up on a talkgroup now patched. The
                            // grant can come first (WMATA's does): one skipped
                            // as unknown gets another look.
                            for c in self.calls.iter_mut() {
                                if c.note_patch(self.patches.members_of(c.talkgroup)) {
                                    if c.reason == Some(Reason::UnknownTg) {
                                        Self::admit(c, &self.cfg, policy, host);
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

    pub fn call_mut(&mut self, id: CallId) -> Option<&mut Call> {
        self.calls.iter_mut().find(|c| c.id == id)
    }

    /// The recorder delivered audio for this call.
    #[cfg(test)]
    pub fn note_audio(&mut self, id: CallId, time_s: f64) {
        if let Some(c) = self.call_mut(id) {
            c.note_audio(time_s);
        }
    }

    /// The radio talking now (`src`; none: whoever was last) and what its
    /// transmission said of itself. Returns whether that changed the call:
    /// a new source, or a flag the source hadn't.
    pub fn note_source(c: &mut Call, src: Option<u32>, time_s: f64, flags: Flags) -> bool {
        let src = src.filter(|&s| s != 0);
        if let Some(s) = c.sources.last_mut().filter(|s| src.is_none_or(|src| s.src == src)) {
            let merged = s.flags.merge(flags);
            return std::mem::replace(&mut s.flags, merged) != merged;
        }
        let Some(src) = src else { return false };
        c.sources.push(CallSource { src, time_s, audio_pos: None, flags, position: None });
        true
    }

    /// What a voice channel's link control said of the call at `now_s`
    /// (who talks, encrypted, emergency), trunked or conventional; `tx`: its
    /// recording's transmissions, which drop an encrypted one's audio.
    /// Returns whether the call changed.
    pub fn note_info(c: &mut Call, tx: Option<&mut super::record::Transmissions>, source: Option<u32>, flags: Flags, now_s: f64) -> bool {
        if flags.encrypted {
            if let Some(tx) = tx {
                tx.mark_encrypted(now_s);
            }
        }
        let mut changed = false;
        if flags.encrypted && !c.encrypted {
            c.encrypted = true;
            changed = true;
        }
        if flags.emergency && !c.emergency {
            c.emergency = true;
            changed = true;
        }
        changed | Self::note_source(c, source, now_s, flags)
    }

    /// Its voice was decrypted (at `now_s`): its transmission's audio is kept. Returns whether the call changed.
    pub fn note_decrypted(c: &mut Call, tx: Option<&mut super::record::Transmissions>, now_s: f64) -> bool {
        if let Some(tx) = tx {
            tx.mark_decrypted(now_s);
        }
        !std::mem::replace(&mut c.decrypted, true)
    }

    /// A voice channel gave a radio's position: kept on its latest entry
    /// (returns true if it moved).
    pub fn note_position(c: &mut Call, p: &Position) -> bool {
        let Some(s) = c.sources.iter_mut().rev().find(|s| s.src == p.unit) else { return false };
        let at = Some((p.lat, p.lon));
        let new = s.position != at;
        s.position = at;
        new
    }

    /// Advance the clock: end calls that have gone quiet, and split those
    /// past the length limit.
    pub fn tick(&mut self, now_s: f64, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        let t = self.cfg.call_timeout_s;
        let max = if self.cfg.max_call_s > 0.0 { self.cfg.max_call_s } else { STUCK_CALL_S };
        let mut i = self.calls.len();
        while i > 0 {
            i -= 1;
            let c = &self.calls[i];
            let quiet_cc = now_s - c.heard_s > t;
            let quiet_audio = now_s - c.last_audio_s > t;
            if quiet_cc && (!c.recording || quiet_audio) {
                let c = self.calls.remove(i);
                host.release(&c);
                ev.push(CallEvent::End(c));
            } else if c.recording && now_s - c.start_s >= max {
                self.split(i, now_s, host, ev);
            }
        }
        self.patches.expire(now_s);
    }

    /// End call `i` and carry on in a new one on the same channel: the new
    /// call takes the voice channel over first, so it stays open and nothing
    /// is lost or replayed between the two.
    fn split(&mut self, i: usize, now_s: f64, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        let old = &self.calls[i];
        let mut c = Call {
            id: self.ids.next(),
            recording: false,
            followed: false,
            reason: None,
            start_s: now_s,
            last_update_s: old.last_update_s.max(now_s),
            heard_s: now_s,
            last_audio_s: now_s,
            // The one talking now carries on.
            sources: old.sources.last().map(|s| CallSource { time_s: now_s, ..s.clone() }).into_iter().collect(),
            ..old.clone()
        };
        // (Only a recorded call is split: it carries on recorded.)
        match host.record_continued(old, &c) {
            Ok(()) => c.recording = true,
            Err(r) => c.reason = Some(r),
        }
        let old = self.calls.remove(i);
        host.release(&old);
        ev.push(CallEvent::End(old));
        ev.push(CallEvent::Start(c.clone()));
        self.calls.push(c);
    }

    /// End everything (source stopped).
    pub fn end_all(&mut self, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        for c in std::mem::take(&mut self.calls) {
            host.release(&c);
            ev.push(CallEvent::End(c));
        }
    }

    fn matches(c: &Call, m: &Message) -> bool {
        c.talkgroup == m.talkgroup && c.freq_hz == m.freq_hz && c.voice.same_place(m.voice)
    }

    fn refresh(c: &mut Call, m: &Message, now_s: f64, patches: &Patches) -> bool {
        c.last_update_s = m.time_s;
        c.heard_s = now_s;
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
            changed |= Self::note_source(c, Some(m.source as u32), m.time_s, Flags::of(m));
        }
        changed
    }

    fn update(&mut self, m: &Message, now_s: f64, ev: &mut Vec<CallEvent>) {
        for c in self.calls.iter_mut().filter(|c| Self::matches(c, m)) {
            if Self::refresh(c, m, now_s, &self.patches) {
                ev.push(CallEvent::Update(c.clone()));
            }
        }
    }

    fn grant(&mut self, m: &Message, now_s: f64, unit_to_unit: bool, p: Policy, host: &mut dyn RecorderHost, ev: &mut Vec<CallEvent>) {
        if m.freq_hz == 0 {
            return; // channel not resolvable yet (no IDEN seen)
        }
        if let Some(c) = self.calls.iter_mut().find(|c| Self::matches(c, m)) {
            if Self::refresh(c, m, now_s, &self.patches) {
                ev.push(CallEvent::Update(c.clone()));
            }
            return;
        }
        let tg = p.talkgroups.get(&m.talkgroup).cloned();
        let mut c = Call {
            id: self.ids.next(),
            site: Some(self.site),
            system: self.system,
            twin: false,
            talkgroup: m.talkgroup,
            talkgroup_ota: None,
            freq_hz: m.freq_hz,
            // The control channel says (SmartNet), or the talkgroup file does (TR's mode
            // "A": a Phase 1 grant is of analog voice; a two-slot grant keeps its slot).
            voice: match m.voice {
                Voice::P25 if tg.as_ref().is_some_and(|t| t.mode.starts_with('A')) => Voice::Fm,
                v => v,
            },
            unit_to_unit,
            recording: false,
            followed: false,
            reason: None,
            encrypted: m.encrypted || tg.as_ref().is_some_and(|t| t.encrypted_mode()),
            cipher: None,
            decrypted: false,
            emergency: m.emergency,
            priority: m.priority,
            duplex: m.duplex,
            mode: m.mode,
            start_s: m.time_s,
            last_update_s: m.time_s,
            heard_s: now_s,
            last_audio_s: m.time_s,
            sources: if m.source > 0 { vec![CallSource { src: m.source as u32, time_s: m.time_s, audio_pos: None, flags: Flags::of(m), position: None }] } else { vec![] },
            talkgroup_info: tg.clone(),
            patched_talkgroups: self.patches.members_of(m.talkgroup),
            ran: m.ran,
            nac: None,
            tone: None,
            tone_set: None,
        };
        // P25 voice: the site's NAC (its voice channels carry the control channel's).
        if c.voice.decryptable() && m.nac != 0 {
            c.nac = Some(m.nac);
        }
        Self::admit(&mut c, &self.cfg, p, host);
        ev.push(CallEvent::Start(c.clone()));
        self.calls.push(c);
    }

    /// Record, follow or skip a call: Trunk Recorder's start_recorder()
    /// gates, in its order. A talkgroup not in the file is recorded while
    /// it's patched with one that is.
    fn admit(c: &mut Call, cfg: &CallConfig, p: Policy, host: &mut dyn RecorderHost) {
        c.reason = None;
        let known = c.talkgroup_info.is_some() || c.patched_talkgroups.iter().any(|g| p.talkgroups.contains_key(g));
        if c.talkgroup_info.as_ref().is_some_and(|t| t.ignore) {
            c.reason = Some(Reason::Ignored);
        } else if !known && !cfg.record_unknown && !p.talkgroups.is_empty() {
            c.reason = Some(Reason::UnknownTg);
        } else if c.encrypted && !cfg.record_encrypted && !(p.can_decrypt && c.voice.decryptable()) {
            // No audio to record, but its terminators' link control is in
            // the clear: who spoke, and their talker aliases.
            c.reason = Some(Reason::Encrypted);
            c.followed = host.follow(c);
        } else {
            match host.record(c) {
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
        fn record(&mut self, _: &Call) -> Result<(), Reason> {
            Ok(())
        }
        fn release(&mut self, _: &Call) {}
    }

    /// A one-site system: its call manager, with the policy it is handed.
    struct Sys {
        m: CallManager,
        tgs: Talkgroups,
        can_decrypt: bool,
    }
    impl Sys {
        fn new(cfg: CallConfig, tgs: Talkgroups) -> Self {
            Sys { m: CallManager::new(cfg), tgs, can_decrypt: false }
        }
        /// `msgs`, taken in as they were on the air (no control channel delay).
        fn handle(&mut self, msgs: &[Message], host: &mut Host, ev: &mut Vec<CallEvent>) {
            let now_s = msgs.iter().map(|m| m.time_s).fold(0.0, f64::max);
            self.handle_at(msgs, now_s, host, ev);
        }
        fn handle_at(&mut self, msgs: &[Message], now_s: f64, host: &mut Host, ev: &mut Vec<CallEvent>) {
            self.m.handle(msgs, now_s, Policy { talkgroups: &self.tgs, can_decrypt: self.can_decrypt }, host, ev);
        }
    }
    impl std::ops::Deref for Sys {
        type Target = CallManager;
        fn deref(&self) -> &CallManager {
            &self.m
        }
    }
    impl std::ops::DerefMut for Sys {
        fn deref_mut(&mut self) -> &mut CallManager {
            &mut self.m
        }
    }

    fn msg(kind: MessageType, t: f64, tg: u32) -> Message {
        Message { kind, time_s: t, talkgroup: tg, freq_hz: 851_000_000, source: 7, ..Default::default() }
    }
    fn patch(t: f64, sg: u32, ga: [u32; 3]) -> Message {
        Message { kind: MessageType::PatchAdd, time_s: t, patch: Some(Patch { sg, ga }), ..Default::default() }
    }
    /// Talkgroup 101 is in the file; supergroups aren't.
    fn manager() -> Sys {
        let tgs = [(101, Talkgroup { number: 101, alpha_tag: "Fire Disp".into(), ..Default::default() })].into_iter().collect();
        Sys::new(CallConfig { record_unknown: false, ..Default::default() }, tgs)
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
    fn the_timeout_counts_from_when_an_update_comes_in_not_its_air_time() {
        // The engine's blocks, 50 ms apart: the messages that came in, then the clock.
        // Each message comes off the control channel 450 ms after it was on the air.
        let mut m = Sys::new(CallConfig { call_timeout_s: 1.0, ..Default::default() }, Talkgroups::default());
        let mut ev = Vec::new();
        let sent_ms = [(MessageType::Grant, 0), (MessageType::Update, 800), (MessageType::Update, 1600)];
        let (mut up, mut ended_ms) = (false, None);
        for now_ms in (50..=5000).step_by(50) {
            let came_in = |t: i64| (now_ms - 50..now_ms).contains(&(t + 450));
            let due: Vec<Message> = sent_ms.iter().filter(|(_, t)| came_in(*t)).map(|(kind, t)| msg(*kind, *t as f64 / 1000.0, 101)).collect();
            m.handle_at(&due, now_ms as f64 / 1000.0, &mut Host, &mut ev);
            m.tick(now_ms as f64 / 1000.0, &mut Host, &mut ev);
            up |= !m.calls.is_empty();
            if up && m.calls.is_empty() && ended_ms.is_none() {
                ended_ms = Some(now_ms);
            }
        }
        // The last update (1.6 s on the air) comes in with the block to 2.1 s: the call ends 1 s
        // after that. Counted from air time, it ended at 1.05 s, before its first update came in.
        assert_eq!(ended_ms, Some(3150));
    }

    #[test]
    fn a_call_past_the_length_limit_carries_on_in_a_new_one() {
        let mut m = Sys::new(CallConfig { max_call_s: 10.0, ..Default::default() }, Talkgroups::default());
        let mut ev = Vec::new();
        m.handle(&[msg(MessageType::Grant, 0.0, 101)], &mut Host, &mut ev);
        for k in 1..=12 {
            m.handle(&[msg(MessageType::Update, k as f64, 101)], &mut Host, &mut ev);
            let id = m.calls[0].id;
            m.note_audio(id, k as f64);
            m.tick(k as f64, &mut Host, &mut ev);
        }
        let ends: Vec<(CallId, f64)> = ev.iter().filter_map(|e| if let CallEvent::End(c) = e { Some((c.id, c.start_s)) } else { None }).collect();
        assert_eq!(ends, [(1, 0.0)]);
        assert_eq!((m.calls.len(), m.calls[0].id, m.calls[0].start_s), (1, 2, 10.0));
        assert!(m.calls[0].recording && m.calls[0].sources[0].src == 7);
    }

    #[test]
    fn encrypted_calls_are_recorded_when_the_system_may_decrypt_them() {
        let enc = |t: f64, tg: u32, phase2: bool| Message { encrypted: true, voice: if phase2 { Voice::P25Tdma { slot: 0 } } else { Voice::P25 }, ..msg(MessageType::Grant, t, tg) };
        let mut m = Sys::new(CallConfig::default(), Talkgroups::default());
        let mut ev = Vec::new();
        m.handle(&[enc(0.0, 101, false)], &mut Host, &mut ev);
        assert_eq!(m.calls[0].reason, Some(Reason::Encrypted));
        m.can_decrypt = true;
        // P25: recorded, to see whether a key fits; DMR not yet.
        let dmr = Message { voice: Voice::Dmr { slot: 0, color_code: Some(1) }, ..enc(0.0, 104, false) };
        m.handle(&[enc(0.0, 102, false), enc(0.0, 103, true), dmr], &mut Host, &mut ev);
        assert!(m.calls[1].recording && m.calls[1].reason.is_none());
        assert!(m.calls[2].recording && m.calls[2].reason.is_none());
        assert_eq!(m.calls[3].reason, Some(Reason::Encrypted));
    }

    #[test]
    fn an_ignored_talkgroup_is_not_recorded() {
        let tgs = [(101, Talkgroup { number: 101, ignore: true, ..Default::default() })].into_iter().collect();
        let mut m = Sys::new(CallConfig::default(), tgs);
        let mut ev = Vec::new();
        m.handle(&[msg(MessageType::Grant, 0.0, 101), msg(MessageType::Grant, 0.0, 102)], &mut Host, &mut ev);
        assert_eq!((m.calls[0].recording, m.calls[0].reason), (false, Some(Reason::Ignored)));
        assert!(m.calls[1].recording, "talkgroups not in the file are as before");
    }

    #[test]
    fn the_call_json_lists_the_patch() {
        let mut m = manager();
        let mut ev = Vec::new();
        m.handle(&[patch(0.0, 65001, [101, 0, 0]), msg(MessageType::Grant, 0.5, 65001), msg(MessageType::Grant, 0.5, 303)], &mut Host, &mut ev);
        let errors = Default::default();
        let info = ConcludeInfo { short_name: "s", system_type: "p25", site: "", epoch_ms_at_zero: 0.0, audio_seconds: 1.0, errors: &errors, recorder_num: 0, end_s: 1.0, units: None, unit_tags: None, freq_error_hz: 0, reception: Default::default() };
        assert!(call_record(&m.calls[0], &info).0.to_json().contains("\"short_name\":\"s\",\"patched_talkgroups\":[101,65001],\"freqList\""));
        assert!(!call_record(&m.calls[1], &info).0.to_json().contains("patched"));
    }

    /// Each radio keeps what its own transmission said: the grant's options
    /// for the one it names, the voice channel's for the rest, and a word
    /// naming no one is about whoever is talking.
    #[test]
    fn each_source_keeps_its_own_flags() {
        let mut m = Sys::new(CallConfig::default(), Talkgroups::default());
        let mut ev = Vec::new();
        let grant = Message { emergency: true, duplex: true, priority: 7, ..msg(MessageType::Grant, 0.0, 101) };
        m.handle(&[grant], &mut Host, &mut ev);
        let c = &mut m.calls[0];
        assert_eq!(c.sources[0].flags, Flags { emergency: true, duplex: true, priority: 7, ..Default::default() });
        assert!(CallManager::note_source(c, Some(8), 1.0, Flags { encrypted: true, priority: 4, ..Default::default() }));
        assert!(CallManager::note_source(c, None, 1.5, Flags::emergency(true)), "a new flag for the one talking");
        assert!(!CallManager::note_source(c, Some(8), 2.0, Flags::emergency(true)), "nothing new");
        let flags: Vec<(u32, Flags)> = c.sources.iter().map(|s| (s.src, s.flags)).collect();
        assert_eq!(flags, [(7, Flags { emergency: true, duplex: true, priority: 7, ..Default::default() }), (8, Flags { emergency: true, encrypted: true, priority: 4, ..Default::default() })]);
    }

    /// The talkgroup file's mode "A" (Trunk Recorder's) says a Phase 1 grant is of analog voice; a two-slot grant keeps its slot.
    #[test]
    fn an_analog_talkgroup_keeps_a_two_slot_grants_slot() {
        let tgs = |mode: &str| [(101, Talkgroup { number: 101, mode: mode.into(), ..Default::default() })].into_iter().collect();
        let mut ev = Vec::new();
        let mut m = Sys::new(CallConfig::default(), tgs("A"));
        m.handle(&[Message { voice: Voice::P25Tdma { slot: 1 }, ..msg(MessageType::Grant, 0.0, 101) }], &mut Host, &mut ev);
        assert_eq!(m.m.calls[0].voice, Voice::P25Tdma { slot: 1 });
        m.handle(&[Message { voice: Voice::P25Tdma { slot: 1 }, ..msg(MessageType::Update, 1.0, 101) }], &mut Host, &mut ev);
        assert_eq!((m.m.calls.len(), m.m.calls[0].last_update_s), (1, 1.0), "its update is its own");
        let mut m = Sys::new(CallConfig::default(), tgs("A"));
        m.handle(&[msg(MessageType::Grant, 0.0, 101)], &mut Host, &mut ev);
        assert_eq!(m.m.calls[0].voice, Voice::Fm);
        let mut m = Sys::new(CallConfig::default(), tgs("D"));
        m.handle(&[msg(MessageType::Grant, 0.0, 101)], &mut Host, &mut ev);
        assert_eq!(m.m.calls[0].voice, Voice::P25);
    }
}
