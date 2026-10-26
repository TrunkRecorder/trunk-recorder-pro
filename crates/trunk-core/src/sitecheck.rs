//! Which of a system's sites can be heard here: the survey probes each
//! site's frequencies ([`crate::survey::SurveyConfig::targets`]), and each
//! site is judged by what they showed, by the kind of system:
//!
//! ```text
//! P25        a control channel decodes (and, where the site says them, its NAC, RFSS and site match)
//! SmartNet   a control channel decodes
//! EDACS      a control channel decodes (and its site ID matches)
//! NXDN       a Type-C control channel decodes (and its system and site match)
//! DMR        DMR on any of its frequencies, of the system's kind of trunking
//!            and its colour code (a Capacity Plus site is quiet between calls:
//!            listen longer)
//! ```
//!
//! The sites are judged together: a frequency more than one of them lists
//! confirms none by itself (except where the air says which site it is), and
//! a DMR system's kind of trunking is the one most of its sites decode as, so
//! a neighbour of another kind on a shared frequency isn't taken for it.
//!
//! From Bill Johnson's (photounit) check-sites (PR #95).

use std::collections::BTreeMap;

use crate::survey::{Candidate, Kind};
use crate::trunk::identity::{IdField, Identity};

/// A carrier counts as there when it's up at least this share of the time.
const SOME_UP: f64 = 0.02;
/// A control channel's carrier is up about all the time (half of this will do, not decoding).
const CONTROL_UP: f64 = 0.8;
/// A decode on less than this (dB over the floor) is marginal however clean.
const GOOD_DB: f64 = 12.0;

/// One site to check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Site {
    pub label: String,
    /// The frequencies it is checked on: its control channels (a Capacity
    /// Plus site, which has none: its repeaters), Hz.
    pub freqs_hz: Vec<f64>,
    /// What it says about itself, where known (its site lock).
    pub expect: Identity,
    /// DMR: its colour code.
    pub color_code: Option<u8>,
}

/// What became of a site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Heard: its control channel decodes (DMR: its kind of DMR on a frequency of its own).
    Active,
    /// A control channel decodes on its frequency, but says it's another site.
    OtherSite,
    /// Something is there, not confirmed as this site: a carrier that didn't
    /// decode, another kind of DMR, or a decode only on a frequency other sites list too.
    Unconfirmed,
    /// Nothing heard.
    Inactive,
    /// None of its frequencies listened to (yet, or out of the radio's range).
    NotChecked,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Active => "active",
            Verdict::OtherSite => "otherSite",
            Verdict::Unconfirmed => "unconfirmed",
            Verdict::Inactive => "inactive",
            Verdict::NotChecked => "notChecked",
        }
    }
}

/// How well a frequency decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// Nothing decoded, or most of what framed failed.
    Poor,
    /// It decodes, but weakly: errors, a weak signal, or not as a control channel.
    Marginal,
    /// It decodes cleanly on a good signal.
    Good,
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Marginal => "marginal",
            Quality::Poor => "poor",
        }
    }
}

/// A site's verdict, why, and the frequency it rests on (with how well that decoded).
#[derive(Clone, Debug, PartialEq)]
pub struct Judged {
    pub verdict: Verdict,
    pub why: String,
    pub freq_hz: Option<f64>,
    pub quality: Option<Quality>,
}

/// What a candidate is, for a person: "P25 control channel, NAC 212 site 1-3".
pub fn describe(c: &Candidate) -> String {
    let mut s = match c.kind {
        Kind::Control => "P25 control channel".to_string(),
        Kind::P25 => "P25, no control messages".to_string(),
        Kind::SmartNet => "SmartNet control channel".to_string(),
        Kind::Edacs => "EDACS control channel".to_string(),
        Kind::DmrControl => c.dmr.and_then(|d| d.variant).map_or("DMR trunking".to_string(), |v| v.name().to_string()),
        Kind::Dmr => "DMR".to_string(),
        Kind::NxdnControl => "NXDN control channel".to_string(),
        Kind::Nxdn => "NXDN".to_string(),
        Kind::Other if c.up.unwrap_or(0.0) >= SOME_UP => "a carrier that didn't decode".to_string(),
        Kind::Other => "nothing".to_string(),
    };
    let id = identity(c);
    let id: Vec<String> = id.iter().map(|(f, v)| if matches!(f, IdField::Nac | IdField::SysId | IdField::Wacn) { format!("{} {v:X}", f.label()) } else { format!("{} {v}", f.label()) }).collect();
    if !id.is_empty() {
        s.push_str(&format!(", {}", id.join(" ")));
    }
    if let Some(cc) = c.dmr.and_then(|d| d.color_code) {
        s.push_str(&format!(", colour code {cc}"));
    }
    if let Some(r) = c.nxdn.and_then(|n| n.ran) {
        s.push_str(&format!(", RAN {r}"));
    }
    s
}

/// What the air said of the site (NXDN's system and site codes as SysID and site).
fn identity(c: &Candidate) -> Identity {
    let mut id = c.identity.clone();
    if let Some((sys, site)) = c.nxdn.and_then(|n| n.location) {
        id.set(IdField::SysId, sys);
        id.set(IdField::Site, site);
    }
    id
}

/// Whether `c` decodes as a system of type `ty` ("p25", "smartnet", "edacs", "dmr", "nxdn").
fn decodes_as(ty: &str, c: &Candidate) -> bool {
    match ty {
        "p25" => c.kind == Kind::Control,
        "smartnet" => c.kind == Kind::SmartNet,
        "edacs" => c.kind == Kind::Edacs,
        "dmr" => matches!(c.kind, Kind::Dmr | Kind::DmrControl),
        "nxdn" => c.kind == Kind::NxdnControl,
        _ => matches!(c.kind, Kind::Control | Kind::SmartNet | Kind::Edacs | Kind::DmrControl | Kind::NxdnControl),
    }
}

/// How well `c` decoded as a system of type `ty`.
pub fn quality(ty: &str, c: &Candidate) -> Quality {
    if c.kind == Kind::Other {
        return Quality::Poor;
    }
    let (g, b) = (c.good, c.bad);
    let share = if g + b > 0 && matches!(c.kind, Kind::Control | Kind::P25 | Kind::SmartNet | Kind::Edacs) { g as f64 / (g + b) as f64 } else { 1.0 };
    if share < 0.5 {
        Quality::Poor
    } else if !decodes_as(ty, c) || share < 0.9 || c.snr_db < GOOD_DB {
        Quality::Marginal
    } else {
        Quality::Good
    }
}

/// The identity fields a system of type `ty` says about its sites.
fn fields(ty: &str) -> &'static [IdField] {
    match ty {
        "p25" => &[IdField::Nac, IdField::Wacn, IdField::SysId, IdField::Rfss, IdField::Site],
        "smartnet" | "nxdn" => &[IdField::SysId, IdField::Site],
        "edacs" => &[IdField::Site],
        _ => &[],
    }
}

/// Every site judged by what the survey found (`heard`: its candidates;
/// `probed`: the frequencies listened to so far), for a system of type `ty`.
pub fn judge_all(ty: &str, sites: &[Site], heard: &[Candidate], probed: &[f64]) -> Vec<Judged> {
    let key = |f: f64| (f / 100.0).round() as u64;
    // What each target showed.
    let on: BTreeMap<u64, &Candidate> = heard.iter().filter_map(|c| Some((key(c.target?), c))).collect();
    let probed: Vec<u64> = probed.iter().map(|&f| key(f)).collect();
    let mut sharing: BTreeMap<u64, usize> = BTreeMap::new();
    for s in sites {
        let mut mine: Vec<u64> = s.freqs_hz.iter().map(|&f| key(f)).collect();
        mine.dedup();
        for f in mine {
            *sharing.entry(f).or_default() += 1;
        }
    }
    // The system's kind of DMR: the most common among the decodes on frequencies only one site has.
    let variant = (ty == "dmr")
        .then(|| {
            let mut votes: Vec<(crate::dmr::Variant, usize)> = Vec::new();
            for v in on.iter().filter(|(f, _)| sharing.get(f) == Some(&1)).filter_map(|(_, c)| c.dmr.and_then(|d| d.variant)) {
                match votes.iter_mut().find(|(x, _)| *x == v) {
                    Some(e) => e.1 += 1,
                    None => votes.push((v, 1)),
                }
            }
            votes.into_iter().max_by_key(|&(_, n)| n).map(|(v, _)| v)
        })
        .flatten();
    sites
        .iter()
        .map(|s| {
            let listened: Vec<f64> = s.freqs_hz.iter().copied().filter(|&f| probed.contains(&key(f))).collect();
            let found: Vec<(f64, &Candidate)> = listened.iter().filter_map(|&f| on.get(&key(f)).map(|c| (f, *c))).collect();
            judge(ty, s, &listened, &found, |f| sharing.get(&key(f)).copied().unwrap_or(0) > 1, variant)
        })
        .collect()
}

fn judge(ty: &str, site: &Site, listened: &[f64], found: &[(f64, &Candidate)], shared: impl Fn(f64) -> bool, variant: Option<crate::dmr::Variant>) -> Judged {
    let mhz = |f: f64| format!("{:.5} MHz", f / 1e6);
    let verdict = |verdict: Verdict, why: String, at: Option<(f64, &Candidate)>| Judged { verdict, why, freq_hz: at.map(|a| a.0), quality: at.map(|a| quality(ty, a.1)) };
    if listened.is_empty() {
        return verdict(Verdict::NotChecked, "none of its frequencies listened to".into(), None);
    }
    let decoded: Vec<(f64, &Candidate)> = found.iter().copied().filter(|(_, c)| decodes_as(ty, c)).collect();
    // The air says which site it is: its own, or another's.
    let conflict = |c: &Candidate| {
        let id = identity(c);
        let other = id.conflict(&site.expect, fields(ty));
        let cc = match (site.color_code, c.dmr.and_then(|d| d.color_code)) {
            (Some(w), Some(h)) if w != h => Some(format!("colour code {h} (expected {w})")),
            _ => None,
        };
        other.or(cc)
    };
    let says_site = |c: &Candidate| !fields(ty).is_empty() && !site.expect.is_empty() && identity(c).confirms(&site.expect, fields(ty)) && site.expect.iter().any(|(f, _)| fields(ty).contains(&f));
    let right_kind = |c: &Candidate| variant.is_none_or(|v| c.dmr.and_then(|d| d.variant).is_none_or(|x| x == v));
    if let Some(&(f, c)) = decoded.iter().find(|(f, c)| conflict(c).is_none() && right_kind(c) && (says_site(c) || !shared(*f))) {
        return verdict(Verdict::Active, format!("{} on {}", describe(c), mhz(f)), Some((f, c)));
    }
    if let Some(&(f, c)) = decoded.iter().find(|(_, c)| conflict(c).is_some()) {
        return verdict(Verdict::OtherSite, format!("{} on {}: {}", describe(c), mhz(f), conflict(c).unwrap_or_default()), Some((f, c)));
    }
    if let Some(&(f, c)) = decoded.iter().find(|(_, c)| !right_kind(c)) {
        return verdict(Verdict::Unconfirmed, format!("{} on {}, but this system is {}", describe(c), mhz(f), variant.map_or("another kind", |v| v.name())), Some((f, c)));
    }
    if let Some(&(f, c)) = decoded.first() {
        return verdict(Verdict::Unconfirmed, format!("{} on {}, which other sites list too", describe(c), mhz(f)), Some((f, c)));
    }
    // Something there, not of its kind: another kind's decode, or a carrier.
    let bar = if ty == "dmr" { SOME_UP } else { CONTROL_UP / 2.0 };
    let carrier = found.iter().copied().filter(|(_, c)| c.kind != Kind::Other || c.up.unwrap_or(0.0) >= bar).max_by(|a, b| a.1.up.unwrap_or(0.0).total_cmp(&b.1.up.unwrap_or(0.0)));
    if let Some((f, c)) = carrier {
        let up = c.up.map_or(String::new(), |u| format!(", up {:.0} % of the time", u * 100.0));
        return verdict(Verdict::Unconfirmed, format!("{} on {}{up}", describe(c), mhz(f)), Some((f, c)));
    }
    verdict(Verdict::Inactive, format!("nothing on {} frequenc{}", listened.len(), if listened.len() == 1 { "y" } else { "ies" }), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::Variant;
    use crate::survey::DmrFound;

    fn heard(target: f64, kind: Kind, f: impl FnOnce(&mut Candidate)) -> Candidate {
        let mut c = Candidate {
            freq_hz: target,
            band: "",
            snr_db: 20.0,
            width_hz: 0.0,
            kind,
            frames: 10,
            good: 10,
            bad: 0,
            modulation: "",
            identity: Identity::default(),
            dmr: None,
            nxdn: None,
            target: Some(target),
            up: Some(1.0),
        };
        f(&mut c);
        c
    }

    fn site(label: &str, freqs: &[f64]) -> Site {
        Site { label: label.into(), freqs_hz: freqs.to_vec(), ..Default::default() }
    }

    #[test]
    fn p25_sites_by_what_they_say() {
        let mut a = site("A", &[851.0125e6]);
        a.expect = Identity::default().with(IdField::Nac, 0x212).with(IdField::Site, 3);
        let mut b = site("B", &[852.5e6]);
        b.expect = Identity::default().with(IdField::Site, 4);
        let c = site("C", &[853.0e6]);
        let d = site("D", &[860.0e6]);
        let id = Identity::default().with(IdField::Nac, 0x212).with(IdField::Rfss, 1).with(IdField::Site, 3);
        let found = [heard(851.0125e6, Kind::Control, |c| c.identity = id.clone()), heard(852.5e6, Kind::Control, |c| c.identity = id.clone()), heard(853.0e6, Kind::Other, |c| c.up = Some(0.95))];
        let probed = [851.0125e6, 852.5e6, 853.0e6];
        let v: Vec<Verdict> = judge_all("p25", &[a, b, c, d], &found, &probed).iter().map(|j| j.verdict).collect();
        assert_eq!(v, [Verdict::Active, Verdict::OtherSite, Verdict::Unconfirmed, Verdict::NotChecked]);
    }

    #[test]
    fn a_shared_frequency_confirms_no_one_and_dmr_is_one_kind() {
        let plus = |c: &mut Candidate| c.dmr = Some(DmrFound { variant: Some(Variant::ConnectPlus), color_code: Some(1) });
        let tier3 = |c: &mut Candidate| c.dmr = Some(DmrFound { variant: Some(Variant::TierIII), color_code: Some(1) });
        let sites = [site("A", &[452.1e6]), site("B", &[452.2e6]), site("C", &[452.3e6]), site("D", &[452.3e6]), site("E", &[452.4e6]), site("F", &[452.5e6])];
        let found = [heard(452.1e6, Kind::DmrControl, plus), heard(452.2e6, Kind::DmrControl, plus), heard(452.3e6, Kind::DmrControl, plus), heard(452.4e6, Kind::DmrControl, tier3)];
        let probed = [452.1e6, 452.2e6, 452.3e6, 452.4e6, 452.5e6];
        let v: Vec<Verdict> = judge_all("dmr", &sites, &found, &probed).iter().map(|j| j.verdict).collect();
        assert_eq!(v, [Verdict::Active, Verdict::Active, Verdict::Unconfirmed, Verdict::Unconfirmed, Verdict::Unconfirmed, Verdict::Inactive]);
        // Another colour code: another site.
        let mut g = site("G", &[452.1e6]);
        g.color_code = Some(5);
        assert_eq!(judge_all("dmr", &[g], &found, &probed)[0].verdict, Verdict::OtherSite);
    }

    #[test]
    fn decode_grades() {
        let good = heard(851e6, Kind::Control, |_| {});
        assert_eq!(quality("p25", &good), Quality::Good);
        assert_eq!(quality("p25", &heard(851e6, Kind::Control, |c| c.snr_db = 8.0)), Quality::Marginal);
        assert_eq!(quality("p25", &heard(851e6, Kind::Control, |c| (c.good, c.bad) = (3, 7))), Quality::Poor);
        assert_eq!(quality("p25", &heard(851e6, Kind::P25, |_| {})), Quality::Marginal);
        assert_eq!(quality("p25", &heard(851e6, Kind::Other, |_| {})), Quality::Poor);
    }
}
