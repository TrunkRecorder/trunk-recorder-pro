//! Plugin delays: how long a plugin waits for what's sent to it about a call
//! (the config's `pluginDelays`, and each system's). This decides; the
//! [`super::hold`] keeps things until then.
//!
//! A rule picks what it's for by its fields: the system it's in (or every
//! system, at the top of the config), `plugins` and `talkgroups`, each left
//! out picking everything. Of the rules that pick something:
//!
//! 1. one that narrows another replaces it: it picks only what the other
//!    does, and isn't the same (a system's narrows every system's; one for
//!    some of a system's talkgroups narrows the system's; one for a plugin
//!    narrows one for every plugin);
//! 2. of those left, the longest wins.
//!
//! So a sensitive system's delay isn't cut short by a plugin's own, and a
//! system can still change a plugin's for itself:
//!
//! | Rules | OpenMHz waits |
//! |---|---|
//! | every system: OpenMHz 2 min; dcfd: 30 min | 30 min on dcfd (neither narrows the other) |
//! | every system: OpenMHz 10 min; dcfd: OpenMHz 0 | none on dcfd |
//! | dcfd: talkgroups 1000-1099 30 min; dcfd: OpenMHz 0 | 30 min for 1000–1099, else none |
//! | dcfd: 10 min; dcfd: talkgroup 1001 0 | none for 1001 |
//! | every system: OpenMHz 10 min; dcfd: talkgroup 1001 0 | 10 min (the talkgroup's rule isn't OpenMHz's) |
//!
//! Another way to pick (a talkgroup's tag, emergencies, the hour) is a field
//! of [`About`], one of [`Rule`], and its part of `picks` and `within`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use trunk_recorder_plugin::filter::glob;

use crate::config::{Config, DelayRule};

/// What something sent to the plugins is about: what the rules pick by.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct About {
    /// Its system's short name (empty: no one system's, as the recorder's
    /// status: never delayed).
    pub system: String,
    pub talkgroup: Option<u32>,
    /// Some of it was decrypted with our keys (only plugins with
    /// `shareDecrypted` may have it, then or later).
    pub decrypted: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    /// None: every system's.
    system: Option<String>,
    /// Plugin ids or copies' labels (`openmhz#2`); empty: all.
    plugins: Vec<String>,
    /// Patterns; empty: all.
    talkgroups: Vec<String>,
    delay: Duration,
}

impl Rule {
    fn new(system: Option<&str>, r: &DelayRule) -> Rule {
        let clean = |v: &[String]| -> Vec<String> {
            let mut v: Vec<String> = v.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            v.sort();
            v.dedup();
            v
        };
        let delay = if r.delay_s.is_finite() && r.delay_s > 0.0 { Duration::from_secs_f64(r.delay_s) } else { Duration::ZERO };
        Rule { system: system.map(str::to_string), plugins: clean(&r.plugins), talkgroups: clean(&r.talkgroups), delay }
    }

    fn picks(&self, label: &str, about: &About) -> bool {
        self.system.as_ref().is_none_or(|s| *s == about.system)
            && (self.plugins.is_empty() || self.plugins.iter().any(|p| covers(p, label)))
            && (self.talkgroups.is_empty() || about.talkgroup.is_some_and(|tg| self.talkgroups.iter().any(|p| talkgroup_matches(p, tg))))
    }

    /// It picks only what `other` picks (as written: a pattern is only within
    /// another list when that list has it too).
    fn within(&self, other: &Rule) -> bool {
        (other.system.is_none() || self.system == other.system)
            && (other.plugins.is_empty() || (!self.plugins.is_empty() && self.plugins.iter().all(|p| other.plugins.iter().any(|o| covers(o, p)))))
            && (other.talkgroups.is_empty() || (!self.talkgroups.is_empty() && self.talkgroups.iter().all(|t| other.talkgroups.contains(t))))
    }

    fn narrows(&self, other: &Rule) -> bool {
        self.within(other) && !other.within(self)
    }
}

/// A rule's plugin `p` (an id, or one copy's label) covers `label`, a running
/// copy's: the same, or its plugin.
fn covers(p: &str, label: &str) -> bool {
    p == label || super::parse_label(label).0 == p
}

/// `1000-1099` (a range), or a pattern: `507*`, `12?45`, `8001`.
fn talkgroup_matches(p: &str, tg: u32) -> bool {
    if let Some((a, b)) = p.split_once('-') {
        if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
            return (a.min(b)..=a.max(b)).contains(&tg);
        }
    }
    glob(p, &tg.to_string())
}

/// The config's delay rules, ready to ask.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Delays {
    rules: Vec<Rule>,
}

impl Delays {
    pub fn of(cfg: &Config) -> Delays {
        let mut rules: Vec<Rule> = cfg.plugin_delays.iter().map(|r| Rule::new(None, r)).collect();
        for s in &cfg.systems {
            rules.extend(s.plugin_delays.iter().map(|r| Rule::new(Some(&s.short_name), r)));
        }
        for c in &cfg.conventional {
            rules.extend(c.plugin_delays.iter().map(|r| Rule::new(Some(&c.short_name), r)));
        }
        Delays { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.iter().all(|r| r.delay.is_zero())
    }

    /// The longest any rule asks for (what may be waiting, at most).
    pub fn longest(&self) -> Duration {
        self.rules.iter().map(|r| r.delay).max().unwrap_or_default()
    }

    /// How long plugin `label` (its id; a copy's `id#2`) waits for something about `about`.
    pub fn delay(&self, label: &str, about: &About) -> Duration {
        if about.system.is_empty() {
            return Duration::ZERO;
        }
        let picked: Vec<&Rule> = self.rules.iter().filter(|r| r.picks(label, about)).collect();
        picked.iter().filter(|r| !picked.iter().any(|o| o.narrows(r))).map(|r| r.delay).max().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MIN: u64 = 60;

    fn delays(v: serde_json::Value) -> Delays {
        Delays::of(&serde_json::from_value(v).unwrap())
    }
    fn on(system: &str, tg: Option<u32>) -> About {
        About { system: system.into(), talkgroup: tg, decrypted: false }
    }
    fn mins(d: &Delays, plugin: &str, about: &About) -> u64 {
        d.delay(plugin, about).as_secs() / MIN
    }

    /// The examples in the module's documentation.
    #[test]
    fn the_documented_cases() {
        let d = delays(json!({
            "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 2 * MIN }],
            "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "delayS": 30 * MIN }] }]
        }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1))), 30, "a sensitive system isn't cut short by a plugin's delay");
        assert_eq!(mins(&d, "openmhz", &on("wmata", Some(1))), 2);
        assert_eq!(mins(&d, "rdioscanner", &on("wmata", Some(1))), 0);

        let d = delays(json!({
            "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 10 * MIN }],
            "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 0 }] }]
        }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1))), 0, "a system changes a plugin's delay for itself");
        assert_eq!(mins(&d, "openmhz", &on("wmata", Some(1))), 10);

        let d = delays(json!({ "systems": [{ "shortName": "dcfd", "pluginDelays": [
            { "talkgroups": ["1000-1099"], "delayS": 30 * MIN }, { "plugins": ["openmhz"], "delayS": 0 }] }] }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1050))), 30);
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(2000))), 0);

        let d = delays(json!({ "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "delayS": 10 * MIN }, { "talkgroups": [1001], "delayS": 0 }] }] }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1001))), 0, "a talkgroup let through");
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1002))), 10);
        assert_eq!(mins(&d, "openmhz", &on("dcfd", None)), 10, "a unit's event with no talkgroup is the system's");

        let d = delays(json!({
            "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 10 * MIN }],
            "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "talkgroups": [1001], "delayS": 0 }] }]
        }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1001))), 10, "the talkgroup's rule isn't OpenMHz's");
    }

    /// The narrowest rule for a talkgroup and a plugin together: a private
    /// plugin let have a sensitive talkgroup at once, the others waiting.
    #[test]
    fn a_talkgroup_for_one_plugin() {
        let d = delays(json!({ "systems": [{ "shortName": "dcfd", "pluginDelays": [
            { "talkgroups": ["1001", "1002"], "delayS": 30 * MIN },
            { "talkgroups": ["1001"], "plugins": ["rdioscanner"], "delayS": 0 }] }] }));
        assert_eq!(mins(&d, "rdioscanner", &on("dcfd", Some(1001))), 0);
        assert_eq!(mins(&d, "rdioscanner", &on("dcfd", Some(1002))), 30, "1002 isn't in the narrower rule");
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1001))), 30);
    }

    /// A copy of a plugin: picked by its plugin's rules, and its own narrow them.
    #[test]
    fn copies_of_a_plugin() {
        let d = delays(json!({ "pluginDelays": [{ "plugins": ["openmhz"], "delayS": 5 * MIN }, { "plugins": ["openmhz#2"], "delayS": 0 }] }));
        assert_eq!(mins(&d, "openmhz", &on("dcfd", Some(1))), 5);
        assert_eq!(mins(&d, "openmhz#2", &on("dcfd", Some(1))), 0);
        assert_eq!(mins(&d, "openmhz#3", &on("dcfd", Some(1))), 5);
    }

    /// The same rule twice, or two that pick the same: the longer.
    #[test]
    fn equals_give_the_longer() {
        let d = delays(json!({ "systems": [{ "shortName": "dcfd", "pluginDelays": [{ "delayS": 60 }, { "delayS": 120 }, { "talkgroups": ["5*"], "delayS": 30 }, { "talkgroups": ["5*"], "delayS": 90 }] }] }));
        assert_eq!(d.delay("x", &on("dcfd", Some(1))).as_secs(), 120);
        assert_eq!(d.delay("x", &on("dcfd", Some(51))).as_secs(), 90);
        assert_eq!(d.longest().as_secs(), 120);
    }

    #[test]
    fn what_is_never_delayed() {
        let d = delays(json!({ "pluginDelays": [{ "delayS": 600 }] }));
        assert_eq!(d.delay("x", &on("", None)), Duration::ZERO, "the recorder's status is no system's");
        assert!(delays(json!({})).is_empty());
        assert!(delays(json!({ "pluginDelays": [{ "delayS": 0 }, { "delayS": -5 }] })).is_empty());
        assert!(!d.is_empty());
    }

    #[test]
    fn talkgroup_patterns() {
        assert!(talkgroup_matches("1000-1099", 1000) && talkgroup_matches("1000-1099", 1099) && !talkgroup_matches("1000-1099", 1100));
        assert!(talkgroup_matches("1099 - 1000", 1050), "either way round");
        assert!(talkgroup_matches("507*", 50712) && !talkgroup_matches("507*", 5170));
        assert!(talkgroup_matches("8001", 8001) && !talkgroup_matches("8001", 80011));
    }
}
