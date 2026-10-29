//! What a call store backend does (research/sqlite-plan.md, "Backends"):
//! keep rows and answer for them. Everything above it is shared: the writer
//! thread's batching and retention timing (`writer.rs`), the live list merged
//! into every query (`runtime::calls`), the paging rules (`read.rs`), and the
//! backfill. A backend is SQLite (`sqlite.rs`) or PostgreSQL (`postgres.rs`);
//! `storage.backend: "files"` is none at all (the store off).

use serde_json::Value;

use super::read::{CallsFilter, Page};
use super::row::CallRow;

/// A change for the store, as the writer hands it over in a batch.
pub enum Change {
    /// A call's row and its children: a live one replaces what is there, a
    /// backfilled one only fills a gap.
    Call { row: Box<CallRow>, live: bool },
    /// Which of a call's files are kept.
    CallFiles { path: String, audio: bool, json: bool },
    /// A `meta` value (the backfill's markers).
    Meta { key: String, value: String },
    /// A document kept between runs, by kind and key (a band plan, a
    /// system's talker aliases, AutoTune's scores…: `crate::kept`); `None`
    /// removes it.
    Kept { kind: String, key: String, value: Option<String> },
    /// A system's radio registry changes since the last ([`StoredRegistry`]), as JSON.
    RegistryChanges { system: String, json: String },
    /// A system's whole radio registry, as JSON, in place of what's stored
    /// (its changes go with it).
    RegistryWhole { system: String, json: String },
}

/// A system's radio registry as stored: the last whole copy (the
/// registry's own JSON, as `files` keeps it) and the changes since, oldest
/// first (each a [`trunk_app::stats::radio::RegistryChanges`] as JSON).
/// Changes are appended, not rows updated in place: a busy system changes
/// thousands of rows every 15 minutes, which as row updates writes more
/// than the whole file did (research/sqlite-plan.md).
pub struct StoredRegistry {
    pub system: String,
    pub whole: Option<String>,
    pub changes: Vec<String>,
}

/// Each system's [`StoredRegistry`] from its whole copy (system, JSON) and
/// changes (system, JSON; oldest first).
pub fn stored_registry(whole: Vec<(String, String)>, changes: Vec<(String, String)>) -> Vec<StoredRegistry> {
    let mut out: std::collections::BTreeMap<String, StoredRegistry> = Default::default();
    fn at(out: &mut std::collections::BTreeMap<String, StoredRegistry>, system: String) -> &mut StoredRegistry {
        out.entry(system.clone()).or_insert_with(|| StoredRegistry { system, whole: None, changes: Vec::new() })
    }
    for (system, json) in whole {
        at(&mut out, system).whole = Some(json);
    }
    for (system, json) in changes {
        at(&mut out, system).changes.push(json);
    }
    out.into_values().collect()
}

/// What `GET /api/v1/storage` says of a store.
pub struct StoreInfo {
    /// Where it is: the SQLite file, or the PostgreSQL database (no password).
    pub location: String,
    pub schema_version: u32,
    pub calls: i64,
    pub oldest_ms: Option<i64>,
    pub newest_ms: Option<i64>,
}

/// A call store backend. Synchronous: the writer is a thread of its own, and
/// queries run under `spawn_blocking`. **Never call one from async code** (a
/// tokio worker): it blocks, and PostgreSQL's client refuses outright (it
/// runs a runtime of its own). Only [`CallStore::kind`] and
/// [`CallStore::location`] are free to call anywhere. Errors are strings:
/// they're logged, or said to whoever asked.
pub trait CallStore: Send + Sync {
    /// The backend's name, as `storage.backend` says it.
    fn kind(&self) -> &'static str;
    /// Where it is (the SQLite file, the database without its password): no query.
    fn location(&self) -> String;
    /// One batch in one transaction; each call whole or not at all (a
    /// change that fails is logged and the rest go on).
    fn apply(&self, batch: Vec<Change>) -> Result<(), String>;
    /// A page of the calls `f` wants, newest first, equal starts whole
    /// ([`super::read::paged`] does the paging over the backend's rows).
    fn page(&self, f: &CallsFilter) -> Result<Page, String>;
    /// The call filed under `path`, as its entry.
    fn one(&self, path: &str) -> Result<Option<Value>, String>;
    /// Whether a call is filed under `path` (the backfill skips it).
    fn known(&self, path: &str) -> Result<bool, String>;
    fn meta(&self, key: &str) -> Result<Option<String>, String>;
    /// A document kept between runs ([`Change::Kept`]).
    fn kept(&self, kind: &str, key: &str) -> Result<Option<String>, String>;
    /// Every system's radio registry, as stored (at startup).
    fn registry(&self) -> Result<Vec<StoredRegistry>, String>;
    /// Delete up to `max` calls that started before `before_ms`, oldest
    /// first, with their children; how many went.
    fn retire(&self, before_ms: i64, max: usize) -> Result<usize, String>;
    /// The newest call's start (retention counts back from it at most a day).
    fn newest_start(&self) -> Result<Option<i64>, String>;
    fn info(&self) -> Result<StoreInfo, String>;
    /// Fold what's pending into the store proper (SQLite: the WAL), at exit.
    fn checkpoint(&self) {}
    /// The writer is going: the last tidy-up (SQLite: `pragma optimize`).
    fn close(&self) {}
    /// A number from a plain SQL query, the same in every dialect (tests).
    #[cfg(test)]
    fn scalar(&self, sql: &str) -> i64;
    /// A trigger refusing transmissions by radio `unit`, to fail a call
    /// part-way (tests).
    #[cfg(test)]
    fn refuse_unit(&self, unit: i64);
}
