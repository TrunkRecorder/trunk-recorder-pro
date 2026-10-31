//! Recorded calls asked for ([`CallsQuery`]) out of calls held in memory —
//! the desktop app's live list, the browser build's whole index — with the
//! same order and cursor as the desktop app's store (trunk-pro `db/read.rs`):
//! newest first by start, then path; the cursor is where the last call of a
//! page sits in that order, so paging on loses and repeats nothing.

pub use trunk_recorder_plugin::{CallsPage, CallsQuery, StoredCall};

/// Calls on a page by default, and at most.
pub const LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;

/// Where a call sits in the order: its start (Unix ms) and path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub start_ms: i64,
    pub path: String,
}

impl Cursor {
    pub fn of(c: &StoredCall) -> Cursor {
        Cursor { start_ms: c.record.start_time_ms, path: c.path.clone() }
    }

    /// As a page's `next` says it (`<start>:<path>`).
    pub fn encode(&self) -> String {
        format!("{}:{}", self.start_ms, self.path)
    }

    pub fn decode(s: &str) -> Option<Cursor> {
        let (start, path) = s.split_once(':')?;
        Some(Cursor { start_ms: start.parse().ok()?, path: path.to_string() })
    }
}

/// `q`'s page size.
pub fn limit(q: &CallsQuery) -> usize {
    q.limit.unwrap_or(LIMIT).clamp(1, MAX_LIMIT)
}

/// Whether call `c` is one `q` wants: its system (any of `names`, every name
/// its calls were filed under; none: any), talkgroup, radio, time, and past
/// the cursor.
pub fn matches(c: &StoredCall, q: &CallsQuery, names: &[String]) -> bool {
    let r = &c.record;
    let after = q.cursor.as_deref().and_then(Cursor::decode);
    (names.is_empty() || names.iter().any(|n| *n == r.short_name))
        && q.talkgroup.is_none_or(|tg| r.talkgroup == tg)
        && q.unit.is_none_or(|u| r.src_list.iter().any(|s| s.src == u))
        && q.from.is_none_or(|t| r.start_time_ms >= t)
        && q.to.is_none_or(|t| r.start_time_ms < t)
        && after.is_none_or(|a| Cursor::of(c) < a)
}

/// Newest first: by start, then path, both descending.
pub fn sort(v: &mut [StoredCall]) {
    v.sort_by(|a, b| Cursor::of(b).cmp(&Cursor::of(a)));
}

/// The page of `calls` that `q` wants.
pub fn page_of(calls: &[StoredCall], q: &CallsQuery, names: &[String]) -> CallsPage {
    let mut v: Vec<StoredCall> = calls.iter().filter(|c| matches(c, q, names)).cloned().collect();
    sort(&mut v);
    cut(v, limit(q))
}

/// The first `limit` of `v` (in order), and the cursor after them when more remain.
pub fn cut(mut v: Vec<StoredCall>, limit: usize) -> CallsPage {
    let more = v.len() > limit;
    v.truncate(limit);
    let next = if more { v.last().map(|c| Cursor::of(c).encode()) } else { None };
    CallsPage { entries: v, next, error: None }
}

/// `stored` (a store's page of `q`) with the `live` calls `q` wants that
/// belong on it: those not stored yet, down to the page's last call when the
/// store has more (older ones belong on a later page: the store's calls
/// between would be skipped). A call in both is the live copy.
pub fn with_live(stored: CallsPage, live: &[StoredCall], q: &CallsQuery, names: &[String]) -> CallsPage {
    let floor = stored.next.as_deref().and_then(Cursor::decode);
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<StoredCall> = live
        .iter()
        .filter(|c| matches(c, q, names) && floor.as_ref().is_none_or(|f| Cursor::of(c) >= *f))
        .cloned()
        .chain(stored.entries)
        .filter(|c| seen.insert(c.path.clone()))
        .collect();
    sort(&mut v);
    let limit = limit(q);
    if v.len() > limit { cut(v, limit) } else { CallsPage { entries: v, next: stored.next, error: stored.error } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(path: &str, start_ms: i64, tg: u32) -> StoredCall {
        let mut c = StoredCall { path: path.into(), audio: true, json: true, ..Default::default() };
        c.record.start_time_ms = start_ms;
        c.record.talkgroup = tg;
        c.record.short_name = "a".into();
        c
    }

    /// Pages follow each other by cursor: none lost or repeated, equal starts
    /// ordered by path, wherever the page ends.
    #[test]
    fn pages_follow_by_cursor() {
        let calls: Vec<StoredCall> = vec![call("a/1", 3000, 1), call("a/2", 2000, 1), call("a/3", 2000, 2), call("a/4", 2000, 1), call("a/5", 1000, 1)];
        let mut q = CallsQuery { limit: Some(2), ..Default::default() };
        let mut got = Vec::new();
        loop {
            let p = page_of(&calls, &q, &[]);
            got.extend(p.entries.iter().map(|c| c.path.clone()));
            match p.next {
                Some(n) => q.cursor = Some(n),
                None => break,
            }
        }
        assert_eq!(got, ["a/1", "a/4", "a/3", "a/2", "a/5"]);
        let tg = page_of(&calls, &CallsQuery { talkgroup: Some(2), ..Default::default() }, &[]);
        assert_eq!((tg.entries.len(), tg.next), (1, None));
        assert!(page_of(&calls, &CallsQuery::default(), &["b".into()]).entries.is_empty(), "another system's");
        assert_eq!(Cursor::decode("12:a/b:c"), Some(Cursor { start_ms: 12, path: "a/b:c".into() }));
    }

    /// A stored page with the live calls merged in: the live copy wins, and
    /// the page is cut where the store's was.
    #[test]
    fn live_calls_join_their_page() {
        let q = CallsQuery { limit: Some(2), ..Default::default() };
        let stored = page_of(&[call("a/2", 2000, 1), call("a/1", 1000, 1), call("a/0", 500, 1)], &q, &[]);
        let mut fresher = call("a/2", 2000, 1);
        fresher.audio = false;
        // (One newer than all, the live copy of a stored one, one older than the stored page.)
        let live = vec![call("a/3", 3000, 1), fresher, call("a/00", 400, 1)];
        let p = with_live(stored, &live, &q, &[]);
        assert_eq!(p.entries.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["a/3", "a/2"]);
        assert!(!p.entries[1].audio, "the live copy");
        assert_eq!(p.next.as_deref(), Some("2000:a/2"));
        // The next page: the store's from the cursor, and the old live one where it belongs.
        let q2 = CallsQuery { cursor: p.next, ..q.clone() };
        let stored = page_of(&[call("a/2", 2000, 1), call("a/1", 1000, 1), call("a/0", 500, 1)], &q2, &[]);
        let p = with_live(stored, &live, &q2, &[]);
        assert_eq!(p.entries.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["a/1", "a/0"]);
        let q3 = CallsQuery { cursor: p.next, ..q };
        let p = with_live(page_of(&[], &q3, &[]), &live, &q3, &[]);
        assert_eq!((p.entries.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), p.next), (vec!["a/00"], None), "every call once");
    }
}
