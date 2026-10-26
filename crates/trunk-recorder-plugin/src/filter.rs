//! Talkgroup allow and deny lists, as Trunk Recorder's uploaders take them:
//! patterns matched against the talkgroup number written out (`"507*"`,
//! `"12?45"`, `"8001"`), where `*` is any run of characters and `?` any one,
//! or ranges (`"1000-1099"`). The recorder's own talkgroup lists (delay
//! rules, signals) are read the same way: [`talkgroup_matches`].
//!
//! ```
//! # use trunk_recorder_plugin::TalkgroupFilter;
//! let f = TalkgroupFilter::new(&["507*".into()], &["50799".into()]);
//! assert!(f.passes(50712));
//! assert!(!f.passes(50799)); // denied
//! assert!(!f.passes(101)); // not allowed
//! ```
//!
//! A plugin that lets each system pick its talkgroups flattens [`Talkgroups`]
//! into its system settings: a menu (every talkgroup, only some, all but
//! some) and the lists that apply, checked as they're read
//! ([`Talkgroups::filter`]).

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct TalkgroupFilter {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl TalkgroupFilter {
    /// Empty patterns are ignored; an empty allow list allows everything.
    pub fn new(allow: &[String], deny: &[String]) -> TalkgroupFilter {
        let clean = |v: &[String]| v.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        TalkgroupFilter { allow: clean(allow), deny: clean(deny) }
    }

    /// As [`new`](Self::new), the lists read as typed ([`talkgroup_list`]):
    /// an entry that isn't a talkgroup is an error, naming it.
    pub fn parse(allow: &[String], deny: &[String]) -> Result<TalkgroupFilter, String> {
        Ok(TalkgroupFilter { allow: talkgroup_list(allow)?, deny: talkgroup_list(deny)? })
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    /// Allowed (matches an allow pattern, when there are any) and not denied.
    pub fn passes(&self, talkgroup: u32) -> bool {
        (self.allow.is_empty() || self.allow.iter().any(|p| talkgroup_matches(p, talkgroup))) && !self.deny.iter().any(|p| talkgroup_matches(p, talkgroup))
    }

    /// "only 507*, 12???; except 50799", "all but 50799", "" (every
    /// talkgroup) — for the log, in the form's words.
    pub fn describe(&self) -> String {
        match (self.allow.is_empty(), self.deny.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!("all but {}", self.deny.join(", ")),
            (false, true) => format!("only {}", self.allow.join(", ")),
            (false, false) => format!("only {}; except {}", self.allow.join(", "), self.deny.join(", ")),
        }
    }
}

/// A talkgroup list's entry names `tg`: a range (`1000-1099`, either way
/// round), or a pattern (`507*`, `12?45`, `8001`).
pub fn talkgroup_matches(entry: &str, tg: u32) -> bool {
    let e = entry.trim();
    if let Some((a, b)) = e.split_once('-')
        && let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>())
    {
        return (a.min(b)..=a.max(b)).contains(&tg);
    }
    glob(e, &tg.to_string())
}

/// A talkgroup list as typed, one entry per talkgroup, range or pattern:
/// spaces, commas and semicolons separate them (`"101 102"` is two), and a
/// range may have spaces around its dash (`"1000 - 1099"`). An entry that is
/// none of those is an error, naming it ([`check_talkgroup`]).
pub fn talkgroup_list(entries: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for e in entries {
        // Trimming each side of every dash joins a range's parts up.
        let joined = e.split('-').map(str::trim).collect::<Vec<_>>().join("-");
        for x in joined.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|x| !x.is_empty()) {
            check_talkgroup(x)?;
            out.push(x.to_string());
        }
    }
    Ok(out)
}

/// One entry of a talkgroup list is a talkgroup (`8001`), a range
/// (`1000-1099`) or a pattern (`507*`, `12?45`); else what to type instead.
pub fn check_talkgroup(entry: &str) -> Result<(), String> {
    let e = entry.trim();
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok();
    let ok = match e.split_once('-') {
        Some((a, b)) => number(a.trim()) && number(b.trim()),
        None if e.contains(['*', '?']) => e.bytes().all(|b| b.is_ascii_digit() || b == b'*' || b == b'?'),
        None => number(e),
    };
    match ok {
        true => Ok(()),
        false => Err(format!("\"{e}\" isn't a talkgroup: give numbers, ranges like 1000-1099, or patterns like 507* (* for any digits, ? for one)")),
    }
}

/// Which talkgroups a system's settings pick: a plugin's own menu and lists,
/// for a `#[serde(flatten)]` field of its `SystemConfig`. The lists keep the
/// names plugins have always stored them under (`talkgroupAllow`,
/// `talkgroupDeny`), and read Trunk Recorder's (`broadcastifyAllow`, …).
///
/// ```
/// # use trunk_recorder_plugin::filter::Talkgroups;
/// let t: Talkgroups = serde_json::from_value(serde_json::json!({
///     "talkgroupMode": "only", "talkgroupAllow": ["1000-1099", "507*"], "talkgroupDeny": [1050]
/// })).unwrap();
/// let f = t.filter().unwrap();
/// assert!(f.passes(1001) && f.passes(50712) && !f.passes(1050) && !f.passes(2000));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct Talkgroups {
    /// Talkgroups
    ///
    /// Which of this system's talkgroups the plugin takes: every one, only some, or all but some.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talkgroup_mode: Option<TalkgroupMode>,
    /// Only these
    ///
    /// Numbers, ranges like 1000-1099, and patterns like 507* (* stands for any digits, ? for one).
    #[serde(
        alias = "broadcastifyAllow",
        alias = "broadcastifyWhitelist",
        alias = "talkgroupWhitelist",
        deserialize_with = "patterns",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "sdk", schemars(with = "Vec<String>", extend("x-when" = {"talkgroupMode": "only"}, "x-talkgroups" = true)))]
    pub talkgroup_allow: Vec<String>,
    /// Except these
    ///
    /// Numbers, ranges and patterns, as above.
    #[serde(
        alias = "broadcastifyDeny",
        alias = "broadcastifyBlacklist",
        alias = "talkgroupBlacklist",
        deserialize_with = "patterns",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "sdk", schemars(with = "Vec<String>", extend("x-when" = {"talkgroupMode": ["only", "allBut"]}, "x-talkgroups" = true)))]
    pub talkgroup_deny: Vec<String>,
}

/// [`Talkgroups`]' menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sdk", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TalkgroupMode {
    /// Every talkgroup
    #[default]
    All,
    /// Only some
    Only,
    /// All but some
    AllBut,
}

impl Talkgroups {
    /// The menu's choice; settings from before it (lists alone) are as their lists say.
    pub fn mode(&self) -> TalkgroupMode {
        self.talkgroup_mode.unwrap_or(match (self.talkgroup_allow.is_empty(), self.talkgroup_deny.is_empty()) {
            (false, _) => TalkgroupMode::Only,
            (true, false) => TalkgroupMode::AllBut,
            (true, true) => TalkgroupMode::All,
        })
    }

    /// Lists from before the menu, with no choice made in it: they still
    /// apply, but the form shows "Every talkgroup" until one is.
    pub fn from_lists_alone(&self) -> bool {
        self.talkgroup_mode.is_none() && self.mode() != TalkgroupMode::All
    }

    /// The filter the choice comes to. A list that doesn't apply to it is
    /// ignored (it's kept, hidden, in case it's chosen again); an entry that
    /// isn't a talkgroup, or "Only some" with none listed, is an error.
    pub fn filter(&self) -> Result<TalkgroupFilter, String> {
        match self.mode() {
            TalkgroupMode::All => Ok(TalkgroupFilter::default()),
            TalkgroupMode::Only => {
                let f = TalkgroupFilter::parse(&self.talkgroup_allow, &self.talkgroup_deny)?;
                if f.allow.is_empty() {
                    return Err("list the talkgroups for \"Only some\", or choose \"Every talkgroup\"".into());
                }
                Ok(f)
            }
            TalkgroupMode::AllBut => TalkgroupFilter::parse(&[], &self.talkgroup_deny),
        }
    }

    /// [`filter`](Self::filter), for system `system` as the plugin starts:
    /// an error names the system (return it from `start`), and lists from
    /// before the menu are warned about.
    #[cfg(feature = "sdk")]
    pub fn filter_for(&self, host: &crate::Host, system: &str) -> Result<TalkgroupFilter, String> {
        let f = self.filter().map_err(|e| format!("{system}'s talkgroups: {e}"))?;
        if self.from_lists_alone() {
            host.warn(format!(
                "{system}'s talkgroups are set in older settings ({}): they still apply. Choose Talkgroups on {system}'s card in Setup to see them there",
                f.describe()
            ));
        }
        Ok(f)
    }
}

/// `text` matches all of `pattern`: `*` any run of characters, `?` any one.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut i, mut j) = (0, 0);
    // The last `*` seen, and where in the text it started matching.
    let mut star: Option<(usize, usize)> = None;
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            // Let that `*` take one more character.
            star = Some((si, sj + 1));
            i = si + 1;
            j = sj + 1;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

/// For `#[serde(deserialize_with = "trunk_recorder_plugin::filter::patterns")]`:
/// a list of patterns, taking numbers as well as strings (Trunk Recorder's
/// configs have both), or one pattern on its own.
pub fn patterns<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let one = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        v => one(&v).into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talkgroup_lists() {
        assert!(talkgroup_matches("1000-1099", 1000) && talkgroup_matches("1000-1099", 1099) && !talkgroup_matches("1000-1099", 1100));
        assert!(talkgroup_matches("1099 - 1000", 1050), "either way round");
        assert!(talkgroup_matches("507*", 50712) && !talkgroup_matches("507*", 5170) && talkgroup_matches("4?1", 401));
        assert!(talkgroup_matches(" 8001 ", 8001) && !talkgroup_matches("8001", 80011));
        assert!(TalkgroupFilter::new(&["400-499".into()], &["450".into()]).passes(401), "filters take ranges too");
    }

    #[test]
    fn lists_as_typed() {
        let typed = |v: &[&str]| talkgroup_list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(typed(&["101 102", " 1000 - 1099 ", "507*;12?45", "", "8001,"]).unwrap(), ["101", "102", "1000-1099", "507*", "12?45", "8001"]);
        assert_eq!(typed(&["101 1000 -1099 102"]).unwrap(), ["101", "1000-1099", "102"]);
        for bad in ["abc", "10-", "-5", "10-20-30", "5a*", "99999999999", "1000-x"] {
            let e = typed(&[bad]).unwrap_err();
            assert!(e.contains(bad.trim()) && e.contains("1000-1099"), "{bad}: {e}");
        }
        assert!(check_talkgroup("*").is_ok() && check_talkgroup("1099-1000").is_ok());
        assert!(TalkgroupFilter::parse(&["400 - 499".into()], &["450 451".into()]).unwrap().passes(452));
        assert!(TalkgroupFilter::parse(&["4o1".into()], &[]).is_err());
    }

    #[test]
    fn talkgroups_settings() {
        let read = |v: Value| serde_json::from_value::<Talkgroups>(v).unwrap();
        let only = read(serde_json::json!({ "talkgroupMode": "only", "talkgroupAllow": ["1000-1099"], "talkgroupDeny": [1050] }));
        let f = only.filter().unwrap();
        assert!(f.passes(1001) && !f.passes(1050) && !f.passes(2000));
        assert!(!only.from_lists_alone());

        let but = read(serde_json::json!({ "talkgroupMode": "allBut", "talkgroupAllow": ["1000-1099"], "talkgroupDeny": ["1050"] }));
        let f = but.filter().unwrap();
        assert!(f.passes(2000) && f.passes(1001) && !f.passes(1050), "the hidden allow list doesn't apply");

        let every = read(serde_json::json!({ "talkgroupMode": "all", "talkgroupAllow": ["1000-1099"], "talkgroupDeny": ["1050"] }));
        assert!(every.filter().unwrap().is_empty(), "chosen: every talkgroup, the lists kept but not applied");

        // Before the menu: the lists alone, under any of their names.
        let old = read(serde_json::json!({ "broadcastifyAllow": ["10*", 205], "talkgroupBlacklist": "109" }));
        assert_eq!(old.mode(), TalkgroupMode::Only);
        assert!(old.from_lists_alone());
        let f = old.filter().unwrap();
        assert!(f.passes(101) && f.passes(205) && !f.passes(109) && !f.passes(300));
        assert_eq!(read(serde_json::json!({ "talkgroupDeny": [9] })).mode(), TalkgroupMode::AllBut);
        assert_eq!(read(serde_json::json!({})), Talkgroups::default());
        assert!(Talkgroups::default().filter().unwrap().is_empty());

        assert!(read(serde_json::json!({ "talkgroupMode": "only" })).filter().unwrap_err().contains("Only some"));
        assert!(read(serde_json::json!({ "talkgroupMode": "allBut", "talkgroupDeny": ["x1"] })).filter().unwrap_err().contains("\"x1\""));
        // Saved back as read: nothing for what's unset.
        assert_eq!(serde_json::to_value(Talkgroups::default()).unwrap(), serde_json::json!({}));
        assert_eq!(serde_json::to_value(&only).unwrap(), serde_json::json!({ "talkgroupMode": "only", "talkgroupAllow": ["1000-1099"], "talkgroupDeny": ["1050"] }));
    }

    /// What a plugin's form gets from it, flattened among its own fields.
    #[cfg(feature = "sdk")]
    #[test]
    fn talkgroups_in_a_form() {
        #[derive(Serialize, Deserialize, schemars::JsonSchema, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct SystemConfig {
            /// API key
            api_key: String,
            #[serde(flatten)]
            talkgroups: Talkgroups,
            /// Sites
            sites: Vec<String>,
        }
        let s = crate::schema::schema_for::<SystemConfig>();
        assert_eq!(s["x-order"], serde_json::json!(["apiKey", "talkgroupMode", "talkgroupAllow", "talkgroupDeny", "sites"]), "{s:#}");
        let p = &s["properties"];
        assert_eq!(p["talkgroupMode"]["enum"], serde_json::json!(["all", "only", "allBut"]));
        assert_eq!(p["talkgroupMode"]["x-enum-labels"], serde_json::json!(["Every talkgroup", "Only some", "All but some"]));
        assert_eq!(p["talkgroupMode"]["title"], "Talkgroups");
        assert_eq!(p["talkgroupAllow"]["title"], "Only these");
        assert_eq!(p["talkgroupAllow"]["x-when"], serde_json::json!({ "talkgroupMode": "only" }));
        assert_eq!(p["talkgroupDeny"]["x-when"], serde_json::json!({ "talkgroupMode": ["only", "allBut"] }));
        assert_eq!(p["talkgroupDeny"]["items"]["type"], "string");
        assert_eq!(p["talkgroupDeny"]["x-talkgroups"], true);
        let c: SystemConfig = serde_json::from_value(serde_json::json!({ "apiKey": "k", "talkgroupAllow": [101], "sites": ["a"] })).unwrap();
        assert_eq!((c.api_key.as_str(), c.talkgroups.mode(), c.sites.len()), ("k", TalkgroupMode::Only, 1));
    }

    #[test]
    fn globs() {
        assert!(glob("507*", "50712"));
        assert!(glob("507*", "507"));
        assert!(!glob("507*", "1507"));
        assert!(glob("12???", "12345"));
        assert!(!glob("12???", "1234"));
        assert!(glob("*9", "50799"));
        assert!(glob("5*7*9", "5077779"));
        assert!(!glob("5*7*9", "50778"));
        assert!(glob("*", ""));
        assert!(glob("8001", "8001"));
        assert!(!glob("8001", "80011"));
    }

    #[test]
    fn allow_then_deny() {
        let f = TalkgroupFilter::new(&["507*".into(), "12???".into(), " ".into()], &["507?9".into()]);
        assert!(f.passes(50712) && f.passes(12345));
        assert!(!f.passes(50709) && !f.passes(999));
        let none = TalkgroupFilter::new(&[], &[]);
        assert!(none.is_empty() && none.passes(1));
        assert_eq!(f.describe(), "only 507*, 12???; except 507?9");
        assert_eq!(TalkgroupFilter::new(&[], &["9".into()]).describe(), "all but 9");
        assert_eq!(none.describe(), "");
    }

    #[test]
    fn patterns_take_numbers_and_strings() {
        #[derive(Deserialize)]
        struct S {
            #[serde(deserialize_with = "patterns")]
            p: Vec<String>,
        }
        let s: S = serde_json::from_str(r#"{"p": ["507*", 12345, null]}"#).unwrap();
        assert_eq!(s.p, ["507*", "12345"]);
        let s: S = serde_json::from_str(r#"{"p": 8001}"#).unwrap();
        assert_eq!(s.p, ["8001"]);
    }
}
