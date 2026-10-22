//! Plugin routing: which plugins a talkgroup's calls go to. A system's
//! talkgroup file (`<short name>.talkgroups.csv`, trunked or conventional) can
//! say so in a `Send To` column, and a
//! conventional channel in its `sendTo` (its CSV's `Send To`); anything not
//! named there goes to every plugin, as before.
//!
//! It decides for plugins that take finished calls (OpenMHz, Rdio Scanner…).
//! What such a plugin isn't sent: the call when it ends, its start and end,
//! its live audio and unit events on its talkgroup. The archive doesn't wait
//! for that plugin's answer on those calls (it never gets them), so the Keep
//! settings apply as they would with one plugin fewer. A plugin that takes no
//! finished calls (a live audio stream such as simplestream) isn't affected:
//! it filters by its own settings.
//!
//! So one set of radios can feed two services with different lists: the
//! channels both carry are left as they are, and the rest name the one
//! plugin that takes them (`rdioscanner`).

use std::collections::HashMap;

use super::delay::About;
use crate::config::Config;

/// The config's routes, ready to ask.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Routes {
    /// (system, talkgroup) → plugin ids or copy labels; empty: none.
    by_talkgroup: HashMap<(String, u32), Vec<String>>,
}

impl Routes {
    /// From `cfg` and its systems' talkgroup files.
    pub fn of(cfg: &Config, talkgroups: &trunk_app::config::TalkgroupFiles) -> Routes {
        Routes { by_talkgroup: cfg.plugin_routes(talkgroups).into_iter().collect() }
    }

    /// Whether the plugin running as `label` (`openmhz`, `rdioscanner#2`) is
    /// sent what `about` is about.
    pub fn allows(&self, label: &str, about: &About) -> bool {
        let Some(tg) = about.talkgroup else { return true };
        match self.by_talkgroup.get(&(about.system.clone(), tg)) {
            None => true,
            Some(to) => to.iter().any(|p| covers(p, label)),
        }
    }
}

/// A route's `p` names the running copy `label`: the same, or its plugin (`rdioscanner`
/// covers `rdioscanner#2`). Ids are compared as [`crate::config::send_to`] writes them.
fn covers(p: &str, label: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| *c != '-' && *c != '_').collect::<String>().to_ascii_lowercase();
    let (p, label) = (norm(p), norm(label));
    p == label || norm(super::parse_label(&label).0) == p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn routes(v: serde_json::Value) -> Routes {
        Routes::of(&serde_json::from_value(v).unwrap(), &Default::default())
    }

    fn about(system: &str, tg: u32) -> About {
        About { system: system.into(), talkgroup: Some(tg), decrypted: false }
    }

    #[test]
    fn conventional_channels_name_their_plugins() {
        // 154.430 goes everywhere; 155.100 only to Rdio Scanner; 154.250 nowhere.
        let r = routes(json!({ "conventional": [{ "shortName": "dcfire", "channels": [
            { "freqHz": 154430000, "talkgroup": 1 },
            { "freqHz": 155100000, "talkgroup": 2, "sendTo": ["rdioscanner"] },
            { "freqHz": 154250000, "sendTo": [] }
        ] }] }));
        assert!(r.allows("openmhz", &about("dcfire", 1)) && r.allows("rdioscanner", &about("dcfire", 1)));
        assert!(!r.allows("openmhz", &about("dcfire", 2)));
        assert!(r.allows("rdioscanner", &about("dcfire", 2)));
        assert!(r.allows("rdioscanner#2", &about("dcfire", 2)), "a copy goes by its plugin");
        // The channel's default talkgroup: its frequency in kHz.
        assert!(!r.allows("rdioscanner", &about("dcfire", 154250)));
        // Another system's talkgroup 2, and things about no talkgroup, go everywhere.
        assert!(r.allows("openmhz", &about("other", 2)));
        assert!(r.allows("openmhz", &About { system: "dcfire".into(), talkgroup: None, decrypted: false }));
    }

    #[test]
    fn a_talkgroup_files_send_to_column() {
        let csv = "Decimal,Alpha Tag,Send To\n101,Disp,\n102,Tac,Rdio Scanner\n103,Ops,openmhz#2\n104,Data,none\n";
        let cfg = serde_json::from_value(json!({ "systems": [{ "shortName": "ocp25", "type": "p25" }] })).unwrap();
        let r = Routes::of(&cfg, &[("ocp25".to_string(), csv.to_string())].into_iter().collect());
        assert!(r.allows("openmhz", &about("ocp25", 101)));
        assert!(!r.allows("openmhz", &about("ocp25", 102)) && r.allows("rdioscanner", &about("ocp25", 102)));
        assert!(r.allows("openmhz#2", &about("ocp25", 103)) && !r.allows("openmhz", &about("ocp25", 103)), "a copy's label picks only that copy");
        assert!(!r.allows("openmhz", &about("ocp25", 104)) && !r.allows("rdioscanner", &about("ocp25", 104)));
    }
}
