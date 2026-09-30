//! Trunk Recorder talkgroup CSV (trunk-recorder/talkgroups.cc): the headed
//! format (first column "Decimal"; columns Decimal, Hex, Mode, Alpha Tag,
//! Description, Tag, Category, Priority, Preferred NAC in any order) or the
//! legacy headerless one: Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority].

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
    pub preferred_nac: u32,
}

impl Talkgroup {
    /// Mode letters Trunk Recorder treats as "encrypted, don't record".
    pub fn encrypted_mode(&self) -> bool {
        matches!(self.mode.as_str(), "E" | "TE" | "DE")
    }
}

pub type Talkgroups = HashMap<u32, Talkgroup>;

/// RFC-4180-ish split: commas, double-quoted fields, "" escapes.
fn split_csv_line(line: &str) -> Vec<String> {
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
        } else if ch == ',' {
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
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#')).collect();
    let mut out = Talkgroups::new();
    let Some(first) = lines.first().map(|l| split_csv_line(l)) else { return out };
    let headed = first.first().map(|s| s.as_str()) == Some("Decimal");
    let col = |name: &str| first.iter().position(|c| c == name);
    for line in if headed { &lines[1..] } else { &lines[..] } {
        let f = split_csv_line(line);
        let get = |name: &str, legacy: usize| -> Option<&str> {
            if headed {
                col(name).and_then(|i| f.get(i)).map(|s| s.as_str())
            } else {
                f.get(legacy).map(|s| s.as_str())
            }
        };
        let Some(number) = get("Decimal", 0).and_then(|s| s.parse::<u32>().ok()) else { continue };
        let s = |name: &str, legacy: usize| get(name, legacy).unwrap_or("").to_string();
        out.insert(
            number,
            Talkgroup {
                number,
                mode: s("Mode", 2),
                alpha_tag: s("Alpha Tag", 3),
                description: s("Description", 4),
                tag: s("Tag", 5),
                group: s("Category", 6),
                priority: get("Priority", 7).and_then(|v| v.parse().ok()).unwrap_or(1),
                preferred_nac: get("Preferred NAC", 99).and_then(|v| v.parse().ok()).unwrap_or(0),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headed_and_legacy() {
        let h = parse_csv("Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n101,65,\"Fire, Dispatch\",D,Main,Fire,County\n");
        assert_eq!(h[&101].alpha_tag, "Fire, Dispatch");
        assert_eq!(h[&101].mode, "D");
        let l = parse_csv("202,ca,E,Police,Tac 2,Law,City,3\n");
        assert!(l[&202].encrypted_mode());
        assert_eq!(l[&202].priority, 3);
    }
}
