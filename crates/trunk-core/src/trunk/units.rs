//! Each system's radios' talker aliases, as heard over the air — Trunk
//! Recorder's unitTagsOTA file (unit_tags.cc): headerless CSV,
//! `unitID,alias,source,timestamp,WACN,SYS,talkgroup`, the newest line for a
//! unit winning, so the two can share files.

use std::collections::HashMap;
use std::fmt::Write;

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

#[cfg(test)]
mod tests {
    use super::*;

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
