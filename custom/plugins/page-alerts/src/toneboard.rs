//! Bringing Tone Board's setup over: its channels (name, frequency, the TRP
//! talkgroup each follows, its Rdio Scanner talkgroup) from its saved
//! settings, and its agency list from its CSV.

use std::path::{Path, PathBuf};

use crate::{Channel, MultiStation};

/// `~` at the start: the home folder.
pub fn expand(p: &str) -> PathBuf {
    let p = p.trim();
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ if p == "~" => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default(),
        _ => PathBuf::from(p),
    }
}

/// `key:"value"` and `key:number` pairs, in order, from Tone Board's app.ron
/// (its settings are a RON string inside it, so quotes come escaped).
fn unescape(text: &str) -> String {
    text.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// `t` without the spaces and line breaks around RON's punctuation (pretty-printed reads as compact).
fn compact(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    while i < c.len() {
        if c[i].is_whitespace() {
            let mut j = i;
            while j < c.len() && c[j].is_whitespace() {
                j += 1;
            }
            let before = out.chars().last().is_none_or(|p| matches!(p, '(' | '[' | ',' | ':'));
            let after = j >= c.len() || matches!(c[j], ')' | ']' | ',' | ':');
            if !(before || after) {
                out.extend(&c[i..j]);
            }
            i = j;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn pairs(text: &str) -> Vec<(String, String)> {
    let t = unescape(text);
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // A key: letters and _ before a ':'.
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if i < b.len() && b[i] == b':' && (s == 0 || !(b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'"')) {
                let key = t[s..i].to_string();
                i += 1;
                // A string holding more settings ("(radios:[…])"): read inside it.
                if i + 1 < b.len() && b[i] == b'"' && matches!(b[i + 1], b'(' | b'[') {
                    i += 1;
                    continue;
                }
                if i < b.len() && b[i] == b'"' {
                    let v0 = i + 1;
                    let mut j = v0;
                    while j < b.len() && b[j] != b'"' {
                        j += 1;
                    }
                    out.push((key, t[v0..j.min(b.len())].to_string()));
                    i = j + 1;
                } else {
                    let v0 = i;
                    let mut j = v0;
                    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'-') {
                        j += 1;
                    }
                    if j > v0 {
                        out.push((key, t[v0..j].to_string()));
                    }
                    i = j.max(i);
                }
                continue;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Tone Board's channels, in its order.
pub fn channels(app_ron: &str) -> Vec<Channel> {
    let mut out: Vec<Channel> = Vec::new();
    // The channels come before the agency list (whose names, units and Multi-Station rows aren't channels').
    let t = unescape(app_ron);
    let head = t.find("agencies:[").map_or(&t[..], |i| &t[..i]);
    for (k, v) in pairs(head) {
        match k.as_str() {
            "freq_hz" => {
                if let Ok(hz) = v.parse::<f64>() {
                    out.push(Channel { name: String::new(), freq_mhz: (hz / 1e6 * 1e6).round() / 1e6, follow: String::new(), talkgroup: None, rdio_system: None, send: false });
                }
            }
            "channel_name" | "name" => {
                if let Some(c) = out.last_mut().filter(|c| c.name.is_empty()) {
                    c.name = v.trim().to_string();
                }
            }
            "follow_talkgroup" => {
                if let Some(c) = out.last_mut().filter(|c| c.follow.is_empty()) {
                    c.follow = v.trim().to_string();
                }
            }
            "talkgroup" => {
                if let Some(c) = out.last_mut().filter(|c| c.talkgroup.is_none()) {
                    c.talkgroup = v.trim().parse().ok();
                }
            }
            // The agency list starts: no more channels.
            "agencies" | "agencies_path" if !out.is_empty() => {}
            _ => {}
        }
    }
    // Channels that follow nothing in TRP (OpenMHz pages, a radio of its own) can't be matched here.
    out.retain(|c| c.freq_mhz > 0.0);
    out
}

/// The quoted string starting at `t[i]` (`"…"`), and where it ends.
fn quoted(t: &str, i: usize) -> Option<(String, usize)> {
    let rest = t.get(i..)?.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((rest[..end].to_string(), i + end + 2))
}

/// The value after `key:` inside `entry` (a quoted string, or a bare word / number).
fn field(entry: &str, key: &str) -> Option<String> {
    let at = entry.find(&format!("{key}:"))? + key.len() + 1;
    if entry[at..].starts_with('"') {
        return quoted(entry, at).map(|(v, _)| v);
    }
    let rest = &entry[at..];
    let end = rest.find([',', ')', ']', '\n']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// Tone Board's Multi-Station rows: `multi_stations:[(name:"…",agencies:["…","…","",""],talkgroup:"…",alert:true,sound:"beep"),…]`.
pub fn multi_stations(app_ron: &str) -> Vec<MultiStation> {
    let t = compact(&unescape(app_ron));
    let Some(start) = t.find("multi_stations:[") else { return Vec::new() };
    let mut i = start + "multi_stations:[".len();
    let b = t.as_bytes();
    let mut out = Vec::new();
    while i < b.len() && b[i] == b'(' {
        // The row runs to its closing ')' (strings may hold brackets).
        let mut j = i + 1;
        let mut q = false;
        while j < b.len() && (q || b[j] != b')') {
            if b[j] == b'"' {
                q = !q;
            }
            j += 1;
        }
        let e = &t[i..j.min(b.len())];
        let mut agencies = Vec::new();
        if let Some(a) = e.find("agencies:[") {
            let mut k = a + "agencies:[".len();
            while let Some((v, next)) = quoted(e, k) {
                if !v.trim().is_empty() {
                    agencies.push(v.trim().to_string());
                }
                k = next;
                if e[k..].starts_with(',') {
                    k += 1;
                } else {
                    break;
                }
            }
        }
        out.push(MultiStation {
            name: field(e, "name").unwrap_or_default(),
            agencies,
            talkgroup: field(e, "talkgroup").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
            alert: field(e, "alert").as_deref() == Some("true"),
            sound: field(e, "sound").unwrap_or_default(),
        });
        i = j + 1;
        if i < b.len() && b[i] == b',' {
            i += 1;
        }
    }
    out
}

/// Tone Board's Rdio Scanner choices: each agency its own talkgroup, stacked pages with patches, the Multi-Station system.
pub fn rdio_choices(app_ron: &str) -> (Option<bool>, Option<bool>, Option<u32>) {
    let t = compact(&unescape(app_ron));
    let b = |k: &str| field(&t, k).and_then(|v| v.parse::<bool>().ok());
    (b("agency_talkgroups"), b("patches"), field(&t, "multi_system").and_then(|v| v.trim().parse().ok()))
}

/// Where Tone Board keeps its agency list (`agencies_path` in app.ron).
pub fn agencies_path(app_ron: &str) -> Option<PathBuf> {
    // (Stored as a quoted string inside a string: "\"/home/…/agencies.csv\"".)
    let at = app_ron.find("agencies_path")? + "agencies_path".len();
    let rest = app_ron[at..].trim_start_matches([':', ' ', '"', '\\']);
    let end = rest.find(['"', '\\', '\n']).unwrap_or(rest.len());
    Some(PathBuf::from(&rest[..end])).filter(|p| !p.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_its_channels() {
        let ron = r#"(window:(),settings:"(radios:[(freq_hz:453900000.0,channel_name:\"DC 911\",talkgroup:\"\",follow_talkgroup:\"UHF/1\",relay_talkgroup:\"\",extra_channels:[(freq_hz:453650000.0,name:\"Poughkeepsie FD Dispatch\",talkgroup:\"\",follow_talkgroup:\"UHF/31\",relay_talkgroup:\"\"),(freq_hz:155440000.0,name:\"City of Pok Patrol 1\",talkgroup:\"149112\",follow_talkgroup:\"NYCOMCO/193\",relay_talkgroup:\"\")])],agencies:[(name:\"Countywide Disaster Call\",tone_a:950.0)])",agencies_path:"\"/home/rdioa/Downloads/agencies.csv\"")"#;
        let c = channels(ron);
        assert_eq!(c.len(), 3, "{c:?}");
        assert_eq!((c[0].name.as_str(), c[0].freq_mhz, c[0].follow.as_str(), c[0].talkgroup), ("DC 911", 453.9, "UHF/1", None));
        assert_eq!((c[1].name.as_str(), c[1].follow.as_str()), ("Poughkeepsie FD Dispatch", "UHF/31"));
        assert_eq!((c[2].follow.as_str(), c[2].talkgroup), ("NYCOMCO/193", Some(149112)));
        assert_eq!(agencies_path(ron), Some(PathBuf::from("/home/rdioa/Downloads/agencies.csv")));
    }

    #[test]
    fn reads_its_multi_station_rows_and_rdio_choices() {
        let ron = r#"(settings:"(radios:[(freq_hz:453900000.0,channel_name:\"DC 911\")],agencies:[(name:\"Chelsea FD All\",unit:\"35\")],multi_stations:[(name:\"Chelsea + FDVA CP\",agencies:[\"Chelsea FD All\",\"FDVA Castle Point\",\"\",\"\"],talkgroup:\"1413591\",alert:true,sound:\"beep\"),(name:\"Hughsonville + New Hackensak\",agencies:[\"Hughsonville FD Fire\",\"New Hackensack FD Fire\",\"\",\"\"],talkgroup:\"1413552\",alert:false,sound:\"\")],rdio:(agency_talkgroups:true,patches:false,multi_system:\"141\",multi_system_label:\"Multi-Station\"))")"#;
        let m = multi_stations(ron);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!((m[0].name.as_str(), m[0].agencies.clone(), m[0].talkgroup, m[0].alert, m[0].sound.as_str()), ("Chelsea + FDVA CP", vec!["Chelsea FD All".to_string(), "FDVA Castle Point".into()], 1413591, true, "beep"));
        assert_eq!((m[1].talkgroup, m[1].alert), (1413552, false));
        assert_eq!(rdio_choices(ron), (Some(true), Some(false), Some(141)));
        assert_eq!(channels(ron).len(), 1, "the agencies' and rows' names aren't channels");
        // Pretty-printed, and talkgroups as numbers.
        let pretty = "(\n  multi_stations: [\n    (\n      name: \"Chelsea + FDVA CP\",\n      agencies: [\"Chelsea FD All\", \"FDVA Castle Point\", \"\", \"\"],\n      talkgroup: 1413591,\n      alert: true,\n      sound: \"beep\",\n    ),\n  ],\n)";
        let m = multi_stations(pretty);
        assert_eq!((m.len(), m[0].agencies.len(), m[0].talkgroup, m[0].name.as_str()), (1, 2, 1413591, "Chelsea + FDVA CP"), "{m:?}");
    }
}
