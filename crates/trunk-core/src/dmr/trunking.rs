//! A trunked DMR site: every frequency the config names is watched at once
//! (one receiver each costs a small fraction of a core), and what they carry
//! becomes [`Message`]s for the call manager:
//!
//! ```text
//! each carrier: C4fm → Framer → slot::Channel
//!   voice link control on a slot  → Grant (talkgroup, source, that carrier and slot)
//!   Tier III / Capacity Max / Connect Plus grants → Grant on the logical channel's frequency
//!   Capacity Plus site status     → the rest channel; busy slots
//! ```
//!
//! The systems differ in how a call gets its channel:
//!
//! * Capacity Plus: no grants. Radios idle on a rest channel that moves;
//!   every repeater is watched, so a call is found by its own link control.
//! * Connect Plus, Tier III, Capacity Max: grants name a logical channel. Its
//!   frequency comes from the config's LCN table, an absolute-parameters
//!   grant or channel announcement, or is learned: a grant for an unknown
//!   channel, then the same talkgroup's link control on a watched carrier's
//!   slot within a few seconds, ties the two together.
//!
//! The learned table is kept like a band plan ([`Site::map_to_string`]).

use std::collections::BTreeMap;

use num_complex::Complex32;

use super::burst::{Burst, Framer};
use super::slot::{Channel, Csbk, LcFrom, SlotEvent, FID_MOTOROLA, FID_STANDARD};
use crate::dsp::c4fm::C4fm;
use crate::dsp::{Receiver, Symbol};
use crate::trunk::message::{Message, MessageType};

pub const FID_CONNECT_PLUS: u8 = 0x06;

/// A grant waits this long for its talkgroup to show up on a watched carrier.
const LEARN_WINDOW_S: f64 = 4.0;
/// Link control repeats every superframe (360 ms); a call is refreshed at most this often.
const LC_REFRESH_S: f64 = 1.0;
/// An unchanged site status is reported again after this long.
const STATUS_REPEAT_S: f64 = 10.0;
/// A carrier names itself the rest channel this long before it is believed.
const REST_RUN_S: f64 = 3.0;
/// A carrier counts as the control (or rest) channel this long after its last control block.
const CONTROL_HOLD_S: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    CapacityPlus,
    ConnectPlus,
    CapacityMax,
    TierIII,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::CapacityPlus => "DMR Capacity Plus",
            Variant::ConnectPlus => "DMR Connect Plus",
            Variant::CapacityMax => "DMR Capacity Max",
            Variant::TierIII => "DMR Tier III",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DmrConfig {
    /// Logical channel number → frequency (Trunk Recorder's `lcnTable`); wins over what is learned.
    pub lcn_table: BTreeMap<u32, u64>,
    /// Voice frequencies to watch besides the control channels (Trunk Recorder's `channels`).
    pub channels: Vec<f64>,
    /// Only slots with this colour code.
    pub color_code: Option<u8>,
}

/// A DMR site, for the dashboard ([`Site::status`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteStatus {
    pub variant: Option<Variant>,
    pub color_code: Option<u8>,
    /// Capacity Plus: the rest channel (logical slot number, frequency; 0 = not known yet).
    pub rest: Option<(u32, u64)>,
    /// Keyed CRCs (restricted access).
    pub keyed: bool,
    /// Logical channel → frequency, configured or learned.
    pub channels: Vec<ChannelEntry>,
    /// Every watched frequency.
    pub carriers: Vec<CarrierStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelEntry {
    pub lcn: u32,
    pub hz: u64,
    /// From the config's table (else learned from the air).
    pub configured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarrierStatus {
    pub hz: u64,
    /// Sending control blocks now (the control or rest channel).
    pub control: bool,
    pub color_code: Option<u8>,
    /// Each slot's call now: (talkgroup, radio; 0 = not named yet).
    pub slots: [Option<(u32, u32)>; 2],
}

/// Capacity Plus / Connect Plus logical slot number → (logical channel, slot).
pub fn lsn(n: u32) -> (u32, u8) {
    ((n.max(1) - 1) / 2 + 1, ((n.max(1) - 1) % 2) as u8)
}

pub struct Carrier {
    pub hz: u64,
    rx: C4fm,
    framer: Framer,
    pub chan: Channel,
    syms: Vec<Symbol>,
    bursts: Vec<Burst>,
    ev: Vec<(u8, SlotEvent)>,
    /// Per slot: the call link control last reported (talkgroup, source, when).
    last: [Option<(u32, u32, f64)>; 2],
    /// Per slot: an MBC header waiting for its continuation.
    mbc: [Option<Csbk>; 2],
    /// Last control block (CSBK) heard on it, s.
    pub control_s: f64,
    /// Capacity Plus: last site status heard on each slot, s.
    status_s: [f64; 2],
    /// Capacity Plus: the repeater it has named as the rest channel on both
    /// slots without a break, and since when.
    rest_run: Option<(u32, f64)>,
}

struct Pending {
    lcn: u32,
    slot: u8,
    talkgroup: u32,
    /// Only the talkgroup's low 8 bits are known (Capacity Plus site status).
    low8: bool,
    t: f64,
}

pub struct Site {
    pub carriers: Vec<Carrier>,
    pub variant: Option<Variant>,
    cfg: DmrConfig,
    /// Logical channel → frequency, from the air (announced or learned).
    pub learned: BTreeMap<u32, u64>,
    pending: Vec<Pending>,
    /// Capacity Plus: the rest channel's logical slot number and frequency.
    pub rest: Option<(u32, u64)>,
    /// Keyed CRCs seen (restricted access).
    pub keyed: bool,
    /// The site's colour code: configured, else the first control block's on a control channel.
    pub color_code: Option<u8>,
    /// Carriers `0..n_control` are the configured control channels.
    n_control: usize,
    notes: Vec<String>,
    now_s: f64,
    /// Site statuses reported lately, and when (each repeats several times a second).
    recent_status: Vec<(String, f64)>,
}

fn mhz(hz: u64) -> String {
    format!("{:.5} MHz", hz as f64 / 1e6)
}

impl Site {
    pub fn new(freqs: &[f64], rate: f64, cfg: DmrConfig) -> Self {
        let n_control = freqs.len();
        let mut hzs: Vec<u64> = freqs.iter().chain(cfg.channels.iter()).map(|f| f.round() as u64).collect();
        hzs.dedup();
        let mut seen = std::collections::HashSet::new();
        hzs.retain(|h| seen.insert(*h));
        let carriers = hzs
            .into_iter()
            .map(|hz| Carrier {
                hz,
                rx: C4fm::dmr(rate),
                framer: Framer::default(),
                chan: Channel::default(),
                syms: Vec::new(),
                bursts: Vec::new(),
                ev: Vec::new(),
                last: [None, None],
                mbc: [None, None],
                control_s: f64::NEG_INFINITY,
                status_s: [f64::NEG_INFINITY; 2],
                rest_run: None,
            })
            .collect();
        Site {
            carriers,
            variant: None,
            color_code: cfg.color_code,
            cfg,
            learned: BTreeMap::new(),
            pending: Vec::new(),
            rest: None,
            keyed: false,
            n_control,
            notes: Vec::new(),
            now_s: 0.0,
            recent_status: Vec::new(),
        }
    }

    /// For the dashboard: what the site looks like now.
    pub fn status(&self) -> SiteStatus {
        let mut channels: Vec<ChannelEntry> = self.cfg.lcn_table.iter().map(|(&lcn, &hz)| ChannelEntry { lcn, hz, configured: true }).collect();
        channels.extend(self.learned.iter().filter(|(l, _)| !self.cfg.lcn_table.contains_key(l)).map(|(&lcn, &hz)| ChannelEntry { lcn, hz, configured: false }));
        channels.sort_by_key(|c| c.lcn);
        let carriers = self
            .carriers
            .iter()
            .map(|c| CarrierStatus {
                hz: c.hz,
                control: self.now_s - c.control_s < CONTROL_HOLD_S,
                color_code: c.chan.slots.iter().find_map(|s| s.color_code),
                // A slot's call: its link control in the last few seconds.
                slots: std::array::from_fn(|i| c.last[i].filter(|l| self.now_s - l.2 < 3.0).map(|l| (l.0, l.1))),
            })
            .collect();
        SiteStatus { variant: self.variant, color_code: self.color_code, rest: self.rest, keyed: self.keyed, channels, carriers }
    }

    /// Bursts carrier `idx` has framed with a sync, so far.
    pub fn syncs(&self, idx: usize) -> u64 {
        self.carriers[idx].framer.syncs
    }

    /// Notes worth a log line since the last call (channels learned, …).
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// The frequency of logical channel `lcn`: configured, else heard.
    pub fn resolve(&self, lcn: u32) -> Option<u64> {
        self.cfg.lcn_table.get(&lcn).or_else(|| self.learned.get(&lcn)).copied()
    }

    /// The carrier sending control blocks now (control or rest channel).
    pub fn control_hz(&self) -> Option<u64> {
        self.carriers.iter().filter(|c| self.now_s - c.control_s < CONTROL_HOLD_S).max_by(|a, b| a.control_s.total_cmp(&b.control_s)).map(|c| c.hz)
    }

    /// The learned table as `lcn=hz` lines.
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
        c.syms.clear();
        c.rx.push(iq, &mut c.syms);
        c.bursts.clear();
        for s in &c.syms {
            c.framer.push(s, &mut c.bursts);
        }
        let bursts = std::mem::take(&mut c.bursts);
        for b in &bursts {
            let t = t0 + b.sample / rate;
            self.now_s = self.now_s.max(t);
            let c = &mut self.carriers[idx];
            if self.keyed {
                // One system, one key: every carrier of the site is keyed.
                for s in c.chan.slots.iter_mut() {
                    s.keyed = true;
                }
            }
            c.ev.clear();
            c.chan.burst(b, &mut c.ev);
            self.keyed |= c.chan.slots.iter().any(|s| s.keyed);
            for (slot, e) in std::mem::take(&mut self.carriers[idx].ev) {
                self.event(idx, slot, e, t, out);
            }
        }
        self.carriers[idx].bursts = bursts;
        self.pending.retain(|p| self.now_s - p.t < LEARN_WINDOW_S);
    }

    fn base(&self, idx: usize, slot: u8, kind: MessageType, t: f64) -> Message {
        let c = &self.carriers[idx];
        Message { kind, time_s: t, color_code: Some(c.chan.slots[slot as usize].color_code.unwrap_or(0)), ..Default::default() }
    }

    fn event(&mut self, idx: usize, slot: u8, e: SlotEvent, t: f64, out: &mut Vec<Message>) {
        let cc = self.carriers[idx].chan.slots[slot as usize].color_code;
        if self.color_code.is_none() && idx < self.n_control && matches!(e, SlotEvent::Csbk { .. }) {
            if let Some(c) = cc {
                self.color_code = Some(c);
                self.notes.push(format!("Colour code {c}"));
            }
        }
        // Another system's carrier (a neighbour on a listed frequency, or a
        // stray): until the site's colour code is known, only the control channels count.
        match self.color_code {
            Some(want) if cc != Some(want) => return,
            None if idx >= self.n_control && self.n_control > 0 => return,
            _ => {}
        }
        match e {
            SlotEvent::Lc { lc, from } => {
                let Some(group) = lc.voice_user() else { return };
                if from == LcFrom::Terminator {
                    // The end of a transmission still says where the talkgroup was.
                    let hz = self.carriers[idx].hz;
                    self.learn(lc.target(), slot, hz);
                    self.carriers[idx].last[slot as usize] = None;
                    return;
                }
                if let Some(n) = lc.rest_lsn().filter(|&n| n != 0) {
                    // Where the rest channel is (not this carrier: it has a call on it).
                    let hz = self.resolve(lsn(n as u32).0).unwrap_or(0);
                    self.rest = Some((n as u32, hz));
                }
                let (tg, src) = (lc.target(), lc.source());
                let hz = self.carriers[idx].hz;
                self.learn(tg, slot, hz);
                let c = &mut self.carriers[idx];
                let prev = c.last[slot as usize];
                let repeat = prev.is_some_and(|(g, s, _)| g == tg && (s == src || src == 0));
                if repeat && prev.is_some_and(|(_, _, at)| t - at < LC_REFRESH_S) {
                    return;
                }
                c.last[slot as usize] = Some((tg, if src != 0 { src } else { prev.map_or(0, |p| p.1) }, t));
                let kind = match (group, repeat) {
                    (true, false) => MessageType::Grant,
                    (true, true) => MessageType::Update,
                    (false, false) => MessageType::UuVGrant,
                    (false, true) => MessageType::UuVUpdate,
                };
                let mut m = self.base(idx, slot, kind, t);
                m.freq_hz = hz;
                m.tdma_slot = slot;
                m.talkgroup = tg;
                m.source = if src != 0 { src as i64 } else { -1 };
                m.encrypted = lc.encrypted();
                m.emergency = lc.emergency();
                m.opcode = lc.flco();
                m.meta = format!("{} TG {tg} from {src} on {} slot {}{}", if group { "Voice" } else { "Private voice" }, mhz(hz), slot + 1, if m.encrypted { " (encrypted)" } else { "" });
                out.push(m);
            }
            SlotEvent::Csbk { csbk, mbc } => {
                self.carriers[idx].control_s = t;
                if mbc {
                    self.carriers[idx].mbc[slot as usize] = Some(csbk);
                    return;
                }
                self.csbk(idx, slot, &csbk, None, t, out);
            }
            SlotEvent::MbcContinuation(bytes) => {
                if let Some(h) = self.carriers[idx].mbc[slot as usize].take() {
                    self.csbk(idx, slot, &h, Some(&bytes), t, out);
                }
            }
            _ => {}
        }
    }

    fn set_variant(&mut self, v: Variant) {
        // Capacity Max also sends the standard (Tier III) blocks: it wins.
        if self.variant != Some(v) && !(self.variant == Some(Variant::CapacityMax) && v == Variant::TierIII) {
            self.variant = Some(v);
            self.notes.push(format!("This is a {} system", v.name()));
        }
    }

    /// Site status named rest channel LSN `n`. A repeater with a call on one
    /// slot sends site status on the other, naming the rest channel elsewhere,
    /// and one in hang time may send it on both for a while; the rest channel
    /// sends it on both, naming itself, for as long as it is the rest channel.
    fn rest_channel(&mut self, n: u32, idx: usize, t: f64) {
        let (lcn, _) = lsn(n);
        let c = &mut self.carriers[idx];
        let both = c.status_s.iter().all(|&s| t - s < 0.5);
        c.rest_run = match c.rest_run {
            Some((l, since)) if both && l == lcn => Some((l, since)),
            _ if both => Some((lcn, t)),
            _ => None,
        };
        if c.rest_run.is_some_and(|(_, since)| t - since >= REST_RUN_S) {
            let hz = c.hz;
            self.set_learned(lcn, hz, "it is the rest channel");
        }
        self.rest = Some((n, self.resolve(lcn).unwrap_or(0)));
    }

    fn set_learned(&mut self, lcn: u32, hz: u64, why: &str) {
        if self.cfg.lcn_table.contains_key(&lcn) || self.learned.get(&lcn) == Some(&hz) {
            return;
        }
        // A frequency is one logical channel: drop what said otherwise.
        self.learned.retain(|_, h| *h != hz);
        self.learned.insert(lcn, hz);
        self.notes.push(format!("Channel {lcn} is {} ({why})", mhz(hz)));
    }

    /// A talkgroup's link control on a watched carrier's slot: the frequency
    /// of a logical channel a recent grant sent it to.
    fn learn(&mut self, tg: u32, slot: u8, hz: u64) {
        let hit = self.pending.iter().position(|p| p.slot == slot && if p.low8 { p.talkgroup == tg & 0xff } else { p.talkgroup == tg });
        if let Some(i) = hit {
            let p = self.pending.remove(i);
            if self.resolve(p.lcn).is_none() || (self.learned.contains_key(&p.lcn) && !self.cfg.lcn_table.contains_key(&p.lcn)) {
                self.set_learned(p.lcn, hz, &format!("talkgroup {tg} granted there came up on it"));
            }
        }
    }

    /// A grant to logical channel `lcn` (or to `abs_hz`, absolute parameters).
    #[allow(clippy::too_many_arguments)]
    fn grant(&mut self, idx: usize, slot_of_block: u8, lcn: u32, slot: u8, tg: u32, src: Option<u32>, group: bool, update: bool, emergency: bool, abs_hz: Option<u64>, what: &str, t: f64, out: &mut Vec<Message>) {
        if tg == 0 {
            return;
        }
        let hz = abs_hz.or_else(|| self.resolve(lcn));
        if hz.is_none() {
            self.pending.retain(|p| !(p.lcn == lcn && p.slot == slot));
            self.pending.push(Pending { lcn, slot, talkgroup: tg, low8: false, t });
        }
        let kind = match (group, update) {
            (true, false) => MessageType::Grant,
            (true, true) => MessageType::Update,
            (false, false) => MessageType::UuVGrant,
            (false, true) => MessageType::UuVUpdate,
        };
        let mut m = self.base(idx, slot_of_block, kind, t);
        m.freq_hz = hz.unwrap_or(0);
        m.tdma_slot = slot;
        m.talkgroup = tg;
        m.source = src.map_or(-1, |s| s as i64);
        m.emergency = emergency;
        m.meta = format!(
            "{what}: TG {tg}{} on channel {lcn} slot {} ({})",
            src.map_or(String::new(), |s| format!(" from {s}")),
            slot + 1,
            hz.map_or("frequency not known yet".into(), mhz)
        );
        out.push(m);
    }

    fn status_msg(&self, idx: usize, slot: u8, meta: String, t: f64, out: &mut Vec<Message>) {
        let mut m = self.base(idx, slot, MessageType::Status, t);
        m.freq_hz = self.carriers[idx].hz;
        m.meta = meta;
        out.push(m);
    }

    fn csbk(&mut self, idx: usize, slot: u8, k: &Csbk, cont: Option<&[u8; 12]>, t: f64, out: &mut Vec<Message>) {
        let (op, fid) = (k.opcode(), k.fid());
        match (fid, op) {
            // ── Capacity Plus ──
            (FID_MOTOROLA, 0x3e) => {
                self.set_variant(Variant::CapacityPlus);
                self.site_status(idx, slot, k, t, out);
            }
            (FID_MOTOROLA, 0x3b) | (FID_MOTOROLA, 0x3d) | (FID_MOTOROLA, 0x29) | (FID_MOTOROLA, 0x2a) => self.set_variant(Variant::CapacityPlus),
            // ── Capacity Max ──
            (FID_MOTOROLA, 0x19) => self.set_variant(Variant::CapacityMax),
            (FID_MOTOROLA, 0x21) => {
                // Open mode voice channel update: a channel and each slot's talkgroup.
                self.set_variant(Variant::CapacityMax);
                let ch = k.bits(16, 28);
                for (s, (a, b)) in [(32, 56), (56, 80)].into_iter().enumerate() {
                    let tg = k.bits(a, b);
                    self.grant(idx, slot, ch, s as u8, tg, None, true, true, false, None, "Capacity Max channel update", t, out);
                }
            }
            (FID_MOTOROLA, 0x22) => {
                // Advantage mode: two channels, each slot's talkgroup (10 bits).
                self.set_variant(Variant::CapacityMax);
                for (c0, s1, s2) in [(16, 28, 38), (48, 60, 70)] {
                    let ch = k.bits(c0, c0 + 12);
                    if ch == 0 {
                        continue;
                    }
                    for (s, a) in [s1, s2].into_iter().enumerate() {
                        let tg = k.bits(a, a + 10);
                        self.grant(idx, slot, ch, s as u8, tg, None, true, true, false, None, "Capacity Max channel update", t, out);
                    }
                }
            }
            // ── Connect Plus ──
            (FID_CONNECT_PLUS, 0x03) => {
                self.set_variant(Variant::ConnectPlus);
                let (src, tg, v) = (k.bits(16, 40), k.bits(40, 64), k.bits(64, 69));
                self.grant(idx, slot, v >> 1, (v & 1) as u8, tg, Some(src), true, false, false, None, "Connect Plus voice channel user", t, out);
            }
            (FID_CONNECT_PLUS, _) => self.set_variant(Variant::ConnectPlus),
            // ── Tier III (ETSI) ──
            (FID_STANDARD, 0x19) => self.set_variant(Variant::TierIII),
            (FID_STANDARD, 0x30..=0x32) => {
                self.set_variant(Variant::TierIII);
                let ch = k.bits(16, 28);
                let abs = cont.and_then(|c| (ch == 0xfff).then(|| absolute_downlink(c)).flatten());
                let group = op != 0x30;
                let what = ["Private voice grant", "Talkgroup voice grant", "Broadcast talkgroup voice grant"][(op - 0x30) as usize];
                self.grant(idx, slot, ch, k.bits(28, 29) as u8, k.bits(32, 56), Some(k.bits(56, 80)), group, false, k.bits(30, 31) != 0, abs, what, t, out);
            }
            (FID_STANDARD, 0x28) => {
                self.set_variant(Variant::TierIII);
                // C_BCAST: a channel's frequency (type 5, with absolute parameters).
                if k.bits(16, 21) == 5 {
                    if let Some(c) = cont {
                        let lcn = bits(c, 22, 34);
                        if let Some(hz) = absolute_downlink(c) {
                            if !self.cfg.lcn_table.contains_key(&lcn) && self.learned.get(&lcn) != Some(&hz) {
                                self.learned.insert(lcn, hz);
                                self.notes.push(format!("Channel {lcn} is {} (announced)", mhz(hz)));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Capacity Plus site status: the rest channel, and the busy slots with
    /// their talkgroups' low bytes (a slot's full talkgroup is in its LC).
    fn site_status(&mut self, idx: usize, slot: u8, k: &Csbk, t: f64, out: &mut Vec<Message>) {
        self.carriers[idx].status_s[slot as usize] = t;
        let rest = k.bits(19, 24);
        if rest != 0 {
            self.rest_channel(rest, idx, t);
        }
        // Only the first segment (bits 16–17 = 1x) carries the bitmaps.
        if k.bits(16, 17) == 0 {
            return;
        }
        let mut busy = Vec::new();
        let mut p = 24;
        for base in [0u32, 8] {
            if p > 72 {
                break;
            }
            let map = k.bits(p, p + 8);
            p += 8;
            for i in 0..8 {
                if map >> (7 - i) & 1 != 0 {
                    let tg = if p <= 72 { Some(k.bits(p, p + 8)) } else { None };
                    if tg.is_some() {
                        p += 8;
                    }
                    busy.push((base + i + 1, tg));
                }
            }
        }
        for &(n, tg) in &busy {
            let (lcn, s) = lsn(n);
            if let Some(tg) = tg.filter(|&g| g != 0) {
                if self.resolve(lcn).is_none() {
                    self.pending.retain(|q| !(q.lcn == lcn && q.slot == s));
                    self.pending.push(Pending { lcn, slot: s, talkgroup: tg, low8: true, t });
                }
            }
        }
        let rest_s = self.rest.map_or(String::new(), |(n, hz)| if hz > 0 { format!("rest channel LSN {n} ({})", mhz(hz)) } else { format!("rest channel LSN {n}") });
        let busy_s: Vec<String> = busy.iter().map(|(n, tg)| format!("LSN {n}{}", tg.map_or(String::new(), |g| format!(" TG …{g}")))).collect();
        let meta = format!("Capacity Plus site status: {rest_s}{}{}", if busy.is_empty() { "" } else { "; busy: " }, busy_s.join(", "));
        // Only when it changes (or every so often): it comes several times a second.
        self.recent_status.retain(|(_, at)| t - at < STATUS_REPEAT_S);
        if self.recent_status.iter().any(|(m, _)| *m == meta) {
            return;
        }
        self.recent_status.push((meta.clone(), t));
        self.status_msg(idx, slot, meta, t, out);
    }
}

/// Bits `a..b` of a 12-byte block.
fn bits(b: &[u8; 12], a: usize, e: usize) -> u32 {
    (a..e).fold(0, |v, i| v << 1 | (b[i / 8] >> (7 - i % 8) & 1) as u32)
}

/// An MBC continuation's absolute channel parameters → downlink frequency.
fn absolute_downlink(c: &[u8; 12]) -> Option<u64> {
    let hz = bits(c, 57, 67) as u64 * 1_000_000 + bits(c, 67, 80) as u64 * 125;
    (hz > 0).then_some(hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csbk(bytes: [u8; 10]) -> Csbk {
        Csbk(bytes)
    }

    #[test]
    fn capacity_max_grant_is_learned_from_the_voice_channel() {
        // As heard on 452.175 / 452.275 MHz: a Tier III talkgroup grant for
        // channel 101 slot 2, then that talkgroup's link control on 452.275 slot 2.
        let mut s = Site::new(&[452_175_000.0], 24_000.0, DmrConfig { channels: vec![452_275_000.0], ..Default::default() });
        let mut out = Vec::new();
        let g = csbk([0xb1, 0x00, 0x06, 0x58, 0x00, 0x00, 0xce, 0x00, 0x07, 0xe7]);
        s.csbk(0, 0, &g, None, 1.0, &mut out);
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].talkgroup, out[0].tdma_slot, out[0].source), (MessageType::Grant, 0, 206, 1, 2023));
        let lc = super::super::slot::Lc([0x00, 0x10, 0x40, 0x00, 0x00, 0xce, 0x00, 0x07, 0xe7]);
        // Colour code 0, from the control channel and on the voice channel's slot.
        s.color_code = Some(0);
        s.carriers[1].chan.slots[1].color_code = Some(0);
        out.clear();
        s.event(1, 1, SlotEvent::Lc { lc, from: LcFrom::Header }, 1.4, &mut out);
        assert_eq!(s.resolve(101), Some(452_275_000));
        assert!(out[0].encrypted && out[0].freq_hz == 452_275_000 && out[0].tdma_slot == 1);
        // The next grant for the channel resolves at once.
        out.clear();
        s.csbk(0, 0, &g, None, 30.0, &mut out);
        assert_eq!(out[0].freq_hz, 452_275_000);
        let mut t = Site::new(&[452_175_000.0], 24_000.0, DmrConfig::default());
        t.map_from_str(&s.map_to_string());
        assert_eq!(t.resolve(101), Some(452_275_000));
    }

    #[test]
    fn capacity_plus_site_status() {
        // As heard on 452.3625 MHz: rest LSN 2; LSN 1 busy with talkgroup …106.
        let mut s = Site::new(&[452_362_500.0, 463_000_000.0], 24_000.0, DmrConfig::default());
        let mut out = Vec::new();
        for i in 0..14 {
            let t = i as f64 * 0.3;
            s.csbk(0, 1, &csbk([0xbe, 0x10, 0xe2, 0, 0, 0, 0, 0, 0, 0]), None, t, &mut out);
            s.csbk(0, 0, &csbk([0xbe, 0x10, 0xc2, 0x80, 0x6a, 0, 0, 0, 0, 0]), None, t + 0.03, &mut out);
        }
        assert_eq!(s.variant, Some(Variant::CapacityPlus));
        assert_eq!(s.rest, Some((2, 452_362_500)));
        assert_eq!(s.resolve(1), Some(452_362_500), "LSN 2 is repeater 1");
        assert!(out.iter().any(|m| m.meta.contains("LSN 1 TG …106")), "{out:?}");
        assert!(out.len() <= 4, "site status reported only as it changes: {}", out.len());
        let s0 = s.carriers[0].status_s;
        s.carriers[0].status_s = [s0[0], f64::NEG_INFINITY];
        // A call on repeater 1's slot 2: its slot 1 names the rest channel, LSN 3, elsewhere.
        s.csbk(0, 0, &csbk([0xbe, 0x10, 0xc3, 0, 0, 0, 0, 0, 0, 0]), None, 5.0, &mut out);
        assert_eq!((s.rest, s.resolve(2), s.resolve(1)), (Some((3, 0)), None, Some(452_362_500)));
        // The other carrier sends it on both slots: it is repeater 2.
        for i in 0..12 {
            let t = 5.1 + i as f64 * 0.3;
            s.csbk(1, 0, &csbk([0xbe, 0x10, 0xc3, 0, 0, 0, 0, 0, 0, 0]), None, t, &mut out);
            s.csbk(1, 1, &csbk([0xbe, 0x10, 0xe3, 0, 0, 0, 0, 0, 0, 0]), None, t + 0.03, &mut out);
        }
        assert_eq!((s.rest, s.resolve(2)), (Some((3, 463_000_000)), Some(463_000_000)));
    }

    /// Set bits `a..b` of a block to `v`.
    fn put(b: &mut [u8], a: usize, e: usize, v: u64) {
        for (k, i) in (a..e).enumerate() {
            let bit = (v >> (e - a - 1 - k) & 1) as u8;
            b[i / 8] = b[i / 8] & !(0x80 >> (i % 8)) | bit << (7 - i % 8);
        }
    }

    fn tier3_grant(op: u8, ch: u64, slot: u64, tg: u64, src: u64) -> Csbk {
        let mut b = [0u8; 10];
        b[0] = 0x80 | op;
        put(&mut b, 16, 28, ch);
        put(&mut b, 28, 29, slot);
        put(&mut b, 32, 56, tg);
        put(&mut b, 56, 80, src);
        Csbk(b)
    }

    /// An MBC continuation with absolute channel parameters (downlink `hz`).
    fn absolute(lcn: u64, hz: u64) -> [u8; 12] {
        let mut c = [0u8; 12];
        put(&mut c, 22, 34, lcn);
        put(&mut c, 57, 67, hz / 1_000_000);
        put(&mut c, 67, 80, hz % 1_000_000 / 125);
        c
    }

    #[test]
    fn tier3_grants_resolve_by_table_absolute_parameters_and_announcement() {
        let mut lcn = BTreeMap::new();
        lcn.insert(7, 451_012_500);
        let mut s = Site::new(&[450_500_000.0], 24_000.0, DmrConfig { lcn_table: lcn, ..Default::default() });
        let mut out = Vec::new();
        // Configured channel 7.
        s.csbk(0, 0, &tier3_grant(0x31, 7, 0, 3001, 12), None, 1.0, &mut out);
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].tdma_slot, out[0].talkgroup, out[0].source), (MessageType::Grant, 451_012_500, 0, 3001, 12));
        assert_eq!(s.variant, Some(Variant::TierIII));
        // A private call with absolute parameters (channel 0xFFF, MBC).
        out.clear();
        s.csbk(0, 0, &tier3_grant(0x30, 0xfff, 1, 777, 12), Some(&absolute(9, 451_587_500)), 2.0, &mut out);
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].tdma_slot), (MessageType::UuVGrant, 451_587_500, 1));
        // C_BCAST type 5: channel 12's frequency.
        let mut ann = [0u8; 10];
        ann[0] = 0x28;
        put(&mut ann, 16, 21, 5);
        s.csbk(0, 0, &Csbk(ann), Some(&absolute(12, 452_000_000)), 3.0, &mut out);
        assert_eq!(s.resolve(12), Some(452_000_000));
        assert!(s.take_notes().iter().any(|n| n.contains("Channel 12 is 452.00000 MHz (announced)")));
    }

    #[test]
    fn connect_plus_grant() {
        let mut lcn = BTreeMap::new();
        lcn.insert(3, 453_100_000);
        let mut s = Site::new(&[453_000_000.0], 24_000.0, DmrConfig { lcn_table: lcn, ..Default::default() });
        let mut b = [0u8; 10];
        b[0] = 0x83;
        b[1] = FID_CONNECT_PLUS;
        put(&mut b, 16, 40, 4242);
        put(&mut b, 40, 64, 55);
        put(&mut b, 64, 69, 3 << 1 | 1); // LCN 3, slot 2
        let mut out = Vec::new();
        s.csbk(0, 0, &Csbk(b), None, 1.0, &mut out);
        assert_eq!(s.variant, Some(Variant::ConnectPlus));
        assert_eq!((out[0].freq_hz, out[0].tdma_slot, out[0].talkgroup, out[0].source), (453_100_000, 1, 55, 4242));
    }

    #[test]
    fn lc_repeats_refresh_instead_of_granting_again() {
        let mut s = Site::new(&[463_550_000.0], 24_000.0, DmrConfig::default());
        let lc = super::super::slot::Lc([0, 0, 0, 0, 0, 25, 0, 1, 0xd6]);
        let mut out = Vec::new();
        for i in 0..8 {
            s.event(0, 0, SlotEvent::Lc { lc, from: LcFrom::Embedded }, i as f64 * 0.36, &mut out);
        }
        let kinds: Vec<MessageType> = out.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [MessageType::Grant, MessageType::Update, MessageType::Update]);
        assert_eq!((out[0].talkgroup, out[0].source, out[0].freq_hz), (25, 470, 463_550_000));
    }
}
