//! EDACS Standard and Networked control channel messages (TIA/EIA TSB-69.3,
//! "EDACS Digital Air Interface for Channel Access, Modulation, Messages and
//! Formats", with the field layouts DSD-FME found on the air) → trunking
//! [`Message`]s.
//!
//! Each frame's first message is the command; the second carries what didn't
//! fit (the low bits of a caller's ID, the caller of an individual call).
//! Groups are 11-bit AFS numbers (agency 4 bits, fleet 4, subfleet 3: "03-091"
//! is 457); radios are 14-bit logical IDs (LIDs). Channels are logical
//! channel numbers (LCNs, 1–25) the site's channel list turns into frequencies.

use std::collections::{BTreeMap, BTreeSet};

use crate::trunk::{Message, MessageType, Voice};

/// What an EDACS site needs to be followed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdacsConfig {
    /// Logical channel number → frequency, Hz.
    pub lcns: BTreeMap<u8, u64>,
    /// The system scrambles its messages with ESK 0xA0 (some Harris systems).
    pub esk: bool,
}

impl EdacsConfig {
    pub fn freq(&self, lcn: u8) -> Option<u64> {
        self.lcns.get(&lcn).copied().filter(|&f| f > 0)
    }
}

/// An AFS number as "AA-FFS" (4:4:3 bits).
pub fn afs(group: u32) -> String {
    format!("{:02}-{:02}{}", group >> 7 & 0xf, group >> 3 & 0xf, group & 7)
}

/// LCNs 26–31 aren't channels: they say a call is queued, busy, denied…
const LAST_LCN: u8 = 25;

/// Site IDs in a row that must agree (site and control channel LCN) before
/// they're believed: a frame that slips past the error checks now and then
/// mustn't renumber the channel list or rename the site.
const SITE_AGREE: u32 = 3;

/// A site is taken to be in failsoft only when its site IDs say so this many
/// times in a row and it has assigned no call for [`FAILSOFT_QUIET_S`]: the
/// flag alone shows on some sites that trunk all the same.
const FAILSOFT_IDS: u32 = 5;
const FAILSOFT_QUIET_S: f64 = 30.0;

pub struct Parser {
    pub cfg: EdacsConfig,
    /// The site's ID (5 bits), and its control channel's LCN, as it says.
    pub site: Option<u32>,
    pub cc_lcn: Option<u8>,
    /// The frequency of the control channel being heard, Hz (the engine says).
    pub cc_hz: Option<u64>,
    /// The last site ID heard (site, control channel LCN), how many in a row,
    /// and the last one believed.
    site_heard: Option<(u32, u8)>,
    site_run: u32,
    site_believed: Option<(u32, u8)>,
    failsoft: bool,
    /// Site IDs in a row with the failsoft flag; when a call was last assigned
    /// (or the control channel first heard).
    failsoft_run: u32,
    last_assigned: Option<f64>,
    /// The channel list was corrected from the air (said once).
    corrected: bool,
    /// Grants to the control channel's own frequency (said once).
    to_cc_said: bool,
    /// LCNs granted that aren't in the channel list (said once each).
    missing: BTreeSet<u8>,
    notes: Vec<(bool, String)>,
    /// Calls assigned a channel: (LCN, when), for [`super::site::Site`] to learn channels from.
    pub granted: Vec<(u8, f64)>,
}

impl Parser {
    pub fn new(cfg: EdacsConfig) -> Self {
        Parser { cfg, site: None, cc_lcn: None, cc_hz: None, site_heard: None, site_run: 0, site_believed: None, failsoft: false, failsoft_run: 0, last_assigned: None, corrected: false, to_cc_said: false, missing: BTreeSet::new(), notes: Vec::new(), granted: Vec::new() }
    }

    /// News for the log: (a warning?, text).
    pub fn take_notes(&mut self) -> Vec<(bool, String)> {
        std::mem::take(&mut self.notes)
    }

    fn msg(&self, kind: MessageType, t: f64) -> Message {
        Message { kind, time_s: t, site: self.site.unwrap_or(0), source: -1, priority: 3, ..Default::default() }
    }

    /// The frequency of channel `lcn`, or None (and a note, once) when the
    /// site's list doesn't have it.
    fn channel(&mut self, lcn: u8, what: &str) -> Option<u64> {
        if lcn == 0 || lcn > LAST_LCN {
            return None;
        }
        let f = self.cfg.freq(lcn);
        if f.is_none() && self.missing.insert(lcn) {
            self.notes.push((true, format!("{what} on LCN {lcn}, which isn't in this site's channel list: not followed until its frequency is known (learned from the air when it's one of the site's listed frequencies, or added to the list)")));
        }
        f
    }

    /// A voice call on `lcn`: `kind` Grant or Update (group), UuVGrant /
    /// UuVUpdate (individual), PhoneGrant.
    #[allow(clippy::too_many_arguments)]
    fn voice(&mut self, kind: MessageType, t: f64, lcn: u8, target: u32, source: Option<u32>, digital: bool, emergency: bool, out: &mut Vec<Message>) {
        let group = matches!(kind, MessageType::Grant | MessageType::Update);
        if matches!(kind, MessageType::Grant | MessageType::UuVGrant | MessageType::PhoneGrant) && (1..=LAST_LCN).contains(&lcn) {
            self.granted.push((lcn, t));
        }
        // Assigning calls: trunking, whatever its site ID's flag says.
        self.last_assigned = Some(t);
        if self.failsoft {
            self.failsoft = false;
            self.notes.push((false, format!("Site {} is trunking again", self.site.unwrap_or(0))));
        }
        let what = if group { format!("Talkgroup {target} ({})", afs(target)) } else { format!("Radio {target}") };
        let Some(f) = self.channel(lcn, &what) else { return };
        // Never the frequency the control channel is on: recording it records its data.
        // (A list that puts another LCN there has its numbers off; a grant of the control
        // channel's own LCN is the site's business — it doesn't carry voice while it's control.)
        if self.cc_hz.is_some_and(|cc| cc.abs_diff(f) < 1000) {
            if !self.to_cc_said {
                self.to_cc_said = true;
                let why = if self.cc_lcn == Some(lcn) {
                    "the control channel's own LCN".to_string()
                } else {
                    "which the channel list puts on this control channel's frequency: the list's LCN numbers don't match the site's".to_string()
                };
                self.notes.push((true, format!("{what} was granted LCN {lcn} ({:.4} MHz), {why}. Not followed (it carries control data)", f as f64 / 1e6)));
            }
            return;
        }
        let mut m = self.msg(kind, t);
        m.freq_hz = f;
        m.talkgroup = target;
        m.source = source.map_or(-1, |s| s as i64);
        m.emergency = emergency;
        m.voice = if digital { Voice::ProVoice } else { Voice::Fm };
        m.meta = format!(
            "{} {} {} LCN {lcn} {:.4} MHz{}{}{}",
            if group { "tg" } else { "radio" },
            if group { format!("{target} ({})", afs(target)) } else { target.to_string() },
            kind.as_str(),
            f as f64 / 1e6,
            source.map_or(String::new(), |s| format!(" src {s}")),
            if digital { " ProVoice" } else { " analog" },
            if emergency { " emerg" } else { "" },
        );
        out.push(m);
    }

    /// The site says its control channel is LCN `cc_lcn`, and this is its
    /// frequency: a channel list that has that LCN elsewhere (or this
    /// frequency as another LCN) is wrong there. It's corrected for this run,
    /// and the log says what the list should have.
    fn learn_cc(&mut self, site: u32, cc_lcn: u8) {
        let Some(cc) = self.cc_hz else { return };
        if cc_lcn == 0 || cc_lcn > LAST_LCN || self.cfg.freq(cc_lcn).is_some_and(|f| f.abs_diff(cc) < 1000) {
            return;
        }
        let was = self.cfg.freq(cc_lcn);
        let listed_as: Vec<u8> = self.cfg.lcns.iter().filter(|&(_, &f)| f.abs_diff(cc) < 1000).map(|(&l, _)| l).collect();
        for l in &listed_as {
            self.cfg.lcns.remove(l);
        }
        self.cfg.lcns.insert(cc_lcn, cc);
        if !self.corrected {
            self.corrected = true;
            let mhz = |f: u64| format!("{:.4} MHz", f as f64 / 1e6);
            let listed = if listed_as.is_empty() { String::new() } else { format!(" (the list has it as LCN {})", listed_as.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(", ")) };
            let had = was.map_or("no frequency".to_string(), mhz);
            self.notes.push((
                true,
                format!(
                    "Site {site} says its control channel, {}{listed}, is LCN {cc_lcn}; the channel list has {had} for LCN {cc_lcn}. The list's LCN numbers are off: following LCN {cc_lcn} = {} for now. Check the site's channel list",
                    mhz(cc),
                    mhz(cc)
                ),
            ));
        }
    }

    /// One frame's messages (28 bits each), heard at `t`.
    pub fn frame(&mut self, m1: Option<u32>, m2: Option<u32>, t: f64, out: &mut Vec<Message>) {
        let esk = if self.cfg.esk { 0xA0u32 << 20 } else { 0 };
        let Some(m1) = m1.map(|m| m ^ esk) else { return };
        let m2 = m2.map(|m| m ^ esk);
        let mt_a = m1 >> 25 & 7;
        let mt_b = m1 >> 22 & 7;
        let mt_d = m1 >> 17 & 0x1f;
        match mt_a {
            // Group voice channel assignment (6.2.4.1, emergency 6.2.4.2); 2 and 3 are digital.
            0..=3 => {
                let lcn = (m1 >> 12 & 0x1f) as u8;
                let group = m1 & 0x7ff;
                let lid = m2.map(|m2| (m1 & 0x1FC_0000) >> 11 | (m2 & 0xFE_0000) >> 17);
                self.voice(MessageType::Grant, t, lcn, group, lid.filter(|&l| l > 0), mt_a & 2 != 0, mt_a & 1 != 0, out);
            }
            // Login acknowledge (6.2.4.4): a radio joined a group.
            6 => {
                let mut m = self.msg(MessageType::Affiliation, t);
                m.talkgroup = (m1 & 0x1FF_C000) >> 14;
                m.source = (m1 & 0x3fff) as i64;
                m.meta = format!("radio {} logged in on tg {} ({})", m.source, m.talkgroup, afs(m.talkgroup));
                out.push(m);
            }
            7 => match mt_b {
                // Interconnect (telephone) channel assignment (6.2.4.6).
                1 => {
                    let mt_c = m1 >> 20 & 3;
                    let lcn = (m1 >> 15 & 0x1f) as u8;
                    let target = if m1 >> 14 & 1 == 1 { m1 & 0x3fff } else { m1 & 0x7ff };
                    self.voice(MessageType::PhoneGrant, t, lcn, target, Some(target), mt_c & 2 != 0, false, out);
                }
                // Channel update (6.2.4.7): a call still going.
                3 => {
                    let mt_c = m1 >> 20 & 3;
                    let lcn = (m1 >> 15 & 0x1f) as u8;
                    let individual = m1 >> 14 & 1 == 1;
                    let digital = mt_c & 1 != 0;
                    if individual {
                        let (target, source) = (m1 & 0x3fff, m2.map(|m| m & 0x3fff));
                        if target != 0 || source.is_some_and(|s| s != 0) {
                            self.voice(MessageType::UuVUpdate, t, lcn, target, source.filter(|&s| s > 0), digital, false, out);
                        }
                    } else {
                        self.voice(MessageType::Update, t, lcn, m1 & 0x7ff, None, digital, m1 >> 13 & 1 == 1, out);
                    }
                }
                // Individual call channel assignment (6.2.4.9); target and source 0 is a test call.
                5 => {
                    let lcn = (m1 >> 15 & 0x1f) as u8;
                    let digital = m1 >> 14 & 1 == 1;
                    let (target, source) = (m1 & 0x3fff, m2.map(|m| m & 0x3fff));
                    if target != 0 || source.is_some_and(|s| s != 0) {
                        self.voice(MessageType::UuVGrant, t, lcn, target, source.filter(|&s| s > 0), digital, false, out);
                    }
                }
                7 => match mt_d {
                    // Site ID (6.2.4.18).
                    0x08..=0x0B => {
                        let cc_lcn = (m1 >> 12 & 0x1f) as u8;
                        let failsoft = m1 >> 6 & 1 == 1;
                        let aux = m1 >> 5 & 1 == 1;
                        let site = m1 & 0x1f;
                        if !aux {
                            if self.site_heard == Some((site, cc_lcn)) {
                                self.site_run += 1;
                            } else {
                                (self.site_heard, self.site_run) = (Some((site, cc_lcn)), 1);
                            }
                            // Believed once it's said the same enough times in a row.
                            if self.site_run >= SITE_AGREE && self.site_believed != Some((site, cc_lcn)) {
                                self.site_believed = Some((site, cc_lcn));
                                self.notes.push((false, format!("EDACS site {site}, control channel on LCN {cc_lcn}")));
                                self.site = Some(site);
                                self.cc_lcn = Some(cc_lcn);
                                self.learn_cc(site, cc_lcn);
                            }
                        }
                        if self.site.is_none() {
                            self.site = Some(site);
                        }
                        if self.site == Some(site) {
                            self.failsoft_run = if failsoft { self.failsoft_run + 1 } else { 0 };
                            let since = *self.last_assigned.get_or_insert(t);
                            let quiet = t - since >= FAILSOFT_QUIET_S;
                            if !self.failsoft && self.failsoft_run >= FAILSOFT_IDS && quiet {
                                self.failsoft = true;
                                self.notes.push((true, format!("Site {site} is in failsoft: its site ID says so, and it has assigned no call for {FAILSOFT_QUIET_S:.0} s")));
                            } else if self.failsoft && self.failsoft_run == 0 {
                                self.failsoft = false;
                                self.notes.push((false, format!("Site {site} is trunking again")));
                            }
                        }
                        let mut m = self.msg(MessageType::SysId, t);
                        m.site = site;
                        m.freq_hz = self.cfg.freq(cc_lcn).unwrap_or(0);
                        m.meta = format!("site {site} control channel LCN {cc_lcn}{}", if failsoft { " failsoft" } else { "" });
                        out.push(m);
                    }
                    // System all-call (6.2.4.19): to every radio (talkgroup 0).
                    0x0F => {
                        let lcn = (m1 >> 12 & 0x1f) as u8;
                        let digital = m1 >> 11 & 1 == 1;
                        let update = m1 >> 10 & 1 == 1;
                        let lid = m2.map(|m2| m1 & 0x7f | (m2 & 0xfe) << 6);
                        let kind = if update { MessageType::Update } else { MessageType::Grant };
                        self.voice(kind, t, lcn, 0, lid.filter(|&l| l > 0), digital, false, out);
                    }
                    _ => {}
                },
                _ => {}
            },
            // Data calls (5) and the rest: nothing to record.
            _ => {}
        }
    }
}

/// The messages that grant (or update) a call, for test signals and the
/// bench: (message 1, message 2).
pub fn group_grant(group: u32, lid: u32, lcn: u8, digital: bool, emergency: bool) -> (u32, u32) {
    let mt_a = (digital as u32) << 1 | emergency as u32;
    let m1 = mt_a << 25 | (lid >> 7 & 0x7f) << 18 | (lcn as u32 & 0x1f) << 12 | (group & 0x7ff);
    let m2 = (lid & 0x7f) << 17;
    (m1, m2)
}

pub fn group_update(group: u32, lcn: u8, digital: bool) -> (u32, u32) {
    let mt_c = digital as u32;
    (7 << 25 | 3 << 22 | mt_c << 20 | (lcn as u32 & 0x1f) << 15 | (group & 0x7ff), 0)
}

pub fn site_id(site: u32, cc_lcn: u8) -> (u32, u32) {
    (7 << 25 | 7 << 22 | 0x08 << 17 | (cc_lcn as u32 & 0x1f) << 12 | 1 << 9 | (site & 0x1f), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EdacsConfig {
        EdacsConfig { lcns: [(1, 856_187_500), (4, 854_137_500), (5, 856_687_500)].into_iter().collect(), esk: false }
    }

    fn parse(p: &mut Parser, (m1, m2): (u32, u32)) -> Vec<Message> {
        let mut out = Vec::new();
        p.frame(Some(m1), Some(m2), 1.0, &mut out);
        out
    }

    #[test]
    fn grants_updates_and_the_site() {
        let mut p = Parser::new(cfg());
        let s = parse(&mut p, site_id(2, 1));
        assert_eq!((s[0].kind, s[0].site, s[0].freq_hz), (MessageType::SysId, 2, 856_187_500));
        assert_eq!(p.site, Some(2));
        // DC 911 Police 1: AFS 03-091 = 457, analog, from LID 12345, on LCN 5.
        let g = parse(&mut p, group_grant(457, 12345, 5, false, false));
        assert_eq!((g[0].kind, g[0].talkgroup, g[0].source, g[0].freq_hz, g[0].voice), (MessageType::Grant, 457, 12345, 856_687_500, Voice::Fm));
        assert!(g[0].meta.contains("03-091"), "{}", g[0].meta);
        let d = parse(&mut p, group_grant(205, 7, 4, true, true));
        assert_eq!((d[0].voice, d[0].emergency, d[0].freq_hz), (Voice::ProVoice, true, 854_137_500));
        let u = parse(&mut p, group_update(205, 4, true));
        assert_eq!((u[0].kind, u[0].talkgroup, u[0].voice), (MessageType::Update, 205, Voice::ProVoice));
    }

    #[test]
    fn a_channel_not_in_the_list_is_said_once() {
        let mut p = Parser::new(cfg());
        assert!(parse(&mut p, group_grant(457, 1, 9, false, false)).is_empty());
        assert!(parse(&mut p, group_update(457, 9, false)).is_empty());
        let notes = p.take_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].0 && notes[0].1.contains("LCN 9"));
    }

    #[test]
    fn a_list_with_its_numbers_off_is_corrected_by_the_control_channel() {
        let mut p = Parser::new(cfg());
        // Listening on 854.1375 (listed as LCN 4), which says it is LCN 3.
        p.cc_hz = Some(854_137_500);
        // Once isn't believed (a frame that slipped past the checks)…
        parse(&mut p, site_id(2, 3));
        assert!(p.take_notes().iter().all(|n| !n.0) && p.cfg.freq(4) == Some(854_137_500));
        // …nor a stray between agreeing ones; three in a row are.
        parse(&mut p, site_id(2, 8));
        parse(&mut p, site_id(2, 3));
        parse(&mut p, site_id(2, 3));
        assert_eq!(p.cfg.freq(3), None);
        parse(&mut p, site_id(2, 3));
        let notes: Vec<_> = p.take_notes().into_iter().filter(|n| n.0).collect();
        assert!(notes.len() == 1 && notes[0].1.contains("is LCN 3") && notes[0].1.contains("as LCN 4"), "{notes:?}");
        assert_eq!(p.cfg.freq(3), Some(854_137_500));
        assert_eq!(p.cfg.freq(4), None);
        // A grant to the control channel's own frequency isn't followed.
        let mut q = Parser::new(cfg());
        q.cc_hz = Some(856_187_500);
        q.cc_lcn = Some(9);
        assert!(parse(&mut q, group_grant(457, 1, 1, false, false)).is_empty());
        assert!(q.take_notes()[0].1.contains("control channel's frequency"));
        // Nor one to the control channel's own LCN.
        let mut r = Parser::new(cfg());
        r.cc_hz = Some(856_187_500);
        r.cc_lcn = Some(1);
        assert!(parse(&mut r, group_grant(457, 1, 1, false, false)).is_empty());
        assert!(r.take_notes()[0].1.contains("control channel's own LCN"));
    }

    /// The failsoft flag alone isn't failsoft: only with no calls assigned for a while.
    #[test]
    fn failsoft_needs_the_flag_and_no_calls() {
        let fs = |site: u32, lcn: u8| {
            let (m1, m2) = site_id(site, lcn);
            (m1 | 1 << 6, m2)
        };
        let warned = |p: &mut Parser| p.take_notes().iter().any(|n| n.0 && n.1.contains("failsoft"));
        // A site that flags it but assigns calls: never said.
        let mut p = Parser::new(cfg());
        for k in 0..100 {
            let t = k as f64;
            let mut out = Vec::new();
            p.frame(Some(fs(5, 1).0), Some(0), t, &mut out);
            if k % 10 == 0 {
                p.frame(Some(group_grant(799, 1, 4, false, false).0), group_grant(799, 1, 4, false, false).1.into(), t, &mut out);
            }
        }
        assert!(!warned(&mut p));
        // One that flags it and assigns nothing for 30 s: said once.
        let mut q = Parser::new(cfg());
        for k in 0..40 {
            let mut out = Vec::new();
            q.frame(Some(fs(5, 1).0), Some(0), k as f64, &mut out);
        }
        assert!(warned(&mut q));
    }

    #[test]
    fn esk_systems_are_unscrambled() {
        let mut p = Parser::new(EdacsConfig { esk: true, ..cfg() });
        let (m1, m2) = group_grant(820, 33, 1, false, false);
        let g = parse(&mut p, (m1 ^ 0xA0 << 20, m2 ^ 0xA0 << 20));
        assert_eq!((g[0].talkgroup, g[0].source), (820, 33));
    }

    #[test]
    fn afs_format() {
        assert_eq!(afs(457), "03-091");
        assert_eq!(afs(863), "06-117");
    }
}
