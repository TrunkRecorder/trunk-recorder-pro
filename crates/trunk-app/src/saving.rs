//! What a recording keeps between runs, and when each is due to be written
//! ([`crate::Session::due`]): one rule for every front end, which only does
//! the writing (trunk-pro on a thread of its own, the browser to its storage).
//!
//! Each is kept in memory as it changes and written whole, so how often is
//! a trade between what a crash can lose and how much the disk writes:
//!
//! | Every | What | |
//! |---|---|---|
//! | [`SOON_MS`] | talker aliases, sharing, band plans | small, and wanted back |
//! | [`MINUTE_MS`] | the codes conventional frequencies carried, AutoTune's scores | counted on every call |
//! | [`REGISTRY_MS`] | the radio registry ([`crate::stats::radio`]) | can be megabytes |
//!
//! At the end of a recording everything is due.

use std::collections::HashMap;

use crate::stats::radio::SystemRadio;

/// Talker aliases, sharing, band plans.
pub const SOON_MS: f64 = 10_000.0;
/// The codes conventional frequencies carried, AutoTune's scores.
pub const MINUTE_MS: f64 = 60_000.0;
/// The radio registry (rewritten whole).
pub const REGISTRY_MS: f64 = 15.0 * 60_000.0;

/// Something to keep, due to be written.
#[derive(Debug)]
pub enum Saved {
    /// A system's talker aliases (Trunk Recorder's unitTagsOTA CSV).
    Units { system: String, csv: String },
    /// What sharing with the directory did (each record's version, the last share).
    Sharing(String),
    /// A site's band plan; `key`: [`crate::session::bandplan_key`].
    Bandplan { key: String, plan: String },
    /// The codes conventional frequencies carried ([`crate::heard`]).
    Heard(String),
    /// AutoTune's scores, by source label.
    Tune(String),
    /// A system's radio registry: turned into JSON by whoever writes it (it can be large).
    Registry { system: String, radio: SystemRadio },
}

/// When each kind was last due, and the band plans as last given.
#[derive(Default)]
pub(crate) struct Clock {
    /// The first [`Clock::tick`]: nothing is due until a period has passed.
    started: bool,
    soon_ms: f64,
    minute_ms: f64,
    registry_ms: f64,
    /// [`REGISTRY_MS`] unless set ([`crate::Session::save_registry_every`]).
    pub registry_every_ms: Option<f64>,
    pub plans: HashMap<String, String>,
}

/// Which kinds are due now.
pub(crate) struct Due {
    pub soon: bool,
    pub minute: bool,
    pub registry: bool,
}

impl Clock {
    /// Which kinds are due at `now_ms` (`all`: every one).
    pub fn tick(&mut self, now_ms: f64, all: bool) -> Due {
        if !self.started {
            self.started = true;
            (self.soon_ms, self.minute_ms, self.registry_ms) = (now_ms, now_ms, now_ms);
        }
        let every = self.registry_every_ms.unwrap_or(REGISTRY_MS);
        Due { soon: take(&mut self.soon_ms, SOON_MS, now_ms, all), minute: take(&mut self.minute_ms, MINUTE_MS, now_ms, all), registry: take(&mut self.registry_ms, every, now_ms, all) }
    }
}

/// Due when `every` has passed since `at` (or `all`): then from now.
fn take(at: &mut f64, every: f64, now_ms: f64, all: bool) -> bool {
    let due = all || now_ms - *at >= every;
    if due {
        *at = now_ms;
    }
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_is_due_by_its_period() {
        let mut c = Clock::default();
        let d = c.tick(1000.0, false);
        assert!(!d.soon && !d.minute && !d.registry, "nothing at the start");
        let d = c.tick(1000.0 + SOON_MS, false);
        assert!(d.soon && !d.minute && !d.registry);
        assert!(!c.tick(1000.0 + SOON_MS + 1.0, false).soon, "not again until another period");
        let d = c.tick(1000.0 + MINUTE_MS, false);
        assert!(d.soon && d.minute && !d.registry);
        assert!(c.tick(1000.0 + REGISTRY_MS, false).registry);
        let d = c.tick(1000.0 + REGISTRY_MS + 1.0, true);
        assert!(d.soon && d.minute && d.registry, "everything at the end");
        c.registry_every_ms = Some(60_000.0);
        assert!(c.tick(1000.0 + REGISTRY_MS + 60_001.0, false).registry, "a front end's own period");
    }
}
