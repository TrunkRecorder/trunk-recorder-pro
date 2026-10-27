//! How the radio registry could be kept on disk, measured on the disk it
//! will run on (research/storage.md):
//!
//! - **whole**: the registry rewritten whole every 15 minutes (as now);
//! - **log**: the rows that changed ([`Registry::take_changes`]) appended as a
//!   JSON line every 10 s, onto a snapshot written once;
//! - **sqlite**: the same rows upserted into SQLite (WAL, synchronous=NORMAL)
//!   every 10 s, each row one JSON value;
//! - **log-n** and **sqlite-n**: the same, with a row's lists kept apart
//!   (normalized): only a talkgroup's or frequency's current hour, a radio's
//!   new transmissions, and the rest of the row without them. What a schema
//!   or log made that way writes (the bytes; reading back isn't built);
//! - **facts**: issue #100's design: each call, its transmissions and each
//!   affiliation appended as rows (rowid keys: the last page), talkgroups'
//!   and radios' state upserted, a call's record JSON kept, indexes by
//!   talkgroup and by radio; one transaction every `facts every` s (#100: 1).
//!
//! A system's registry is filled to about half its caps (a week of a large
//! system), then an hour of a busy one is played: a call a second (three
//! talkers of 10,000 radios, 2,500 talkgroups) and two affiliations a second.
//! Each way is told the same changes; it reports the bytes the disk was sent
//! (Linux: /proc/self/io), the time each save took, and reading it all back.
//!
//!   cargo run --release -- <dir on the disk to measure> [minutes] [save every s (10)]

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trunk_app::stats::radio::{Registry, RegistryChanges, SystemRadio, Tg, Unit};
use trunk_core::trunk::{Call, CallSource, Message, MessageType};

const SYS: &str = "big";
/// Activity is played in steps of this long; each row way saves every `every` of them.
const STEP_S: i64 = 10;
const WHOLE_EVERY_S: i64 = 15 * 60;

/// Bytes this process has sent to storage so far (0 where /proc isn't there).
fn written() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("write_bytes: ").and_then(|v| v.trim().parse().ok())))
        .unwrap_or(0)
}

/// A small deterministic generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as u32
    }
}

/// A registry of about half the caps, a week of activity.
fn filled(t: i64) -> SystemRadio {
    let mut s = SystemRadio::default();
    s.created = t - 7 * 86_400;
    for g in 0..2500u32 {
        let hours = (0..168).map(|h| (t / 3600 - h, (12u32, 95.5f32))).collect();
        s.tgs.insert(10_000 + g, Tg { first: t - 7 * 86_400, last: t, name: format!("TG {g} Dispatch"), calls: 4321, secs: 98_765.5, enc: 12, ciphers: vec![], hours });
    }
    for u in 0..10_000u32 {
        let recent = (0..30i64).map(|k| (t - k * 600, 10_000 + u % 2500, 4.25f32)).collect();
        s.units.insert(1_000_000 + u, Unit { first: t - 7 * 86_400, last: t, last_tx: t, tx: 321, tx_secs: 1234.5, aff: Some(10_001), aff_t: t, reg: Some(true), tgs: (0..8).map(|k| (10_000 + k, 40 - k)).collect(), recent });
    }
    for p in 0..25_000u64 {
        s.pairs.insert((1_000_000 + p % 10_000) << 32 | (1_000_000 + (p * 7) % 10_000), trunk_app::stats::radio::Pair { n: 17, last: t });
    }
    s.affiliations = (0..2000i64).map(|k| (t - k, 1_000_000 + k as u32, 10_001)).collect::<VecDeque<_>>();
    s.lengths = (0..5000i64).map(|k| (t - k, 10_001, 5.5f32)).collect::<VecDeque<_>>();
    s
}

/// What a second of a busy system did: its calls (talkgroup, talkers) and affiliations (radio, talkgroup).
#[derive(Default)]
struct Second {
    at: i64,
    calls: Vec<(u32, Vec<u32>)>,
    affiliations: Vec<(u32, u32)>,
}

/// Ten seconds of a busy system, ending at `t`, told to the registry; each second's events.
fn play(r: &mut Registry, rng: &mut Rng, t: i64) -> Vec<Second> {
    let mut out = Vec::new();
    for k in 0..STEP_S {
        let at = t - STEP_S + k;
        let mut sec = Second { at, ..Default::default() };
        let talkers: Vec<CallSource> = (0..3).map(|j| CallSource { src: 1_000_000 + rng.next(10_000), time_s: j as f64 * 2.0, ..Default::default() }).collect();
        let call = Call { talkgroup: 10_000 + rng.next(2500), start_s: 0.0, last_update_s: 6.0, sources: talkers, ..Default::default() };
        r.call_end(SYS, &call, at);
        sec.calls.push((call.talkgroup, call.sources.iter().map(|x| x.src).collect()));
        for _ in 0..2 {
            let m = Message { kind: MessageType::Affiliation, source: (1_000_000 + rng.next(10_000)) as i64, talkgroup: 10_000 + rng.next(2500), ..Default::default() };
            r.message(SYS, &m, at);
            sec.affiliations.push((m.source as u32, m.talkgroup));
        }
        out.push(sec);
    }
    out
}

/// Issue #100's store: facts appended, state upserted.
fn facts_open(path: &Path) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS calls (id INTEGER PRIMARY KEY, system TEXT, talkgroup INTEGER, start INTEGER, length REAL, freq INTEGER, encrypted INTEGER, path TEXT UNIQUE, record TEXT);
         CREATE INDEX IF NOT EXISTS calls_tg ON calls (talkgroup, start);
         CREATE TABLE IF NOT EXISTS call_transmissions (id INTEGER PRIMARY KEY, call INTEGER, unit INTEGER, pos REAL, length REAL);
         CREATE INDEX IF NOT EXISTS tx_unit ON call_transmissions (unit, call);
         CREATE TABLE IF NOT EXISTS unit_events (id INTEGER PRIMARY KEY, t INTEGER, unit INTEGER, kind TEXT, talkgroup INTEGER);
         CREATE TABLE IF NOT EXISTS talkgroups (key INTEGER PRIMARY KEY, first INTEGER, last INTEGER, calls INTEGER, secs REAL, enc INTEGER, name TEXT);
         CREATE TABLE IF NOT EXISTS units (key INTEGER PRIMARY KEY, first INTEGER, last INTEGER, last_tx INTEGER, tx INTEGER, tx_secs REAL, aff INTEGER, aff_t INTEGER, reg INTEGER);",
    )
    .unwrap();
    db
}

/// A call record's JSON as Trunk Recorder writes it (about 1.3 kB).
fn record_json(tg: u32, at: i64, talkers: &[u32]) -> String {
    let src: Vec<String> = talkers.iter().enumerate().map(|(i, u)| format!(r#"{{"src":{u},"time":{},"pos":{}.0,"emergency":0,"signal_system":"","tag":""}}"#, at + i as i64 * 2, i * 2)).collect();
    format!(
        r#"{{"freq":851012500,"freq_error":-31,"signal":-42,"noise":-61,"source_num":0,"recorder_num":3,"tdma_slot":0,"phase2_tdma":0,"start_time":{at},"stop_time":{},"emergency":0,"priority":0,"mode":0,"duplex":0,"encrypted":0,"call_length":6,"talkgroup":{tg},"talkgroup_tag":"TG {tg} Dispatch","talkgroup_description":"County dispatch","talkgroup_group_tag":"Fire Dispatch","talkgroup_group":"County Fire","audio_type":"digital","short_name":"big","freqList":[{{"freq":851012500,"time":{at},"pos":0.0,"len":6.0,"error_count":12,"spike_count":0}}],"srcList":[{}],"snr":19.4,"clean_voice_pct":98.7}}"#,
        at + 6,
        src.join(",")
    )
}

fn facts_write(db: &mut rusqlite::Connection, secs: &[Second]) {
    let tx = db.transaction().unwrap();
    {
        let mut call = tx.prepare_cached("INSERT INTO calls (system, talkgroup, start, length, freq, encrypted, path, record) VALUES ('big', ?1, ?2, 6.0, 851012500, 0, ?3, ?4)").unwrap();
        let mut ctx = tx.prepare_cached("INSERT INTO call_transmissions (call, unit, pos, length) VALUES (?1, ?2, ?3, 2.0)").unwrap();
        let mut ev = tx.prepare_cached("INSERT INTO unit_events (t, unit, kind, talkgroup) VALUES (?1, ?2, 'join', ?3)").unwrap();
        let mut tg = tx.prepare_cached("INSERT INTO talkgroups (key, first, last, calls, secs, enc, name) VALUES (?1, ?2, ?2, 1, 6.0, 0, '') ON CONFLICT(key) DO UPDATE SET last = ?2, calls = calls + 1, secs = secs + 6.0").unwrap();
        let mut unit = tx.prepare_cached("INSERT INTO units (key, first, last, last_tx, tx, tx_secs) VALUES (?1, ?2, ?2, ?2, 1, 2.0) ON CONFLICT(key) DO UPDATE SET last = ?2, last_tx = ?2, tx = tx + 1, tx_secs = tx_secs + 2.0").unwrap();
        let mut aff = tx.prepare_cached("INSERT INTO units (key, first, last, aff, aff_t) VALUES (?1, ?2, ?2, ?3, ?2) ON CONFLICT(key) DO UPDATE SET last = ?2, aff = ?3, aff_t = ?2").unwrap();
        for s in secs {
            for (g, talkers) in &s.calls {
                call.execute(rusqlite::params![g, s.at, format!("big/{}/{g}-{}.wav", s.at / 86_400, s.at), record_json(*g, s.at, talkers)]).unwrap();
                let id = tx.last_insert_rowid();
                tg.execute(rusqlite::params![g, s.at]).unwrap();
                for (i, u) in talkers.iter().enumerate() {
                    ctx.execute(rusqlite::params![id, u, i as f64 * 2.0]).unwrap();
                    unit.execute(rusqlite::params![u, s.at]).unwrap();
                }
            }
            for (u, g) in &s.affiliations {
                ev.execute(rusqlite::params![s.at, u, g]).unwrap();
                aff.execute(rusqlite::params![u, s.at, g]).unwrap();
            }
        }
    }
    tx.commit().unwrap();
}

#[derive(Default)]
struct Tally {
    bytes: u64,
    saves: Vec<Duration>,
}

impl Tally {
    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (b, at) = (written(), Instant::now());
        let v = f();
        self.saves.push(at.elapsed());
        self.bytes += written() - b;
        v
    }
    fn report(&self, name: &str, minutes: i64) {
        let mut s = self.saves.clone();
        s.sort();
        let p = |q: f64| s.get(((s.len() as f64 - 1.0) * q) as usize).copied().unwrap_or_default();
        let per_day = self.bytes as f64 * (24.0 * 60.0 / minutes as f64);
        println!("{name:>7}: {:>8.1} MB sent to disk ({:>7.2} GB/day), {:>4} saves, median {:>8.1?}, max {:>8.1?}", self.bytes as f64 / 1e6, per_day / 1e9, s.len(), p(0.5), p(1.0));
    }
}

fn log_line(path: &Path, c: &RegistryChanges) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_vec(c).unwrap();
    line.push(b'\n');
    f.write_all(&line)?;
    f.sync_data()
}

fn sqlite_open(path: &Path) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS row (kind TEXT, key INTEGER, v TEXT, PRIMARY KEY (kind, key)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS recent (kind TEXT, n INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT);",
    )
    .unwrap();
    db
}

fn sqlite_write(db: &mut rusqlite::Connection, c: &RegistryChanges) {
    let tx = db.transaction().unwrap();
    {
        let mut up = tx.prepare_cached("INSERT OR REPLACE INTO row (kind, key, v) VALUES (?1, ?2, ?3)").unwrap();
        let mut del = tx.prepare_cached("DELETE FROM row WHERE kind = ?1 AND key = ?2").unwrap();
        let mut add = tx.prepare_cached("INSERT INTO recent (kind, v) VALUES (?1, ?2)").unwrap();
        let j = |v: &dyn erased::Json| v.json();
        for (k, v) in &c.tgs {
            up.execute(rusqlite::params!["tg", *k as i64, j(v)]).unwrap();
        }
        for (k, v) in &c.units {
            up.execute(rusqlite::params!["unit", *k as i64, j(v)]).unwrap();
        }
        for (k, v) in &c.pairs {
            up.execute(rusqlite::params!["pair", *k as i64, j(v)]).unwrap();
        }
        for (k, v) in &c.freqs {
            up.execute(rusqlite::params!["freq", *k as i64, j(v)]).unwrap();
        }
        for (kind, keys) in [("tg", c.gone.tgs.iter().map(|k| *k as i64).collect::<Vec<_>>()), ("unit", c.gone.units.iter().map(|k| *k as i64).collect()), ("pair", c.gone.pairs.iter().map(|k| *k as i64).collect()), ("freq", c.gone.freqs.iter().map(|k| *k as i64).collect())] {
            for k in keys {
                del.execute(rusqlite::params![kind, k]).unwrap();
            }
        }
        for a in &c.affiliations {
            add.execute(rusqlite::params!["aff", j(a)]).unwrap();
        }
        for l in &c.lengths {
            add.execute(rusqlite::params!["len", j(l)]).unwrap();
        }
    }
    tx.commit().unwrap();
}

/// `c` normalized: each talkgroup and frequency with only the hours since
/// `since` (Unix s), each radio with only its transmissions since.
fn slim(c: &RegistryChanges, since: i64) -> RegistryChanges {
    let mut c = c.clone();
    for (_, g) in &mut c.tgs {
        g.hours.retain(|(h, _)| (h + 1) * 3600 > since);
    }
    for (_, f) in &mut c.freqs {
        f.hours.retain(|(h, _)| (h + 1) * 3600 > since);
    }
    for (_, u) in &mut c.units {
        u.recent.retain(|(at, _, _)| *at > since);
    }
    c
}

/// Normalized SQLite: a row's scalars, its hours and its transmissions in tables of their own.
fn sqlite_norm_open(path: &Path) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS tg (key INTEGER PRIMARY KEY, first INTEGER, last INTEGER, name TEXT, calls INTEGER, secs REAL, enc INTEGER);
         CREATE TABLE IF NOT EXISTS tg_hour (tg INTEGER, hour INTEGER, calls INTEGER, secs REAL, PRIMARY KEY (tg, hour)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS unit (key INTEGER PRIMARY KEY, first INTEGER, last INTEGER, last_tx INTEGER, tx INTEGER, tx_secs REAL, aff INTEGER, aff_t INTEGER, reg INTEGER, tgs TEXT);
         CREATE TABLE IF NOT EXISTS unit_tx (unit INTEGER, at INTEGER, tg INTEGER, secs REAL);
         CREATE INDEX IF NOT EXISTS unit_tx_unit ON unit_tx (unit, at);
         CREATE TABLE IF NOT EXISTS pair (key INTEGER PRIMARY KEY, n INTEGER, last INTEGER);
         CREATE TABLE IF NOT EXISTS recent (kind TEXT, at INTEGER, a INTEGER, b REAL);",
    )
    .unwrap();
    db
}

fn sqlite_norm_write(db: &mut rusqlite::Connection, c: &RegistryChanges) {
    let tx = db.transaction().unwrap();
    {
        let mut tg = tx.prepare_cached("INSERT OR REPLACE INTO tg VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)").unwrap();
        let mut hour = tx.prepare_cached("INSERT OR REPLACE INTO tg_hour VALUES (?1, ?2, ?3, ?4)").unwrap();
        let mut unit = tx.prepare_cached("INSERT OR REPLACE INTO unit VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)").unwrap();
        let mut utx = tx.prepare_cached("INSERT INTO unit_tx VALUES (?1, ?2, ?3, ?4)").unwrap();
        let mut pair = tx.prepare_cached("INSERT OR REPLACE INTO pair VALUES (?1, ?2, ?3)").unwrap();
        let mut recent = tx.prepare_cached("INSERT INTO recent VALUES (?1, ?2, ?3, ?4)").unwrap();
        for (k, g) in &c.tgs {
            tg.execute(rusqlite::params![*k as i64, g.first, g.last, g.name, g.calls, g.secs, g.enc]).unwrap();
            for (h, (n, secs)) in &g.hours {
                hour.execute(rusqlite::params![*k as i64, h, n, secs]).unwrap();
            }
        }
        for (k, u) in &c.units {
            unit.execute(rusqlite::params![*k as i64, u.first, u.last, u.last_tx, u.tx, u.tx_secs, u.aff, u.aff_t, u.reg, serde_json::to_string(&u.tgs).unwrap()]).unwrap();
            for (at, g, secs) in &u.recent {
                utx.execute(rusqlite::params![*k as i64, at, g, secs]).unwrap();
            }
        }
        for (k, p) in &c.pairs {
            pair.execute(rusqlite::params![*k as i64, p.n, p.last]).unwrap();
        }
        for (at, u, g) in &c.affiliations {
            recent.execute(rusqlite::params!["aff", at, u, *g as f64]).unwrap();
        }
        for (at, g, secs) in &c.lengths {
            recent.execute(rusqlite::params!["len", at, g, secs]).unwrap();
        }
    }
    tx.commit().unwrap();
}

/// Each row as JSON (a value of any of the registry's row types).
mod erased {
    pub trait Json {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> String {
            serde_json::to_string(self).unwrap()
        }
    }
}

/// The whole registry as SQLite rows (the first write: an import).
fn sqlite_import(db: &mut rusqlite::Connection, s: &SystemRadio) {
    let all = RegistryChanges {
        tgs: s.tgs.iter().map(|(k, v)| (*k, v.clone())).collect(),
        units: s.units.iter().map(|(k, v)| (*k, v.clone())).collect(),
        pairs: s.pairs.iter().map(|(k, v)| (*k, *v)).collect(),
        freqs: s.freqs.iter().map(|(k, v)| (*k, v.clone())).collect(),
        affiliations: s.affiliations.iter().copied().collect(),
        lengths: s.lengths.iter().copied().collect(),
        ..Default::default()
    };
    sqlite_write(db, &all);
}

/// Every row read back and parsed, as at startup; how many.
fn sqlite_read(db: &rusqlite::Connection) -> usize {
    let mut s = SystemRadio::default();
    let mut q = db.prepare("SELECT kind, key, v FROM row").unwrap();
    let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))).unwrap();
    for row in rows {
        let (kind, k, v) = row.unwrap();
        match kind.as_str() {
            "tg" => drop(s.tgs.insert(k as u32, serde_json::from_str(&v).unwrap())),
            "unit" => drop(s.units.insert(k as u32, serde_json::from_str(&v).unwrap())),
            "pair" => drop(s.pairs.insert(k as u64, serde_json::from_str(&v).unwrap())),
            _ => drop(s.freqs.insert(k as u64, serde_json::from_str(&v).unwrap())),
        }
    }
    s.tgs.len() + s.units.len() + s.pairs.len() + s.freqs.len()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("usage: storage-bench <dir> [minutes]"));
    let minutes: i64 = args.get(2).and_then(|m| m.parse().ok()).unwrap_or(60);
    let every_s: i64 = args.get(3).and_then(|m| m.parse().ok()).unwrap_or(STEP_S).max(STEP_S) / STEP_S * STEP_S;
    let facts_every: usize = args.get(4).and_then(|m| m.parse().ok()).unwrap_or(1).max(1);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t0 = 1_790_000_000i64;
    let mut r = Registry::default();
    r.systems.insert(SYS.into(), filled(t0));
    // Everything as of now counts as changed for the whole file; the row ways start from an import.
    let snapshot = serde_json::to_string(&r.systems[SYS]).unwrap();
    println!("registry: {:.1} MB as JSON; {} minutes of a busy system, saved every {every_s} s (whole: every {} min)", snapshot.len() as f64 / 1e6, minutes, WHOLE_EVERY_S / 60);
    let (whole_path, log_snap, log_path, db_path) = (dir.join("whole.json"), dir.join("log.snapshot.json"), dir.join("log.jsonl"), dir.join("registry.sqlite"));
    let mut import = Tally::default();
    import.time(|| trunk_app::config::write_atomic(&log_snap, &snapshot).unwrap());
    let mut db = sqlite_open(&db_path);
    let mut sq_import = Tally::default();
    sq_import.time(|| sqlite_import(&mut db, &r.systems[SYS]));
    let (slim_path, norm_path) = (dir.join("log-n.jsonl"), dir.join("registry-n.sqlite"));
    let mut norm = sqlite_norm_open(&norm_path);
    let all = RegistryChanges {
        tgs: r.systems[SYS].tgs.iter().map(|(k, v)| (*k, v.clone())).collect(),
        units: r.systems[SYS].units.iter().map(|(k, v)| (*k, v.clone())).collect(),
        pairs: r.systems[SYS].pairs.iter().map(|(k, v)| (*k, *v)).collect(),
        ..Default::default()
    };
    let mut norm_import = Tally::default();
    norm_import.time(|| sqlite_norm_write(&mut norm, &all));
    // #100's store, with the radios and talkgroups it already knows (a day of calls before is no matter here).
    let mut facts = facts_open(&dir.join("trunk-pro.sqlite"));
    {
        let tx = facts.transaction().unwrap();
        for (k, g) in &r.systems[SYS].tgs {
            tx.execute("INSERT INTO talkgroups VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", rusqlite::params![k, g.first, g.last, g.calls, g.secs, g.enc, g.name]).unwrap();
        }
        for (k, u) in &r.systems[SYS].units {
            tx.execute("INSERT INTO units VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", rusqlite::params![k, u.first, u.last, u.last_tx, u.tx, u.tx_secs, u.aff, u.aff_t, u.reg]).unwrap();
        }
        tx.commit().unwrap();
    }
    let mut facts_t = Tally::default();
    let mut waiting: Vec<Second> = Vec::new();
    let _ = r.take_changes();
    let _ = r.take_dirty_copies();

    let (mut whole, mut log, mut sqlite, mut log_n, mut sqlite_n) = (Tally::default(), Tally::default(), Tally::default(), Tally::default(), Tally::default());
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut t = t0;
    while t < t0 + minutes * 60 {
        t += STEP_S;
        waiting.extend(play(&mut r, &mut rng, t));
        while waiting.len() >= facts_every {
            let batch: Vec<Second> = waiting.drain(..facts_every).collect();
            facts_t.time(|| facts_write(&mut facts, &batch));
        }
        if (t - t0) % every_s != 0 {
            continue;
        }
        let changes = r.take_changes();
        for (_, c) in &changes {
            log.time(|| log_line(&log_path, c).unwrap());
            sqlite.time(|| sqlite_write(&mut db, c));
            let thin = slim(c, t - every_s);
            log_n.time(|| log_line(&slim_path, &thin).unwrap());
            sqlite_n.time(|| sqlite_norm_write(&mut norm, &thin));
        }
        if (t - t0) % WHOLE_EVERY_S == 0 {
            let copies = r.take_dirty_copies();
            whole.time(|| {
                for (_, s) in &copies {
                    trunk_app::config::write_atomic(&whole_path, serde_json::to_string(s).unwrap()).unwrap();
                }
            });
        }
    }
    println!();
    whole.report("whole", minutes);
    log.report("log", minutes);
    sqlite.report("sqlite", minutes);
    log_n.report("log-n", minutes);
    sqlite_n.report("sqlite-n", minutes);
    facts_t.report("facts", minutes);
    println!("  (facts: one transaction every {facts_every} s; its file {:.1} MB, its WAL {:.1} MB)", std::fs::metadata(dir.join("trunk-pro.sqlite")).map_or(0.0, |m| m.len() as f64 / 1e6), std::fs::metadata(dir.join("trunk-pro.sqlite-wal")).map_or(0.0, |m| m.len() as f64 / 1e6));
    println!("  first: snapshot {:.1} MB in {:.1?}; SQLite import in {:.1?} ({:.1} MB); normalized import in {:.1?} ({:.1} MB)", import.bytes as f64 / 1e6, import.saves[0], sq_import.saves[0], sq_import.bytes as f64 / 1e6, norm_import.saves[0], norm_import.bytes as f64 / 1e6);
    drop(db);
    drop(norm);
    drop(facts);

    // Reading it back, as at startup.
    let at = Instant::now();
    let w: SystemRadio = serde_json::from_str(&std::fs::read_to_string(&whole_path).unwrap()).unwrap();
    let whole_read = at.elapsed();
    let at = Instant::now();
    let mut l: SystemRadio = serde_json::from_str(&std::fs::read_to_string(&log_snap).unwrap()).unwrap();
    let text = std::fs::read_to_string(&log_path).unwrap();
    for line in text.lines() {
        l.apply(&serde_json::from_str(line).unwrap());
    }
    let log_read = at.elapsed();
    assert_eq!(serde_json::to_value(&l).unwrap(), serde_json::to_value(&r.systems[SYS]).unwrap(), "the log makes the registry");
    let at = Instant::now();
    let db = sqlite_open(&db_path);
    let n = sqlite_read(&db);
    let sqlite_read_t = at.elapsed();
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
    println!("  read back: whole {whole_read:.1?} ({} radios); log {log_read:.1?} (snapshot + {:.1} MB of lines); sqlite {sqlite_read_t:.1?} ({n} rows, {:.1} MB + {:.1} MB WAL)", w.units.len(), size(&log_path), size(&db_path), size(&dir.join("registry.sqlite-wal")));
}
