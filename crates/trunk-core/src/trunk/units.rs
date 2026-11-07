//! Each system's radios' talker aliases, as heard over the air — Trunk
//! Recorder's unitTagsOTA file (unit_tags.cc): headerless CSV,
//! `unitID,alias,source,timestamp,WACN,SYS,talkgroup`, the newest line for a
//! unit winning, so the two can share files.

use std::collections::HashMap;
use std::fmt::Write;

use super::names::{stamp, NameRow, Origin};
use super::talkgroups::{csv_lines, split_csv_line};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitAlias {
    pub alias: String,
    /// The decoder that heard it ("MotoP25_FDMA", …).
    pub source: String,
    /// Unix seconds it was (last) learned.
    pub time: i64,
    pub wacn: String,
    pub sys: String,
    pub talkgroup: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct UnitAliases {
    map: HashMap<u32, UnitAlias>,
    /// Learned something since [`UnitAliases::take_changed`].
    changed: bool,
}

fn field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

impl UnitAliases {
    pub fn parse_csv(text: &str) -> Self {
        let mut map: HashMap<u32, UnitAlias> = HashMap::new();
        for line in csv_lines(text) {
            let f = split_csv_line(line);
            let (Some(unit), Some(alias)) = (f.first().and_then(|s| s.parse::<u32>().ok()), f.get(1).filter(|s| !s.is_empty())) else { continue };
            let get = |i: usize| f.get(i).cloned().unwrap_or_default();
            let a = UnitAlias {
                alias: alias.clone(),
                source: get(2),
                time: get(3).parse().unwrap_or(0),
                wacn: get(4),
                sys: get(5),
                talkgroup: get(6).parse().ok(),
            };
            if map.get(&unit).is_none_or(|old| a.time >= old.time) {
                map.insert(unit, a);
            }
        }
        UnitAliases { map, changed: false }
    }

    /// One line per unit, by unit ID.
    pub fn to_csv(&self) -> String {
        let mut units: Vec<_> = self.map.iter().collect();
        units.sort_by_key(|&(&u, _)| u);
        let mut s = String::new();
        for (u, a) in units {
            let tg = a.talkgroup.map_or(String::new(), |t| t.to_string());
            let _ = writeln!(s, "{u},{},{},{},{},{},{tg}", field(&a.alias), field(&a.source), a.time, field(&a.wacn), field(&a.sys));
        }
        s
    }

    /// Each unit's alias, by unit ID.
    pub fn entries(&self) -> Vec<(u32, &UnitAlias)> {
        let mut v: Vec<_> = self.map.iter().map(|(u, a)| (*u, a)).collect();
        v.sort_by_key(|(u, _)| *u);
        v
    }

    /// The aliases `entries` gives.
    pub fn from_entries(entries: impl IntoIterator<Item = (u32, UnitAlias)>) -> Self {
        UnitAliases { map: entries.into_iter().collect(), changed: false }
    }

    pub fn get(&self, unit: u32) -> Option<&str> {
        self.map.get(&unit).map(|a| a.alias.as_str())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Note an alias heard for `unit`; true when it is new or changed.
    pub fn learn(&mut self, unit: u32, a: UnitAlias) -> bool {
        match self.map.get_mut(&unit) {
            Some(old) if old.alias == a.alias => {
                // Same name: fill in what the old line lacked.
                if old.talkgroup.is_none() && a.talkgroup.is_some() {
                    old.talkgroup = a.talkgroup;
                    self.changed = true;
                }
                false
            }
            _ => {
                self.map.insert(unit, a);
                self.changed = true;
                true
            }
        }
    }

    /// Whether anything was learned since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// Which names a unit gets: Trunk Recorder's unitTagsMode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnitTagsMode {
    /// The unit names file first, then the talker aliases heard ("user").
    #[default]
    UserFirst,
    /// The aliases heard first ("ota").
    OtaFirst,
    /// Only the unit names file ("user_only").
    UserOnly,
    /// No names ("none").
    None,
}

impl UnitTagsMode {
    pub fn from_name(s: &str) -> UnitTagsMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "ota" => UnitTagsMode::OtaFirst,
            "user_only" => UnitTagsMode::UserOnly,
            "none" => UnitTagsMode::None,
            _ => UnitTagsMode::UserFirst,
        }
    }
}

/// One of a system's own names for its radios — a line of Trunk Recorder's
/// unitTagsFile (headerless CSV `unit,name`): the radio's unit ID, or a
/// regular expression between slashes whose groups the name can use
/// (`/^1(\d{3})$/,Engine $1` names 1123 "Engine 123"). The first row that
/// matches names it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UnitNameRow {
    pub unit: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

impl UnitNameRow {
    /// A pattern (many radios), not one radio's ID.
    pub fn is_pattern(&self) -> bool {
        self.unit.starts_with('/')
    }
}

impl NameRow for UnitNameRow {
    type Key = String;
    fn key(&self) -> String {
        self.unit.clone()
    }
    fn origin(&self) -> &Option<Origin> {
        &self.origin
    }
    fn origin_mut(&mut self) -> &mut Option<Origin> {
        &mut self.origin
    }
    fn same_names(&self, other: &Self) -> bool {
        (&self.unit, &self.name) == (&other.unit, &other.name)
    }
}

/// A unit names file read ([`read_unit_names`]): its rows, in order, and
/// the patterns that aren't regular expressions (kept; they name nothing).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitNamesFile {
    pub unit_names: Vec<UnitNameRow>,
    pub bad: Vec<String>,
}

/// A unit names file's rows (`#` comments and lines without both a unit and
/// a name passed over). A unit in two rows: the first, as the first match
/// names a radio.
pub fn read_unit_names(text: &str) -> UnitNamesFile {
    let mut out = UnitNamesFile::default();
    let mut seen = std::collections::HashSet::new();
    for line in csv_lines(text).map(str::trim) {
        let f = split_csv_line(line);
        let (Some(unit), Some(name)) = (f.first().map(|s| s.trim()), f.get(1).map(|s| s.trim())) else { continue };
        if unit.is_empty() || name.is_empty() || !seen.insert(unit.to_string()) {
            continue;
        }
        if pattern(unit).is_none() {
            out.bad.push(unit.to_string());
        }
        out.unit_names.push(UnitNameRow { unit: unit.into(), name: name.into(), origin: None });
    }
    out
}

/// `rows` as a unit names file: a `unit,name` line each, in their order.
pub fn unit_names_csv(rows: &[UnitNameRow]) -> String {
    rows.iter().map(|r| format!("{},{}\n", field(&r.unit), field(&r.name))).collect()
}

/// Radio `unit` named `name` among `rows` (blank: its own row taken out, so
/// a pattern may name it). A radio's own row goes before the patterns, so
/// it wins. A change makes `by` its origin. Its row as it is now (none:
/// none of its own).
pub fn edit_unit_names(rows: &mut Vec<UnitNameRow>, unit: u32, name: &str, by: &Origin) -> Option<UnitNameRow> {
    let (key, name) = (unit.to_string(), name.trim());
    let at = rows.iter().position(|r| r.unit == key);
    if name.is_empty() {
        if let Some(i) = at {
            rows.remove(i);
        }
        return None;
    }
    let i = at.unwrap_or_else(|| {
        let i = rows.iter().position(UnitNameRow::is_pattern).unwrap_or(rows.len());
        rows.insert(i, UnitNameRow { unit: key, ..Default::default() });
        i
    });
    let before = at.map(|_| rows[i].clone());
    rows[i].name = name.to_string();
    stamp(&mut rows[i], before.as_ref(), by);
    Some(rows[i].clone())
}

/// A row's unit as a regular expression (none: a pattern that isn't one).
fn pattern(unit: &str) -> Option<regex_lite::Regex> {
    let re = match unit.strip_prefix('/').and_then(|p| p.strip_suffix('/')) {
        Some(r) => r.to_string(),
        None => format!("^{}$", regex_lite::escape(unit)),
    };
    regex_lite::Regex::new(&re).ok()
}

/// A system's own names for its radios ([`UnitNameRow`]s) as the engine
/// looks them up, and which come first ([`UnitTagsMode`]).
#[derive(Clone, Debug, Default)]
pub struct UnitTags {
    tags: Vec<(regex_lite::Regex, String)>,
    pub mode: UnitTagsMode,
}

impl UnitTags {
    /// `rows` to look names up by (a pattern that isn't a regular expression names nothing).
    pub fn new(rows: &[UnitNameRow], mode: UnitTagsMode) -> UnitTags {
        // Trunk Recorder's (boost's) replacements: $1 and \1 alike.
        let tags = rows.iter().filter_map(|r| Some((pattern(&r.unit)?, sed_groups(&r.name)))).collect();
        UnitTags { tags, mode }
    }

    pub fn len(&self) -> usize {
        self.tags.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// The name the rows give `unit`.
    pub fn user(&self, unit: u32) -> Option<String> {
        let id = unit.to_string();
        self.tags.iter().find(|(r, _)| r.is_match(&id)).map(|(r, name)| r.replace(&id, name.as_str()).into_owned())
    }

    /// A unit's name by the mode (Trunk Recorder's `tag` in srcList): the
    /// rows' or the alias heard, whichever comes first. No rows: the alias heard.
    pub fn name(tags: Option<&UnitTags>, heard: Option<&UnitAliases>, unit: u32) -> Option<String> {
        UnitTags::name_heard(tags, heard.and_then(|h| h.get(unit)), unit)
    }

    /// [`UnitTags::name`], `heard` the alias heard for it.
    pub fn name_heard(tags: Option<&UnitTags>, heard: Option<&str>, unit: u32) -> Option<String> {
        let ota = || heard.filter(|h| !h.is_empty()).map(str::to_string);
        let Some(t) = tags else { return ota() };
        let user = || t.user(unit);
        match t.mode {
            UnitTagsMode::UserFirst => user().or_else(ota),
            UnitTagsMode::OtaFirst => ota().or_else(user),
            UnitTagsMode::UserOnly => user(),
            UnitTagsMode::None => None,
        }
    }
}

/// `\1` → `${1}` (and `$1` → `${1}`, so a letter after it isn't read as part of the group's name).
fn sed_groups(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if (c == '\\' || c == '$') && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            let mut n = String::new();
            while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(*d);
                chars.next();
            }
            out.push_str(&format!("${{{n}}}"));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_file() {
        let f = read_unit_names("# names\n1234,Engine 12\n/^7(\\d{3})$/,Medic $1\n/^8(\\d)(\\d)$/,Car \\2-\\1x\n/[/,broken\n1234,Second\n");
        assert_eq!((f.unit_names.len(), f.bad.clone()), (4, vec!["/[/".to_string()]), "a unit twice: the first");
        assert_eq!(read_unit_names(&unit_names_csv(&f.unit_names)), f, "the file written reads back the same");
        let t = UnitTags::new(&f.unit_names, UnitTagsMode::UserFirst);
        assert_eq!(t.len(), 3);
        assert_eq!(t.user(1234).as_deref(), Some("Engine 12"));
        assert_eq!(t.user(12345), None, "a plain number matches only itself");
        assert_eq!(t.user(7042).as_deref(), Some("Medic 042"));
        assert_eq!(t.user(812).as_deref(), Some("Car 2-1x"));
        let heard = UnitAliases::parse_csv("1234,E12 CAPT,,1,,,\n555,HEARD,,1,,,\n");
        assert_eq!(UnitTags::name(Some(&t), Some(&heard), 1234).as_deref(), Some("Engine 12"));
        assert_eq!(UnitTags::name(Some(&t), Some(&heard), 555).as_deref(), Some("HEARD"));
        let ota = UnitTags { mode: UnitTagsMode::OtaFirst, ..t.clone() };
        assert_eq!(UnitTags::name(Some(&ota), Some(&heard), 1234).as_deref(), Some("E12 CAPT"));
        let only = UnitTags { mode: UnitTagsMode::UserOnly, ..t.clone() };
        assert_eq!(UnitTags::name(Some(&only), Some(&heard), 555), None);
        assert_eq!(UnitTags::name(None, Some(&heard), 555).as_deref(), Some("HEARD"));
    }

    /// A radio named on its page: its own row, ahead of the patterns, or
    /// taken out when the name is cleared; the origin changes with the name.
    #[test]
    fn a_radio_named_and_unnamed() {
        let mut rows = read_unit_names("/^7(\\d{3})$/,Medic $1\n").unit_names;
        let by = Origin::edited(50);
        let r = edit_unit_names(&mut rows, 7042, "Bus 14", &by).unwrap();
        assert_eq!((rows[0].unit.as_str(), r.origin.as_ref()), ("7042", Some(&by)));
        assert_eq!(UnitTags::new(&rows, UnitTagsMode::UserFirst).user(7042).as_deref(), Some("Bus 14"), "its own row wins");
        assert_eq!(edit_unit_names(&mut rows, 7042, " Bus 14 ", &Origin::edited(60)).unwrap().origin, Some(by.clone()), "the same name: unchanged");
        assert_eq!(edit_unit_names(&mut rows, 7042, "", &by), None);
        assert_eq!(UnitTags::new(&rows, UnitTagsMode::UserFirst).user(7042).as_deref(), Some("Medic 042"));
        assert_eq!(edit_unit_names(&mut rows, 5, "", &by), None, "nothing to take out");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn newest_wins_and_round_trips() {
        let mut u = UnitAliases::parse_csv("1234,OLD NAME,MotoP25_FDMA,100,bee00,445,\n1234,E12 CAPT,MotoP25_FDMA,200,bee00,445,2207\n99,\"Car, 5\",HarrisP25_TDMA,50,,,\n");
        assert_eq!(u.get(1234), Some("E12 CAPT"));
        assert_eq!(u.get(99), Some("Car, 5"));
        assert!(!u.learn(1234, UnitAlias { alias: "E12 CAPT".into(), ..Default::default() }));
        assert!(!u.take_changed());
        assert!(u.learn(7, UnitAlias { alias: "MEDIC 7".into(), time: 300, ..Default::default() }));
        assert!(u.take_changed());
        let again = UnitAliases::parse_csv(&u.to_csv());
        assert_eq!((again.len(), again.get(99), again.get(7)), (3, Some("Car, 5"), Some("MEDIC 7")));
        assert!(u.to_csv().starts_with("7,MEDIC 7,,300,,,\n99,\"Car, 5\""));
    }
}
