//! A trunked NXDN site: every frequency the config names is watched at once
//! (one 4FSK receiver each, a small fraction of a core), and what they carry
//! becomes [`Message`]s for the call manager:
//!
//! ```text
//! each carrier: C4fm → Framer → by its LICH:
//!   control channel (CAC)         → VCALL_ASSGN / _DUP: Grant / Update on the channel's frequency
//!                                    SITE_INFO / SRV_INFO: the site (system and site code, DFA)
//!                                    ADJ_SITE_INFO: Adjacent
//!   traffic channel (VCALL, SCCH)  → Grant / Update on that carrier
//! ```
//!
//! The two kinds of trunking:
//!
//! * Type-C (Kenwood NEXEDGE, Icom IDAS Type-C): a control channel assigns
//!   calls to channels. A channel is a 10-bit number, whose frequency comes
//!   from the config's channel table, from Direct Frequency Assignment
//!   (SITE_INFO says so; base + number × step), or is learned: a grant for an
//!   unknown channel, then the same group's VCALL on a watched carrier
//!   within a few seconds, ties the two together.
//! * Type-D (Icom IDAS / NEXEDGE distributed trunking): no control channel.
//!   Each repeater announces itself idle, busy or where a call went in its
//!   SCCH; every listed repeater is watched, so a call is found where it is.
//!
//! The learned table is kept like a band plan ([`Site::map_to_string`]).

use std::collections::BTreeMap;

use num_complex::Complex32;

use super::channel::{self, SacchAssembler, Scch};
use super::frame::{Body, Frame, Framer, Lich, RfChannel};
use super::layer3::{CallHead, Channel, ChannelAccess, Context, Location, Message as L3};
use super::Rate;
use crate::dsp::c4fm::C4fm;
use crate::dsp::{Receiver, Symbol};
use crate::trunk::engine::NoteLevel;
use crate::trunk::identity::{IdField, Identity};
use crate::trunk::message::{Message, MessageType};

/// A grant waits this long for its group to show up on a watched carrier.
const LEARN_WINDOW_S: f64 = 4.0;
/// A call on a carrier is refreshed (an Update) at most this often.
const CALL_REFRESH_S: f64 = 1.0;
/// A carrier counts as a control channel this long after its last CAC.
const CONTROL_HOLD_S: f64 = 2.0;
/// A carrier's call shows on the dashboard this long after it was last heard.
const CALL_SHOW_S: f64 = 3.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// A control channel assigns traffic channels.
    #[default]
    TypeC,
    /// Distributed: no control channel (IDAS Type-D).
    TypeD,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::TypeC => "NXDN Type-C",
            Kind::TypeD => "NXDN Type-D",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Kind::TypeC => "typeC",
            Kind::TypeD => "typeD",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NxdnConfig {
    pub kind: Kind,
    /// The control channel's (Type-D: the repeaters') rate.
    pub rate: Rate,
    /// Channel number → frequency (Type-D: repeater number → frequency); wins over what is learned.
    pub channel_table: BTreeMap<u32, u64>,
    /// Voice frequencies to watch besides the control channels (all of a
    /// Type-D site's repeaters go here or in the control channel list).
    pub channels: Vec<f64>,
    /// Only frames with this RAN.
    pub ran: Option<u8>,
}

impl Default for NxdnConfig {
    fn default() -> Self {
        NxdnConfig { kind: Kind::TypeC, rate: Rate::N48, channel_table: BTreeMap::new(), channels: Vec::new(), ran: None }
    }
}

/// An NXDN site, for the dashboard ([`Site::status`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteStatus {
    pub kind: Kind,
    pub rate: Option<Rate>,
    /// From SITE_INFO / SRV_INFO: (category, system code, site code).
    pub location: Option<(u8, u32, u32)>,
    pub ran: Option<u8>,
    /// SITE_INFO's channel access: Direct Frequency Assignment (base Hz, step Hz).
    pub dfa: Option<(u64, u32)>,
    /// Channel number → frequency, configured or learned.
    pub channels: Vec<ChannelEntry>,
    /// Channel numbers granted whose frequency isn't known.
    pub unknown: Vec<u32>,
    pub carriers: Vec<CarrierStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelEntry {
    pub number: u32,
    pub hz: u64,
    /// From the config's table (else learned from the air).
    pub configured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarrierStatus {
    pub hz: u64,
    /// Sending CAC now (the control channel).
    pub control: bool,
    pub ran: Option<u8>,
    /// Its call now: (group or unit, source; 0 = not named yet).
    pub call: Option<(u32, u32)>,
    /// Type-D: the repeater number it calls itself, when it has said.
    pub repeater: Option<u8>,
}

/// What a traffic carrier's frames have said about its call.
#[derive(Clone, Copy, Debug, Default)]
struct CallState {
    tg: Option<u32>,
    group: bool,
    src: Option<u32>,
    encrypted: bool,
    emergency: bool,
    rate9600: Option<bool>,
}

pub struct Carrier {
    pub hz: u64,
    rx: C4fm,
    rate: f64,
    framer: Framer,
    syms: Vec<Symbol>,
    frames: Vec<Frame>,
    sf: SacchAssembler,
    state: CallState,
    /// The call last reported (group, source, when).
    last: Option<(u32, u32, f64)>,
    pub control_s: f64,
    /// Last frame with a valid LICH heard on it, s.
    pub heard_s: f64,
    pub ran: Option<u8>,
    repeater: Option<u8>,
    good: u64,
    bad: u64,
}

struct Pending {
    channel: u32,
    tg: u32,
    t: f64,
}

pub struct Site {
    pub carriers: Vec<Carrier>,
    cfg: NxdnConfig,
    /// Channel number → frequency, learned from the air.
    pub learned: BTreeMap<u32, u64>,
    pending: Vec<Pending>,
    unknown: std::collections::BTreeSet<u32>,
    access: Option<ChannelAccess>,
    location: Option<Location>,
    /// The site's RAN: configured, else the first CAC's.
    pub ran: Option<u8>,
    n_control: usize,
    notes: Vec<(NoteLevel, String)>,
    now_s: f64,
    identity: Identity,
}

fn mhz(hz: u64) -> String {
    format!("{:.5} MHz", hz as f64 / 1e6)
}

impl Site {
    pub fn new(freqs: &[f64], rate: f64, cfg: NxdnConfig) -> Self {
        let n_control = freqs.len();
        let mut seen = std::collections::HashSet::new();
        let hzs: Vec<u64> = freqs.iter().chain(cfg.channels.iter()).map(|f| f.round() as u64).filter(|h| seen.insert(*h)).collect();
        let carriers = hzs
            .into_iter()
            .map(|hz| Carrier {
                hz,
                rx: cfg.rate.receiver(rate),
                rate,
                framer: Framer::new(),
                syms: Vec::new(),
                frames: Vec::new(),
                sf: SacchAssembler::default(),
                state: CallState::default(),
                last: None,
                control_s: f64::NEG_INFINITY,
                heard_s: f64::NEG_INFINITY,
                ran: None,
                repeater: None,
                good: 0,
                bad: 0,
            })
            .collect();
        Site {
            carriers,
            ran: cfg.ran,
            cfg,
            learned: BTreeMap::new(),
            pending: Vec::new(),
            unknown: Default::default(),
            access: None,
            location: None,
            n_control,
            notes: Vec::new(),
            now_s: 0.0,
            identity: Identity::default(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.cfg.kind
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Frames (with a valid LICH) decoded and lost, over every carrier.
    pub fn counts(&self) -> (u64, u64) {
        (self.carriers.iter().map(|c| c.good).sum(), self.carriers.iter().map(|c| c.bad).sum())
    }

    pub fn status(&self) -> SiteStatus {
        let mut channels: Vec<ChannelEntry> = self.cfg.channel_table.iter().map(|(&number, &hz)| ChannelEntry { number, hz, configured: true }).collect();
        channels.extend(self.learned.iter().filter(|(n, _)| !self.cfg.channel_table.contains_key(n)).map(|(&number, &hz)| ChannelEntry { number, hz, configured: false }));
        channels.sort_by_key(|c| c.number);
        let carriers = self
            .carriers
            .iter()
            .map(|c| CarrierStatus {
                hz: c.hz,
                control: self.now_s - c.control_s < CONTROL_HOLD_S,
                ran: c.ran,
                call: c.last.filter(|l| self.now_s - l.2 < CALL_SHOW_S).map(|l| (l.0, l.1)),
                repeater: c.repeater,
            })
            .collect();
        SiteStatus {
            kind: self.cfg.kind,
            rate: Some(self.cfg.rate),
            location: self.location.map(|l| (l.category(), l.system(), l.site())),
            ran: self.ran,
            dfa: self.access.filter(|a| a.dfa).map(|a| (a.base_hz, a.step_hz)),
            channels,
            unknown: self.unknown.iter().copied().filter(|n| self.resolve(*n).is_none()).collect(),
            carriers,
        }
    }

    pub fn take_notes(&mut self) -> Vec<(NoteLevel, String)> {
        std::mem::take(&mut self.notes)
    }

    /// The frequency of channel `n`: configured, else heard.
    pub fn resolve(&self, n: u32) -> Option<u64> {
        self.cfg.channel_table.get(&n).or_else(|| self.learned.get(&n)).copied()
    }

    /// The carrier sending CAC now; on Type-D (no control channel), the
    /// last one heard, so the site doesn't look like it is hunting.
    pub fn control_hz(&self) -> Option<u64> {
        let at = |c: &Carrier| if self.cfg.kind == Kind::TypeD { c.heard_s } else { c.control_s };
        self.carriers.iter().filter(|c| self.now_s - at(c) < CONTROL_HOLD_S).max_by(|a, b| at(a).total_cmp(&at(b))).map(|c| c.hz)
    }

    /// The learned table as `channel=hz` lines.
    pub fn map_to_string(&self) -> String {
        self.learned.iter().map(|(l, h)| format!("{l}={h}\n")).collect()
    }
    pub fn map_from_str(&mut self, s: &str) {
        for line in s.lines() {
            if let Some((l, h)) = line.split_once('=') {
                if let (Ok(l), Ok(h)) = (l.trim().parse(), h.trim().parse()) {
                    self.learned.insert(l, h);
                }
            }
        }
    }

    /// Carrier `idx`'s channel IQ; `t0` + sample / `rate` is a sample's air time (s).
    pub fn push(&mut self, idx: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Message>) {
        let c = &mut self.carriers[idx];
        if c.rate != rate {
            (c.rx, c.rate) = (self.cfg.rate.receiver(rate), rate);
        }
        c.syms.clear();
        c.rx.push(iq, &mut c.syms);
        c.frames.clear();
        for s in &c.syms {
            c.framer.push(s, &mut c.frames);
        }
        let frames = std::mem::take(&mut c.frames);
        for f in &frames {
            let t = t0 + f.sample / rate;
            self.now_s = self.now_s.max(t);
            self.frame(idx, f, t, out);
        }
        self.carriers[idx].frames = frames;
        self.pending.retain(|p| self.now_s - p.t < LEARN_WINDOW_S);
    }

    fn frame(&mut self, idx: usize, f: &Frame, t: f64, out: &mut Vec<Message>) {
        let Some((lich, _)) = f.lich() else {
            self.carriers[idx].bad += 1;
            return;
        };
        self.carriers[idx].heard_s = t;
        if !lich.outbound() {
            // A radio's own transmission (heard on a repeater input): not the site's.
            return;
        }
        // The control channel; on Type-C also a composite one (its CRC decides).
        let try_cac = lich.is_cac() || (self.cfg.kind == Kind::TypeC && lich.rf() == RfChannel::Composite && lich.fct() == 0);
        if try_cac {
            if let Some((sr, o, _)) = channel::cac(f) {
                self.carriers[idx].good += 1;
                if !self.ran_ok(idx, sr.ran) {
                    return;
                }
                self.carriers[idx].control_s = t;
                let dfa = self.access.is_some_and(|a| a.dfa);
                let first = L3::parse(&o, Context::Control, dfa);
                self.control(idx, first, t, out);
                if sr.structure & 1 != 0 {
                    let second = L3::parse(&o[9..], Context::Control, dfa);
                    self.control(idx, second, t, out);
                }
                return;
            }
            if lich.is_cac() {
                self.carriers[idx].bad += 1;
                return;
            }
        }
        self.carriers[idx].good += 1;
        self.traffic(idx, lich, f, t, out);
    }

    /// Whether a frame with RAN `ran` belongs to the site; the first control
    /// channel's RAN becomes the site's.
    fn ran_ok(&mut self, idx: usize, ran: u8) -> bool {
        self.carriers[idx].ran = Some(ran);
        match self.ran {
            Some(want) => ran == want,
            None if idx < self.n_control => {
                self.ran = Some(ran);
                self.notes.push((NoteLevel::Info, format!("RAN {ran}")));
                true
            }
            None => true,
        }
    }

    fn control(&mut self, idx: usize, m: L3, t: f64, out: &mut Vec<Message>) {
        match m {
            L3::SiteInfo { location, access, control, .. } => {
                if self.location != Some(location) {
                    self.notes.push((NoteLevel::Info, format!("Site: system {:X} site {} (category {})", location.system(), location.site(), location.category())));
                    for (i, &cc) in control.iter().enumerate().filter(|(_, c)| **c != 0) {
                        let hz = self.resolve(cc as u32).map_or_else(|| "frequency not known".into(), mhz);
                        self.notes.push((NoteLevel::Info, format!("Control channel {}: channel {cc} ({hz})", i + 1)));
                    }
                }
                self.set_location(location);
                if self.access != Some(access) {
                    self.access = Some(access);
                    if access.dfa {
                        self.notes.push((NoteLevel::Info, format!("Direct frequency assignment: base {} step {} Hz", mhz(access.base_hz), access.step_hz)));
                    }
                }
                // The channel the site's first control channel number names is this carrier, when it is the only one heard sending CAC.
                if control[0] != 0 && self.resolve(control[0] as u32).is_none() && !self.access.is_some_and(|a| a.dfa) {
                    let hz = self.carriers[idx].hz;
                    let others = self.carriers.iter().enumerate().any(|(i, c)| i != idx && self.now_s - c.control_s < CONTROL_HOLD_S);
                    if !others {
                        self.set_learned(control[0] as u32, hz, "the control channel says it is");
                    }
                }
            }
            L3::SrvInfo { location, .. } => self.set_location(location),
            L3::VCallAssgn { head, channel, .. } => self.grant(idx, &head, channel, false, t, out),
            L3::VCallAssgnDup { head, channel, .. } => self.grant(idx, &head, channel, true, t, out),
            L3::AdjSiteInfo { sites } => {
                for (loc, _, ch) in sites {
                    let hz = self.resolve(ch as u32).unwrap_or(0);
                    let mut m = self.base(MessageType::Adjacent, t);
                    m.sys_id = loc.system();
                    m.site = loc.site();
                    m.freq_hz = hz;
                    m.meta = format!("Adjacent site: system {:X} site {} on channel {ch}", loc.system(), loc.site());
                    out.push(m);
                }
            }
            _ => {}
        }
    }

    fn set_location(&mut self, l: Location) {
        self.location = Some(l);
        self.identity.set(IdField::SysId, l.system());
        self.identity.set(IdField::Site, l.site());
    }

    fn base(&self, kind: MessageType, t: f64) -> Message {
        Message { kind, time_s: t, nxdn: Some(self.cfg.rate), ran: self.ran, sys_id: self.location.map_or(0, |l| l.system()), site: self.location.map_or(0, |l| l.site()), ..Default::default() }
    }

    fn set_learned(&mut self, n: u32, hz: u64, why: &str) {
        if self.cfg.channel_table.contains_key(&n) || self.learned.get(&n) == Some(&hz) {
            return;
        }
        // A frequency is one channel: drop what said otherwise.
        self.learned.retain(|_, h| *h != hz);
        self.learned.insert(n, hz);
        self.notes.push((NoteLevel::Info, format!("Channel {n} is {} ({why})", mhz(hz))));
    }

    /// A group's call on a watched carrier: the frequency of a channel a recent grant sent it to.
    fn learn(&mut self, tg: u32, hz: u64) {
        if let Some(i) = self.pending.iter().position(|p| p.tg == tg) {
            let p = self.pending.remove(i);
            if self.resolve(p.channel).is_none() || (self.learned.contains_key(&p.channel) && !self.cfg.channel_table.contains_key(&p.channel)) {
                self.set_learned(p.channel, hz, &format!("group {tg} granted there came up on it"));
            }
        }
    }

    fn grant(&mut self, _idx: usize, head: &CallHead, channel: Channel, dup: bool, t: f64, out: &mut Vec<Message>) {
        let tg = head.destination as u32;
        if tg == 0 {
            return;
        }
        let (hz, rate, number) = match channel {
            Channel::Number(n) => {
                let hz = self.resolve(n as u32);
                if hz.is_none() {
                    self.pending.retain(|p| p.channel != n as u32);
                    self.pending.push(Pending { channel: n as u32, tg, t });
                    if self.unknown.insert(n as u32) {
                        self.notes.push((NoteLevel::Warning, format!("Channel {n} isn't in the channel table: listening for the call on the watched frequencies")));
                    }
                }
                (hz, if head.rate_9600() { Rate::N96 } else { Rate::N48 }, n as u32)
            }
            Channel::Dfa { ofn, bandwidth, .. } => {
                (self.access.and_then(|a| a.dfa_hz(ofn)).map(|h| h.round() as u64), if bandwidth == 1 { Rate::N96 } else { Rate::N48 }, ofn as u32)
            }
        };
        let kind = match (head.group(), dup) {
            (true, false) => MessageType::Grant,
            (true, true) => MessageType::Update,
            (false, false) => MessageType::UuVGrant,
            (false, true) => MessageType::UuVUpdate,
        };
        let mut m = self.base(kind, t);
        m.freq_hz = hz.unwrap_or(0);
        m.talkgroup = tg;
        m.source = if head.source != 0 { head.source as i64 } else { -1 };
        m.emergency = head.emergency();
        m.nxdn = Some(rate);
        m.opcode = if dup { super::layer3::VCALL_ASSGN_DUP } else { super::layer3::VCALL_ASSGN };
        m.meta = format!(
            "{}: {} {tg} from {} on channel {number} ({})",
            if dup { "VCALL_ASSGN_DUP" } else { "VCALL_ASSGN" },
            if head.group() { "group" } else { "unit" },
            head.source,
            hz.map_or("frequency not known yet".into(), mhz)
        );
        out.push(m);
    }

    /// A traffic frame on carrier `idx`.
    fn traffic(&mut self, idx: usize, lich: Lich, f: &Frame, t: f64, out: &mut Vec<Message>) {
        let mut msgs = Vec::new();
        match lich.body() {
            Body::Voice { superframe, facch, idle } => {
                if lich.rf() == RfChannel::Composite {
                    if let Some(s) = channel::scch(f) {
                        self.scch(idx, &s);
                    }
                } else if let Some(s) = channel::sacch(f) {
                    if !self.ran_ok(idx, s.sr.ran) {
                        return;
                    }
                    if superframe {
                        if let Some(o) = self.carriers[idx].sf.push(&s) {
                            msgs.push(L3::parse(&o, Context::Traffic, false));
                        }
                    }
                } else {
                    self.carriers[idx].sf.miss();
                }
                for h in 0..2 {
                    if facch[h] {
                        if let Some((o, _)) = channel::facch1(f, h) {
                            msgs.push(L3::parse(&o, Context::Traffic, false));
                        }
                    }
                }
                for m in msgs {
                    self.traffic_message(idx, m);
                }
                // Voice on the air: the call is here.
                if !idle && !(facch[0] && facch[1]) {
                    self.call_heard(idx, t, out);
                }
            }
            Body::Facch2 => {
                if let Some((_, o, _)) = channel::udch(f) {
                    self.traffic_message(idx, L3::parse(&o, Context::Traffic, false));
                }
            }
            Body::Udch => {}
        }
    }

    fn traffic_message(&mut self, idx: usize, m: L3) {
        let st = &mut self.carriers[idx].state;
        match m {
            L3::VCall { head, cipher, .. } => {
                if head.destination != 0 {
                    st.tg = Some(head.destination as u32);
                }
                st.group = head.group();
                st.src = (head.source != 0).then_some(head.source as u32);
                st.encrypted = cipher != 0;
                st.emergency = head.emergency();
                st.rate9600 = Some(head.rate_9600());
            }
            L3::TxRel { .. } | L3::TxRelEx { .. } | L3::Disc { .. } => {
                self.carriers[idx].state = CallState::default();
                self.carriers[idx].last = None;
            }
            _ => {}
        }
    }

    fn scch(&mut self, idx: usize, s: &Scch) {
        let special = (2041..=2046).contains(&s.short_id());
        let c = &mut self.carriers[idx];
        match s.info() {
            1 => c.state.encrypted = s.cipher() != 0,
            2 | 4 if s.short_id() == 2046 || s.short_id() == 2044 || s.short_id() == 2041 => {
                // Idle / free repeater / site ID: no call on it.
                if s.short_id() == 2046 {
                    c.state = CallState::default();
                    c.last = None;
                }
            }
            2 | 4 if !special && s.short_id() != 0 => {
                if s.repeater() == 31 {
                    c.state = CallState::default();
                    c.last = None;
                    return;
                }
                if s.info() == 2 && s.repeater() != 0 && c.repeater != Some(s.repeater()) {
                    // INFO2 on a call's own repeater names it as the repeater in use.
                    c.repeater = Some(s.repeater());
                }
                c.state.tg = Some(s.id() as u32);
                c.state.group = s.group();
            }
            3 if s.short_id() != 0 => c.state.src = Some(s.id() as u32),
            _ => {}
        }
    }

    /// Voice on carrier `idx`: report its call once the group is known.
    fn call_heard(&mut self, idx: usize, t: f64, out: &mut Vec<Message>) {
        let st = self.carriers[idx].state;
        let Some(tg) = st.tg else { return };
        let hz = self.carriers[idx].hz;
        self.learn(tg, hz);
        let c = &mut self.carriers[idx];
        let src = st.src.unwrap_or(0);
        let prev = c.last;
        let repeat = prev.is_some_and(|(g, s, _)| g == tg && (s == src || src == 0));
        if repeat && prev.is_some_and(|(_, _, at)| t - at < CALL_REFRESH_S) {
            return;
        }
        c.last = Some((tg, if src != 0 { src } else { prev.map_or(0, |p| p.1) }, t));
        let kind = match (st.group, repeat) {
            (true, false) => MessageType::Grant,
            (true, true) => MessageType::Update,
            (false, false) => MessageType::UuVGrant,
            (false, true) => MessageType::UuVUpdate,
        };
        let rate = match st.rate9600 {
            Some(true) => Rate::N96,
            Some(false) => Rate::N48,
            None => self.cfg.rate,
        };
        let mut m = self.base(kind, t);
        m.ran = self.carriers[idx].ran.or(self.ran);
        m.freq_hz = hz;
        m.talkgroup = tg;
        m.source = if src != 0 { src as i64 } else { -1 };
        m.encrypted = st.encrypted;
        m.emergency = st.emergency;
        m.nxdn = Some(rate);
        m.meta = format!("Voice: {} {tg} from {src} on {}{}", if st.group { "group" } else { "unit" }, mhz(hz), if st.encrypted { " (encrypted)" } else { "" });
        out.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nxdn::channel::{build as cb, Sr};
    use crate::nxdn::layer3::{build as l3, CALL_CONFERENCE, CALL_INDIVIDUAL};
    use crate::nxdn::synth::{ambe, cac_frame, dibits, modulate, voice_frame, Half, Tx};

    fn head(tg: u16, src: u16, call_type: u8) -> CallHead {
        CallHead { cc_option: 0, call_type, option: 0, source: src, destination: tg }
    }

    /// IQ for carrier frames (with random symbols before, to settle the receiver).
    fn iq_of(frames: &[[u8; 192]], rate: Rate, fs: f64) -> Vec<Complex32> {
        let mut x = 7u32;
        let mut d: Vec<u8> = (0..600)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 30) as u8
            })
            .collect();
        d.extend(dibits(frames));
        d.extend((0..1200).map(|i| (i % 4) as u8));
        let mut ph = 0.0;
        modulate(&d, None, rate, fs, 0.0, 1.0, &mut ph)
    }

    fn run(site: &mut Site, carriers: &[Vec<Complex32>], fs: f64) -> Vec<Message> {
        let mut out = Vec::new();
        let n = carriers.iter().map(Vec::len).max().unwrap();
        let block = 4800;
        let mut at = 0;
        while at < n {
            for (i, iq) in carriers.iter().enumerate() {
                if at < iq.len() {
                    let end = (at + block).min(iq.len());
                    site.push(i, &iq[at..end], at as f64 / fs, fs, &mut out);
                }
            }
            at += block;
        }
        out
    }

    fn control_frames(msgs: &[Vec<u8>], ran: u8) -> Vec<[u8; 192]> {
        msgs.iter().map(|o| cac_frame(Sr { structure: 2, ran }, o)).collect()
    }

    #[test]
    fn type_c_grants_resolve_by_table_dfa_and_learning() {
        let fs = 24_000.0;
        let (cc, voice) = (451_018_750.0, 451_118_750.0);
        let site_info = l3::site_info(0x123 << 12 | 0x045, 0x0200, 0, 0, [1, 0]);
        let g = |ch: u16| l3::vcall_assgn(false, &head(3001, 77, CALL_CONFERENCE), 4, ch);
        // Channel 12 in the table; channel 20 not.
        let mut cfg = NxdnConfig::default();
        cfg.channel_table.insert(12, 451_200_000);
        cfg.channels = vec![voice];
        let mut site = Site::new(&[cc], fs, cfg);
        let mut msgs = vec![site_info.clone(), g(12), g(20)];
        msgs.extend(std::iter::repeat_n(l3::idle(), 20));
        let ctrl = iq_of(&control_frames(&msgs, 5), Rate::N48, fs);
        // Group 3001's call comes up on the watched voice frequency (channel 20, unknown).
        let t = Tx { rate: Rate::N48, ran: 5, head: head(3001, 77, CALL_CONFERENCE), cipher: 0, superframes: 3, rf: 1, outbound: true };
        let mut vframes = vec![voice_frame(0x39, Sr { structure: 0, ran: 5 }, 0, &[Half::Facch1(l3::idle()), Half::Facch1(l3::idle())]); 4];
        vframes.extend(t.frames());
        let vc = iq_of(&vframes, Rate::N48, fs);
        let out = run(&mut site, &[ctrl, vc], fs);
        let grants: Vec<(MessageType, u64, u32)> = out.iter().filter(|m| m.kind != MessageType::Adjacent).map(|m| (m.kind, m.freq_hz, m.talkgroup)).collect();
        assert!(grants.contains(&(MessageType::Grant, 451_200_000, 3001)), "{grants:?}");
        assert!(grants.contains(&(MessageType::Grant, 0, 3001)), "{grants:?}");
        // The call on the voice carrier: a grant there, and channel 20 learned.
        assert!(grants.iter().any(|g| g.1 == voice as u64), "{grants:?}");
        assert_eq!(site.resolve(20), Some(voice as u64));
        assert_eq!(site.identity().sys_id(), Some(0x123));
        assert_eq!(site.ran, Some(5));
        let st = site.status();
        assert_eq!(st.location, Some((0, 0x123, 0x045)));
        assert!(st.unknown.is_empty());
        // DFA: grants carry the frequency.
        let mut site = Site::new(&[cc], fs, NxdnConfig::default());
        let dfa = 1 << 23 | 2 << 21 | 3 << 18;
        let msgs = vec![l3::site_info(0x123 << 12 | 0x045, 0x0200, dfa, 0, [1, 0]), l3::vcall_assgn_dfa(&head(55, 9, CALL_INDIVIDUAL), 4, 40_100, 39_000, 1), l3::idle(), l3::idle(), l3::idle()];
        let out = run(&mut site, &[iq_of(&control_frames(&msgs, 5), Rate::N48, fs)], fs);
        let g = out.iter().find(|m| m.kind == MessageType::UuVGrant).expect("unit grant");
        assert_eq!((g.freq_hz, g.talkgroup, g.nxdn), (450_125_000, 55, Some(Rate::N96)));
    }

    /// A Type-D repeater: idle messages, then a call whose who is in the SCCH.
    #[test]
    fn type_d_calls_come_from_the_scch() {
        let fs = 24_000.0;
        let rep = 452_312_500.0;
        let cfg = NxdnConfig { kind: Kind::TypeD, ..Default::default() };
        let mut site = Site::new(&[rep], fs, cfg);
        let sr = |s: u8| s;
        let tg: u16 = 3 << 11 | 101;
        let src: u16 = 3 << 11 | 1234;
        let idle = cb::scch_id(0, 2046, false);
        let mut frames = Vec::new();
        for k in 0..4 {
            frames.push(scch_frame(0x7f, sr(3 - k), if k == 3 { idle } else { 0 }, None));
        }
        for n in 0..12usize {
            let k = n % 4;
            let data = match k {
                0 => cb::scch_info1(0, 0, 0, 0, 0),
                1 | 3 => cb::scch_id(5, tg, true),
                _ => cb::scch_id(0, src, false),
            };
            frames.push(scch_frame(0x77, 3 - k as u8, data, Some(n)));
        }
        let out = run(&mut site, &[iq_of(&frames, Rate::N48, fs)], fs);
        let m = out.iter().find(|m| m.kind == MessageType::Grant).expect("a grant");
        assert_eq!((m.freq_hz, m.talkgroup), (rep as u64, tg as u32));
        // INFO3 names the source a frame later.
        assert!(out.iter().any(|m| m.talkgroup == tg as u32 && m.source == src as i64), "{out:?}");
        assert_eq!(site.status().carriers[0].repeater, Some(5));
    }

    /// A Type-D frame: LICH, SCCH, and voice (or idle).
    fn scch_frame(lich: u8, structure: u8, data: u32, voice: Option<usize>) -> [u8; 192] {
        use crate::nxdn::channel::{HALF_AT, SACCH_AT};
        use crate::nxdn::frame::{frame_dibits, Lich, BODY_SYMBOLS};
        let mut body = [0u8; BODY_SYMBOLS];
        body[..8].copy_from_slice(&Lich { raw: lich }.dibits());
        let bits = cb::scch(structure, false, data);
        for (i, &b) in bits.iter().enumerate() {
            let k = SACCH_AT + i;
            body[k / 2] |= b << (1 - k % 2);
        }
        if let Some(n) = voice {
            for h in 0..2 {
                for k in 0..2 {
                    let d = crate::ambe::encode_vcw(&ambe(4 * n + 2 * h + k));
                    let at = (HALF_AT[h] + 72 * k) / 2;
                    body[at..at + 36].copy_from_slice(&d);
                }
            }
        }
        frame_dibits(&body)
    }
}
