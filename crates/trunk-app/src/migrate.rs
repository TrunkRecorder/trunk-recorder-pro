//! Upgrading what an older version left — for a while only. Before
//! 2026-10 each site was a system of its own (see
//! research/multi-site-systems.md). Everything that reads an older config,
//! or the files kept under a former system's name, goes through here; the
//! web's side is web/src/migrate.ts. To drop the upgrade, delete both (and
//! their tests) and the few lines the compiler then points at.

use std::path::{Path, PathBuf};

use trunk_core::trunk::SiteConfig;

use crate::config::{Config, System};

/// A site's keys in a system's config (before sites, a system had them itself).
const SITE_KEYS: &[&str] = &[
    "controlChannelsHz", "modulation", "expect", "voiceChannelsHz", "bandplan", "bandplanBaseHz", "bandplanSpacingHz", "bandplanOffset", "bandplanHighHz",
    "lcnTableHz", "dmrChannelsHz", "colorCode", "nxdnRate", "nxdnChannelsHz", "ran",
];

/// Before systems had sites, each site was a system of its own (its control
/// channels and the rest at the system's top level), joined to others by a
/// `siteGroup`, or by site locks naming one system. Such systems become
/// systems with sites, in `v` (a config's JSON): each old system a site named
/// as it was — so its band plan file, the talkgroup file's Preferred Site and
/// its recordings still match — and those joined one system, the first's
/// short name and settings its own. Returns what changed, to tell.
pub fn config(v: &mut serde_json::Value) -> Vec<String> {
    use serde_json::{Map, Value};
    let Some(systems) = v.get_mut("systems").and_then(Value::as_array_mut) else { return vec![] };
    if systems.iter().all(|s| s.get("sites").is_some()) {
        return vec![];
    }
    let str_of = |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    // Each old system made one of a site, with the system it joins (by its group or site lock).
    let mut made: Vec<(Option<String>, Map<String, Value>)> = Vec::new();
    for sys in systems.drain(..) {
        let Value::Object(mut o) = sys else { continue };
        if o.contains_key("sites") {
            made.push((None, o));
            continue;
        }
        let mut site = Map::new();
        site.insert("name".into(), Value::String(str_of(&o, "shortName")));
        if let Some(e) = o.get("enabled") {
            site.insert("enabled".into(), e.clone());
        }
        for k in SITE_KEYS {
            if let Some(x) = o.remove(*k) {
                site.insert((*k).into(), x);
            }
        }
        let kind = match str_of(&o, "type").to_ascii_lowercase().as_str() {
            "" => "p25".to_string(),
            k => k.to_string(),
        };
        let expect = site.get("expect").and_then(Value::as_object);
        let id = |k: &str| expect.and_then(|e| e.get(k)).and_then(Value::as_u64);
        let locked = match kind.as_str() {
            "p25" => id("wacn").zip(id("sysId")).map(|(w, s)| format!("p25:{w:x}.{s:x}")),
            "smartnet" => id("sysId").map(|s| format!("smartnet:{s:x}")),
            "nxdn" if !str_of(&o, "nxdnType").eq_ignore_ascii_case("typeD") => id("sysId").map(|s| format!("nxdn:{s:x}")),
            _ => None,
        };
        let group = match str_of(&o, "siteGroup") {
            g if !g.is_empty() => Some(format!("group:{g}")),
            _ => locked,
        };
        o.remove("siteGroup");
        o.insert("sites".into(), Value::Array(vec![Value::Object(site)]));
        made.push((group, o));
    }
    // What a site's old system had for the system: kept only from the first.
    const SYSTEM_KEYS: &[(&str, &str)] = &[
        ("talkgroupsCsv", "talkgroup file"), ("unitNames", "unit names"), ("recording", "recording rules"), ("plugins", "plugin settings"),
        ("pluginDelays", "plugin delays"), ("keys", "keys"), ("defaultMode", "default mode"), ("nxdnType", "NXDN type"),
    ];
    let mut notes = vec!["Systems have sites now: each system's control channels are its first site's (Setup, Systems).".to_string()];
    // Each system made: its group, its JSON, the old systems that joined it.
    type Made = (Option<String>, Map<String, Value>, Vec<String>);
    let mut out: Vec<Made> = Vec::new();
    for (group, o) in made {
        let joins = group.as_ref().and_then(|g| out.iter().position(|(h, ..)| h.as_ref() == Some(g)));
        let Some(i) = joins else {
            out.push((group, o, vec![]));
            continue;
        };
        let (_, first, joined) = &mut out[i];
        let name = str_of(&o, "shortName");
        let set = |x: &&Value| !(x.is_null() || x.as_str() == Some("") || x.as_object().is_some_and(Map::is_empty) || x.as_array().is_some_and(Vec::is_empty));
        let dropped: Vec<&str> = SYSTEM_KEYS.iter().filter(|(k, _)| o.get(*k).filter(set).is_some_and(|x| first.get(*k) != Some(x))).map(|(_, what)| *what).collect();
        if !dropped.is_empty() {
            notes.push(format!("{name}'s {} weren't kept: {} has its system's now.", dropped.join(", "), str_of(first, "shortName")));
        }
        if o.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            first.insert("enabled".into(), Value::Bool(true));
        }
        let more = o.get("sites").and_then(Value::as_array).cloned().unwrap_or_default();
        if let Some(Value::Array(to)) = first.get_mut("sites") {
            to.extend(more);
        }
        joined.push(name);
    }
    for (_, o, joined) in &out {
        if !joined.is_empty() {
            notes.push(format!("{} and {} are one system now, {}, with a site each (Setup, Systems).", str_of(o, "shortName"), joined.join(", "), str_of(o, "shortName")));
        }
    }
    // A site named as its system leaves its name out.
    for (_, o, _) in out.iter_mut() {
        let short = str_of(o, "shortName");
        for site in o.get_mut("sites").and_then(Value::as_array_mut).into_iter().flatten() {
            if site.get("name").and_then(Value::as_str).is_some_and(|n| n.trim() == short) {
                site.as_object_mut().map(|s| s.remove("name"));
            }
        }
    }
    *systems = out.into_iter().map(|(_, o, _)| Value::Object(o)).collect();
    notes
}

/// After [`config`] changed a config: the one read kept beside it
/// (`config.json.before-sites`, once), and the upgraded one saved over it.
/// Returns where the old one is.
pub fn save(cfg: &Config, path: &Path) -> std::io::Result<PathBuf> {
    let old = path.with_extension("json.before-sites");
    if !old.exists() {
        std::fs::copy(path, &old)?;
    }
    cfg.save(path)?;
    Ok(old)
}

/// The names `sys`'s sites had as systems of their own (but its short
/// name): their talker aliases and recordings are still kept under them.
pub fn old_names(sys: &System) -> Vec<String> {
    sys.sites.iter().map(|x| sys.site_name(x).to_string()).filter(|n| *n != sys.short_name).collect()
}

/// The name a site's band plan was kept under when it was a system of its
/// own (read once; it is saved under its key from then on).
pub fn old_bandplan(s: &SiteConfig) -> &str {
    s.site_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An older config: each system a site of its own, those its site group
    /// or site locks joined made one system.
    #[test]
    fn older_systems_become_systems_with_sites() {
        let c = Config::from_json(r#"{ "systems": [{ "shortName": "a", "controlChannelsHz": [851012500], "modulation": "qpsk", "talkgroupsCsv": "x" }] }"#).unwrap();
        assert!(c.recording.drop_duplicate_calls);
        assert_eq!(c.systems.len(), 1);
        let (sys, site) = (&c.systems[0], &c.systems[0].sites[0]);
        assert_eq!((sys.talkgroups_csv.as_str(), site.name.as_str(), site.control_channels_hz.as_slice(), site.modulation.as_str()), ("x", "", &[851_012_500.0][..], "qpsk"));
        assert_eq!(c.notes.len(), 1, "{:?}", c.notes);
        let saved = serde_json::to_value(&c).unwrap();
        assert!(saved["systems"][0].get("controlChannelsHz").is_none() && saved["systems"][0]["sites"][0]["controlChannelsHz"][0] == 851012500.0);
        assert!(Config::from_value(saved).unwrap().notes.is_empty(), "read again: nothing to change");

        let c = Config::from_json(
            r#"{ "recording": { "dropDuplicateCalls": false }, "systems": [
                { "shortName": "cm1", "type": "dmr", "controlChannelsHz": [452175000], "siteGroup": " capmax ", "talkgroupsCsv": "tg" },
                { "shortName": "p1", "controlChannelsHz": [851012500], "expect": { "wacn": 781824, "sysId": 418, "site": 1 } },
                { "shortName": "cm2", "type": "dmr", "controlChannelsHz": [452375000], "siteGroup": "capmax", "talkgroupsCsv": "other", "enabled": false },
                { "shortName": "p2", "controlChannelsHz": [851512500], "expect": { "wacn": 781824, "sysId": 418, "site": 2 } },
                { "shortName": "p3", "controlChannelsHz": [852012500], "expect": { "wacn": 781824, "sysId": 419 } } ] }"#,
        )
        .unwrap();
        let names = |s: &System| s.sites.iter().map(|x| s.site_name(x).to_string()).collect::<Vec<_>>();
        assert_eq!(c.systems.iter().map(|s| (s.short_name.as_str(), names(s))).collect::<Vec<_>>(), [
            ("cm1", vec!["cm1".to_string(), "cm2".into()]),
            ("p1", vec!["p1".into(), "p2".into()]),
            ("p3", vec!["p3".into()]),
        ]);
        assert_eq!(c.systems[0].talkgroups_csv, "tg");
        assert!(c.systems[0].sites[0].enabled && !c.systems[0].sites[1].enabled, "each site keeps its own switch");
        assert!(c.notes.iter().any(|n| n.contains("cm2's talkgroup file weren't kept: cm1")), "{:?}", c.notes);
        assert!(c.notes.iter().any(|n| n.starts_with("p1 and p2 are one system now")), "{:?}", c.notes);
        let e = c.engine_config(0.0);
        assert!(!e.drop_duplicates);
        assert_eq!(e.sites.iter().map(|s| (s.short_name.as_str(), s.site_name())).collect::<Vec<_>>(), [("cm1", "cm1"), ("p1", "p1"), ("p1", "p2"), ("p3", "p3")]);
        assert_eq!(trunk_core::trunk::system_numbers(&e.sites), [0, 1, 1, 2]);
        assert_eq!((c.site_of(2).map(|(s, x)| s.site_name(x)), c.short_name_of(1)), (Some("p2"), Some("p1")));
    }
}
