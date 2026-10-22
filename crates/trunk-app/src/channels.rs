//! Conventional channels as CSV, for editing in a spreadsheet: the channel
//! file a config can link to, and the interface's import / export (the same
//! rules are in web/src/config.ts — keep them together).
//!
//! ```text
//! TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Bandwidth kHz,Enable,AutoTune
//! 1001,154.4300,,,,fm,County Fire Dispatch,,Fire Dispatch,Fire,,,true
//! ,154.3250,D223N,,,fm,County A Fire,,,Fire,,,true
//! ,154.3250,151.4,,,fm,County B Fire,,,Fire,,,true
//! ,460.1250,,,,p25,PD Tac 2,,Law Tac,Police,12,,true
//! 45351,453.5000,293,,1,p25,Townville PW,,Public Works,Public Works,,,true
//! 9001,451.5000,1,1,1,dmr,Dispatch,,Business,Business,,,true
//! ,119.1000,,,,am,DCA Tower,,Aircraft,Airport,,,true
//! ```
//!
//! - A header row names the columns, in any order, any case. Trunk Recorder's
//!   channel file reads as is; `Code`, `Slot`, `Match TG`, `Mode`, `Squelch dB`
//!   and `Bandwidth kHz` are additions.
//! - `Frequency`: MHz when it has a decimal point (and is under 10 000), else Hz.
//! - `Mode`: `fm` / `analog` / `A`, `am`, `p25` / `digital` / `D`, `dmr`,
//!   `nxdn48` (or `nxdn`) / `nxdn96`; empty = fm.
//! - `TG Number`: what calls are filed under; empty = the talkgroup the air
//!   names (p25, dmr, nxdn), else the frequency in kHz (further rows on the
//!   same frequency: that and a digit, 1543251, 1543252 …).
//! - `Code`: the code the row records, as Trunk Recorder or RadioReference
//!   write it — FM's CTCSS tone or DCS code (`151.4`, `151.4 PL`, `D023N`,
//!   `023 DPL`), P25's NAC (`293`, `293 NAC`, `$293`), DMR's colour code
//!   (`1`, `CC1`), NXDN's RAN (`5`, `RAN 5`); empty = any. A slot or
//!   talkgroup in it (`CC1 TS2 TG201`, RadioReference's) is read into
//!   `Slot` / `Match TG`. Trunk Recorder's `Tone` column reads as `Code`.
//! - `Slot`: DMR's, 1 or 2; empty = either. `Match TG`: p25, dmr, nxdn —
//!   record only transmissions naming this talkgroup; empty = any.
//!   Rows sharing a frequency split it by these (one may have none: the
//!   rest; the most specific that fits wins). With no `Mode`, a NAC means
//!   p25, a colour code dmr and a RAN nxdn48.
//! - `Squelch dB`: dB above the noise floor; empty = the section's. Trunk
//!   Recorder's `Squelch` column is an absolute level and is not read.
//! - `Bandwidth kHz`: an analog channel's filter width, 4–20; empty = the mode's.
//! - `Enable`: `false` / `no` / `0` switches a channel off; empty = on.
//! - `AutoTune`: `true` / `yes` / `1` — a digital row's calls score its
//!   source's AutoTune (a repeater output to trust); empty = off.
//! - Commas, semicolons or tabs (from the header row); a UTF-8 byte-order
//!   mark and decimal commas (with semicolons) are accepted, as Excel
//!   writes them in some locales. Blank rows and `#` comments are skipped.

use crate::config::{Channel, ChannelMode};
use trunk_core::trunk::Access;

/// The columns [`write`] produces.
pub const HEADER: &str = "TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag,Description,Tag,Category,Squelch dB,Bandwidth kHz,Enable,AutoTune,Send To";

/// What a CSV held: its channels and anything worth telling the user.
#[derive(Debug, Default)]
pub struct Parsed {
    pub channels: Vec<Channel>,
    pub notes: Vec<String>,
}

fn split(line: &str, delim: char) -> Vec<String> {
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

/// A frequency cell: MHz with a decimal point (under 10 GHz in MHz), else Hz.
fn freq(cell: &str, decimal_comma: bool) -> Option<f64> {
    let t = if decimal_comma { cell.replace(',', ".") } else { cell.to_string() };
    let v: f64 = t.trim().parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    Some(if t.contains('.') && v < 10_000.0 { (v * 1e6).round() } else { v.round() })
}

fn rows_list(rows: &[usize]) -> String {
    let shown: Vec<String> = rows.iter().take(8).map(|r| r.to_string()).collect();
    format!("{}{}", shown.join(", "), if rows.len() > 8 { format!(" and {} more", rows.len() - 8) } else { String::new() })
}

pub fn parse(text: &str) -> Result<Parsed, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // (1-based row number, line)
    let lines: Vec<(usize, &str)> =
        text.lines().enumerate().map(|(i, l)| (i + 1, l)).filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#')).collect();
    let Some(&(_, head_line)) = lines.first() else { return Err("The channel file is empty.".into()) };
    let delim = [',', ';', '\t'].into_iter().max_by_key(|d| head_line.matches(*d).count()).unwrap_or(',');
    let decimal_comma = delim != ',';
    let head: Vec<String> = split(head_line, delim).into_iter().map(|h| h.to_lowercase()).collect();
    let col = |names: &[&str]| head.iter().position(|h| names.contains(&h.as_str()));
    let Some(c_freq) = col(&["frequency", "freq", "freqhz"]) else {
        return Err("No Frequency column: the first row must name the columns (TG Number, Frequency, Mode, Alpha Tag, …).".into());
    };
    let c_tg = col(&["tg number", "talkgroup", "tg"]);
    let c_name = col(&["alpha tag", "name"]);
    let c_desc = col(&["description"]);
    let c_tag = col(&["tag"]);
    let c_group = col(&["category", "group"]);
    let c_mode = col(&["mode"]);
    let c_sq = col(&["squelch db", "squelchdb"]);
    let c_bw = col(&["bandwidth khz", "bandwidthkhz", "bandwidth"]);
    let c_tr_sq = col(&["squelch"]);
    let c_enable = col(&["enable", "enabled"]);
    let c_tune = col(&["autotune", "auto tune"]);
    let c_code = col(&["code"]);
    // Before Code, Slot and Match TG (and Trunk Recorder's): one Tone column.
    let old = c_code.is_none() && col(&["tone"]).is_some();
    let c_tone = c_code.or(col(&["tone"]));
    let c_slot = col(&["slot", "ts"]);
    let c_mtg = col(&["match tg", "matchtalkgroup", "match talkgroup"]);
    let c_send = col(&["send to", "sendto", "plugins"]);

    let mut out = Parsed::default();
    let (mut bad_freq, mut bad_mode, mut bad_tg, mut bad_sq, mut bad_bw) = (vec![], vec![], vec![], vec![], vec![]);
    let (mut bad_tone, mut search) = (vec![], false);
    for &(row, line) in &lines[1..] {
        let f = split(line, delim);
        let at = |c: Option<usize>| c.and_then(|i| f.get(i)).map(String::as_str).unwrap_or("");
        let Some(freq_hz) = freq(at(Some(c_freq)), decimal_comma) else {
            bad_freq.push(row);
            continue;
        };
        let raw = at(c_tone);
        let mode = match at(c_mode).to_lowercase().as_str() {
            "" if raw.to_uppercase().contains("NAC") || raw.starts_with('$') => ChannelMode::P25,
            "" if raw.to_uppercase().starts_with("CC") => ChannelMode::Dmr,
            "" if raw.to_uppercase().starts_with("RAN") => ChannelMode::Nxdn48,
            "" | "fm" | "nfm" | "analog" | "a" => ChannelMode::Fm,
            "am" => ChannelMode::Am,
            "p25" | "digital" | "d" => ChannelMode::P25,
            "dmr" => ChannelMode::Dmr,
            "nxdn" | "nxdn48" => ChannelMode::Nxdn48,
            "nxdn96" => ChannelMode::Nxdn96,
            _ => {
                bad_mode.push(row);
                ChannelMode::Fm
            }
        };
        let talkgroup = match at(c_tg) {
            "" => None,
            s => match s.parse::<u32>() {
                Ok(v) if v > 0 => Some(v),
                _ => {
                    bad_tg.push(row);
                    None
                }
            },
        };
        let squelch_db = match at(c_sq) {
            "" => None,
            s => match (if decimal_comma { s.replace(',', ".") } else { s.to_string() }).parse::<f64>() {
                Ok(v) if (3.0..=40.0).contains(&v) => Some(v),
                _ => {
                    bad_sq.push(row);
                    None
                }
            },
        };
        let (lo, hi) = crate::config::BANDWIDTH_KHZ;
        let bandwidth_khz = match at(c_bw) {
            "" => None,
            s => match (if decimal_comma { s.replace(',', ".") } else { s.to_string() }).parse::<f64>() {
                Ok(v) if (lo..=hi).contains(&v) && matches!(mode, ChannelMode::Fm | ChannelMode::Am) => Some(v),
                _ => {
                    bad_bw.push(row);
                    None
                }
            },
        };
        let enabled = !matches!(at(c_enable).to_lowercase().as_str(), "false" | "no" | "0" | "off");
        let auto_tune = matches!(at(c_tune).to_lowercase().as_str(), "true" | "yes" | "1" | "on");
        search |= raw.eq_ignore_ascii_case("s");
        let conv = mode.conv();
        let (slot, mtg) = (at(c_slot), at(c_mtg));
        let num = |s: &str| if s.is_empty() { Ok(None) } else { s.parse::<u32>().map(Some).map_err(|_| format!("\"{s}\" isn't a number")) };
        let access = num(slot)
            .and_then(|s| Ok((s.map(|s| u8::try_from(s).unwrap_or(u8::MAX)), num(mtg)?)))
            .and_then(|(s, t)| Access::join(conv, raw, s, t));
        let (code, slot, match_talkgroup) = match access {
            Ok(a) => a.map_or((String::new(), None, None), Access::parts),
            Err(e) => {
                bad_tone.push(format!("row {row}: {e}"));
                (String::new(), None, None)
            }
        };
        out.channels.push(Channel {
            freq_hz,
            mode,
            name: at(c_name).to_string(),
            talkgroup,
            description: at(c_desc).to_string(),
            tag: at(c_tag).to_string(),
            group: at(c_group).to_string(),
            squelch_db,
            bandwidth_khz,
            code,
            slot,
            match_talkgroup,
            enabled,
            auto_tune,
            send_to: crate::config::send_to(at(c_send)),
        });
    }
    if old {
        let n = crate::config::keep_p25_numbers(&mut out.channels);
        if n > 0 {
            out.notes.push(format!("{n} P25 row(s) sharing a frequency had no TG Number: given the one their calls were filed under before (a row without one now files calls under the talkgroup the air names)."));
        }
    }
    if !bad_freq.is_empty() {
        out.notes.push(format!("Skipped row(s) {} — no usable frequency.", rows_list(&bad_freq)));
    }
    if !bad_mode.is_empty() {
        out.notes.push(format!("Row(s) {}: unknown Mode (use fm, am, p25, dmr, nxdn48 or nxdn96) — read as fm.", rows_list(&bad_mode)));
    }
    if !bad_tg.is_empty() {
        out.notes.push(format!("Row(s) {}: TG Number isn't a positive whole number — using the default.", rows_list(&bad_tg)));
    }
    if !bad_sq.is_empty() {
        out.notes.push(format!("Row(s) {}: Squelch dB must be 3–40 (dB above the noise) — using the default.", rows_list(&bad_sq)));
    }
    if !bad_bw.is_empty() {
        out.notes.push(format!("Row(s) {}: Bandwidth kHz must be 4–20, on an fm or am channel — using the default.", rows_list(&bad_bw)));
    }
    if !bad_tone.is_empty() {
        out.notes.push(format!("Code, Slot or Match TG not read (the row records any): {}.", bad_tone.join("; ")));
    }
    if search {
        out.notes.push("Tone S (search) needs no setting here: every analog call's tone is identified and written to its JSON.".into());
    }
    if c_tr_sq.is_some() && c_sq.is_none() {
        out.notes.push("The Squelch column (Trunk Recorder's absolute level) was not read: use Squelch dB, in dB above the noise floor.".into());
    }
    Ok(out)
}

/// MHz with at least 4 and at most 6 decimals (154.4300, 154.43125).
pub fn mhz(hz: f64) -> String {
    let s = format!("{:.6}", hz / 1e6);
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let frac = frac.trim_end_matches('0');
    format!("{int}.{frac:0<4}")
}

fn cell(s: &str) -> String {
    if s.contains([',', '"', '\n', ';']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The channels as CSV ([`HEADER`]'s columns), which [`parse`] reads back unchanged.
pub fn write(channels: &[Channel]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for c in channels {
        let row = [
            c.talkgroup.map_or(String::new(), |t| t.to_string()),
            mhz(c.freq_hz),
            cell(&c.code),
            c.slot.map_or(String::new(), |s| s.to_string()),
            c.match_talkgroup.map_or(String::new(), |t| t.to_string()),
            match c.mode {
                ChannelMode::Fm => "fm".into(),
                ChannelMode::Am => "am".into(),
                ChannelMode::P25 => "p25".into(),
                ChannelMode::Dmr => "dmr".into(),
                ChannelMode::Nxdn48 => "nxdn48".into(),
                ChannelMode::Nxdn96 => "nxdn96".into(),
            },
            cell(&c.name),
            cell(&c.description),
            cell(&c.tag),
            cell(&c.group),
            c.squelch_db.map_or(String::new(), |v| v.to_string()),
            c.bandwidth_khz.map_or(String::new(), |v| v.to_string()),
            c.enabled.to_string(),
            if c.auto_tune { "true".into() } else { String::new() },
            cell(&crate::config::send_to_cell(&c.send_to)),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trunk_recorder_channel_file() {
        let p = parse(
            "TG Number,Frequency,Tone,Alpha Tag,Description,Tag,Category,Enable,Signal Detector,Squelch\n\
             300,462275000,94.8,Town A Police,Town A Police Dispatch,Police,Town A,,false,\n\
             325,462.2875,151.4,Town B DPW,\"Trash, Recycling\",DPW,Town B,false,,-50\n\
             ,oops,,,,,,,,\n\
             326,462.2875,023 DPL,Town C,,,,,,\n\
             327,462.2875,S,Town D,,,,,,\n\
             328,462.3,151.5,Town E,,,,,,\n\
             329,460.5,293 NAC,PD North,,,,,,\n\
             330,452.1,CC1 TS2 TG201,Ops,,,,,,\n",
        )
        .unwrap();
        assert_eq!(p.channels.len(), 7);
        let codes: Vec<_> = p.channels.iter().map(|c| (c.code.as_str(), c.slot, c.match_talkgroup)).collect();
        assert_eq!(codes, [("94.8", None, None), ("151.4", None, None), ("D023N", None, None), ("", None, None), ("", None, None), ("293", None, None), ("1", Some(2), Some(201))]);
        assert_eq!((p.channels[5].mode, p.channels[6].mode), (ChannelMode::P25, ChannelMode::Dmr));
        let (a, b) = (&p.channels[0], &p.channels[1]);
        assert_eq!((a.freq_hz, a.talkgroup, a.mode, a.enabled), (462_275_000.0, Some(300), ChannelMode::Fm, true));
        assert_eq!(a.name, "Town A Police");
        assert_eq!((b.freq_hz, b.enabled, b.description.as_str()), (462_287_500.0, false, "Trash, Recycling"));
        let notes = p.notes.join(" ");
        assert!(notes.contains("row(s) 4"), "{notes}");
        assert!(notes.contains("row 7: 151.5 Hz isn't a standard CTCSS tone — 151.4?"), "{notes}");
        assert!(notes.contains("Tone S (search)"), "{notes}");
        assert!(notes.contains("Squelch column"), "{notes}");
    }

    #[test]
    fn send_to_spellings() {
        let p = parse("Frequency,Alpha Tag,Send To\n154.43,Both,\n155.1,Rdio only,Rdio Scanner\n154.25,Neither,none\n153.8,Two,\"OpenMHz; rdio-scanner#2\"\n").unwrap();
        let to: Vec<Option<Vec<String>>> = p.channels.iter().map(|c| c.send_to.clone()).collect();
        assert_eq!(to[0], None);
        assert_eq!(to[1], Some(vec!["rdioscanner".to_string()]));
        assert_eq!(to[2], Some(vec![]));
        assert_eq!(to[3], Some(vec!["openmhz".to_string(), "rdioscanner#2".to_string()]));
    }

    #[test]
    fn round_trip_and_excel_variants() {
        let chans = vec![
            Channel {
                freq_hz: 154_431_250.0,
                mode: ChannelMode::Fm,
                name: "Fire, Main".into(),
                talkgroup: Some(1001),
                description: "Say \"hi\"".into(),
                tag: String::new(),
                group: "Fire".into(),
                squelch_db: Some(10.5),
                bandwidth_khz: None,
                code: "D023N".into(),
                slot: None,
                match_talkgroup: None,
                enabled: true, auto_tune: false,
                send_to: None,
            },
            Channel {
                freq_hz: 460_125_000.0,
                mode: ChannelMode::P25,
                name: "PD Tac".into(),
                talkgroup: None,
                description: String::new(),
                tag: String::new(),
                group: String::new(),
                squelch_db: None,
                bandwidth_khz: None,
                code: "293".into(),
                slot: None,
                match_talkgroup: Some(1),
                enabled: false, auto_tune: false,
                send_to: Some(vec!["rdioscanner".into()]),
            },
            Channel {
                freq_hz: 451_500_000.0,
                mode: ChannelMode::Dmr,
                name: "Dispatch".into(),
                talkgroup: Some(9001),
                description: String::new(),
                tag: String::new(),
                group: String::new(),
                squelch_db: None,
                bandwidth_khz: None,
                code: "1".into(),
                slot: Some(1),
                match_talkgroup: Some(1),
                enabled: true, auto_tune: false,
                send_to: None,
            },
        ];
        let csv = write(&chans);
        assert!(csv.contains("1001,154.43125,D023N,,,fm,\"Fire, Main\""), "{csv}");
        assert!(csv.contains(",460.1250,293,,1,p25,PD Tac,"), "{csv}");
        assert!(csv.contains("9001,451.5000,1,1,1,dmr,Dispatch,"), "{csv}");
        let back = parse(&csv).unwrap();
        assert_eq!(back.channels, chans);
        assert!(back.notes.is_empty(), "{:?}", back.notes);
        // Excel in a decimal-comma locale: semicolons, a byte-order mark, 154,43.
        let eu = parse("\u{feff}Frequency;Mode;Alpha Tag;Squelch dB\r\n154,43;P25;Tac;12,5\r\n").unwrap();
        assert_eq!(eu.channels[0].freq_hz, 154_430_000.0);
        assert_eq!(eu.channels[0].mode, ChannelMode::P25);
        assert_eq!(eu.channels[0].squelch_db, Some(12.5));
        assert!(parse("Name,Mode\nx,fm\n").unwrap_err().contains("No Frequency column"));
    }

    /// Code, Slot and Match TG; a slot named twice must agree; an older file
    /// (a Tone column) keeps the numbers its P25 rows were filed under.
    /// AutoTune: yes / true / 1 / on asks it; written only when asked.
    #[test]
    fn autotune_column() {
        let p = parse("Frequency,Mode,AutoTune\n453.375,p25,yes\n451.75,p25,\n453.225,fm,false\n").unwrap();
        assert_eq!(p.channels.iter().map(|c| c.auto_tune).collect::<Vec<_>>(), [true, false, false]);
        let out = write(&p.channels);
        assert!(out.lines().next().unwrap().ends_with(",Enable,AutoTune,Send To"), "{out}");
        assert!(out.contains("\n,453.3750,,,,p25,,,,,,,true,true,\n") && out.contains("\n,451.7500,,,,p25,,,,,,,true,,\n"), "{out}");
        assert_eq!(parse(&out).unwrap().channels.iter().map(|c| c.auto_tune).collect::<Vec<_>>(), [true, false, false]);
    }

    #[test]
    fn code_slot_and_match_tg() {
        let p = parse("Frequency,Mode,Code,Slot,Match TG\n451.5,dmr,CC1 TS2,2,201\n451.6,dmr,CC1 TS2,1,\n453.5,p25,293,,1\n154.43,fm,,,5\n").unwrap();
        let got: Vec<_> = p.channels.iter().map(|c| (c.code.as_str(), c.slot, c.match_talkgroup)).collect();
        assert_eq!(got, [("1", Some(2), Some(201)), ("", None, None), ("293", None, Some(1)), ("", None, None)]);
        let notes = p.notes.join(" ");
        assert!(notes.contains("row 3: \"CC1 TS2\" names another slot") && notes.contains("row 5: Match TG 5: FM names no talkgroup"), "{notes}");
        let old = parse("Frequency,Mode,Tone\n453.5,p25,293 NAC\n453.5,p25,4F1\n460.125,p25,293\n").unwrap();
        assert_eq!(old.channels.iter().map(|c| c.talkgroup).collect::<Vec<_>>(), [Some(453500), Some(4535001), None]);
        assert!(old.notes.iter().any(|n| n.contains("2 P25 row(s)")), "{:?}", old.notes);
        // The same columns from the new writer: nothing written in.
        let new = parse("Frequency,Mode,Code\n453.5,p25,293\n453.5,p25,4F1\n").unwrap();
        assert_eq!(new.channels.iter().map(|c| c.talkgroup).collect::<Vec<_>>(), [None, None]);
    }

    #[test]
    fn am_and_bandwidth_round_trip() {
        let p = parse("Frequency,Mode,Alpha Tag,Bandwidth kHz\n119.1,am,Tower,\n128.1,AM,Approach,6\n154.43,fm,Fire,16\n460.125,p25,PD,16\n").unwrap();
        let got: Vec<_> = p.channels.iter().map(|c| (c.mode, c.bandwidth_khz)).collect();
        assert_eq!(got, [(ChannelMode::Am, None), (ChannelMode::Am, Some(6.0)), (ChannelMode::Fm, Some(16.0)), (ChannelMode::P25, None)]);
        assert!(p.notes.iter().any(|n| n.contains("Row(s) 5: Bandwidth")), "{:?}", p.notes);
        let back = parse(&write(&p.channels)).unwrap();
        assert_eq!(back.channels, p.channels);
    }
}
