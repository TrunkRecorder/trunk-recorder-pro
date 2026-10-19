//! Upgrading what an older version left — for a while only: a system's
//! keys file named in `keys` (before key sources, v0.1.10), interfaces as a
//! list and `server.home` (before interfaces had ports, v0.1.14), the audio
//! kept as `audioArchive` and `compressWav` (before v0.2.4), and each
//! site a system of its own (before v0.1.15; see
//! research/multi-site-systems.md). Everything that reads an older config,
//! or the files kept under a former system's name, goes through here; the
//! web's side is web/src/migrate.ts. To drop the upgrade, delete both (and
//! their tests) and the few lines the compiler then points at.

use std::path::{Path, PathBuf};

use trunk_core::trunk::SiteConfig;

use crate::config::{Config, System, TalkgroupFiles};

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
    keys(v);
    server(v);
    archive(v);
    let mut notes = sites(v);
    notes.extend(talkgroups(v));
    notes
}

/// Talkgroup files were kept in the config (`talkgroupsCsv`, before
/// v0.1.16); they are files of their own now ([`TalkgroupFiles`]). Their
/// copies leave the config here; [`talkgroups_of`] gives them to the
/// platform to keep (the desktop app: [`move_talkgroups`]). Returns which
/// moved, to tell.
fn talkgroups(v: &mut serde_json::Value) -> Vec<String> {
    let moved: Vec<String> = talkgroups_of(v).into_keys().collect();
    for sys in v.get_mut("systems").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
        if let Some(o) = sys.as_object_mut() {
            o.remove("talkgroupsCsv");
            o.remove("talkgroupsName");
        }
    }
    if moved.is_empty() {
        return vec![];
    }
    vec![format!("Talkgroup files are kept beside the config now, as <short name>.talkgroups.csv: moved {}'s there.", moved.join("'s, "))]
}

/// The talkgroup files an older config (`v`, its JSON) holds, by its
/// systems' short names once it is upgraded (systems it joins into one keep
/// the first's file, as [`sites`] says).
pub fn talkgroups_of(v: &serde_json::Value) -> TalkgroupFiles {
    let mut v = v.clone();
    sites(&mut v);
    let systems = v.get("systems").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    systems
        .iter()
        .filter_map(|s| {
            let csv = s.get("talkgroupsCsv")?.as_str()?.to_string();
            Some((s.get("shortName")?.as_str()?.to_string(), csv)).filter(|(_, csv)| !csv.trim().is_empty())
        })
        .collect()
}

/// Before a config (at `path`) is read ([`Config::load`]): an older config's
/// talkgroup files written beside it ([`crate::config::talkgroups_path`]) —
/// those not there already. Reading the config then leaves them out of it.
pub fn move_talkgroups(path: &Path) -> std::io::Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Ok(()) };
    let dir = path.parent().unwrap_or(Path::new("."));
    for (short, csv) in talkgroups_of(&v) {
        let to = crate::config::talkgroups_path(dir, &short);
        if !to.exists() {
            crate::config::write_atomic(&to, csv)?;
        }
    }
    Ok(())
}

/// Before key sources (v0.1.10), a system's `keys` named its keys file: such
/// a file becomes a source (named after it), which the system names instead.
fn keys(v: &mut serde_json::Value) {
    use serde_json::{json, Map, Value};
    let mut sources: Map<String, Value> = v.get("keys").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut changed = false;
    for list in ["systems", "conventional"] {
        for sys in v.get_mut(list).and_then(Value::as_array_mut).into_iter().flatten() {
            let Some(file) = sys.get("keys").and_then(Value::as_str).map(|k| k.trim().to_string()) else { continue };
            if file.is_empty() || sources.contains_key(&file) {
                continue;
            }
            let name = match sources.iter().find(|(_, s)| s["file"] == file.as_str()) {
                Some((n, _)) => n.clone(),
                None => {
                    let stem = Path::new(&file).file_stem().and_then(|s| s.to_str()).unwrap_or("keys").to_string();
                    let name = (1..).map(|i| if i == 1 { stem.clone() } else { format!("{stem}-{i}") }).find(|n| !sources.contains_key(n)).unwrap();
                    sources.insert(name.clone(), json!({ "file": file }));
                    name
                }
            };
            sys["keys"] = Value::String(name);
            changed = true;
        }
    }
    if changed {
        v["keys"] = Value::Object(sources);
    }
}

/// Before v0.2.4 one setting kept a call's audio (`audioArchive`), and
/// `compressWav` an .m4a of it too; now each kind of file has its own
/// (`archiveWav`, `archiveM4a`). The recording settings, and each system's
/// own, keep what they kept: the WAV with the audio, the M4A with both. (A
/// Trunk Recorder config says the same, so it's read the same.)
fn archive(v: &mut serde_json::Value) {
    use serde_json::Value;
    let old = |r: &Value, k: &str| r.get(k).and_then(Value::as_bool);
    // The recording settings' (each default as it was: audio kept, no M4A).
    let base = v.get("recording").map_or((true, false), |r| (old(r, "audioArchive").unwrap_or(true), old(r, "compressWav").unwrap_or(false)));
    let upgrade = |r: &mut Value, (audio, m4a): (bool, bool)| {
        let Some(o) = r.as_object_mut() else { return };
        let (a, c) = (o.remove("audioArchive").and_then(|x| x.as_bool()), o.remove("compressWav").and_then(|x| x.as_bool()));
        if a.is_none() && c.is_none() {
            return;
        }
        let (audio, m4a) = (a.unwrap_or(audio), c.unwrap_or(m4a));
        o.entry("archiveWav").or_insert(Value::Bool(audio));
        o.entry("archiveM4a").or_insert(Value::Bool(audio && m4a));
    };
    if let Some(r) = v.get_mut("recording") {
        upgrade(r, (true, false));
    }
    for list in ["systems", "conventional"] {
        for sys in v.get_mut(list).and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(r) = sys.get_mut("recording") {
                upgrade(r, base);
            }
        }
    }
}

/// Before interfaces had ports of their own (v0.1.14), `server.interfaces`
/// was a list (`{name, path}` each, served at /ui/<name>/) and `server.home`
/// said what / showed: the list becomes a map by id (ports are given on
/// load), and `home` goes (/ is the built-in interface).
fn server(v: &mut serde_json::Value) {
    use serde_json::{json, Map, Value};
    let Some(server) = v.get_mut("server").and_then(Value::as_object_mut) else { return };
    server.remove("home");
    if let Some(Value::Array(list)) = server.get("interfaces") {
        let map: Map<String, Value> = list
            .iter()
            .filter_map(|o| Some((o.get("name")?.as_str()?.to_ascii_lowercase().replace('_', "-"), json!({ "path": o.get("path")?.as_str()? }))))
            .collect();
        server.insert("interfaces".into(), Value::Object(map));
    }
}

/// Systems become systems with sites (see [`config`]).
fn sites(v: &mut serde_json::Value) -> Vec<String> {
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
    &s.name
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An older config: each system a site of its own, those its site group
    /// or site locks joined made one system.
    /// An older config's talkgroup files move beside it, one per system once
    /// upgraded (systems joined into one keep the first's); a file already
    /// there stays as it is.
    #[test]
    fn talkgroup_files_move_out_of_the_config() {
        let dir = std::env::temp_dir().join(format!("trunk-migrate-tg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{ "systems": [
            { "shortName": "a", "controlChannelsHz": [1], "siteGroup": "g", "talkgroupsCsv": "1,a" },
            { "shortName": "b", "controlChannelsHz": [2], "siteGroup": "g", "talkgroupsCsv": "2,b" },
            { "shortName": "c", "sites": [{ "controlChannelsHz": [3] }], "talkgroupsCsv": "3,c" },
            { "shortName": "d", "sites": [{ "controlChannelsHz": [4] }], "talkgroupsCsv": "" } ] }"#).unwrap();
        std::fs::write(crate::config::talkgroups_path(&dir, "c"), "kept").unwrap();
        move_talkgroups(&path).unwrap();
        let read = |n: &str| std::fs::read_to_string(crate::config::talkgroups_path(&dir, n)).ok();
        assert_eq!((read("a"), read("b"), read("c"), read("d")), (Some("1,a".into()), None, Some("kept".into()), None));
        let c = Config::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!serde_json::to_string(&c).unwrap().contains("talkgroupsCsv"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `audioArchive` and `compressWav` become a setting for each kind of
    /// file, keeping what was kept: everywhere, and in a system's own
    /// settings by what it set and the rest from the recording settings.
    #[test]
    fn the_audio_kept_becomes_a_wav_and_an_m4a() {
        let c = Config::from_json(
            r#"{ "recording": { "audioArchive": true, "compressWav": true }, "systems": [
                { "shortName": "a", "sites": [{ "controlChannelsHz": [1] }], "recording": { "audioArchive": false, "callLog": false } },
                { "shortName": "b", "sites": [{ "controlChannelsHz": [2] }], "recording": { "compressWav": false } },
                { "shortName": "c", "sites": [{ "controlChannelsHz": [3] }], "recording": { "minCallS": 2 } } ] }"#,
        )
        .unwrap();
        assert_eq!((c.recording.archive_wav, c.recording.archive_m4a), (true, true));
        let own = |i: usize| (c.systems[i].recording.archive_wav, c.systems[i].recording.archive_m4a);
        assert_eq!(own(0), (Some(false), Some(false)), "no audio: neither");
        assert_eq!(c.systems[0].recording.call_log, Some(false));
        assert_eq!(own(1), (Some(true), Some(false)), "the audio, without an M4A");
        assert_eq!(own(2), (None, None), "set nothing: still follows");
        let saved = serde_json::to_string(&c).unwrap();
        assert!(!saved.contains("audioArchive") && !saved.contains("compressWav"));
        // Never set: the defaults, as before (the WAV, no M4A).
        let c = Config::from_json(r#"{ "systems": [] }"#).unwrap();
        assert_eq!((c.recording.archive_wav, c.recording.archive_m4a), (true, false));
        let c = Config::from_json(r#"{ "recording": { "audioArchive": false, "compressWav": true } }"#).unwrap();
        assert_eq!((c.recording.archive_wav, c.recording.archive_m4a), (false, false));
    }

    #[test]
    fn older_systems_become_systems_with_sites() {
        let c = Config::from_json(r#"{ "systems": [{ "shortName": "a", "controlChannelsHz": [851012500], "modulation": "qpsk", "talkgroupsCsv": "x" }] }"#).unwrap();
        assert!(c.recording.drop_duplicate_calls);
        assert_eq!(c.systems.len(), 1);
        let site = &c.systems[0].sites[0];
        assert_eq!((site.name.as_str(), site.control_channels_hz.as_slice(), site.modulation), ("", &[851_012_500.0][..], crate::config::Modulation::Qpsk));
        assert_eq!(c.notes.len(), 2, "{:?}", c.notes);
        assert!(c.notes[1].contains("moved a's there"), "its talkgroup file moves out, and the note says so: {:?}", c.notes);
        assert!(!serde_json::to_string(&c).unwrap().contains("talkgroupsCsv"));
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
        assert!(c.systems[0].sites[0].enabled && !c.systems[0].sites[1].enabled, "each site keeps its own switch");
        assert!(c.notes.iter().any(|n| n.starts_with("p1 and p2 are one system now")), "{:?}", c.notes);
        assert!(c.notes.iter().any(|n| n.contains("cm2's talkgroup file weren't kept: cm1")), "{:?}", c.notes);
        let e = c.engine_config(0.0, &Default::default());
        assert!(!e.drop_duplicates);
        let sites: Vec<_> = trunk_core::trunk::sites(&e.systems).map(|(n, s)| (n, e.systems[n as usize].short_name.as_str(), s.name.as_str())).collect();
        assert_eq!(sites, [(0, "cm1", "cm1"), (1, "p1", "p1"), (1, "p1", "p2"), (2, "p3", "p3")]);
        assert_eq!((c.site_of(2).map(|(s, x)| s.site_name(x)), c.short_name_of(1)), (Some("p2"), Some("p1")));
    }
}
