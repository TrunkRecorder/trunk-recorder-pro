//! A system's names for its talkgroups and radios, kept as rows ([`NameRow`]),
//! and where each row's names came from ([`Origin`]): the last change to it,
//! not its history. Both kinds of rows are replaced and edited the same way
//! here, by the desktop recorder and the browser build alike.

use std::collections::HashMap;
use std::hash::Hash;

/// How a row's names came in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OriginKind {
    /// A file loaded (Trunk Recorder's CSV): the origin's name is the file's.
    #[default]
    File,
    /// The talkgroup table copied from RadioReference's web page.
    RadioReference,
    /// A systems directory (trunkrecorder.pro): the origin's name is the directory's.
    Directory,
    /// A Trunk Recorder config imported: the origin's name is the file it named.
    TrunkRecorder,
    /// Typed in, one row at a time.
    Edited,
}

impl OriginKind {
    /// Its name, as JSON and the store write it.
    pub fn as_str(self) -> &'static str {
        match self {
            OriginKind::File => "file",
            OriginKind::RadioReference => "radioReference",
            OriginKind::Directory => "directory",
            OriginKind::TrunkRecorder => "trunkRecorder",
            OriginKind::Edited => "edited",
        }
    }

    /// The kind [`OriginKind::as_str`] names (none: not one).
    pub fn from_name(s: &str) -> Option<OriginKind> {
        [OriginKind::File, OriginKind::RadioReference, OriginKind::Directory, OriginKind::TrunkRecorder, OriginKind::Edited].into_iter().find(|k| k.as_str() == s)
    }
}

/// Where a row's names came from, and when (its last change).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Origin {
    pub kind: OriginKind,
    /// The file's or the directory's name ("" when the kind needs none).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Unix seconds.
    pub at: i64,
}

impl Origin {
    /// Typed in at `at`.
    pub fn edited(at: i64) -> Origin {
        Origin { kind: OriginKind::Edited, name: String::new(), at }
    }

    /// This origin, at `at`.
    pub fn at(&self, at: i64) -> Origin {
        Origin { at, ..self.clone() }
    }
}

/// A row of a system's names: one a key, with an origin.
pub trait NameRow: Clone {
    type Key: Eq + Hash + Clone;
    fn key(&self) -> Self::Key;
    fn origin(&self) -> &Option<Origin>;
    fn origin_mut(&mut self) -> &mut Option<Origin>;
    /// Whether `other` gives the same names, whatever either's origin
    /// (and what isn't a name: a talkgroup's Ignore).
    fn same_names(&self, other: &Self) -> bool;
}

/// `new`, in place of `old`: a key in two rows of `new` is the later row,
/// where the first was. A row `old` gave the same names keeps its origin;
/// the others are `from`'s. No `from`: each keeps the origin it comes with
/// (a system's rows moved to its new name).
pub fn replace_rows<R: NameRow>(old: &[R], new: Vec<R>, from: Option<&Origin>) -> Vec<R> {
    let was: HashMap<R::Key, &R> = old.iter().map(|r| (r.key(), r)).collect();
    let mut at: HashMap<R::Key, usize> = HashMap::new();
    let mut out: Vec<R> = Vec::with_capacity(new.len());
    for mut r in new {
        match was.get(&r.key()) {
            Some(o) if o.same_names(&r) => *r.origin_mut() = o.origin().clone(),
            _ => {
                if let Some(f) = from {
                    *r.origin_mut() = Some(f.clone());
                }
            }
        }
        match at.get(&r.key()) {
            Some(&i) => out[i] = r,
            None => {
                at.insert(r.key(), out.len());
                out.push(r);
            }
        }
    }
    out
}

/// Stamp `row` with `by` when an edit changed its names (`before`: the row
/// before it; none: the edit added it).
pub(crate) fn stamp<R: NameRow>(row: &mut R, before: Option<&R>, by: &Origin) {
    if before.is_none_or(|b| !b.same_names(row)) {
        *row.origin_mut() = Some(by.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::{parse_rows, TalkgroupRow};

    fn origin(r: &TalkgroupRow) -> Option<OriginKind> {
        r.origin.as_ref().map(|o| o.kind)
    }

    /// A list loaded over another: the rows it changes or adds are the new
    /// file's, the rest keep where they came from; a number twice is the
    /// later row, where the first was; moved rows keep their own.
    #[test]
    fn replaced_rows_keep_their_origin_unless_changed() {
        let rr = Origin { kind: OriginKind::RadioReference, name: String::new(), at: 100 };
        let old = replace_rows(&[], parse_rows("Decimal,Alpha Tag,Ignore\n1,Fire\n2,EMS\n"), Some(&rr));
        assert_eq!(old.iter().map(origin).collect::<Vec<_>>(), [Some(OriginKind::RadioReference); 2]);
        let file = Origin { kind: OriginKind::File, name: "dc.csv".into(), at: 200 };
        let new = replace_rows(&old, parse_rows("Decimal,Alpha Tag,Ignore\n1,Fire,x\n2,EMS 2\n3,Police\n"), Some(&file));
        assert_eq!(new.iter().map(origin).collect::<Vec<_>>(), [Some(OriginKind::RadioReference), Some(OriginKind::File), Some(OriginKind::File)], "Ignore isn't a name");
        assert_eq!(new[0].origin.as_ref().unwrap().at, 100);
        let twice = replace_rows(&[], parse_rows("Decimal,Alpha Tag\n1,A\n2,B\n").into_iter().chain(parse_rows("Decimal,Alpha Tag\n1,C\n")).collect(), None);
        assert_eq!(twice.iter().map(|r| r.tg.alpha_tag.as_str()).collect::<Vec<_>>(), ["C", "B"]);
        let moved = replace_rows(&[], new.clone(), None);
        assert_eq!(moved, new);
    }
}
