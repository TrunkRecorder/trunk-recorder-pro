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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Talkgroup {
    pub number: u32,
    pub mode: String,
    pub alpha_tag: String,
    pub description: String,
    pub tag: String,
    pub group: String,
    pub priority: i32,
    /// Multi-site: the site whose copy of a call to keep, as Trunk Recorder
    /// writes it — its NAC, or RFSS and site as `RRRRssss` (1 and 23: 10023).
    pub preferred_nac: u32,
    /// Multi-site: the same by the site's short name.
    pub preferred_site: String,
    /// Never record it (the Ignore column, or priority −1).
    pub ignore: bool,
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

fn is_data(l: &str) -> bool {
    !l.trim().is_empty() && !l.trim_start().starts_with('#')
}

/// A cell in the file's delimiter: quoted when it holds the delimiter, a quote or a newline.
fn cell(v: &str, delim: char) -> String {
    if v.contains(delim) || v.contains('"') || v.contains('\n') {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// Each line of a talkgroup file (`csv`, as `str::lines` splits it): the
/// talkgroup number it's the row of, or none (the header, a comment, a
/// blank line), as [`parse_csv`] reads it.
pub fn line_numbers(csv: &str) -> Vec<Option<u32>> {
    let text = csv.strip_prefix('\u{feff}').unwrap_or(csv);
    let delim = csv_lines(text).next().map_or(',', csv_delimiter);
    text.lines().map(|l| if is_data(l) { split_csv_on(l, delim).first().and_then(|d| d.parse().ok()) } else { None }).collect()
}

/// Talkgroup `number`'s row of a talkgroup file (`csv`) with `cells` set,
/// each (column, value), the column named as Trunk Recorder names it
/// ("Alpha Tag", "Ignore"…). Only those cells change, in that row: the
/// file's other rows, comments, columns and delimiter stay as they are. A
/// column it lacks is added; a legacy headerless file is given its header;
/// a talkgroup it lacks gets a row when a value is set. Clearing Ignore
/// also undoes Trunk Recorder's way of ignoring, a priority of −1.
/// (web/src/talkgroups.ts `withCells` is the same edit, made in the browser.)
pub fn edit_row(csv: &str, number: u32, cells: &[(&str, &str)]) -> String {
    let text = csv.strip_prefix('\u{feff}').unwrap_or(csv);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let hi = match lines.iter().position(|l| is_data(l)) {
        Some(hi) => hi,
        None => {
            lines.push(NEW_HEADER.to_string());
            lines.len() - 1
        }
    };
    let delim = csv_delimiter(&lines[hi]);
    if canonical(split_csv_on(&lines[hi], delim).first().map_or("", |s| s.as_str())) != "Decimal" {
        let width = lines.iter().filter(|l| is_data(l)).map(|l| split_csv_on(l, delim).len()).max().unwrap_or(7);
        lines.insert(hi, LEGACY[..width.clamp(7, 8)].join(&delim.to_string()));
    }
    let mut header = split_csv_on(&lines[hi], delim);
    let mut cols: Vec<String> = header.iter().map(|h| canonical(h)).collect();
    let mut at = Vec::new();
    for (name, value) in cells {
        let i = cols.iter().position(|c| c == name).unwrap_or_else(|| {
            header.push(name.to_string());
            cols.push(name.to_string());
            cols.len() - 1
        });
        at.push((i, *value));
    }
    lines[hi] = header.iter().map(|h| cell(h, delim)).collect::<Vec<_>>().join(&delim.to_string());
    let priority = cols.iter().position(|c| c == "Priority");
    let unignore = cells.iter().any(|&(n, v)| n == "Ignore" && v.is_empty());
    let mut found = false;
    for line in lines.iter_mut().skip(hi + 1).filter(|l| is_data(l)) {
        let mut row = split_csv_on(line, delim);
        if row.first().and_then(|d| d.parse::<u32>().ok()) != Some(number) {
            continue;
        }
        found = true;
        row.resize(row.len().max(cols.len()), String::new());
        for &(i, v) in &at {
            row[i] = v.to_string();
        }
        if let Some(p) = priority.filter(|_| unignore) {
            if row[p].parse::<i32>().is_ok_and(|v| v < 0) {
                row[p] = "1".into();
            }
        }
        *line = row.iter().map(|c| cell(c, delim)).collect::<Vec<_>>().join(&delim.to_string());
    }
    if !found && cells.iter().any(|(_, v)| !v.is_empty()) {
        let row: Vec<String> = cols
            .iter()
            .map(|c| match c.as_str() {
                "Decimal" => number.to_string(),
                "Hex" => format!("{number:x}"),
                "Mode" => "D".to_string(),
                c => at.iter().find(|&&(i, _)| cols[i] == c).map_or(String::new(), |&(_, v)| v.to_string()),
            })
            .collect();
        lines.push(row.iter().map(|c| cell(c, delim)).collect::<Vec<_>>().join(&delim.to_string()));
    }
    lines.join("\n") + "\n"
}

/// Talkgroup `number` as file `csv` has it (its header and that row read alone).
pub fn row_of(csv: &str, number: u32) -> Option<Talkgroup> {
    let mut lines = csv_lines(csv);
    let head = lines.next()?;
    let delim = csv_delimiter(head);
    let line = std::iter::once(head).chain(lines).find(|l| split_csv_on(l, delim).first().and_then(|d| d.parse::<u32>().ok()) == Some(number))?;
    let one = if line == head { line.to_string() } else { format!("{head}\n{line}") };
    parse_csv(&one).remove(&number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_edited_in_place() {
        let csv = "# county\nDecimal;Alpha Tag;Description\n101;Fire Disp;Main\n102;\"Tac; 2\";\n";
        let out = edit_row(csv, 102, &[("Ignore", "x")]);
        assert_eq!(out, "# county\nDecimal;Alpha Tag;Description;Ignore\n101;Fire Disp;Main\n102;\"Tac; 2\";;x\n", "only that row; the comment, delimiter and column kept");
        assert!(row_of(&out, 102).unwrap().ignore && !row_of(&out, 101).unwrap().ignore);
        let named = edit_row(&out, 101, &[("Alpha Tag", "FIRE4A"), ("Tag", "Fire, Tac")]);
        assert_eq!(row_of(&named, 101).map(|t| (t.alpha_tag, t.tag)), Some(("FIRE4A".into(), "Fire, Tac".into())));
        assert_eq!(row_of(&named, 102).unwrap().alpha_tag, "Tac; 2");
        // A talkgroup the file lacks: a row, when something is set; none to un-ignore it.
        let added = edit_row("Decimal,Alpha Tag\n101,A\n", 205, &[("Ignore", "x"), ("Alpha Tag", "Data")]);
        assert_eq!(added, "Decimal,Alpha Tag,Ignore\n101,A\n205,Data,x\n");
        assert_eq!(edit_row("Decimal,Alpha Tag\n101,A\n", 205, &[("Ignore", "")]), "Decimal,Alpha Tag,Ignore\n101,A\n");
        // Trunk Recorder's priority −1, undone with Ignore; a legacy file given its header; none at all.
        let tr = edit_row("Decimal,Alpha Tag,Priority\n104,Old,-1\n", 104, &[("Ignore", "")]);
        assert!(!row_of(&tr, 104).unwrap().ignore);
        let legacy = edit_row("202,ca,E,Police,Tac 2,Law,City\n", 202, &[("Alpha Tag", "PD Tac")]);
        assert_eq!(legacy, "Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category\n202,ca,E,PD Tac,Tac 2,Law,City\n");
        assert_eq!(edit_row("", 7, &[("Ignore", "x")]), "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category,Ignore\n7,7,,D,,,,x\n");
    }

    #[test]
    fn each_line_s_number() {
        assert_eq!(line_numbers("\u{feff}# county\nDecimal;Alpha Tag\n\n101;Fire\nx;y\n"), [None, None, None, Some(101), None]);
        assert_eq!(line_numbers("202,ca,E\n203,cb,D"), [Some(202), Some(203)]);
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
