//! Trunk Recorder talkgroup CSV (trunk-recorder/talkgroups.cc): the headed
//! format (first column "Decimal"; columns Decimal, Hex, Mode, Alpha Tag,
//! Description, Tag, Category, Priority, Preferred NAC, Preferred Site in
//! any order) or the legacy headerless one:
//! Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority].
//!
//! Delimited by commas, semicolons, tabs or bars (as Trunk Recorder's CSV
//! reader guesses), headers in any case.
//!
//! An `Ignore` column (this app's) marks talkgroups never to record (`true`,
//! `yes`, `1`, `x`); so does Trunk Recorder's priority −1.

use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Talkgroup {
    pub number: u32,
    pub mode: String,
    pub alpha_tag: String,
    pub description: String,
    pub tag: String,
    /// Trunk Recorder's Category column (its legacy format's Group).
    #[serde(rename = "category")]
    pub group: String,
    #[serde(default = "first_priority")]
    pub priority: i32,
    /// Multi-site: the site whose copy of a call to keep, as Trunk Recorder
    /// writes it — its NAC, or RFSS and site as `RRRRssss` (1 and 23: 10023).
    pub preferred_nac: u32,
    /// Multi-site: the same by the site's short name.
    pub preferred_site: String,
    /// Never record it (the Ignore column, or priority −1).
    pub ignore: bool,
}

/// Trunk Recorder's priority for a talkgroup that doesn't give one.
fn first_priority() -> i32 {
    1
}

impl Talkgroup {
    /// Mode letters Trunk Recorder treats as "encrypted, don't record".
    pub fn encrypted_mode(&self) -> bool {
        matches!(self.mode.as_str(), "E" | "TE" | "DE")
    }
}

pub type Talkgroups = HashMap<u32, Talkgroup>;

/// A CSV file's lines that hold something: not blank, not a `#` comment,
/// and without the byte-order mark spreadsheets put at the start.
pub(crate) fn csv_lines(text: &str) -> impl Iterator<Item = &str> {
    text.strip_prefix('\u{feff}').unwrap_or(text).lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
}

/// RFC-4180-ish split: commas, double-quoted fields, "" escapes.
pub(crate) fn split_csv_line(line: &str) -> Vec<String> {
    split_csv_on(line, ',')
}

/// The delimiter a CSV's first line uses: comma, semicolon, tab or bar, as
/// Trunk Recorder's CSV reader guesses (a decimal-comma spreadsheet saves
/// with semicolons).
pub(crate) fn csv_delimiter(line: &str) -> char {
    [',', ';', '\t', '|'].into_iter().fold((',', 0), |best, d| {
        let n = line.matches(d).count();
        if n > best.1 { (d, n) } else { best }
    }).0
}

/// [`split_csv_line`] on `delim`.
pub(crate) fn split_csv_on(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' && chars.peek() == Some(&'"') {
                cur.push('"');
                chars.next();
            } else if ch == '"' {
                quoted = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == delim {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    out.push(cur.trim().to_string());
    out
}

pub fn parse_csv(text: &str) -> Talkgroups {
    let lines: Vec<&str> = csv_lines(text).collect();
    let mut out = Talkgroups::new();
    let Some(&head) = lines.first() else { return out };
    let delim = csv_delimiter(head);
    // Headers in any case, as spreadsheets save them, and RadioReference's DEC and Group.
    let first: Vec<String> = split_csv_on(head, delim)
        .iter()
        .map(|c| match c.trim().to_ascii_lowercase().as_str() {
            "dec" => "decimal".to_string(),
            "group" => "category".to_string(),
            "alphatag" => "alpha tag".to_string(),
            h => h.to_string(),
        })
        .collect();
    let headed = first.first().map(|s| s.as_str()) == Some("decimal");
    let col = |name: &str| first.iter().position(|c| c.eq_ignore_ascii_case(name));
    for line in if headed { &lines[1..] } else { &lines[..] } {
        let f = split_csv_on(line, delim);
        let get = |name: &str, legacy: usize| -> Option<&str> {
            if headed {
                col(name).and_then(|i| f.get(i)).map(|s| s.as_str())
            } else {
                f.get(legacy).map(|s| s.as_str())
            }
        };
        let Some(number) = get("Decimal", 0).and_then(|s| s.parse::<u32>().ok()) else { continue };
        let s = |name: &str, legacy: usize| get(name, legacy).unwrap_or("").to_string();
        let priority = get("Priority", 7).and_then(|v| v.parse().ok()).unwrap_or(1);
        let marked = get("Ignore", 99).is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "y" | "1" | "x" | "ignore"));
        out.insert(
            number,
            Talkgroup {
                number,
                mode: s("Mode", 2),
                alpha_tag: s("Alpha Tag", 3),
                description: s("Description", 4),
                tag: s("Tag", 5),
                group: s("Category", 6),
                priority,
                preferred_nac: get("Preferred NAC", 99).and_then(|v| v.parse().ok()).unwrap_or(0),
                preferred_site: s("Preferred Site", 99),
                ignore: marked || priority < 0,
            },
        );
    }
    out
}

/// A system's talkgroup, as kept and as the interfaces have it: its
/// [`Talkgroup`], the comment written directly above its row in the file it
/// came from (`#` lines), and the columns this app doesn't know ([name,
/// value]), kept for the file it's exported to.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TalkgroupRow {
    #[serde(flatten)]
    pub tg: Talkgroup,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<(String, String)>,
}

/// A talkgroup file read ([`read_rows`]): its rows, and what in it was
/// passed over or would trouble Trunk Recorder.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkgroupFile {
    pub talkgroups: Vec<TalkgroupRow>,
    /// No header: Trunk Recorder's old fixed columns.
    pub legacy: bool,
    /// Rows without a talkgroup number (their text, the first few).
    pub skipped: Vec<String>,
    /// Columns neither Trunk Recorder nor this app knows (kept; Trunk Recorder rejects them).
    pub unknown_columns: Vec<String>,
    /// Columns Trunk Recorder needs that the header lacks (Mode, Description).
    pub missing: Vec<String>,
}

/// The columns a talkgroup file's rows have a field for (any other is `extra`).
const KNOWN: [&str; 11] = ["Decimal", "Hex", "Mode", "Alpha Tag", "Description", "Tag", "Category", "Priority", "Preferred NAC", "Preferred Site", "Ignore"];

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "y" | "1" | "x" | "ignore")
}

/// A talkgroup file's rows ([`read_rows`]).
pub fn parse_rows(text: &str) -> Vec<TalkgroupRow> {
    read_rows(text).talkgroups
}

/// A talkgroup file's rows, in its order (the headed or the legacy format,
/// read as [`parse_csv`] reads it), and what was passed over. A comment
/// line directly above a row is that row's; others (before the header,
/// after a blank line) are dropped. A talkgroup in two rows: the later
/// one, where the first was.
pub fn read_rows(text: &str) -> TalkgroupFile {
    let mut out = TalkgroupFile::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(head) = csv_lines(text).next() else { return out };
    let delim = csv_delimiter(head);
    let header: Vec<String> = split_csv_on(head, delim).iter().map(|h| canonical(h)).collect();
    let headed = header.first().map(String::as_str) == Some("Decimal");
    out.legacy = !headed;
    if headed {
        // (Trunk Recorder's Comment column: kept with the row, as any column of a file's own, and no trouble to it.)
        out.unknown_columns = header.iter().filter(|h| !h.is_empty() && !KNOWN.contains(&h.as_str()) && *h != "Comment").cloned().collect();
        out.missing = ["Mode", "Description"].iter().filter(|h| !header.iter().any(|c| c == *h)).map(|h| h.to_string()).collect();
    }
    let cols: Vec<String> = if headed { header } else { LEGACY.iter().map(|s| s.to_string()).collect() };
    let rows = &mut out.talkgroups;
    let mut at: HashMap<u32, usize> = HashMap::new();
    let mut comment: Vec<&str> = Vec::new();
    let mut header_seen = !headed;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            comment.clear();
            continue;
        }
        if t.starts_with('#') {
            comment.push(line.trim_end());
            continue;
        }
        if !header_seen {
            header_seen = true;
            comment.clear();
            continue;
        }
        let f = split_csv_on(line, delim);
        let get = |name: &str| cols.iter().position(|c| c == name).and_then(|i| f.get(i)).map(String::as_str);
        let Some(number) = get("Decimal").and_then(|s| s.trim().parse::<u32>().ok()) else {
            if out.skipped.len() < 20 {
                out.skipped.push(t.to_string());
            }
            comment.clear();
            continue;
        };
        let s = |name: &str| get(name).unwrap_or("").to_string();
        let priority = get("Priority").and_then(|v| v.parse().ok()).unwrap_or(1);
        let tg = Talkgroup {
            number,
            mode: s("Mode"),
            alpha_tag: s("Alpha Tag"),
            description: s("Description"),
            tag: s("Tag"),
            group: s("Category"),
            priority,
            preferred_nac: get("Preferred NAC").and_then(|v| v.parse().ok()).unwrap_or(0),
            preferred_site: s("Preferred Site"),
            ignore: get("Ignore").is_some_and(truthy) || priority < 0,
        };
        let extra = cols.iter().enumerate().filter(|(_, c)| !KNOWN.contains(&c.as_str()) && !c.is_empty()).filter_map(|(i, c)| Some((c.clone(), f.get(i)?.clone())).filter(|(_, v)| !v.is_empty())).collect();
        let row = TalkgroupRow { tg, comment: comment.join("\n"), extra };
        comment.clear();
        match at.get(&number) {
            Some(&i) => rows[i] = row,
            None => {
                at.insert(number, rows.len());
                rows.push(row);
            }
        }
    }
    out
}

/// `rows` as a talkgroup file (Trunk Recorder's headed CSV): the usual
/// columns, then Priority, Preferred NAC, Preferred Site and Ignore when a
/// row has them, then the columns kept as they came; each row's comment
/// above it. None: "".
pub fn rows_csv(rows: &[TalkgroupRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let any = |f: &dyn Fn(&TalkgroupRow) -> bool| rows.iter().any(f);
    let mut cols: Vec<String> = NEW_HEADER.split(',').map(str::to_string).collect();
    if any(&|r| r.tg.priority != 1) {
        cols.push("Priority".into());
    }
    if any(&|r| r.tg.preferred_nac != 0) {
        cols.push("Preferred NAC".into());
    }
    if any(&|r| !r.tg.preferred_site.is_empty()) {
        cols.push("Preferred Site".into());
    }
    if any(&|r| r.tg.ignore && r.tg.priority >= 0) {
        cols.push("Ignore".into());
    }
    for r in rows {
        for (k, _) in &r.extra {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
    }
    let mut out = cols.iter().map(|c| cell(c, ',')).collect::<Vec<_>>().join(",") + "\n";
    for r in rows {
        if !r.comment.is_empty() {
            out.push_str(&r.comment);
            out.push('\n');
        }
        let t = &r.tg;
        let v: Vec<String> = cols
            .iter()
            .map(|c| match c.as_str() {
                "Decimal" => t.number.to_string(),
                "Hex" => format!("{:x}", t.number),
                "Alpha Tag" => t.alpha_tag.clone(),
                "Mode" => t.mode.clone(),
                "Description" => t.description.clone(),
                "Tag" => t.tag.clone(),
                "Category" => t.group.clone(),
                "Priority" => t.priority.to_string(),
                "Preferred NAC" => if t.preferred_nac == 0 { String::new() } else { t.preferred_nac.to_string() },
                "Preferred Site" => t.preferred_site.clone(),
                "Ignore" => if t.ignore && t.priority >= 0 { "x".into() } else { String::new() },
                k => r.extra.iter().find(|(n, _)| n == k).map_or(String::new(), |(_, v)| v.clone()),
            })
            .collect();
        out.push_str(&v.iter().map(|c| cell(c, ',')).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// Talkgroup `number`'s row of `rows` with `cells` set, each (column,
/// value), the column named as Trunk Recorder names it ("Alpha Tag",
/// "Ignore"…; one it doesn't know is kept as it's given). A talkgroup
/// without a row gets one when a value is set. Clearing Ignore also undoes
/// Trunk Recorder's way of ignoring, a priority of −1. The row as it is now
/// (none: there's none).
pub fn edit_rows(rows: &mut Vec<TalkgroupRow>, number: u32, cells: &[(&str, &str)]) -> Option<TalkgroupRow> {
    let i = match rows.iter().position(|r| r.tg.number == number) {
        Some(i) => i,
        None if cells.iter().any(|(_, v)| !v.is_empty()) => {
            rows.push(TalkgroupRow { tg: Talkgroup { number, mode: "D".into(), priority: 1, ..Default::default() }, ..Default::default() });
            rows.len() - 1
        }
        None => return None,
    };
    let r = &mut rows[i];
    for &(name, v) in cells {
        let t = &mut r.tg;
        match canonical(name).as_str() {
            "Alpha Tag" => t.alpha_tag = v.into(),
            "Mode" => t.mode = v.into(),
            "Description" => t.description = v.into(),
            "Tag" => t.tag = v.into(),
            "Category" => t.group = v.into(),
            "Priority" => t.priority = v.parse().unwrap_or(1),
            "Preferred NAC" => t.preferred_nac = v.parse().unwrap_or(0),
            "Preferred Site" => t.preferred_site = v.into(),
            "Ignore" => {
                if v.is_empty() && t.priority < 0 {
                    t.priority = 1;
                }
                t.ignore = truthy(v) || t.priority < 0;
            }
            "Decimal" | "Hex" => {}
            k => {
                r.extra.retain(|(n, _)| n != k);
                if !v.is_empty() {
                    r.extra.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    Some(r.clone())
}

/// A column's name as Trunk Recorder spells it, from any case and the names
/// RadioReference's export uses.
fn canonical(h: &str) -> String {
    match h.trim().to_ascii_lowercase().as_str() {
        "decimal" | "dec" => "Decimal",
        "hex" => "Hex",
        "mode" => "Mode",
        "alpha tag" | "alphatag" => "Alpha Tag",
        "description" => "Description",
        "tag" => "Tag",
        "category" | "group" => "Category",
        "priority" => "Priority",
        "preferred nac" => "Preferred NAC",
        "preferred site" => "Preferred Site",
        "ignore" => "Ignore",
        "comment" => "Comment",
        _ => return h.trim().to_string(),
    }
    .to_string()
}

/// A legacy, headerless file's columns (7, or 8 with Priority).
const LEGACY: [&str; 8] = ["Decimal", "Hex", "Mode", "Alpha Tag", "Description", "Tag", "Category", "Priority"];
/// A new file's header.
const NEW_HEADER: &str = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category";

/// A cell in the file's delimiter: quoted when it holds the delimiter, a quote or a newline.
fn cell(v: &str, delim: char) -> String {
    if v.contains(delim) || v.contains('"') || v.contains('\n') {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A file's rows and back: comments directly above a row kept with it,
    /// others dropped; unknown columns kept; the legacy format read; an edit
    /// sets only its cells.
    #[test]
    fn rows_and_back() {
        let file = "# county file\n\nDecimal;Alpha Tag;Agency;Priority\n# dispatch\n101;\"Fire; Disp\";County;1\n102;Tac;;-1\n\n# loose\n";
        let rows = parse_rows(file);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].comment.as_str(), rows[0].tg.alpha_tag.as_str()), ("# dispatch", "Fire; Disp"));
        assert_eq!(rows[0].extra, [("Agency".to_string(), "County".to_string())]);
        assert!(rows[1].tg.ignore && rows[1].comment.is_empty(), "priority -1 ignores");
        let csv = rows_csv(&rows);
        assert_eq!(csv, "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Priority,Agency\n# dispatch\n101,65,Fire; Disp,,,,,1,County\n102,66,Tac,,,,,-1,\n");
        assert_eq!(parse_rows(&csv), rows, "the export reads back the same");
        assert_eq!(parse_csv(&csv), parse_csv(file), "and the engine reads the same talkgroups");
        let mut rows = rows;
        assert!(edit_rows(&mut rows, 102, &[("Ignore", "")]).is_some_and(|r| !r.tg.ignore && r.tg.priority == 1));
        let added = edit_rows(&mut rows, 205, &[("Alpha Tag", "Data"), ("Ignore", "x")]).unwrap();
        assert_eq!((added.tg.mode.as_str(), added.tg.ignore, rows.len()), ("D", true, 3));
        assert_eq!(edit_rows(&mut rows, 999, &[("Ignore", "")]), None, "nothing to un-ignore");
        let legacy = parse_rows("202,ca,E,Police,Tac 2,Law,City\n");
        assert_eq!((legacy[0].tg.alpha_tag.as_str(), legacy[0].tg.group.as_str()), ("Police", "City"));
        assert_eq!(rows_csv(&[]), "");
    }

    /// What a file held and what in it was passed over, as Setup shows it.
    #[test]
    fn a_file_read_says_what_it_passed_over() {
        let f = read_rows("Decimal,Alpha Tag,Agency,Comment\n101,Disp,County,main\nabc,Bad\n");
        assert_eq!((f.talkgroups.len(), f.legacy, f.skipped.clone(), f.unknown_columns.clone(), f.missing.clone()), (1, false, vec!["abc,Bad".to_string()], vec!["Agency".to_string()], vec!["Mode".to_string(), "Description".to_string()]));
        assert_eq!(f.talkgroups[0].extra, [("Agency".to_string(), "County".to_string()), ("Comment".to_string(), "main".to_string())], "a Comment column kept with its row");
        assert!(read_rows("202,ca,E,Police,Tac 2,Law,City\n").legacy);
    }

    #[test]
    fn headed_and_legacy() {
        let h = parse_csv("Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,\"Fire, Dispatch\",D,Main,Fire,County\n");
        assert_eq!(h[&101].alpha_tag, "Fire, Dispatch");
        assert_eq!(h[&101].mode, "D");
        let p = parse_csv("Decimal,Alpha Tag,Preferred NAC,Preferred Site\n101,Disp,10023,\n102,Tac,,dcfd-east\n");
        assert_eq!((p[&101].preferred_nac, p[&101].preferred_site.as_str()), (10023, ""));
        assert_eq!((p[&102].preferred_nac, p[&102].preferred_site.as_str()), (0, "dcfd-east"));
        let i = parse_csv("Decimal,Alpha Tag,Ignore\n101,Disp,\n102,Data,yes\n103,Tac,X\n");
        assert_eq!([i[&101].ignore, i[&102].ignore, i[&103].ignore], [false, true, true]);
        assert!(parse_csv("Decimal,Alpha Tag,Priority\n104,Old,-1\n")[&104].ignore);
        let l = parse_csv("202,ca,E,Police,Tac 2,Law,City,3\n");
        assert!(l[&202].encrypted_mode());
        assert_eq!(l[&202].priority, 3);
    }

    #[test]
    fn a_spreadsheets_bom_and_header_case_are_fine() {
        let t = parse_csv("\u{feff}DECIMAL,alpha tag,Mode\r\n101,Disp,D\r\n");
        assert_eq!((t[&101].alpha_tag.as_str(), t[&101].mode.as_str()), ("Disp", "D"));
    }

    #[test]
    fn semicolons_tabs_and_radioreferences_headers() {
        let s = parse_csv("Decimal;Alpha Tag;Mode;Description\n101;\"Fire; Disp\";De;Main\n");
        assert_eq!((s[&101].alpha_tag.as_str(), s[&101].mode.as_str()), ("Fire; Disp", "De"));
        assert!(!s[&101].encrypted_mode());
        let t = parse_csv("DEC\tHEX\tMode\tAlpha Tag\tDescription\tTag\tGroup\n1201\t4b1\tD\tFD Disp\tFire Dispatch\tFire Dispatch\tCounty Fire\n");
        assert_eq!((t[&1201].alpha_tag.as_str(), t[&1201].group.as_str()), ("FD Disp", "County Fire"));
    }
}
