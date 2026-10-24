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

use serde::{Deserialize, Deserializer};
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

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    /// Allowed (matches an allow pattern, when there are any) and not denied.
    pub fn passes(&self, talkgroup: u32) -> bool {
        (self.allow.is_empty() || self.allow.iter().any(|p| talkgroup_matches(p, talkgroup))) && !self.deny.iter().any(|p| talkgroup_matches(p, talkgroup))
    }

    /// "allow 507*, 12???; deny 50799" — for the log.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.allow.is_empty() {
            parts.push(format!("allow {}", self.allow.join(", ")));
        }
        if !self.deny.is_empty() {
            parts.push(format!("deny {}", self.deny.join(", ")));
        }
        parts.join("; ")
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
        assert_eq!(f.describe(), "allow 507*, 12???; deny 507?9");
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
