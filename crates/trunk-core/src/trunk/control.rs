//! Control channels: one implementation per trunking protocol behind
//! [`ControlChannel`], so a trunked system ([`super::engine`]'s `Trunk`)
//! does what every protocol shares — hunting, the site lock, AutoTune, its
//! clock, feeding the call manager — and a new protocol is a module of its
//! own plus a line in [`build`].
//!
//! Every protocol ends in [`Message`]s (grants, updates, identity, patches…),
//! the one vocabulary the call manager speaks.
//!
//! ```text
//! P25:      receiver bank → framer → TSBKs → TsbkParser → Message
//! SmartNet: 2FSK → OSW framer → Parser → Message
//! DMR:      every carrier of the site: 4FSK → bursts → CSBK / link control → Message
//! NXDN:     every carrier of the site: 4FSK → frames → CAC / VCALL / SCCH → Message
//! ```

use std::collections::BTreeMap;

use num_complex::Complex32;

use super::engine::{NoteLevel, SmartnetConfig, SystemConfig};
use super::identity::{IdField, Identity};
use super::message::{Message, MessageType, TsbkParser};
use super::patches;
use super::voice::VoiceParams;
use crate::dmr;
use crate::metrics::{Instrumented, Sink};
use crate::nxdn;
use crate::p25::diversity::{best_frame, best_tsbks, Bank, BankConfig, Group};
use crate::p25::frame::TSDU;
use crate::smartnet;

/// One-sided channel filter cutoff of a P25 control channel, Hz.
pub const P25_CUTOFF_HZ: f64 = 7000.0;

/// A trunked system's protocol, and what it needs to be set up. (Voice
/// isn't part of it: each grant says what its call carries.)
#[derive(Clone, Debug, Default)]
pub enum Protocol {
    #[default]
    P25,
    SmartNet(SmartnetConfig),
    /// Every control channel (and [`dmr::DmrConfig::channels`]) is watched.
    Dmr(dmr::DmrConfig),
    /// Every control channel (and [`nxdn::trunking::NxdnConfig::channels`]) is watched.
    Nxdn(nxdn::trunking::NxdnConfig),
}

/// What a system's protocol has to show beyond what every system shows.
#[derive(Clone, Debug, Default)]
pub enum ProtocolStatus {
    #[default]
    P25,
    SmartNet,
    /// The site's kind, colour code, rest channel, channel table and carriers.
    Dmr(dmr::SiteStatus),
    /// Type-C / Type-D, the site's location and RAN, channel table and carriers.
    Nxdn(nxdn::trunking::SiteStatus),
}

/// Which carriers a system listens to.
#[derive(Clone, Debug, PartialEq)]
pub enum CarrierPlan {
    /// One control channel at a time, hunted through the system's list
    /// when it stops decoding (P25, SmartNet).
    Hunt { cutoff_hz: f64 },
    /// Every one of these carriers at once (a DMR site: Capacity Plus's rest
    /// channel moves), and what to say when it starts.
    Watch { carriers: Vec<f64>, cutoff_hz: f64, note: String },
}

/// What a control channel decoded, in the order it decoded it: a step the
/// system checks against its site lock before following.
#[derive(Clone, Debug)]
pub struct Step {
    /// Something decoded cleanly (it shows the control channel is alive).
    pub good: bool,
    pub msgs: Vec<Message>,
    /// What the site has said about itself, as of these messages.
    pub identity: Identity,
}

/// One trunking protocol's control channel(s): IQ in, [`Step`]s out.
pub trait ControlChannel: Send {
    /// "P25", "SmartNet", "DMR".
    fn name(&self) -> &'static str;
    fn plan(&self) -> CarrierPlan;
    /// Receive afresh at `rate` (a control channel was opened or reopened).
    fn restart(&mut self, _rate: f64) {}
    /// A different control channel: what was learned of the site may not hold.
    fn forget_site(&mut self) {}
    /// IQ of carrier `carrier` (0 for a hunted control channel), whose first
    /// sample was at air time `t0`, at `rate`.
    fn push(&mut self, carrier: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Step>);
    /// End of input: what the receivers still hold.
    fn flush(&mut self, _out: &mut Vec<Step>) {}
    /// Blocks decoded cleanly and lost, so far.
    fn counts(&self) -> (u64, u64);
    /// How far off the control channel comes in, Hz, when known (AutoTune).
    fn offset_hz(&self) -> Option<f32> {
        None
    }
    /// AutoTune may reopen the control channel at the corrected frequency.
    fn reopens(&self) -> bool {
        false
    }
    /// What it can say about its site: the fields the site lock
    /// (`SystemConfig::expect`) compares. None: no site lock.
    fn identity_fields(&self) -> &'static [IdField] {
        &[]
    }
    /// What voice channels need from it, given what the site has said
    /// (P25 Phase 2: the scrambler key, once known).
    fn voice_params(&self, _identity: &Identity) -> VoiceParams {
        VoiceParams::default()
    }
    /// The multi-site group the air puts this site in, if it says.
    fn site_group(&self, identity: &Identity) -> Option<String>;
    /// How it is modulated, for the dashboard ("" while unknown).
    fn modulation(&self) -> &'static str;
    /// Watching several carriers: the one carrying the control channel now.
    fn control_hz(&self) -> Option<u64> {
        None
    }
    /// Log lines it has for the system.
    fn take_notes(&mut self) -> Vec<(NoteLevel, String)> {
        Vec::new()
    }
    /// How long a patch stands without being heard again, s.
    fn patch_hold_s(&self) -> f64 {
        patches::P25_HOLD_S
    }
    /// What it learned that's worth keeping between runs (a band plan,
    /// a channel table), as text; and loading it back.
    fn bandplan(&self) -> String {
        String::new()
    }
    fn load_bandplan(&mut self, _s: &str) {}
    /// What the dashboard shows of it beyond what every system shows.
    fn status(&self) -> ProtocolStatus;
    /// Its receivers' measurements for the dashboard's history (framing,
    /// eye opening, deviation…; see [`crate::metrics`]). Default: none.
    fn report(&self, _sink: &mut dyn Sink) {}
}

/// The control channel for system `cfg`; `rate` is a channel's sample rate.
pub fn build(cfg: &SystemConfig, rate: f64) -> Box<dyn ControlChannel> {
    match &cfg.protocol {
        Protocol::P25 => Box::new(P25 {
            bank_cfg: cfg.bank,
            bank: Bank::new(rate, cfg.bank),
            parser: TsbkParser::default(),
            identity: Identity::default(),
            nac_votes: BTreeMap::new(),
            good: 0,
            bad: 0,
            t0: 0.0,
            rate,
        }),
        Protocol::SmartNet(sn) => Box::new(Smartnet { cfg: sn.clone(), cc: None, good: 0, bad: 0, identity: Identity::default() }),
        Protocol::Dmr(dc) => {
            let note = format!("Watching {} DMR frequencies", cfg.control_channels.len() + dc.channels.len());
            Box::new(Dmr { site: dmr::Site::new(&cfg.control_channels, rate, dc.clone()), note })
        }
        Protocol::Nxdn(nc) => {
            let what = if nc.kind == nxdn::trunking::Kind::TypeD { "repeaters" } else { "frequencies" };
            let note = format!("Watching {} NXDN {what} ({})", cfg.control_channels.len() + nc.channels.len(), nc.kind.name());
            Box::new(Nxdn { site: nxdn::trunking::Site::new(&cfg.control_channels, rate, nc.clone()), note })
        }
    }
}

/// P25: TSBKs from a receiver bank (best of several receivers each frame).
struct P25 {
    bank_cfg: BankConfig,
    bank: Bank,
    parser: TsbkParser,
    identity: Identity,
    /// (Ordered: a tie goes to the higher NAC, every run.)
    nac_votes: BTreeMap<u16, u32>,
    good: u64,
    bad: u64,
    /// The last push's start time and rate (for the flush).
    t0: f64,
    rate: f64,
}

impl P25 {
    fn groups(&mut self, groups: &[Group], out: &mut Vec<Step>) {
        for g in groups {
            let f = best_frame(g);
            *self.nac_votes.entry(f.nid.nac).or_default() += 1;
            self.identity.set_opt(IdField::Nac, self.nac_votes.iter().max_by_key(|&(_, &c)| c).map(|(&n, _)| n as u32));
            if f.nid.duid != TSDU {
                continue;
            }
            let (blocks, missing) = best_tsbks(g);
            self.bad += missing as u64;
            let t = self.t0 + f.sample / self.rate;
            for blk in &blocks {
                self.good += 1;
                let msgs = self.parser.parse(blk, f.nid.nac, t);
                for m in &msgs {
                    match m.kind {
                        MessageType::Status => {
                            self.identity.set(IdField::Wacn, m.wacn);
                            self.identity.set(IdField::SysId, m.sys_id);
                        }
                        MessageType::SysId => {
                            self.identity.set(IdField::SysId, m.sys_id);
                            self.identity.set(IdField::Rfss, m.rfss);
                            self.identity.set(IdField::Site, m.site);
                        }
                        _ => {}
                    }
                }
                out.push(Step { good: true, msgs, identity: self.identity.clone() });
            }
        }
    }
}

impl ControlChannel for P25 {
    fn name(&self) -> &'static str {
        "P25"
    }
    fn report(&self, sink: &mut dyn Sink) {
        self.bank.report(sink);
    }
    fn plan(&self) -> CarrierPlan {
        CarrierPlan::Hunt { cutoff_hz: P25_CUTOFF_HZ }
    }
    fn restart(&mut self, rate: f64) {
        self.bank = Bank::new(rate, self.bank_cfg);
    }
    fn forget_site(&mut self) {
        self.identity = Identity::default();
        self.nac_votes.clear();
    }
    fn push(&mut self, _carrier: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Step>) {
        (self.t0, self.rate) = (t0, rate);
        let mut groups = Vec::new();
        self.bank.push(iq, &mut groups);
        self.groups(&groups, out);
    }
    fn flush(&mut self, out: &mut Vec<Step>) {
        let mut groups = Vec::new();
        self.bank.flush(&mut groups);
        self.groups(&groups, out);
    }
    fn counts(&self) -> (u64, u64) {
        (self.good, self.bad)
    }
    fn offset_hz(&self) -> Option<f32> {
        self.bank.offset_hz()
    }
    fn reopens(&self) -> bool {
        true
    }
    fn identity_fields(&self) -> &'static [IdField] {
        &IdField::ALL
    }
    fn voice_params(&self, id: &Identity) -> VoiceParams {
        let tdma_key = match (id.nac(), id.sys_id(), id.wacn()) {
            (Some(nac), Some(sys), Some(wacn)) => Some((nac as u32, sys, wacn)),
            _ => None,
        };
        VoiceParams { tdma_key }
    }
    fn site_group(&self, id: &Identity) -> Option<String> {
        Some(format!("p25:{:x}.{:x}", id.wacn()?, id.sys_id()?))
    }
    fn modulation(&self) -> &'static str {
        // Frames per receiver: the two CQPSK ones, then C4FM.
        let (q, c) = self.bank.frames_per_rx().iter().enumerate().fold((0, 0), |(q, c), (i, &n)| if i < 2 { (q + n, c) } else { (q, c + n) });
        if q + c < 8 {
            ""
        } else if q >= c {
            "CQPSK"
        } else {
            "C4FM"
        }
    }
    fn bandplan(&self) -> String {
        self.parser.bandplan_to_string()
    }
    fn load_bandplan(&mut self, s: &str) {
        self.parser.bandplan_from_str(s);
    }
    fn status(&self) -> ProtocolStatus {
        ProtocolStatus::P25
    }
}

/// SmartNet: OSWs from a 3600 baud 2FSK receiver. The identity is the
/// System ID (and site, when OBT sends it).
struct Smartnet {
    cfg: super::engine::SmartnetConfig,
    /// Made on each (re)start: a new control channel learns afresh.
    cc: Option<smartnet::ControlChannel>,
    good: u64,
    bad: u64,
    identity: Identity,
}

impl ControlChannel for Smartnet {
    fn name(&self) -> &'static str {
        "SmartNet"
    }
    fn report(&self, sink: &mut dyn Sink) {
        if let Some(cc) = &self.cc {
            cc.report(sink);
        }
    }
    fn plan(&self) -> CarrierPlan {
        CarrierPlan::Hunt { cutoff_hz: smartnet::CHANNEL_CUTOFF_HZ }
    }
    fn restart(&mut self, rate: f64) {
        let mut p = smartnet::Parser::new(self.cfg.bandplan.clone());
        p.analog_default = self.cfg.analog_default;
        self.cc = Some(smartnet::ControlChannel::new(rate, p));
    }
    fn push(&mut self, _carrier: usize, iq: &[Complex32], t0: f64, _rate: f64, out: &mut Vec<Step>) {
        let Some(cc) = self.cc.as_mut() else { return };
        let (good0, bad0) = cc.counts();
        let mut msgs = Vec::new();
        cc.push(iq, t0, &mut msgs);
        let (good, bad) = cc.counts();
        self.good += good - good0;
        self.bad += bad - bad0;
        self.identity.set_opt(IdField::SysId, cc.parser.sys_id);
        self.identity.set_opt(IdField::Site, cc.parser.site);
        out.push(Step { good: good > good0, msgs, identity: self.identity.clone() });
    }
    fn forget_site(&mut self) {
        self.identity = Identity::default();
    }
    fn counts(&self) -> (u64, u64) {
        (self.good, self.bad)
    }
    fn offset_hz(&self) -> Option<f32> {
        self.cc.as_ref().map(|c| c.offset_hz())
    }
    fn identity_fields(&self) -> &'static [IdField] {
        &[IdField::SysId, IdField::Site]
    }
    fn site_group(&self, id: &Identity) -> Option<String> {
        Some(format!("smartnet:{:x}", id.sys_id()?))
    }
    fn modulation(&self) -> &'static str {
        "2FSK"
    }
    fn patch_hold_s(&self) -> f64 {
        patches::SMARTNET_HOLD_S
    }
    fn status(&self) -> ProtocolStatus {
        ProtocolStatus::SmartNet
    }
}

/// Trunked DMR: every carrier of the site at once (link control and CSBKs).
/// Sites are told apart by colour code, not by the site lock.
struct Dmr {
    site: dmr::Site,
    note: String,
}

impl ControlChannel for Dmr {
    fn name(&self) -> &'static str {
        "DMR"
    }
    fn plan(&self) -> CarrierPlan {
        CarrierPlan::Watch { carriers: self.site.carriers.iter().map(|c| c.hz as f64).collect(), cutoff_hz: dmr::CHANNEL_CUTOFF_HZ, note: self.note.clone() }
    }
    fn push(&mut self, carrier: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Step>) {
        let mut msgs = Vec::new();
        self.site.push(carrier, iq, t0, rate, &mut msgs);
        out.push(Step { good: !msgs.is_empty(), msgs, identity: Identity::default() });
    }
    fn counts(&self) -> (u64, u64) {
        // Blocks (CSBKs, link control) decoded and lost, on every carrier.
        let slots = || self.site.carriers.iter().flat_map(|c| c.chan.slots.iter());
        (slots().map(|s| s.good_blocks).sum(), slots().map(|s| s.bad_blocks).sum())
    }
    fn site_group(&self, _: &Identity) -> Option<String> {
        // A DMR site doesn't announce a system: only a configured group joins sites.
        None
    }
    fn modulation(&self) -> &'static str {
        self.site.variant.map_or("DMR", |v| v.name())
    }
    fn control_hz(&self) -> Option<u64> {
        self.site.control_hz()
    }
    fn take_notes(&mut self) -> Vec<(NoteLevel, String)> {
        // (All news: the colour code, the variant, channels learned.)
        self.site.take_notes().into_iter().map(|t| (NoteLevel::Info, t)).collect()
    }
    fn bandplan(&self) -> String {
        self.site.map_to_string()
    }
    fn load_bandplan(&mut self, s: &str) {
        self.site.map_from_str(s);
    }
    fn status(&self) -> ProtocolStatus {
        ProtocolStatus::Dmr(self.site.status())
    }
}

/// Trunked NXDN: every carrier of the site at once (CAC on the control
/// channel; VCALL / SCCH on the traffic channels). Type-C sites state their
/// system and site code (SITE_INFO), which the site lock checks.
struct Nxdn {
    site: nxdn::trunking::Site,
    note: String,
}

impl ControlChannel for Nxdn {
    fn name(&self) -> &'static str {
        "NXDN"
    }
    fn plan(&self) -> CarrierPlan {
        let cutoff_hz = self.site.status().rate.unwrap_or(nxdn::Rate::N96).cutoff_hz();
        CarrierPlan::Watch { carriers: self.site.carriers.iter().map(|c| c.hz as f64).collect(), cutoff_hz, note: self.note.clone() }
    }
    fn push(&mut self, carrier: usize, iq: &[Complex32], t0: f64, rate: f64, out: &mut Vec<Step>) {
        let (good0, _) = self.site.counts();
        let mut msgs = Vec::new();
        self.site.push(carrier, iq, t0, rate, &mut msgs);
        let good = self.site.counts().0 > good0;
        out.push(Step { good: good || !msgs.is_empty(), msgs, identity: self.site.identity().clone() });
    }
    fn counts(&self) -> (u64, u64) {
        self.site.counts()
    }
    fn identity_fields(&self) -> &'static [IdField] {
        match self.site.kind() {
            nxdn::trunking::Kind::TypeC => &[IdField::SysId, IdField::Site],
            nxdn::trunking::Kind::TypeD => &[],
        }
    }
    fn site_group(&self, id: &Identity) -> Option<String> {
        Some(format!("nxdn:{:x}", id.sys_id()?))
    }
    fn modulation(&self) -> &'static str {
        match self.site.status().rate {
            Some(nxdn::Rate::N96) => "NXDN96",
            _ => "NXDN48",
        }
    }
    fn control_hz(&self) -> Option<u64> {
        self.site.control_hz()
    }
    fn take_notes(&mut self) -> Vec<(NoteLevel, String)> {
        self.site.take_notes()
    }
    fn bandplan(&self) -> String {
        self.site.map_to_string()
    }
    fn load_bandplan(&mut self, s: &str) {
        self.site.map_from_str(s);
    }
    fn status(&self) -> ProtocolStatus {
        ProtocolStatus::Nxdn(self.site.status())
    }
}
