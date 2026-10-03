//! Each system's radios' talker aliases, as heard over the air — Trunk
//! Recorder's unitTagsOTA file (unit_tags.cc): headerless CSV,
//! `unitID,alias,source,timestamp,WACN,SYS,talkgroup`, the newest line for a
//! unit winning, so the two can share files.

use std::collections::HashMap;
use std::fmt::Write;

use super::calls::conventional_index;
use super::talkgroups::split_csv_line;

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
        for line in text.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
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
        units.sort_by_key(|(&u, _)| u);
        let mut s = String::new();
        for (u, a) in units {
            let tg = a.talkgroup.map_or(String::new(), |t| t.to_string());
            let _ = writeln!(s, "{u},{},{},{},{},{},{tg}", field(&a.alias), field(&a.source), a.time, field(&a.wacn), field(&a.sys));
        }
        s
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

/// Every system's talker aliases: looked up by the number a call carries
/// (a trunked system's, or a conventional system's), kept between runs
/// under each system's short name.
#[derive(Clone, Debug, Default)]
pub struct AliasBook {
    trunked: Vec<(String, UnitAliases)>,
    /// Conventional system k's, and whether it has channels (only those keep a file).
    conventional: Vec<(String, bool, UnitAliases)>,
}

impl AliasBook {
    /// `trunked`: each trunked system's short name, in the engine's order;
    /// `conventional`: each conventional system's, and whether it has channels.
    pub fn new(trunked: impl IntoIterator<Item = String>, conventional: impl IntoIterator<Item = (String, bool)>) -> Self {
        AliasBook {
            trunked: trunked.into_iter().map(|n| (n, UnitAliases::default())).collect(),
            conventional: conventional.into_iter().map(|(n, used)| (n, used, UnitAliases::default())).collect(),
        }
    }

    /// The table of system `system` (a call's).
    pub fn of(&self, system: u16) -> Option<&UnitAliases> {
        match conventional_index(system) {
            Some(k) => self.conventional.get(k).map(|c| &c.2),
            None => self.trunked.get(system as usize).map(|t| &t.1),
        }
    }

    /// A radio's alias on system `system`.
    pub fn get(&self, system: u16, unit: u32) -> Option<&str> {
        self.of(system)?.get(unit)
    }

    /// Note an alias heard on system `system`; true when it's new (or changed).
    pub fn learn(&mut self, system: u16, unit: u32, a: UnitAlias) -> bool {
        let table = match conventional_index(system) {
            Some(k) => self.conventional.get_mut(k).map(|c| &mut c.2),
            None => self.trunked.get_mut(system as usize).map(|t| &mut t.1),
        };
        table.is_some_and(|t| t.learn(unit, a))
    }

    /// The short names that keep a table: each trunked system's, and each
    /// conventional system's that has channels.
    pub fn names(&self) -> Vec<String> {
        self.trunked.iter().map(|t| t.0.clone()).chain(self.conventional.iter().filter(|c| c.1).map(|c| c.0.clone())).collect()
    }

    /// Preload the table kept under `short_name` (Trunk Recorder's unitTagsOTA CSV).
    pub fn load(&mut self, short_name: &str, csv: &str) {
        if let Some(t) = self.trunked.iter_mut().find(|t| t.0 == short_name) {
            t.1 = UnitAliases::parse_csv(csv);
        } else if let Some(c) = self.conventional.iter_mut().find(|c| c.0 == short_name) {
            c.2 = UnitAliases::parse_csv(csv);
        }
    }

    /// (short name, CSV) of each table that learned something since the last call.
    pub fn changed(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, t) in self.trunked.iter_mut().map(|t| (&t.0, &mut t.1)).chain(self.conventional.iter_mut().map(|c| (&c.0, &mut c.2))) {
            if t.take_changed() {
                out.push((name.clone(), t.to_csv()));
            }
        }
        out
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

/// A system's own names for its radios — Trunk Recorder's unitTagsFile:
/// headerless CSV `unit,name`, `#` comments. The unit is a number, or a
/// regular expression between slashes whose groups the name can use:
/// `/^1(\d{3})$/,Engine $1` names 1123 "Engine 123". The first match wins.
#[derive(Clone, Debug, Default)]
pub struct UnitTags {
    tags: Vec<(regex_lite::Regex, String)>,
    pub mode: UnitTagsMode,
}

impl UnitTags {
    /// The names in `text`, and the patterns that aren't regular expressions.
    pub fn parse_csv(text: &str, mode: UnitTagsMode) -> (UnitTags, Vec<String>) {
        let mut tags = Vec::new();
        let mut bad = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let f = split_csv_line(line);
            let (Some(pat), Some(name)) = (f.first().map(|s| s.trim()), f.get(1).map(|s| s.trim())) else { continue };
            if pat.is_empty() || name.is_empty() {
                continue;
            }
            let re = match pat.strip_prefix('/').and_then(|p| p.strip_suffix('/')) {
                Some(r) => r.to_string(),
                None => format!("^{}$", regex_lite::escape(pat)),
            };
            match regex_lite::Regex::new(&re) {
                // Trunk Recorder's (boost's) replacements: $1 and \1 alike.
                Ok(r) => tags.push((r, sed_groups(name))),
                Err(_) => bad.push(pat.to_string()),
            }
        }
        (UnitTags { tags, mode }, bad)
    }

    pub fn len(&self) -> usize {
        self.tags.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// The name the file gives `unit`.
    pub fn user(&self, unit: u32) -> Option<String> {
        let id = unit.to_string();
        self.tags.iter().find(|(r, _)| r.is_match(&id)).map(|(r, name)| r.replace(&id, name.as_str()).into_owned())
    }

    /// A unit's name by the mode (Trunk Recorder's `tag` in srcList): the
    /// file's or the alias heard, whichever comes first. No unit names
    /// file: the alias heard.
    pub fn name(tags: Option<&UnitTags>, heard: Option<&UnitAliases>, unit: u32) -> Option<String> {
        let ota = || heard.and_then(|h| h.get(unit)).map(str::to_string);
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
        let (t, bad) = UnitTags::parse_csv("# names\n1234,Engine 12\n/^7(\\d{3})$/,Medic $1\n/^8(\\d)(\\d)$/,Car \\2-\\1x\n/[/,broken\n", UnitTagsMode::UserFirst);
        assert_eq!((t.len(), bad), (3, vec!["/[/".to_string()]));
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
