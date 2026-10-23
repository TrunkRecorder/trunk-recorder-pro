//! Sharing a system with the community directory (trunkrecorder.pro/directory):
//! what this recorder hears of it, sent now and then while it's recorded, as
//! long as the system's `share` is on.
//!
//! What's shared is what the radio heard: the sites' identities and control
//! channels, the voice channels granted, the talkgroups granted (their numbers,
//! how often, where, whether encrypted), the neighbours announced, the call
//! signs sent on the air. Not what people typed: talkgroups' names,
//! descriptions, tags and groups are built (see [`talkgroup_rows`]) but left
//! out while [`SHARE_TALKGROUP_TEXT`] is off. Never: keys, audio, radio IDs or
//! aliases, a location.
//!
//! When: once there's something new (a talkgroup or voice channel heard the
//! first time, a call sign), at most hourly; else daily, to say it's still
//! heard; only while it's decoded. The platform sends it (`Output::Share`)
//! and says how it went ([`Ledger::done`]); the directory's answer links the
//! system and its sites ([`crate::config::Config::link_shared`]).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use trunk_core::trunk::{SiteStatus, Talkgroup, Talkgroups};

use crate::config::{FccFrom, System, SystemKind};

/// Whether talkgroups' text (alpha tag, description, mode, tag, group,
/// priority) is shared. Off for now: only what the recorder heard is. (On
/// needs each talkgroup row's source, so RadioReference pastes stay out.)
pub const SHARE_TALKGROUP_TEXT: bool = false;
/// The first share: after this long decoding it (Unix ms).
pub const FIRST_AFTER_MS: f64 = 5.0 * 60_000.0;
/// At most this often (something new to say).
pub const MIN_GAP_MS: f64 = 3600_000.0;
/// At least this often while it's decoded (still heard).
pub const HEARTBEAT_MS: f64 = 24.0 * 3600_000.0;
/// After a failure: this, doubling each time, up to [`MAX_RETRY_MS`].
pub const RETRY_MS: f64 = 5.0 * 60_000.0;
pub const MAX_RETRY_MS: f64 = 6.0 * 3600_000.0;

/// A system can be shared: the directory takes its kind, and its `share` is on.
pub fn shareable(sys: &System) -> bool {
    sys.share && matches!(sys.kind, SystemKind::P25 | SystemKind::Smartnet | SystemKind::Dmr | SystemKind::Nxdn)
}

/// Whether the system has an FCC call sign (Identify, the air, or typed): for
/// now, a system is shared only once it has one.
pub fn has_call_sign(sys: &System) -> bool {
    sys.fcc.values().any(|l| !l.call_sign.trim().is_empty())
}

/// What sharing uses of a system's config (its switch, directory links and
/// licenses), from `src` as it is now into `dst`.
pub fn sync(dst: &mut System, src: &System) {
    (dst.share, dst.directory, dst.fcc) = (src.share, src.directory.clone(), src.fcc.clone());
    for (a, b) in dst.sites.iter_mut().zip(&src.sites) {
        a.directory_id = b.directory_id.clone();
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Heard {
    grants: u32,
    /// Engine site numbers.
    sites: BTreeSet<u16>,
    encrypted: bool,
}

/// What this run has heard of one shared system, since its last share.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    /// When it was first decoded this run (Unix ms).
    since_ms: Option<f64>,
    talkgroups: BTreeMap<u32, Heard>,
    /// (engine site, Hz) → grants.
    voice: BTreeMap<(u16, u64), u32>,
    /// Hz → call sign.
    call_signs: BTreeMap<u64, String>,
    known_talkgroups: BTreeSet<u32>,
    known_voice: BTreeSet<(u16, u64)>,
    /// Something new since the last share.
    changed: bool,
    /// The last share (Unix ms; 0: never).
    last_ms: f64,
    /// A share on its way.
    pending: bool,
    /// Its config has an FCC call sign (or one is in [`Evidence::call_signs`]; neither: not shared yet).
    fcc_call_sign: bool,
    failures: u32,
    retry_at_ms: f64,
    error: Option<String>,
}

impl Evidence {
    /// An FCC call sign, in its config or heard on the air: sharing waits for one.
    fn has_call_sign(&self) -> bool {
        self.fcc_call_sign || !self.call_signs.is_empty()
    }
}

/// Every shared system's evidence and schedule, by short name.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    systems: BTreeMap<String, Evidence>,
    /// In place of [`FIRST_AFTER_MS`] (testing against a short capture).
    first_after_ms: Option<f64>,
}

impl Ledger {
    /// The systems shared in `systems`, each from its last share (its directory link's `sharedAt`).
    pub fn new<'a>(systems: impl IntoIterator<Item = &'a System>) -> Ledger {
        let mut l = Ledger::default();
        for s in systems {
            l.set(s);
        }
        l
    }

    /// A system's `share` as it is now (switched on or off while recording).
    pub fn set(&mut self, sys: &System) {
        if shareable(sys) {
            let e = self.systems.entry(sys.short_name.clone()).or_default();
            e.last_ms = e.last_ms.max(sys.directory.as_ref().map_or(0.0, |d| d.shared_at as f64));
            e.fcc_call_sign = has_call_sign(sys);
        } else {
            self.systems.remove(&sys.short_name);
        }
    }

    /// Share first after `ms` of decoding, not [`FIRST_AFTER_MS`] (testing).
    pub fn first_after(&mut self, ms: f64) {
        self.first_after_ms = Some(ms);
    }

    pub fn shares(&self, short_name: &str) -> bool {
        self.systems.contains_key(short_name)
    }

    /// A call was granted on the system (on engine site `site`; `twin`: another site's copy of one already counted).
    pub fn call(&mut self, short_name: &str, site: Option<u16>, talkgroup: u32, freq_hz: u64, encrypted: bool, twin: bool) {
        let Some(e) = self.systems.get_mut(short_name) else { return };
        let h = e.talkgroups.entry(talkgroup).or_default();
        if !twin {
            h.grants += 1;
        }
        h.encrypted |= encrypted;
        if let Some(s) = site {
            h.sites.insert(s);
            *e.voice.entry((s, freq_hz)).or_default() += 1;
            e.changed |= e.known_voice.insert((s, freq_hz));
        }
        e.changed |= e.known_talkgroups.insert(talkgroup);
    }

    /// A call sign heard on the air for one of the system's frequencies.
    pub fn call_sign(&mut self, short_name: &str, freq_hz: u64, call_sign: &str) {
        let Some(e) = self.systems.get_mut(short_name) else { return };
        if e.call_signs.insert(freq_hz, call_sign.to_string()).as_deref() != Some(call_sign) {
            e.changed = true;
        }
    }

    /// The system is being decoded now.
    pub fn decoding(&mut self, short_name: &str, now_ms: f64) {
        if let Some(e) = self.systems.get_mut(short_name) {
            e.since_ms.get_or_insert(now_ms);
        }
    }

    /// Whether to share it now.
    pub fn due(&self, short_name: &str, now_ms: f64) -> bool {
        let Some(e) = self.systems.get(short_name) else { return false };
        let Some(since) = e.since_ms else { return false };
        if !e.has_call_sign() || e.pending || now_ms < e.retry_at_ms || now_ms - since < self.first_after_ms.unwrap_or(FIRST_AFTER_MS) {
            return false;
        }
        let gap = now_ms - e.last_ms;
        (e.changed && gap >= MIN_GAP_MS) || gap >= HEARTBEAT_MS
    }

    /// Its share is on its way.
    pub fn sent(&mut self, short_name: &str) {
        if let Some(e) = self.systems.get_mut(short_name) {
            e.pending = true;
        }
    }

    /// How its share went: taken (its counts start again), or why not (tried again later).
    pub fn done(&mut self, short_name: &str, now_ms: f64, result: Result<(), String>) {
        let Some(e) = self.systems.get_mut(short_name) else { return };
        e.pending = false;
        match result {
            Ok(()) => {
                e.last_ms = now_ms;
                e.changed = false;
                e.failures = 0;
                e.retry_at_ms = 0.0;
                e.error = None;
                e.talkgroups.clear();
                e.voice.clear();
            }
            Err(err) => {
                e.failures += 1;
                e.retry_at_ms = now_ms + (RETRY_MS * 2f64.powi(e.failures as i32 - 1)).min(MAX_RETRY_MS);
                e.error = Some(err);
            }
        }
    }

    /// For the interface: `{ state: needsCallSign | waiting | shared | error, lastShared, error }`, or null when it isn't shared.
    pub fn status_json(&self, short_name: &str) -> Value {
        let Some(e) = self.systems.get(short_name) else { return Value::Null };
        let state = if !e.has_call_sign() { "needsCallSign" } else if e.error.is_some() { "error" } else if e.last_ms > 0.0 { "shared" } else { "waiting" };
        json!({ "state": state, "lastShared": if e.last_ms > 0.0 { Some(e.last_ms) } else { None }, "error": e.error })
    }

    pub fn evidence(&self, short_name: &str) -> Option<&Evidence> {
        self.systems.get(short_name)
    }
}

/// A talkgroup file's rows as the directory takes them: everything a row has.
/// (Built always; shared only with [`SHARE_TALKGROUP_TEXT`].) `source` is
/// "user" until rows carry where they came from.
pub fn talkgroup_rows<'a>(talkgroups: impl IntoIterator<Item = &'a Talkgroup>) -> Vec<Value> {
    talkgroups
        .into_iter()
        .map(|t| {
            json!({
                "number": t.number, "alpha": t.alpha_tag, "description": t.description, "mode": t.mode,
                "tag": t.tag, "category": t.group, "priority": if t.priority > 0 { t.priority.to_string() } else { String::new() },
                "source": "user",
            })
        })
        .collect()
}

/// The share of `sys` (version 1 of the directory's format): its sites heard
/// (`sites`: its running sites' status), what this run heard of it, and its
/// talkgroups' text if that's shared. With the site names in the order of the
/// share's sites, for linking them to the directory's answer. None: nothing
/// of it heard yet.
pub fn observation(recorder: &str, app: &str, sys: &System, sites: &[&SiteStatus], ev: &Evidence, talkgroups: Option<&Talkgroups>) -> Option<(Value, Vec<String>)> {
    let kind = match sys.kind {
        SystemKind::P25 => "p25",
        SystemKind::Smartnet => "smartnet",
        SystemKind::Dmr => "dmr",
        SystemKind::Nxdn => "nxdn",
        _ => return None,
    };
    // The configured sites that are heard (decoded), in the config's order.
    let mut order: Vec<(&crate::config::Site, &SiteStatus)> = Vec::new();
    for s in sys.active_sites() {
        if let Some(st) = sites.iter().find(|y| y.site_name == sys.site_name(s) && y.good > 0 && y.mismatch.is_none()) {
            order.push((s, st));
        }
    }
    if order.is_empty() {
        return None;
    }
    let index: BTreeMap<u16, usize> = order.iter().enumerate().map(|(i, (_, st))| (st.site, i)).collect();
    let site_json: Vec<Value> = order
        .iter()
        .map(|(cfg, st)| {
            let id = crate::session::identity_json(&st.identity);
            let mut call_signs: BTreeMap<u64, String> = st.callsigns.iter().map(|(c, hz)| (*hz, c.clone())).collect();
            for (hz, c) in &ev.call_signs {
                call_signs.entry(*hz).or_insert_with(|| c.clone());
            }
            let mut site = json!({
                "controlChannelsHz": cfg.control_channels_hz,
                "expect": id,
                "modulation": st.modulation,
                "heard": {
                    "identity": id,
                    "controlChannelHz": st.control_channel_hz,
                    "voice": ev.voice.iter().filter(|((s, _), _)| *s == st.site).map(|((_, hz), n)| json!({ "freqHz": hz, "grants": n })).collect::<Vec<_>>(),
                    "neighbours": st.adjacent.iter().filter(|a| a.freq_hz > 0).map(|a| json!({ "sysId": a.sys_id, "rfss": a.rfss, "site": a.site, "freqHz": a.freq_hz })).collect::<Vec<_>>(),
                    "callSigns": call_signs.iter().filter(|(hz, _)| **hz > 0).map(|(hz, c)| json!({ "freqHz": hz, "callSign": c })).collect::<Vec<_>>(),
                    "good": st.good,
                    "bad": st.bad,
                },
            });
            if !cfg.directory_id.is_empty() {
                site["directoryId"] = json!(cfg.directory_id);
            }
            if !cfg.bandplan.is_empty() {
                site["bandplan"] = json!({ "bandplan": cfg.bandplan, "bandplanBaseHz": cfg.bandplan_base_hz, "bandplanSpacingHz": cfg.bandplan_spacing_hz, "bandplanOffset": cfg.bandplan_offset, "bandplanHighHz": cfg.bandplan_high_hz });
            }
            site
        })
        .collect();
    // Call signs heard on the air (the config's `fcc` entries from the air), not looked up or typed.
    let fcc: serde_json::Map<String, Value> = sys.fcc.iter().filter(|(_, l)| l.from == FccFrom::Air).map(|(k, l)| (k.clone(), json!({ "callSign": l.call_sign, "from": "air" }))).collect();
    let heard_tgs: Vec<Value> = ev
        .talkgroups
        .iter()
        .map(|(tg, h)| json!({ "number": tg, "grants": h.grants, "sites": h.sites.iter().filter_map(|s| index.get(s)).collect::<Vec<_>>(), "encrypted": h.encrypted }))
        .collect();
    let mut o = json!({
        "v": 1,
        "recorder": recorder,
        "app": app,
        "system": { "type": kind, "sites": site_json, "fcc": fcc },
        "heardTalkgroups": heard_tgs,
    });
    if let Some(d) = &sys.directory {
        o["directory"] = json!({ "id": d.id, "rev": d.rev });
    }
    if SHARE_TALKGROUP_TEXT {
        if let Some(t) = talkgroups {
            let mut rows: Vec<&Talkgroup> = t.values().collect();
            rows.sort_by_key(|r| r.number);
            o["talkgroups"] = json!(talkgroup_rows(rows));
            if !sys.name.is_empty() {
                o["system"]["name"] = json!(sys.name);
            }
        }
    }
    Some((o, order.iter().map(|(s, _)| sys.site_name(s).to_string()).collect()))
}

/// A new recorder ID (the install's, anonymous): `r_` and 16 base32 characters from `random`.
pub fn recorder_id(random: &[u8; 16]) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    format!("r_{}", random.iter().map(|b| ALPHABET[(b & 31) as usize] as char).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const MIN: f64 = 60_000.0;
    /// A Unix ms now (the session's clock).
    const NOW: f64 = 1.8e12;

    fn config(share: bool) -> Config {
        let sys = json!({
            "shortName": "dcfd", "name": "DC Fire", "type": "p25", "enabled": true, "share": share,
            "fcc": { "857987500": { "callSign": "WPXT459", "from": "air" }, "858987500": { "callSign": "KNJA391", "name": "DC", "from": "lookup" } },
            "sites": [
                { "name": "north", "enabled": true, "controlChannelsHz": [857_987_500.0], "modulation": "auto", "expect": {}, "voiceChannelsHz": [] },
                { "name": "south", "enabled": true, "controlChannelsHz": [858_987_500.0], "modulation": "auto", "expect": {}, "voiceChannelsHz": [] },
            ],
        });
        Config::from_json(&json!({ "systems": [sys] }).to_string()).unwrap()
    }

    fn site(n: u16, name: &str, good: u64) -> SiteStatus {
        SiteStatus { site: n, short_name: "dcfd".into(), site_name: name.into(), control_channel_hz: Some(857_987_500), good, modulation: "fsk4", ..SiteStatus::default() }
    }

    #[test]
    fn only_shared_trunked_systems() {
        let c = config(true);
        assert!(Ledger::new(&c.systems).shares("dcfd"));
        assert!(!Ledger::new(&config(false).systems).shares("dcfd"));
        let mut s = c.systems[0].clone();
        s.kind = SystemKind::Edacs;
        assert!(!shareable(&s));
    }

    #[test]
    fn first_after_a_few_minutes_then_on_change_at_most_hourly_else_daily() {
        let c = config(true);
        let mut l = Ledger::new(&c.systems);
        let t0 = NOW;
        assert!(!l.due("dcfd", t0), "not decoded yet");
        l.decoding("dcfd", t0);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        assert!(!l.due("dcfd", t0 + 4.0 * MIN));
        assert!(l.due("dcfd", t0 + 5.0 * MIN));
        l.sent("dcfd");
        assert!(!l.due("dcfd", t0 + 6.0 * MIN), "one on its way");
        let t1 = t0 + 6.0 * MIN;
        l.done("dcfd", t1, Ok(()));
        assert_eq!(l.status_json("dcfd")["state"], "shared");
        // The same talkgroup on the same channel again: nothing new.
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        assert!(!l.due("dcfd", t1 + 2.0 * 60.0 * MIN));
        // A new one: but not within the hour.
        l.call("dcfd", Some(0), 102, 851_000_000, true, false);
        assert!(!l.due("dcfd", t1 + 30.0 * MIN));
        assert!(l.due("dcfd", t1 + 60.0 * MIN));
        // Shared, then nothing new: daily.
        let t2 = t1 + 60.0 * MIN;
        l.done("dcfd", t2, Ok(()));
        assert!(!l.due("dcfd", t2 + 23.0 * 60.0 * MIN));
        assert!(l.due("dcfd", t2 + 24.0 * 60.0 * MIN));
    }

    #[test]
    fn not_shared_without_a_call_sign_until_one_is_known() {
        let mut c = config(true);
        c.systems[0].fcc.clear();
        let mut l = Ledger::new(&c.systems);
        l.decoding("dcfd", NOW);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        assert!(!l.due("dcfd", NOW + 10.0 * MIN), "no call sign: not shared");
        assert_eq!(l.status_json("dcfd")["state"], "needsCallSign");
        // One heard on the air (a Motorola base station ID): shared.
        l.call_sign("dcfd", 857_987_500, "WPXT459");
        assert!(l.due("dcfd", NOW + 10.0 * MIN));
        // Or one looked up (Identify), told while recording.
        let mut l = Ledger::new(&c.systems);
        l.decoding("dcfd", NOW);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        assert!(!l.due("dcfd", NOW + 10.0 * MIN));
        c.systems[0].fcc.insert("857987500".into(), crate::config::FccLicense { call_sign: "WPXT459".into(), name: "DISTRICT OF COLUMBIA".into(), from: FccFrom::Lookup });
        l.set(&c.systems[0]);
        assert!(l.due("dcfd", NOW + 10.0 * MIN));
        assert_eq!(l.status_json("dcfd")["state"], "waiting");
        // Removed again: not shared again.
        c.systems[0].fcc.clear();
        l.set(&c.systems[0]);
        assert!(!l.due("dcfd", NOW + 10.0 * MIN));
        assert_eq!(l.status_json("dcfd")["state"], "needsCallSign");
    }

    #[test]
    fn failures_back_off() {
        let c = config(true);
        let mut l = Ledger::new(&c.systems);
        l.decoding("dcfd", NOW);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        let t = NOW + 10.0 * MIN;
        l.sent("dcfd");
        l.done("dcfd", t, Err("down".into()));
        assert_eq!(l.status_json("dcfd")["error"], "down");
        assert!(!l.due("dcfd", t + 4.0 * MIN));
        assert!(l.due("dcfd", t + 5.0 * MIN));
        l.done("dcfd", t + 5.0 * MIN, Err("down".into()));
        assert!(!l.due("dcfd", t + 14.0 * MIN), "twice as long");
        assert!(l.due("dcfd", t + 15.0 * MIN));
        for _ in 0..10 {
            l.done("dcfd", t, Err("down".into()));
        }
        assert!(l.due("dcfd", t + MAX_RETRY_MS), "never longer than its cap");
    }

    #[test]
    fn last_share_survives_a_restart() {
        let mut c = config(true);
        assert!(c.link_shared("dcfd", "sys_1", 3, &["south".into()], &["site_s".into()], NOW));
        let mut l = Ledger::new(&c.systems);
        l.decoding("dcfd", NOW);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        assert!(!l.due("dcfd", NOW + 30.0 * MIN), "shared half an hour ago, before the restart");
        assert!(l.due("dcfd", NOW + 60.0 * MIN));
    }

    #[test]
    fn what_is_shared_is_what_was_heard() {
        let mut c = config(true);
        c.link_shared("dcfd", "sys_1", 3, &["north".into()], &["site_n".into()], 1.0);
        let sys = &c.systems[0];
        let mut l = Ledger::new(&c.systems);
        l.call("dcfd", Some(0), 101, 851_000_000, false, false);
        l.call("dcfd", Some(0), 101, 851_000_000, false, true);
        l.call("dcfd", Some(1), 101, 852_000_000, false, true);
        l.call("dcfd", Some(1), 202, 852_000_000, true, false);
        let mut tgs = Talkgroups::default();
        tgs.insert(101, Talkgroup { number: 101, alpha_tag: "Dispatch".into(), description: "From a website".into(), ..Talkgroup::default() });
        let (north, south, unheard) = (site(0, "north", 50), site(1, "south", 9), site(2, "east", 0));
        let (o, names) = observation("r_0123456789abcdef", "trunk-pro/test", sys, &[&north, &south, &unheard], l.evidence("dcfd").unwrap(), Some(&tgs)).unwrap();
        assert_eq!(names, ["north", "south"]);
        assert_eq!(o["v"], 1);
        assert_eq!(o["directory"], json!({ "id": "sys_1", "rev": 3 }));
        let s = &o["system"];
        assert_eq!(s["type"], "p25");
        assert_eq!(s["sites"].as_array().unwrap().len(), 2);
        assert_eq!(s["sites"][0]["directoryId"], "site_n");
        assert_eq!(s["sites"][1].get("directoryId"), None);
        assert_eq!(s["sites"][0]["heard"]["voice"], json!([{ "freqHz": 851_000_000u64, "grants": 2 }]));
        assert_eq!(s["fcc"], json!({ "857987500": { "callSign": "WPXT459", "from": "air" } }), "call signs from the air only");
        assert_eq!(o["heardTalkgroups"], json!([
            { "number": 101, "grants": 1, "sites": [0, 1], "encrypted": false },
            { "number": 202, "grants": 1, "sites": [1], "encrypted": true },
        ]));
        // Nothing anyone typed.
        assert!(!SHARE_TALKGROUP_TEXT);
        assert_eq!(o.get("talkgroups"), None);
        assert_eq!(s.get("name"), None);
        assert!(!o.to_string().contains("Dispatch"));
        // (What would be, with it on: every field of a row.)
        let rows = talkgroup_rows(tgs.values());
        assert_eq!(rows[0]["alpha"], "Dispatch");
        for k in ["number", "alpha", "description", "mode", "tag", "category", "priority", "source"] {
            assert!(rows[0].get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn nothing_to_share_until_a_site_is_decoded() {
        let c = config(true);
        let l = Ledger::new(&c.systems);
        let mut wrong = site(0, "north", 50);
        wrong.mismatch = Some("another system".into());
        assert!(observation("r_0123456789abcdef", "t", &c.systems[0], &[&site(1, "south", 0), &wrong], l.evidence("dcfd").unwrap(), None).is_none());
    }

    #[test]
    fn recorder_ids_are_the_directorys_shape() {
        let id = recorder_id(&[255; 16]);
        assert_eq!(id.len(), 18);
        assert!(id[2..].chars().all(|c| "0123456789abcdefghjkmnpqrstvwxyz".contains(c)));
    }

    #[test]
    fn linking_names_sites_by_name() {
        let mut c = config(true);
        assert!(c.link_shared("dcfd", "sys_1", 3, &["south".into(), "north".into()], &["site_s".into(), "site_n".into()], 7.0));
        let s = &c.systems[0];
        assert_eq!(s.directory.as_ref().map(|d| (d.id.as_str(), d.rev)), Some(("sys_1", 3)));
        assert_eq!((s.sites[0].directory_id.as_str(), s.sites[1].directory_id.as_str()), ("site_n", "site_s"));
        assert!(!c.link_shared("dcfd", "sys_1", 3, &[], &[], 7.0), "nothing changed");
        // Kept in the config file, read back.
        let back = Config::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.systems[0], c.systems[0]);
    }
}
