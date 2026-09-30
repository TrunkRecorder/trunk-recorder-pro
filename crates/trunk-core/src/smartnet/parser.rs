//! OSWs → trunking messages: Trunk Recorder's SmartnetParser (itself OP25's
//! tk_smartnet.py), branch for branch where it produces something.
//!
//! OSWs are queued and looked at a few at a time, because most messages span
//! two or three OSWs and the site controller interleaves one-OSW idles into
//! them. A lost OSW (bad CRC while in sync) queues a reset marker, so what
//! follows is not stitched onto the wrong first half.
//!
//! Talkgroups are reported with their four status bits cleared (as Trunk
//! Recorder files them); the bits give encrypted / emergency / patch.

use std::collections::{HashMap, VecDeque};

use super::osw::Osw;
use crate::trunk::message::{Message, MessageType, Patch};

/// Three-OSW messages, two idles slipped in, and a reset marker.
const QUEUE_SIZE: usize = 6;
const QUEUE_RESET_CMD: u16 = 0xffe;

/// How a system numbers its channels.
#[derive(Clone, Debug, PartialEq)]
pub enum Bandplan {
    /// 800 MHz, pre-rebanding ("800_standard"), rebanded ("800_reband") or splinter ("800_splinter").
    B800 { rebanded: bool, splinter: bool },
    /// 900 MHz.
    B900,
    /// VHF / UHF (OBT, "400_custom" etc.): receive channels `offset`… map to
    /// `base_hz` + `spacing_hz` · (chan − offset), up to `high_hz`.
    Obt { base_hz: f64, spacing_hz: f64, offset: u16, high_hz: f64 },
    /// Plan unknown (the survey): a channel's "frequency" is its number.
    Raw,
}

impl Bandplan {
    /// From Trunk Recorder's config names: "800_standard", "800_reband",
    /// "800_splinter", "900", "400" / "400_custom" / "obt" (with base, spacing, offset, high).
    pub fn from_config(name: &str, base_hz: f64, spacing_hz: f64, offset: u16, high_hz: f64) -> Result<Self, String> {
        match name {
            "" | "800_standard" | "800_domestic" => Ok(Bandplan::B800 { rebanded: false, splinter: false }),
            "800_reband" | "800_rebanded" => Ok(Bandplan::B800 { rebanded: true, splinter: false }),
            "800_splinter" | "800_domestic_splinter" => Ok(Bandplan::B800 { rebanded: false, splinter: true }),
            "900" => Ok(Bandplan::B900),
            n if n.starts_with("400") || n.eq_ignore_ascii_case("obt") => {
                if base_hz <= 0.0 || spacing_hz <= 0.0 || high_hz <= base_hz {
                    return Err(format!("SmartNet band plan {n} needs bandplanBase, bandplanSpacing, bandplanOffset and bandplanHigh"));
                }
                Ok(Bandplan::Obt { base_hz, spacing_hz, offset, high_hz })
            }
            n => Err(format!("unknown SmartNet band plan {n:?} (800_standard, 800_reband, 800_splinter, 900, 400_custom)")),
        }
    }

    fn is_obt(&self) -> bool {
        matches!(self, Bandplan::Obt { .. })
    }

    /// A channel number's receive (outbound) frequency, Hz.
    pub fn rx_hz(&self, chan: u16) -> Option<u64> {
        let mhz = match *self {
            Bandplan::B800 { rebanded, splinter } => {
                let c = chan as f64;
                if chan <= 0x1b7 && rebanded {
                    851.0125 + 0.025 * c
                } else if (0x1b8..=0x22f).contains(&chan) && rebanded {
                    851.0250 + 0.025 * (c - 440.0)
                } else if chan <= 0x257 && splinter {
                    851.0 + 0.025 * c
                } else if (0x258..=0x2cf).contains(&chan) && splinter {
                    866.0125 + 0.025 * (c - 600.0)
                } else if chan <= 0x2cf && !rebanded && !splinter {
                    851.0125 + 0.025 * c
                } else if (0x2d0..=0x2f7).contains(&chan) {
                    866.0 + 0.025 * (c - 720.0)
                } else if (0x32f..=0x33f).contains(&chan) {
                    867.0 + 0.025 * (c - 815.0)
                } else if (0x3c1..=0x3fe).contains(&chan) {
                    867.425 + 0.025 * (c - 961.0)
                } else if chan == 0x3be {
                    868.975
                } else {
                    return None;
                }
            }
            Bandplan::B900 => {
                if chan > 0x1de {
                    return None;
                }
                935.0125 + 0.0125 * chan as f64
            }
            Bandplan::Raw => return (chan < 0x2f8).then_some(chan as u64),
            Bandplan::Obt { base_hz, spacing_hz, offset, high_hz } => {
                let high = offset as f64 + (high_hz - base_hz) / spacing_hz;
                if chan < offset || chan as f64 >= high + 0.5 {
                    return None;
                }
                return Some((base_hz + spacing_hz * (chan - offset) as f64).round() as u64);
            }
        };
        Some((mhz * 1e6).round() as u64)
    }

    /// OBT: an inbound (radio transmit) channel — they sit below the outbound ones.
    fn is_tx_chan(&self, chan: u16) -> bool {
        match *self {
            Bandplan::Obt { offset, .. } => chan < offset && chan + 380 >= offset,
            Bandplan::Raw => false,
            _ => self.rx_hz(chan).is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Q {
    addr: u16,
    grp: bool,
    cmd: u16,
    /// Outbound frequency when `cmd` is a channel, Hz.
    rx: Option<u64>,
    tx: bool,
    t: f64,
}

impl Q {
    fn is_reset(&self) -> bool {
        self.cmd == QUEUE_RESET_CMD
    }
}

pub struct Parser {
    plan: Bandplan,
    q: VecDeque<Q>,
    /// Voice mode learned from grants, by talkgroup: analog?
    modes: HashMap<u32, bool>,
    /// Mode for a talkgroup never seen granted.
    pub analog_default: bool,
    pub sys_id: Option<u32>,
    pub site: Option<u32>,
    pub cc_hz: Option<u64>,
    pub osws: u64,
}

fn tg_flags(addr: u16) -> (u32, bool, bool) {
    let options = addr & 0x7;
    ((addr & 0xfff0) as u32, addr & 0x8 != 0, matches!(options, 2 | 4 | 5))
}

fn is_patch_group(addr: u16) -> bool {
    matches!(addr & 0x7, 3 | 4)
}
fn is_multiselect_group(addr: u16) -> bool {
    matches!(addr & 0x7, 5 | 7)
}

impl Parser {
    pub fn new(plan: Bandplan) -> Self {
        Parser { plan, q: VecDeque::new(), modes: HashMap::new(), analog_default: false, sys_id: None, site: None, cc_hz: None, osws: 0 }
    }

    pub fn bandplan(&self) -> &Bandplan {
        &self.plan
    }

    /// A good OSW heard at `t` (sample-clock seconds).
    pub fn osw(&mut self, o: Osw, t: f64, out: &mut Vec<Message>) {
        self.osws += 1;
        let rx = self.plan.rx_hz(o.cmd);
        let tx = self.plan.is_tx_chan(o.cmd);
        self.push(Q { addr: o.addr, grp: o.grp, cmd: o.cmd, rx, tx, t });
        self.process(out);
    }

    /// An OSW was lost: what's queued can't be trusted to pair up.
    pub fn bad(&mut self, t: f64, out: &mut Vec<Message>) {
        self.q.clear();
        self.push(Q { addr: 0xffff, grp: true, cmd: QUEUE_RESET_CMD, rx: None, tx: false, t });
        self.process(out);
    }

    fn push(&mut self, q: Q) {
        if self.q.len() >= QUEUE_SIZE {
            self.q.pop_front();
        }
        self.q.push_back(q);
    }

    fn msg(&self, kind: MessageType, t: f64) -> Message {
        Message { kind, time_s: t, sys_id: self.sys_id.unwrap_or(0), site: self.site.unwrap_or(0), source: -1, priority: 3, ..Default::default() }
    }

    fn voice(&mut self, kind: MessageType, t: f64, freq_hz: u64, addr: u16, src: Option<u16>, analog: Option<bool>) -> Message {
        let (tg, encrypted, emergency) = tg_flags(addr);
        let analog = match analog {
            Some(a) => {
                self.modes.insert(tg, a);
                a
            }
            None => self.modes.get(&tg).copied().unwrap_or(self.analog_default),
        };
        let mut m = self.msg(kind, t);
        m.freq_hz = freq_hz;
        m.talkgroup = tg;
        m.source = src.map_or(-1, |s| s as i64);
        m.encrypted = encrypted;
        m.emergency = emergency;
        m.analog = analog;
        m.meta = format!(
            "{} tg {tg} {:.5} MHz{}{}{}{}",
            if kind == MessageType::Grant { "grant" } else { "update" },
            freq_hz as f64 / 1e6,
            src.map_or(String::new(), |s| format!(" src {s}")),
            if analog { " analog" } else { " digital" },
            if encrypted { " enc" } else { "" },
            if emergency { " emerg" } else { "" }
        );
        m
    }

    fn sysid(&mut self, sys: u16, cc: Option<u64>, t: f64, out: &mut Vec<Message>) {
        let changed = self.sys_id != Some(sys as u32) || (cc.is_some() && cc != self.cc_hz);
        self.sys_id = Some(sys as u32);
        if cc.is_some() {
            self.cc_hz = cc;
        }
        if changed {
            let mut m = self.msg(MessageType::SysId, t);
            m.freq_hz = cc.unwrap_or(0);
            m.meta = format!("system {sys:04x}{}", cc.map_or(String::new(), |f| format!(" control channel {:.5} MHz", f as f64 / 1e6)));
            out.push(m);
        }
    }

    fn process(&mut self, out: &mut Vec<Message>) {
        if self.q.len() < QUEUE_SIZE {
            return;
        }
        let Some(mut osw2) = self.q.pop_front() else { return };
        let mut unknown = false;
        let mut reset: Option<Q> = None;
        while osw2.is_reset() {
            reset = Some(osw2);
            let Some(next) = self.q.pop_front() else { break };
            osw2 = next;
            if self.q.len() != QUEUE_SIZE - 2 {
                // More than one reset queued: wait for more OSWs.
                self.q.push_front(reset.unwrap());
                return;
            }
        }
        if osw2.is_reset() {
            self.q.push_front(osw2);
            return;
        }
        let obt = self.plan.is_obt();

        if obt && osw2.tx {
            let Some(osw1) = self.q.pop_front() else { return };
            if osw1.cmd == 0x320 && osw2.grp && osw1.grp {
                // Three-OSW system information.
                match self.q.pop_front() {
                    Some(osw0) if osw0.cmd == 0x30b && osw0.addr & 0xfc00 == 0x6000 => {
                        self.sysid(osw2.addr, None, osw1.t, out);
                        if !osw0.grp {
                            self.site = Some(((osw1.addr as u32 & 0xfc00) >> 10) + 1);
                        }
                    }
                    Some(osw0) => {
                        unknown = true;
                        self.q.push_front(osw0);
                    }
                    None => {}
                }
            } else if osw1.cmd == 0x2f8 {
                // Two-OSW system idle.
            } else if osw1.cmd == 0x30b && osw1.grp && osw1.addr & 0xfc00 == 0x2800 {
                // System ID + this control channel (seen on WMATA; not in TR's parser).
                let cc = self.plan.rx_hz(osw1.addr & 0x3ff);
                self.sysid(osw2.addr, cc, osw1.t, out);
            } else if let (Some(f), true, true, true) = (osw1.rx, osw1.grp, osw1.addr != 0, osw2.addr != 0) {
                // Two-OSW group voice grant: the inbound channel's OSW carries
                // the source and (group bit) the mode — set = analog. Except
                // for a patch / multiselect group, where the bit is set on
                // all-P25 systems too (seen on WMATA): keep what's known.
                let mode = (!is_patch_group(osw1.addr) && !is_multiselect_group(osw1.addr)).then_some(osw2.grp);
                let m = self.voice(MessageType::Grant, osw1.t, f, osw1.addr, Some(osw2.addr), mode);
                out.push(m);
            } else if osw1.rx.is_some() && !osw1.grp && osw1.addr != 0 && osw2.addr != 0 {
                // Two-OSW private / interconnect call.
            } else {
                unknown = true;
                self.q.push_front(osw1);
            }
        } else if let (Some(f), true) = (osw2.rx, osw2.grp) {
            // One-OSW voice update.
            let m = self.voice(MessageType::Update, osw2.t, f, osw2.addr, None, None);
            out.push(m);
        } else if osw2.rx.is_some() && !osw2.grp && osw2.addr & 0xff00 == 0x1f00 {
            // One-OSW control channel broadcast.
            self.cc_hz = osw2.rx;
        } else if osw2.cmd == 0x2f8 && !osw2.grp {
            // Idle.
        } else if (osw2.cmd == 0x300 || osw2.cmd == 0x303) && osw2.grp {
            // Group / emergency busy queued.
        } else if osw2.cmd == 0x308 {
            let Some(osw1) = self.q.pop_front() else {
                self.q.push_front(osw2);
                return;
            };
            if osw1.rx.is_some() && !osw1.grp && osw1.addr & 0xff00 == 0x1f00 {
                // System ID + control channel.
                self.sysid(osw2.addr, osw1.rx, osw1.t, out);
            } else if let (Some(f), true, true, true) = (osw1.rx, osw1.grp, osw1.addr != 0, osw2.addr != 0) {
                // Two-OSW analog group voice grant.
                let m = self.voice(MessageType::Grant, osw1.t, f, osw1.addr, Some(osw2.addr), Some(true));
                out.push(m);
            } else if osw1.rx.is_some() && !osw1.grp && osw1.addr != 0 && osw2.addr != 0 {
                // Analog private / interconnect call.
            } else if osw1.cmd == 0x2f8 {
                // One- or two-OSW idle; an idle's first half may be interleaved.
                if let Some(osw0) = self.q.pop_front() {
                    const LATER: [u16; 13] = [0x30a, 0x30b, 0x30d, 0x310, 0x311, 0x317, 0x318, 0x319, 0x31a, 0x320, 0x322, 0x32e, 0x340];
                    self.q.push_front(osw0);
                    if LATER.contains(&osw0.cmd) {
                        self.q.push_front(osw2);
                    }
                }
            } else if (osw1.cmd == 0x300 || osw1.cmd == 0x303) && osw1.grp || osw1.cmd == 0x302 && !osw1.grp {
                // Busy queued.
            } else if osw1.cmd == 0x308 {
                // A two-OSW idle interleaved with another message?
                match self.q.pop_front() {
                    Some(osw0) if osw0.cmd == 0x2f8 => self.q.push_front(osw1),
                    Some(osw0) => {
                        unknown = true;
                        self.q.push_front(osw0);
                        self.q.push_front(osw1);
                    }
                    None => self.q.push_front(osw1),
                }
            } else if osw1.cmd == 0x30a && !osw1.grp && !osw2.grp {
                // Dynamic regroup.
            } else if osw1.cmd == 0x30b {
                self.extended(osw2, osw1, out);
            } else if osw1.cmd == 0x310 && !osw1.grp && !osw2.grp {
                let mut m = self.msg(MessageType::Affiliation, osw1.t);
                m.source = osw2.addr as i64;
                m.talkgroup = (osw1.addr & 0xfff0) as u32;
                m.meta = format!("affiliation src {} tg {}", osw2.addr, m.talkgroup);
                out.push(m);
            } else if (0x30d..=0x31b).contains(&osw1.cmd) && !osw1.grp && !osw2.grp {
                // Status / message / private call ring / call alert / trespass acknowledgements.
            } else if osw1.cmd == 0x320 {
                // Three-OSW system information (adjacent site).
                match self.q.pop_front() {
                    Some(osw0) if osw0.cmd == 0x2f8 && !osw0.grp => {
                        // An idle delayed into it: skip it too.
                        self.q.pop_front();
                    }
                    Some(osw0) if osw0.cmd == 0x30b && osw0.addr & 0xfc00 == 0x6000 => {}
                    Some(osw0) => {
                        unknown = true;
                        self.q.push_front(osw0);
                    }
                    None => {}
                }
            } else if (osw1.cmd == 0x322 || osw1.cmd == 0x32e) && osw2.grp && osw1.grp {
                // Date / time; emergency PTT.
            } else if osw1.cmd == 0x340 && osw2.grp && osw1.grp && (is_patch_group(osw2.addr) || is_multiselect_group(osw2.addr)) {
                let sg = (osw1.addr as u32 & 0xfff) << 4;
                let tg = (osw2.addr & 0xfff0) as u32;
                let mut m = self.msg(MessageType::PatchAdd, osw1.t);
                m.talkgroup = sg;
                m.patch = Some(Patch { sg, ga: [tg, 0, 0] });
                m.meta = format!("patch tg {sg} + {tg}");
                out.push(m);
            } else {
                unknown = true;
                self.q.push_front(osw1);
            }
        } else if osw2.cmd == 0x321 {
            let Some(osw1) = self.q.pop_front() else {
                self.q.push_front(osw2);
                return;
            };
            if let (Some(f), true, true, true) = (osw1.rx, osw2.grp, osw1.grp, osw1.addr != 0) {
                // Two-OSW digital group voice grant.
                let m = self.voice(MessageType::Grant, osw1.t, f, osw1.addr, Some(osw2.addr), Some(false));
                out.push(m);
            } else if osw1.rx.is_some() && !osw1.grp && osw1.addr != 0 && osw2.addr != 0 {
                // Digital private call.
            } else if osw1.cmd == 0x2f8 {
                if let Some(osw0) = self.q.pop_front() {
                    self.q.push_front(osw0);
                    if osw0.cmd == 0x317 || osw0.cmd == 0x318 {
                        self.q.push_front(osw2);
                    }
                }
            } else if (osw1.cmd == 0x315 || osw1.cmd == 0x317) && !osw1.grp && !osw2.grp {
                // Private call ring.
            } else {
                unknown = true;
                self.q.push_front(osw1);
            }
        } else if (osw2.cmd == 0x324 || osw2.cmd == 0x32a || osw2.cmd == 0x3a0) && osw2.grp
            || (osw2.cmd == 0x32b || osw2.cmd == 0x32c) && !osw2.grp
            || (0x360..=0x39f).contains(&osw2.cmd)
            || osw2.cmd == 0x3bf
            || osw2.cmd == 0x3c0
        {
            // Interconnect reject, affiliation request, BSI, scan marker,
            // roaming, AMSS, system status.
        } else {
            unknown = true;
            self.q.push_front(osw2);
            if reset.is_none() {
                // TR puts an unknown OSW back and retries it; with nothing to
                // explain it, drop it instead so the queue keeps moving.
                self.q.pop_front();
            }
        }
        if unknown {
            if let Some(r) = reset {
                self.q.push_front(r);
            }
        }
    }

    /// `0x308` + `0x30b`: system ID with a control channel, or an extended function.
    fn extended(&mut self, osw2: Q, osw1: Q, out: &mut Vec<Message>) {
        let mut osw0 = self.q.pop_front();
        if matches!(osw0, Some(o) if o.cmd == 0x2f8 && !o.grp) {
            // An idle delayed into the middle of it.
            osw0 = self.q.pop_front();
        }
        if let Some(o0) = osw0 {
            if osw1.grp && !o0.grp && o0.rx.is_some() && o0.addr & 0xff00 == 0x1f00 && osw1.addr & 0xfc00 == 0x2800 && osw1.addr & 0x3ff == o0.cmd {
                // Three-OSW system ID + control channel.
                self.sysid(osw2.addr, o0.rx, osw1.t, out);
                return;
            }
            self.q.push_front(o0);
        }
        if osw1.addr & 0xfc00 == 0x2800 && osw1.grp {
            let cc = self.plan.rx_hz(osw1.addr & 0x3ff);
            self.sysid(osw2.addr, cc, osw1.t, out);
        } else if osw1.addr & 0xfc00 == 0x6000 {
            // System ID + adjacent / alternate control channel.
            self.sysid(osw2.addr, None, osw1.t, out);
        } else if osw1.grp {
            if osw1.addr == 0x2021 && (is_patch_group(osw2.addr) || is_multiselect_group(osw2.addr)) {
                let sg = (osw2.addr & 0xfff0) as u32;
                let mut m = self.msg(MessageType::PatchDelete, osw1.t);
                m.talkgroup = sg;
                m.patch = Some(Patch { sg, ga: [0; 3] });
                m.meta = format!("patch cancel tg {sg}");
                out.push(m);
            }
        } else if osw1.addr == 0x261c {
            let mut m = self.msg(MessageType::Deregistration, osw1.t);
            m.source = osw2.addr as i64;
            m.meta = format!("deaffiliation src {}", osw2.addr);
            out.push(m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wmata() -> Bandplan {
        Bandplan::from_config("400_custom", 489_087_500.0, 25_000.0, 380, 496_612_500.0).unwrap()
    }

    fn feed(p: &mut Parser, osws: &[Osw]) -> Vec<Message> {
        let mut out = Vec::new();
        for (i, &o) in osws.iter().enumerate() {
            p.osw(o, i as f64 * 0.023, &mut out);
        }
        // Flush with idles.
        for i in 0..QUEUE_SIZE {
            p.osw(Osw { addr: 0x1234, grp: false, cmd: 0x2f8 }, 10.0 + i as f64, &mut out);
        }
        out
    }

    #[test]
    fn bandplans() {
        let b = wmata();
        assert_eq!(b.rx_hz(380), Some(489_087_500));
        assert_eq!(b.rx_hz(380 + 301), Some(496_612_500));
        assert_eq!(b.rx_hz(379), None);
        assert!(b.is_tx_chan(0) && b.is_tx_chan(379) && !b.is_tx_chan(380));
        let r = Bandplan::from_config("800_reband", 0.0, 0.0, 0, 0.0).unwrap();
        assert_eq!(r.rx_hz(0x1b8), Some(851_025_000));
        assert_eq!(r.rx_hz(0x2d0), Some(866_000_000));
        let s = Bandplan::from_config("800_standard", 0.0, 0.0, 0, 0.0).unwrap();
        assert_eq!(s.rx_hz(0x100), Some(857_412_500));
        assert_eq!(Bandplan::B900.rx_hz(8), Some(935_112_500));
    }

    #[test]
    fn obt_grant_and_update() {
        let mut p = Parser::new(wmata());
        // Voice channel 496.4625 MHz = chan 380 + 295; its inbound pair below 380.
        let vc = 380 + 295;
        let out = feed(
            &mut p,
            &[
                Osw { addr: 40020, grp: false, cmd: 100 },   // inbound channel, src, digital
                Osw { addr: 0x8053, grp: true, cmd: vc },    // outbound channel, tg 0x8050 + status 3
                Osw { addr: 0x8050, grp: true, cmd: vc },    // one-OSW update
                Osw { addr: 0x1234, grp: false, cmd: 0x2f8 }, // idle
            ],
        );
        let g: Vec<&Message> = out.iter().filter(|m| m.kind == MessageType::Grant).collect();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].talkgroup, g[0].freq_hz, g[0].source, g[0].analog), (0x8050, 496_462_500, 40020, false));
        let u: Vec<&Message> = out.iter().filter(|m| m.kind == MessageType::Update).collect();
        assert_eq!(u.len(), 1);
        assert!(!u[0].analog, "update inherits the grant's mode");
    }

    #[test]
    fn analog_and_digital_grants_800() {
        let mut p = Parser::new(Bandplan::B800 { rebanded: true, splinter: false });
        let out = feed(
            &mut p,
            &[
                Osw { addr: 1001, grp: true, cmd: 0x308 },
                Osw { addr: 0x0210, grp: true, cmd: 0x010 },
                Osw { addr: 2002, grp: true, cmd: 0x321 },
                Osw { addr: 0x0228, grp: true, cmd: 0x020 },
                Osw { addr: 0x0abc, grp: false, cmd: 0x308 },
                Osw { addr: 0x1f00, grp: false, cmd: 0x030 },
            ],
        );
        let g: Vec<(u32, u64, bool, bool)> = out.iter().filter(|m| m.kind == MessageType::Grant).map(|m| (m.talkgroup, m.freq_hz, m.analog, m.encrypted)).collect();
        assert_eq!(g, vec![(0x0210, 851_412_500, true, false), (0x0220, 851_812_500, false, true)]);
        assert_eq!(p.sys_id, Some(0x0abc));
        assert_eq!(p.cc_hz, Some(851_012_500 + 0x30 * 25_000));
    }

    #[test]
    fn bad_osw_does_not_pair_across_the_gap() {
        let mut p = Parser::new(Bandplan::B800 { rebanded: true, splinter: false });
        let mut out = Vec::new();
        // The first half of an analog grant, then a lost OSW, then a lone channel OSW.
        p.osw(Osw { addr: 1001, grp: true, cmd: 0x308 }, 0.0, &mut out);
        p.bad(0.02, &mut out);
        for i in 0..8 {
            p.osw(Osw { addr: 0x1234, grp: false, cmd: 0x2f8 }, 0.1 + i as f64 * 0.02, &mut out);
        }
        assert!(out.iter().all(|m| m.kind != MessageType::Grant));
    }
}
