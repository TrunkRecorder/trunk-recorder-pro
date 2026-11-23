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

use std::collections::{BTreeMap, BTreeSet};

use num_complex::Complex32;

use super::burst::{Burst, Framer};
use super::slot::{Channel, Csbk, LcFrom, SlotEvent, FID_MOTOROLA, FID_STANDARD};
use crate::bits::field;
use crate::dsp::c4fm::C4fm;
use crate::dsp::{Receiver, Symbol};
use crate::trunk::identity::{IdField, Identity};
use crate::trunk::message::{site_code, ChannelTable, LastCall, Message, MessageType};
use crate::trunk::Voice;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Variant {
    CapacityPlus,
    ConnectPlus,
    CapacityMax,
    TierIII,
    Xpt,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::CapacityPlus => "DMR Capacity Plus",
            Variant::ConnectPlus => "DMR Connect Plus",
            Variant::CapacityMax => "DMR Capacity Max",
            Variant::TierIII => "DMR Tier III",
            Variant::Xpt => "Hytera XPT",
        }
    }

    /// Standard trunking's (Tier III, Capacity Max, Connect Plus): heard
    /// once, a site is it. Hytera radios on it send Hytera's own blocks too,
    /// which aren't XPT signalling there.
    pub fn grants(self) -> bool {
        matches!(self, Variant::TierIII | Variant::CapacityMax | Variant::ConnectPlus)
    }
}

/// A Tier III / Capacity Max site's System Identity Code (C_ALOHA's and C_BCAST's 16 bits,
/// ETSI TS 102 361-4). Its top two bits, the model, say how the 12 after them divide between
/// network and site (both as sent); the last two, the partition, which category of radio may
/// use the site. Its network and site are the site's [`Identity`]; the rest is detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SysCode(pub u16);

impl SysCode {
    /// Site bits for each model (the network gets the other 12): tiny, small, large, huge.
    const SITE_BITS: [u32; 4] = [3, 5, 8, 10];

    fn site_bits(self) -> u32 {
        Self::SITE_BITS[(self.0 >> 14) as usize]
    }
    pub fn model(self) -> &'static str {
        ["Tiny", "Small", "Large", "Huge"][(self.0 >> 14) as usize]
    }
    pub fn network(self) -> u32 {
        (self.0 as u32 >> 2 & 0xfff) >> self.site_bits()
    }
    pub fn site(self) -> u32 {
        (self.0 as u32 >> 2) & ((1 << self.site_bits()) - 1)
    }
    /// Which category of radio may use the site.
    pub fn category(self) -> &'static str {
        ["reserved", "A", "B", "A+B"][(self.0 & 3) as usize]
    }
}

impl std::fmt::Display for SysCode {
    /// "B013 (Large, network 12, site 4, category A+B)".
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04X} ({}, network {}, site {}, category {})", self.0, self.model(), self.network(), self.site(), self.category())
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
    /// Tier III / Capacity Max: the system identity code from the control channel.
    pub sys_code: Option<SysCode>,
    /// Capacity Plus and XPT: the rest channel (logical slot number, frequency; 0 = not known yet).
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
    /// The sample rate `rx` was made for (its source's channel rate).
    rate: f64,
    framer: Framer,
    pub chan: Channel,
    syms: Vec<Symbol>,
    bursts: Vec<Burst>,
    ev: Vec<(u8, SlotEvent)>,
    /// Per slot: the call link control last reported (talkgroup, source, when).
    last: [LastCall; 2],
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
    pub table: ChannelTable,
    pending: Vec<Pending>,
    /// Channels learned from a call that came up where it was granted: the
    /// surest evidence, so a rest channel's site status doesn't move them.
    placed_by_call: BTreeSet<u32>,
    /// Capacity Plus and XPT: the rest channel's logical slot number and frequency.
    pub rest: Option<(u32, u64)>,
    /// Keyed CRCs seen (restricted access).
    pub keyed: bool,
    /// The site's colour code: configured, else the first control block's on a control channel.
    pub color_code: Option<u8>,
    /// Tier III / Capacity Max: the System Identity Code, last heard on a control channel.
    sys_code: Option<SysCode>,
    /// What it says it is: its colour code; Tier III its network and site (its [`SysCode`]'s).
    identity: Identity,
    /// Carriers `0..n_control` are the configured control channels.
    n_control: usize,
    notes: Vec<String>,
    now_s: f64,
    /// Site statuses reported lately, and when (each repeats several times a second).
    recent_status: Vec<(String, f64)>,
    /// The carrier taken as the control channel: kept while it sends control
    /// blocks, though others do too (Capacity Plus, XPT repeaters with a free slot).
    control: Option<usize>,
}

fn mhz(hz: u64) -> String {
    format!("{:.5} MHz", hz as f64 / 1e6)
}

impl Site {
    pub fn new(freqs: &[f64], rate: f64, cfg: DmrConfig) -> Self {
        let n_control = freqs.len();
        let mut seen = std::collections::HashSet::new();
        let hzs: Vec<u64> = freqs.iter().chain(cfg.channels.iter()).map(|f| f.round() as u64).filter(|h| seen.insert(*h)).collect();
        let carriers = hzs
            .into_iter()
            .map(|hz| Carrier {
                hz,
                rx: C4fm::dmr(rate),
                rate,
                framer: Framer::new(),
                chan: Channel::default(),
                syms: Vec::new(),
                bursts: Vec::new(),
                ev: Vec::new(),
                last: Default::default(),
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
            sys_code: None,
            identity: cfg.color_code.map_or_else(Identity::default, |c| Identity::default().with(IdField::ColorCode, c as u32)),
            table: ChannelTable::new(cfg.lcn_table.clone()),
            cfg,
            pending: Vec::new(),
            placed_by_call: BTreeSet::new(),
            rest: None,
            keyed: false,
            n_control,
            notes: Vec::new(),
            now_s: 0.0,
            recent_status: Vec::new(),
            control: None,
        }
    }

    /// What the site says it is ([`IdField::ColorCode`]; Tier III also network and site).
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// For the dashboard: what the site looks like now.
    pub fn status(&self) -> SiteStatus {
        let mut channels: Vec<ChannelEntry> = self.cfg.lcn_table.iter().map(|(&lcn, &hz)| ChannelEntry { lcn, hz, configured: true }).collect();
        channels.extend(self.table.learned().map(|(lcn, hz)| ChannelEntry { lcn, hz, configured: false }));
        channels.sort_by_key(|c| c.lcn);
        let carriers = self
            .carriers
            .iter()
            .map(|c| CarrierStatus {
                hz: c.hz,
                control: self.now_s - c.control_s < CONTROL_HOLD_S,
                color_code: c.chan.slots.iter().find_map(|s| s.color_code),
                // A slot's call: its link control in the last few seconds.
                slots: std::array::from_fn(|i| c.last[i].shown(self.now_s)),
            })
            .collect();
        SiteStatus { variant: self.variant, color_code: self.color_code, sys_code: self.sys_code, rest: self.rest, keyed: self.keyed, channels, carriers }
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
        self.table.resolve(lcn)
    }

    /// The carrier sending control blocks now: the rest channel's, else the one taken lately.
    fn control_carrier(&self) -> Option<&Carrier> {
        let sending = |c: &&Carrier| self.now_s - c.control_s < CONTROL_HOLD_S;
        let rest = self.rest.map(|(_, hz)| hz).filter(|&hz| hz > 0);
        self.carriers.iter().filter(sending).find(|c| Some(c.hz) == rest).or_else(|| self.control.map(|i| &self.carriers[i]).filter(sending))
    }

    /// Carrier `idx` sent a control block: the control channel, unless another still is.
    fn heard_control(&mut self, idx: usize, t: f64) {
        if !self.control.is_some_and(|i| i != idx && t - self.carriers[i].control_s < CONTROL_HOLD_S) {
            self.control = Some(idx);
        }
        self.carriers[idx].control_s = t;
    }

    pub fn control_hz(&self) -> Option<u64> {
        self.control_carrier().map(|c| c.hz)
    }

    /// How far off the control carrier comes in, Hz, once its receiver can tell (AutoTune).
    pub fn offset_hz(&self) -> Option<f32> {
        self.control_carrier().and_then(|c| c.rx.offset_hz())
    }

    /// The control carrier's eye opening (the dashboard's).
    pub fn quality(&self) -> Option<f32> {
        self.control_carrier().and_then(|c| c.rx.quality())
    }

    /// The learned table as `lcn=hz` lines.
    pub fn map_to_string(&self) -> String {
        self.table.saved()
    }
    pub fn map_from_str(&mut self, s: &str) {
        self.table.load(s);
    }

    /// Carrier `idx`'s channel IQ; `t0` + sample / `rate` is a sample's air time (s).
    pub fn push(&mut self, idx: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Message>) {
        let c = &mut self.carriers[idx];
        if c.rate != rate {
            // On a source of another rate than the site was made for.
            (c.rx, c.rate) = (C4fm::dmr(rate), rate);
        }
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
        Message { kind, time_s: t, voice: Voice::Dmr { slot, color_code: Some(c.chan.slots[slot as usize].color_code.unwrap_or(0)) }, ..Default::default() }
    }

    fn event(&mut self, idx: usize, slot: u8, e: SlotEvent, t: f64, out: &mut Vec<Message>) {
        let cc = self.carriers[idx].chan.slots[slot as usize].color_code;
        if self.color_code.is_none() && idx < self.n_control && matches!(e, SlotEvent::Csbk { .. }) {
            if let Some(c) = cc {
                self.color_code = Some(c);
                self.identity.set(IdField::ColorCode, c as u32);
                self.notes.push(format!("Colour code {c}"));
            }
        }
        // (A site heard granting as standard trunking does isn't XPT, whatever its radios send.)
        let xpt = super::xpt::control(&e).filter(|_| !self.variant.is_some_and(Variant::grants));
        let is_xpt = xpt.is_some() || self.variant == Some(Variant::Xpt);
        // Another system's carrier (a neighbour on a listed frequency, or a stray): not followed.
        // XPT repeaters may each have their own colour code: every listed one is the site's
        // (unless the config names one code).
        let want = if is_xpt { self.cfg.color_code } else { self.color_code };
        if !site_code(want, cc, idx < self.n_control, self.n_control > 0 && !is_xpt) {
            return;
        }
        if let Some(c) = xpt {
            if matches!(e, SlotEvent::Csbk { .. }) {
                self.heard_control(idx, t);
            }
            self.xpt(idx, slot, c, t, out);
            return;
        }
        match e {
            SlotEvent::Lc { lc, from } => {
                if is_xpt && lc.target() == 0 {
                    return;
                }
                let Some(group) = lc.voice_user() else { return };
                if from == LcFrom::Terminator {
                    // The end of a transmission still says where the talkgroup was.
                    let hz = self.carriers[idx].hz;
                    self.learn(lc.target(), slot, hz);
                    self.carriers[idx].last[slot as usize].clear();
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
                let Some(repeat) = self.carriers[idx].last[slot as usize].heard(tg, src, t, LC_REFRESH_S) else { return };
                let kind = MessageType::voice(group, repeat);
                let mut m = self.base(idx, slot, kind, t);
                m.freq_hz = hz;
                m.talkgroup = tg;
                m.source = if src != 0 { src as i64 } else { -1 };
                m.encrypted = lc.encrypted();
                m.emergency = lc.emergency();
                m.opcode = lc.flco();
                m.meta = format!("{} TG {tg} from {src} on {} slot {}{}", if group { "Voice" } else { "Private voice" }, mhz(hz), slot + 1, if m.encrypted { " (encrypted)" } else { "" });
                out.push(m);
            }
            SlotEvent::Csbk { csbk, mbc } => {
                self.heard_control(idx, t);
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

    /// Hytera XPT signalling. The free repeater is the site's rest channel
    /// (where the next call starts); status banks name the busy slots; a call
    /// setup is a grant (a repeater not in the table is learned when its
    /// talkgroup comes up on a listed carrier). The call itself is recorded
    /// from its voice LC.
    fn xpt(&mut self, idx: usize, slot: u8, c: super::xpt::Control, t: f64, out: &mut Vec<Message>) {
        use super::xpt::Control;
        self.set_variant(Variant::Xpt);
        match c {
            Control::Status { sequence, free, slots } => {
                self.free_repeater(free);
                // A busy slot's target is 8 bits: a group's low byte or a private
                // call's hash. Shown, but nothing is learned from it.
                let busy: Vec<(u32, Option<u32>)> = (sequence as u32 * 6 + 1..)
                    .zip(slots)
                    .filter(|&(n, o)| n <= 16 && o.status != 0)
                    .map(|(n, o)| (n, (o.target_hint != 0).then_some(o.target_hint as u32)))
                    .collect();
                self.report_site("XPT", &busy, idx, slot, t, out);
            }
            Control::Short { free, .. } => {
                self.free_repeater(free);
                self.report_site("XPT", &[], idx, slot, t, out);
            }
            Control::Setup { repeater, slot: s, group, target, source, free, .. } => {
                self.free_repeater(free);
                if let Some(lcn) = super::xpt::repeater(repeater).filter(|&l| target != 0 && self.resolve(l).is_none()) {
                    self.pending.retain(|q| !(q.lcn == lcn && q.slot == s));
                    self.pending.push(Pending { lcn, slot: s, talkgroup: target as u32, low8: false, t });
                }
                let what = if group { "TG" } else { "radio" };
                self.status_once(idx, slot, format!("XPT call setup: {what} {target} from {source} on repeater {repeater} slot {}", s + 1), t, out);
            }
            Control::Adjacent { sites, .. } => {
                let s: Vec<String> = sites
                    .iter()
                    .filter(|n| n.site != 0)
                    .map(|n| format!("site {}{}", n.site, super::xpt::repeater(n.free).map_or(String::new(), |r| format!(" (free repeater {r})"))))
                    .collect();
                if !s.is_empty() {
                    self.status_once(idx, slot, format!("XPT neighbours: {}", s.join(", ")), t, out);
                }
            }
            Control::Preamble { data, group, target, source, .. } => {
                // Data to come (not a voice grant), as heard on a data carrier. One line
                // per transfer: each preamble counts down the blocks still to come.
                let what = if group { "TG" } else { "radio" };
                let kind = if data { "data" } else { "control" };
                self.status_once(idx, slot, format!("XPT {kind} preamble: {what} {target} from {source}"), t, out);
            }
        }
    }

    /// XPT: repeater `n` is the free one, the rest channel (or none is).
    fn free_repeater(&mut self, n: u8) {
        self.rest = super::xpt::repeater(n).map(|lcn| (2 * lcn - 1, self.resolve(lcn).unwrap_or(0)));
    }

    /// A site status (Capacity Plus, XPT): the rest channel and the busy
    /// logical slots, with their talkgroups' low bytes where known.
    fn report_site(&mut self, what: &str, busy: &[(u32, Option<u32>)], idx: usize, slot: u8, t: f64, out: &mut Vec<Message>) {
        let rest_s = self.rest.map_or(String::new(), |(n, hz)| if hz > 0 { format!("rest channel LSN {n} ({})", mhz(hz)) } else { format!("rest channel LSN {n}") });
        let busy_s: Vec<String> = busy.iter().map(|(n, tg)| format!("LSN {n}{}", tg.map_or(String::new(), |g| format!(" TG …{g}")))).collect();
        let meta = format!("{what} site status: {rest_s}{}{}", if busy.is_empty() { "" } else { "; busy: " }, busy_s.join(", "));
        self.status_once(idx, slot, meta, t, out);
    }

    /// A status line, when it changes (or every so often): site statuses come several times a second.
    fn status_once(&mut self, idx: usize, slot: u8, meta: String, t: f64, out: &mut Vec<Message>) {
        self.recent_status.retain(|(_, at)| t - at < STATUS_REPEAT_S);
        if self.recent_status.iter().any(|(m, _)| *m == meta) {
            return;
        }
        self.recent_status.push((meta.clone(), t));
        self.status_msg(idx, slot, meta, t, out);
    }

    fn set_variant(&mut self, v: Variant) {
        // Capacity Max also sends the standard (Tier III) blocks: it wins.
        if self.variant != Some(v) && !(self.variant == Some(Variant::CapacityMax) && v == Variant::TierIII) {
            self.variant = Some(v);
            self.notes.push(format!("This is a {} system", v.name()));
            // Every carrier is the site's: their decoders read its link control as the site's kind.
            for c in &mut self.carriers {
                c.chan.variant = Some(v);
            }
        }
    }

    /// C_ALOHA and C_BCAST carry the site's System Identity Code (bits 40–55): heard on a
    /// control channel, its network and site are the site's identity.
    fn set_sys_code(&mut self, idx: usize, k: &Csbk) {
        let c = SysCode(k.bits(40, 56) as u16);
        if idx >= self.n_control || self.sys_code == Some(c) {
            return;
        }
        self.sys_code = Some(c);
        self.identity.set(IdField::Network, c.network());
        self.identity.set(IdField::Site, c.site());
        self.notes.push(format!("System code {c}"));
    }

    /// Site status named rest channel LSN `n`. A repeater with a call on one
    /// slot sends site status on the other, naming the rest channel elsewhere,
    /// and one in hang time may send it on both for a while; the rest channel
    /// sends it on both, naming itself, for as long as it is the rest channel.
    /// But on some sites an idle repeater stays keyed, sending it on both too:
    /// while another carrier does the same, neither is known to be the rest
    /// channel, and a channel a call placed is never moved by this.
    fn rest_channel(&mut self, n: u32, idx: usize, t: f64) {
        let (lcn, _) = lsn(n);
        let c = &mut self.carriers[idx];
        let both = c.status_s.iter().all(|&s| t - s < 0.5);
        c.rest_run = match c.rest_run {
            Some((l, since)) if both && l == lcn => Some((l, since)),
            _ if both => Some((lcn, t)),
            _ => None,
        };
        let run = c.rest_run.is_some_and(|(_, since)| t - since >= REST_RUN_S);
        let hz = c.hz;
        let rival = self.carriers.iter().enumerate().any(|(i, o)| i != idx && o.rest_run.is_some_and(|(l, _)| l == lcn) && o.status_s.iter().all(|&s| t - s < 1.0));
        if run && !rival && !self.placed_by_call.contains(&lcn) {
            self.set_learned(lcn, hz, "it is the rest channel");
        }
        self.rest = Some((n, self.resolve(lcn).unwrap_or(0)));
    }

    fn set_learned(&mut self, lcn: u32, hz: u64, why: &str) {
        if self.table.learn(lcn, hz) {
            self.notes.push(format!("Channel {lcn} is {} ({why})", mhz(hz)));
        }
    }

    /// A talkgroup's link control on a watched carrier's slot: the frequency
    /// of a logical channel a recent grant sent it to.
    fn learn(&mut self, tg: u32, slot: u8, hz: u64) {
        let hit = self.pending.iter().position(|p| p.slot == slot && if p.low8 { p.talkgroup == tg & 0xff } else { p.talkgroup == tg });
        if let Some(i) = hit {
            let p = self.pending.remove(i);
            if self.resolve(p.lcn).is_none() || self.table.is_learned(p.lcn) {
                self.set_learned(p.lcn, hz, &format!("talkgroup {tg} granted there came up on it"));
                self.placed_by_call.insert(p.lcn);
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
        let kind = MessageType::voice(group, update);
        let mut m = self.base(idx, slot_of_block, kind, t);
        m.freq_hz = hz.unwrap_or(0);
        // (The colour code of the slot the grant came on; the slot it grants.)
        m.voice = Voice::Dmr { slot, color_code: m.voice.color_code() };
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
            (FID_STANDARD, 0x19) => {
                self.set_variant(Variant::TierIII);
                self.set_sys_code(idx, k);
            }
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
                self.set_sys_code(idx, k);
                // C_BCAST: a channel's frequency (type 5, with absolute parameters).
                if k.bits(16, 21) == 5 {
                    if let Some(c) = cont {
                        let lcn = field(c, 22, 34);
                        if let Some(hz) = absolute_downlink(c) {
                            self.set_learned(lcn, hz, "announced");
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
                // (Learned only from site status, it's a guess the call can correct.)
                if self.resolve(lcn).is_none() || self.table.is_learned(lcn) && !self.placed_by_call.contains(&lcn) {
                    self.pending.retain(|q| !(q.lcn == lcn && q.slot == s));
                    self.pending.push(Pending { lcn, slot: s, talkgroup: tg, low8: true, t });
                }
            }
        }
        self.report_site("Capacity Plus", &busy, idx, slot, t, out);
    }
}

/// An MBC continuation's absolute channel parameters → downlink frequency.
fn absolute_downlink(c: &[u8; 12]) -> Option<u64> {
    let hz = field(c, 57, 67) as u64 * 1_000_000 + field(c, 67, 80) as u64 * 125;
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
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].talkgroup, out[0].voice.slot().unwrap_or(9), out[0].source), (MessageType::Grant, 0, 206, 1, 2023));
        let lc = super::super::slot::Lc([0x00, 0x10, 0x40, 0x00, 0x00, 0xce, 0x00, 0x07, 0xe7]);
        // Colour code 0, from the control channel and on the voice channel's slot.
        s.color_code = Some(0);
        s.carriers[1].chan.slots[1].color_code = Some(0);
        out.clear();
        s.event(1, 1, SlotEvent::Lc { lc, from: LcFrom::Header }, 1.4, &mut out);
        assert_eq!(s.resolve(101), Some(452_275_000));
        assert!(out[0].encrypted && out[0].freq_hz == 452_275_000 && out[0].voice.slot().unwrap_or(9) == 1);
        // The next grant for the channel resolves at once.
        out.clear();
        s.csbk(0, 0, &g, None, 30.0, &mut out);
        assert_eq!(out[0].freq_hz, 452_275_000);
        let mut t = Site::new(&[452_175_000.0], 24_000.0, DmrConfig::default());
        t.map_from_str(&s.map_to_string());
        assert_eq!(t.resolve(101), Some(452_275_000));
    }

    /// A System Identity Code splits by its model: tiny 9 + 3 bits, small 7 + 5, large 4 + 8,
    /// huge 2 + 10, then two of partition.
    #[test]
    fn a_system_identity_code_is_its_network_and_site() {
        let code = |model: u16, net: u16, site: u16, site_bits: u16| SysCode(model << 14 | (net << site_bits | site) << 2);
        let parts = |c: SysCode| (c.model(), c.network(), c.site());
        assert_eq!(parts(code(0, 300, 5, 3)), ("Tiny", 300, 5));
        assert_eq!(parts(code(1, 100, 17, 5)), ("Small", 100, 17));
        assert_eq!(parts(code(2, 9, 200, 8)), ("Large", 9, 200));
        let h = SysCode(code(3, 2, 1000, 10).0 | 0b10);
        assert_eq!((parts(h), h.category()), (("Huge", 2, 1000), "B"), "the partition isn't the site's");
        assert_eq!(SysCode(0xb013).to_string(), "B013 (Large, network 12, site 4, category A+B)");
    }

    /// A site says what it is: its colour code, and from a Tier III C_ALOHA its network and site.
    #[test]
    fn a_tier_iii_site_states_its_colour_code_network_and_site() {
        let mut s = Site::new(&[452_175_000.0], 24_000.0, DmrConfig { color_code: Some(1), ..Default::default() });
        assert_eq!(s.identity().get(IdField::ColorCode), Some(1));
        let mut b = [0u8; 10];
        b[0] = 0x80 | 0x19;
        put(&mut b, 40, 56, (1 << 14 | (100 << 5 | 17) << 2) as u64);
        s.csbk(0, 0, &Csbk(b), None, 1.0, &mut Vec::new());
        assert_eq!((s.identity().get(IdField::Network), s.identity().site()), (Some(100), Some(17)));
        assert_eq!(s.variant, Some(Variant::TierIII));
    }

    /// A Tier III site whose Hytera radios send Hytera's own blocks (one that
    /// is an XPT site status's shape) stays Tier III: its voice LC keeps
    /// 24-bit IDs, and it isn't told it's XPT.
    #[test]
    fn hytera_blocks_on_a_tier3_site_are_not_xpt() {
        let mut s = Site::new(&[451_012_500.0], 24_000.0, DmrConfig::default());
        let mut out = Vec::new();
        s.csbk(0, 0, &csbk([0x99, 0x00, 0x00, 0x00, 0x00, 0xb0, 0x13, 0x00, 0x00, 0x00]), None, 1.0, &mut out);
        assert_eq!(s.variant, Some(Variant::TierIII));
        s.carriers[0].chan.slots[0].color_code = Some(1);
        s.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0x0a, 0x68, 0x28, 0, 25, 0, 0, 0, 0, 0]), mbc: false }, 2.0, &mut out);
        assert_eq!((s.variant, s.carriers[0].chan.variant), (Some(Variant::TierIII), Some(Variant::TierIII)));
        assert!(!s.notes.iter().any(|n| n.contains("XPT")), "{:?}", s.notes);
        // Before anything standard is heard, the same block is XPT's (an XPT site's).
        let mut x = Site::new(&[451_012_500.0], 24_000.0, DmrConfig::default());
        x.carriers[0].chan.slots[0].color_code = Some(1);
        x.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0x0a, 0x68, 0x28, 0, 25, 0, 0, 0, 0, 0]), mbc: false }, 2.0, &mut out);
        assert_eq!(x.variant, Some(Variant::Xpt));
    }

    /// C_ALOHA and C_BCAST both carry it, read from a control channel only.
    #[test]
    fn aloha_and_bcast_give_the_sys_code() {
        let mut s = Site::new(&[453_000_000.0], 24_000.0, DmrConfig { channels: vec![453_100_000.0], ..Default::default() });
        let mut out = Vec::new();
        // ALOHA, system code B013 at bits 40–55.
        s.csbk(0, 0, &csbk([0x99, 0x00, 0x00, 0x00, 0x00, 0xb0, 0x13, 0x00, 0x00, 0x00]), None, 1.0, &mut out);
        assert_eq!((s.variant, s.status().sys_code), (Some(Variant::TierIII), Some(SysCode(0xb013))));
        assert_eq!((s.identity().get(IdField::Network), s.identity().site()), (Some(12), Some(4)));
        assert_eq!(s.notes.iter().filter(|n| n.starts_with("System code B013")).count(), 1);
        // Not from a voice carrier.
        s.csbk(1, 0, &csbk([0x99, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x00, 0x00, 0x00]), None, 2.0, &mut out);
        assert_eq!(s.sys_code, Some(SysCode(0xb013)));
        // C_BCAST carries it at the same place.
        s.csbk(0, 0, &csbk([0xa8, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x00, 0x00, 0x00]), None, 3.0, &mut out);
        assert_eq!(s.sys_code, Some(SysCode(0x4005)));
        assert_eq!((s.identity().get(IdField::Network), s.identity().site()), (Some(0), Some(1)));
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

    /// As heard on a two-repeater site (461.45 / 463.225 MHz): both stay keyed,
    /// sending site status on both slots that names rest LSN 1, so neither is
    /// taken for channel 1 (it flipped between them). A call places it.
    #[test]
    fn capacity_plus_two_keyed_repeaters_dont_teach_the_rest_channel() {
        let mut s = Site::new(&[461_450_000.0, 463_225_000.0], 24_000.0, DmrConfig::default());
        let mut out = Vec::new();
        let status = |s: &mut Site, idx: usize, t: f64, busy: [u8; 2], out: &mut Vec<Message>| {
            s.csbk(idx, 1, &csbk([0xbe, 0x10, 0xe1, 0, 0, 0, 0, 0, 0, 0]), None, t, out);
            s.csbk(idx, 0, &csbk([0xbe, 0x10, 0xc1, busy[0], busy[1], 0, 0, 0, 0, 0]), None, t + 0.03, out);
        };
        for i in 0..20 {
            let t = i as f64 * 0.3;
            status(&mut s, 0, t, [0, 0], &mut out);
            status(&mut s, 1, t + 0.1, [0, 0], &mut out);
        }
        assert_eq!(s.resolve(1), None, "{:?}", s.notes);
        // LSN 1 busy with talkgroup …12, which comes up on 461.45 slot 1.
        status(&mut s, 0, 6.0, [0x80, 12], &mut out);
        s.learn(12, 0, 461_450_000);
        assert_eq!(s.resolve(1), Some(461_450_000));
        // Now 463.225 alone names LSN 1 on both slots for a while: still 461.45.
        for i in 0..20 {
            status(&mut s, 1, 10.0 + i as f64 * 0.3, [0, 0], &mut out);
        }
        assert_eq!(s.resolve(1), Some(461_450_000), "{:?}", s.notes);
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
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].voice.slot().unwrap_or(9), out[0].talkgroup, out[0].source), (MessageType::Grant, 451_012_500, 0, 3001, 12));
        assert_eq!(s.variant, Some(Variant::TierIII));
        // A private call with absolute parameters (channel 0xFFF, MBC).
        out.clear();
        s.csbk(0, 0, &tier3_grant(0x30, 0xfff, 1, 777, 12), Some(&absolute(9, 451_587_500)), 2.0, &mut out);
        assert_eq!((out[0].kind, out[0].freq_hz, out[0].voice.slot().unwrap_or(9)), (MessageType::UuVGrant, 451_587_500, 1));
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
        assert_eq!((out[0].freq_hz, out[0].voice.slot().unwrap_or(9), out[0].talkgroup, out[0].source), (453_100_000, 1, 55, 4242));
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
    #[test]
    fn xpt_free_repeater_is_the_rest_channel_and_calls_come_from_voice_lc() {
        use crate::dmr::slot::Lc;
        let mut s = Site::new(&[451_000_000.0], 24_000.0, DmrConfig {
            channels: vec![452_000_000.0],
            lcn_table: BTreeMap::from([(1, 451_000_000)]),
            ..Default::default()
        });
        // Repeaters of one XPT site with their own colour codes.
        s.carriers[0].chan.slots[0].color_code = Some(1);
        s.carriers[1].chan.slots[1].color_code = Some(2);
        let mut out = Vec::new();
        // Status bank 0: free repeater 2, LSN 1 busy (TG …25).
        s.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0x0a, 0x68, 0x28, 0, 25, 0, 0, 0, 0, 0]), mbc: false }, 0.0, &mut out);
        assert_eq!(s.variant, Some(Variant::Xpt));
        assert_eq!(s.rest, Some((3, 0)), "repeater 2 is LSN 3, its frequency not known yet");
        assert_eq!(s.control_hz(), Some(451_000_000));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].kind, out[0].meta.as_str()), (MessageType::Status, "XPT site status: rest channel LSN 3; busy: LSN 1 TG …25"));
        s.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0x0a, 0x68, 0x28, 0, 25, 0, 0, 0, 0, 0]), mbc: false }, 0.5, &mut out);
        assert_eq!(out.len(), 1, "an unchanged status is reported once");
        // A setup for repeater 2 slot 2 (in a terminator burst: not the end of a call).
        let setup = Lc([0xc9, 0x68, 0x27, 0x10, 0, 25, 0, 0x12, 0x34]);
        out.clear();
        s.event(0, 0, SlotEvent::Lc { lc: setup, from: LcFrom::Terminator }, 0.1, &mut out);
        assert!(out.iter().all(|m| m.kind == MessageType::Status), "a setup is not a call");
        // Its voice LC on 452 MHz slot 2 (16-bit IDs, as the channel hands it on): the call, and repeater 2 is learned.
        out.clear();
        assert_eq!(s.carriers[1].chan.variant, Some(Variant::Xpt), "every carrier reads the site's link control as XPT's");
        let lc = Lc([0, 0x68, 0, 0, 0, 25, 0, 0x12, 0x34]);
        s.event(1, 1, SlotEvent::Lc { lc, from: LcFrom::Embedded }, 0.2, &mut out);
        assert_eq!((out[0].kind, out[0].talkgroup, out[0].source, out[0].freq_hz, out[0].voice.slot()), (MessageType::Grant, 25, 0x1234, 452_000_000, Some(1)));
        assert_eq!(s.resolve(2), Some(452_000_000));
        assert_eq!(s.status().carriers[1].slots[1], Some((25, 0x1234)));
        s.event(1, 1, SlotEvent::Lc { lc, from: LcFrom::Terminator }, 0.4, &mut out);
        assert_eq!(s.status().carriers[1].slots[1], None);
    }

    #[test]
    fn the_control_channel_stays_put_while_it_sends() {
        let mut s = Site::new(&[451_000_000.0, 452_000_000.0, 453_000_000.0], 24_000.0, DmrConfig::default());
        let mut out = Vec::new();
        // Two carriers sending control blocks by turns (XPT repeaters with a free slot, say).
        let block = SlotEvent::Csbk { csbk: Csbk([0x3f, 0, 0, 0, 0, 0, 0, 0, 0, 0]), mbc: false };
        for i in 0..10 {
            let t = i as f64 * 0.1;
            s.now_s = t;
            s.event(i % 2 + 1, 0, block.clone(), t, &mut out);
            assert_eq!(s.control_hz(), Some(452_000_000), "{t}");
        }
        // The first stops: the other takes over.
        s.now_s = 3.5;
        s.event(2, 0, block.clone(), 3.5, &mut out);
        assert_eq!(s.control_hz(), Some(453_000_000));
        // The rest channel, once its frequency is known, is the control channel.
        s.event(0, 0, block.clone(), 3.6, &mut out);
        s.now_s = 3.6;
        assert_eq!(s.control_hz(), Some(453_000_000));
        s.rest = Some((1, 451_000_000));
        assert_eq!(s.control_hz(), Some(451_000_000));
    }

    #[test]
    fn xpt_configured_colour_code_still_filters() {
        use crate::dmr::slot::Lc;
        let mut s = Site::new(&[451_000_000.0, 452_000_000.0], 24_000.0, DmrConfig { color_code: Some(1), ..Default::default() });
        let mut out = Vec::new();
        s.carriers[0].chan.slots[0].color_code = Some(1);
        s.carriers[1].chan.slots[0].color_code = Some(2);
        s.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0x0a, 0x68, 0, 0, 0, 0, 0, 0, 0, 0]), mbc: false }, 0.0, &mut out);
        assert_eq!(s.variant, Some(Variant::Xpt));
        out.clear();
        s.event(1, 0, SlotEvent::Lc { lc: Lc([0, 0x68, 0, 0, 0, 25, 0, 0, 5]), from: LcFrom::Header }, 0.1, &mut out);
        assert!(out.is_empty());
        s.cfg.color_code = None;
        s.event(1, 0, SlotEvent::Lc { lc: Lc([0, 0x68, 0, 0, 0, 25, 0, 0, 5]), from: LcFrom::Header }, 0.1, &mut out);
        assert_eq!((out.len(), out[0].voice.color_code()), (1, Some(2)));
    }

    #[test]
    fn data_carrier_identifies_xpt_only_from_preamble_and_never_grants_voice() {
        use crate::dmr::slot::Lc;
        let mut s = Site::new(&[453_637_500.0], 24_000.0, DmrConfig::default());
        s.carriers[0].chan.slots[0].color_code = Some(13);
        let mut out = Vec::new();
        // Hytera proprietary blocks as heard on the data carrier: not XPT, not reported.
        let observed = [
            SlotEvent::Csbk { csbk: Csbk([0x8d, 0x68, 0x7f, 0xff, 0, 0, 0, 0, 0, 0]), mbc: false },
            SlotEvent::Slco([0xc6, 0x87, 0xff, 0xfe, 0xe0]),
            SlotEvent::Lc { lc: Lc([0x0d, 0x68, 0, 0x7f, 0xff, 0x80, 0, 2, 8]), from: LcFrom::Terminator },
            SlotEvent::Lc { lc: Lc([0x30, 0x68, 0xf0, 0x7f, 0xff, 0, 0, 0x10, 0x60]), from: LcFrom::Terminator },
        ];
        for e in observed { s.event(0, 0, e, 0.0, &mut out); }
        assert!(out.is_empty());
        assert_eq!(s.variant, None, "proprietary payloads do not establish XPT");
        s.event(0, 0, SlotEvent::Csbk { csbk: Csbk([0xbd, 0x68, 0x80, 0x12, 0xf0, 2, 8, 0, 0, 16]), mbc: false }, 0.1, &mut out);
        assert_eq!(s.variant, Some(Variant::Xpt));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].kind, out[0].meta.as_str()), (MessageType::Status, "XPT data preamble: radio 520 from 16"));
        assert!(s.rest.is_none() && s.pending.is_empty(), "a preamble's free repeater and addresses are not the voice site's");
        assert!(s.table.saved().is_empty(), "no channel mapping learned from data addresses");
        // Voice after a data terminator is still the voice.
        let lc = Lc([0, 0x68, 0, 0, 0, 149, 0, 0x13, 0x8e]);
        s.event(0, 0, SlotEvent::Lc { lc, from: LcFrom::Header }, 0.3, &mut out);
        assert_eq!((out.last().unwrap().talkgroup, out.last().unwrap().source), (149, 5006));
    }

}
