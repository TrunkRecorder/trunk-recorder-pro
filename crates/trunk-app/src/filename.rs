//! Where a call's files go: Trunk Recorder's `filenameFormat`. The format is
//! a path under the recordings folder made of `{token}`s, with `/` making
//! folders; `-call_<call number>` is added to the name, as Trunk Recorder does.
//!
//! Tokens: `{talkgroup}`, `{talkgroup_tag}` (the tag, "Fire Dispatch"),
//! `{talkgroup_alpha_tag}`, `{talkgroup_description}`, `{talkgroup_group}`,
//! `{talkgroup_display}`, `{short_name}`, `{freq}` (Hz), `{freq_mhz}`,
//! `{call_num}`, `{tdma_slot}` (empty when it has none), `{sys_num}`,
//! `{epoch}`, `{source_num}`, `{recorder_num}`, `{audio_type}`,
//! `{emergency}`, `{encrypted}`, `{priority}`, `{signal}`, `{noise}`,
//! `{color_code}`; and the start time, `{time:FORMAT}` in local time or
//! `{ztime:FORMAT}` in UTC, FORMAT being strftime's (`%Y %m %d %H %M %S`,
//! `%f` for milliseconds, `%-m` for no padding, …) or `iso` / `iso_ms`. Text from the talkgroup
//! file has `\ / : * ? " < > |` and spaces made `_`.

use serde_json::Value;

/// The default layout: `<short name>/<year>/<month>/<day>/<base name>`, local time.
pub fn default_path(short_name: &str, base_name: &str, start_s: i64, utc_offset_s: i32) -> String {
    let (y, m, d) = civil(start_s + utc_offset_s as i64).0;
    format!("{short_name}/{y}/{m}/{d}/{base_name}")
}

/// The path for a call (`record`: its JSON) by `format`, relative to the
/// recordings folder and without an extension; `sys_num` is its system's
/// number, `utc_offset_s` local time's offset at its start.
pub fn render(format: &str, record: &Value, sys_num: u16, utc_offset_s: i32) -> String {
    let mut out = String::new();
    let mut rest = format.trim();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        out.push_str(&token(&rest[open + 1..open + close], record, sys_num, utc_offset_s));
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    // Folders: none empty, none climbing out of the recordings folder.
    let path: Vec<&str> = out.split(['/', '\\']).map(str::trim).filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
    let num = record["call_num"].as_u64().unwrap_or(0);
    format!("{}-call_{num}", if path.is_empty() { "call".to_string() } else { path.join("/") })
}

/// Why a format can't be used, or None. Unknown tokens are named.
pub fn problem(format: &str) -> Option<String> {
    let mut rest = format;
    let mut unknown = Vec::new();
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else { return Some("A { has no closing }.".into()) };
        let t = &rest[open + 1..open + close];
        let known = t.starts_with("time:") || t.starts_with("ztime:") || TOKENS.contains(&t);
        if !known {
            unknown.push(format!("{{{t}}}"));
        }
        rest = &rest[open + close + 1..];
    }
    (!unknown.is_empty()).then(|| format!("Unknown token{}: {}.", if unknown.len() == 1 { "" } else { "s" }, unknown.join(", ")))
}

const TOKENS: &[&str] = &[
    "talkgroup",
    "talkgroup_tag",
    "talkgroup_alpha_tag",
    "talkgroup_description",
    "talkgroup_group",
    "talkgroup_display",
    "short_name",
    "freq",
    "freq_mhz",
    "call_num",
    "tdma_slot",
    "sys_num",
    "epoch",
    "source_num",
    "recorder_num",
    "audio_type",
    "emergency",
    "encrypted",
    "priority",
    "signal",
    "noise",
    "color_code",
];

fn token(t: &str, r: &Value, sys_num: u16, utc_offset_s: i32) -> String {
    let num = |k: &str| r[k].as_i64().unwrap_or(0).to_string();
    let text = |k: &str| clean(r[k].as_str().unwrap_or(""));
    let start_ms = r["start_time_ms"].as_i64().unwrap_or_else(|| r["start_time"].as_i64().unwrap_or(0) * 1000);
    if let Some(f) = t.strip_prefix("time:") {
        return clean_time(&strftime(f, start_ms, utc_offset_s, false));
    }
    if let Some(f) = t.strip_prefix("ztime:") {
        return clean_time(&strftime(f, start_ms, 0, true));
    }
    match t {
        "talkgroup" | "talkgroup_display" => num("talkgroup"),
        // Trunk Recorder's JSON names the alpha tag talkgroup_tag, and the tag talkgroup_group_tag.
        "talkgroup_tag" => text("talkgroup_group_tag"),
        "talkgroup_alpha_tag" => text("talkgroup_tag"),
        "talkgroup_description" => text("talkgroup_description"),
        "talkgroup_group" => text("talkgroup_group"),
        "short_name" => text("short_name"),
        "freq" => num("freq"),
        "freq_mhz" => format!("{:.4}", r["freq"].as_f64().unwrap_or(0.0) / 1e6),
        "call_num" => num("call_num"),
        "tdma_slot" => {
            let slotted = r["phase2_tdma"].as_i64().unwrap_or(0) != 0 || r["color_code"].as_i64().unwrap_or(-1) >= 0;
            if slotted {
                num("tdma_slot")
            } else {
                String::new()
            }
        }
        "sys_num" => sys_num.to_string(),
        "epoch" => num("start_time"),
        "source_num" | "recorder_num" | "emergency" | "encrypted" | "priority" | "signal" | "noise" | "color_code" => num(t),
        "audio_type" => text("audio_type"),
        _ => format!("{{{t}}}"),
    }
}

/// Text from the talkgroup file, safe in a file name.
fn clean(s: &str) -> String {
    s.trim().chars().map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_whitespace() { '_' } else { c }).collect()
}

/// A formatted time may make folders (`%Y/%m`), but nothing else unsafe.
/// `:` (`iso`, `%H:%M`) becomes `-`: on Windows it would name an alternate
/// data stream, and the call would vanish.
fn clean_time(s: &str) -> String {
    s.chars().map(|c| match c {
        ':' => '-',
        '\\' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
        c => c,
    }).collect()
}

/// (year, month, day), (hour, minute, second), weekday 0 = Sunday, day of the year 1..
type Civil = ((i64, u32, u32), (u32, u32, u32), u32, u32);

/// Unix time `t` as a calendar date and time.
fn civil(t: i64) -> Civil {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400) as u32;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    const BEFORE: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let yday = BEFORE[m as usize - 1] + d + u32::from(leap && m > 2);
    let wday = (days + 4).rem_euclid(7) as u32;
    ((y, m, d), (secs / 3600, secs / 60 % 60, secs % 60), wday, yday)
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// strftime's common specifiers, `%f` (milliseconds) and the `iso` / `iso_ms` presets.
fn strftime(f: &str, ms: i64, utc_offset_s: i32, utc: bool) -> String {
    let z = if utc { "Z" } else { "" };
    match f {
        "iso" => return strftime("%Y-%m-%dT%H:%M:%S", ms, utc_offset_s, utc) + z,
        "iso_ms" => return strftime("%Y-%m-%dT%H:%M:%S.%f", ms, utc_offset_s, utc) + z,
        _ => {}
    }
    let ((y, mo, d), (h, mi, s), wday, yday) = civil(ms.div_euclid(1000) + utc_offset_s as i64);
    let mut out = String::new();
    let mut chars = f.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // `%-m`: without the padding (glibc's flag).
        let mut next = chars.next();
        let bare = next == Some('-');
        if bare {
            next = chars.next();
        }
        let start = out.len();
        match next {
            Some('Y') => out += &y.to_string(),
            Some('y') => out += &format!("{:02}", y.rem_euclid(100)),
            Some('m') => out += &format!("{mo:02}"),
            Some('d') => out += &format!("{d:02}"),
            Some('e') => out += &format!("{d:>2}"),
            Some('H') => out += &format!("{h:02}"),
            Some('I') => out += &format!("{:02}", if h % 12 == 0 { 12 } else { h % 12 }),
            Some('p') => out += if h < 12 { "AM" } else { "PM" },
            Some('M') => out += &format!("{mi:02}"),
            Some('S') => out += &format!("{s:02}"),
            Some('f') => out += &format!("{:03}", ms.rem_euclid(1000)),
            Some('j') => out += &format!("{yday:03}"),
            Some('a') => out += &DAYS[wday as usize][..3],
            Some('A') => out += DAYS[wday as usize],
            Some('b') | Some('h') => out += &MONTHS[mo as usize - 1][..3],
            Some('B') => out += MONTHS[mo as usize - 1],
            Some('F') => out += &format!("{y}-{mo:02}-{d:02}"),
            Some('T') => out += &format!("{h:02}:{mi:02}:{s:02}"),
            Some('s') => out += &ms.div_euclid(1000).to_string(),
            Some('z') => {
                let o = if utc { 0 } else { utc_offset_s };
                out += &format!("{}{:02}{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 3600, o.abs() / 60 % 60);
            }
            Some('Z') => out += if utc { "UTC" } else { "" },
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                if bare {
                    out.push('-');
                }
                out.push(o);
                continue;
            }
            None => {
                out.push('%');
                continue;
            }
        }
        if bare && matches!(next, Some('y' | 'm' | 'd' | 'e' | 'H' | 'I' | 'M' | 'S' | 'j')) {
            let v = out[start..].trim_start_matches([' ', '0']).to_string();
            out.truncate(start);
            out += if v.is_empty() { "0" } else { &v };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call() -> Value {
        // 2025-11-21 21:19:39.250 UTC.
        json!({ "call_num": 42, "freq": 851012500, "talkgroup": 12345, "talkgroup_tag": "FD Disp", "talkgroup_group_tag": "Fire Dispatch",
                "talkgroup_description": "Fire / EMS: Main", "talkgroup_group": "Fire", "short_name": "dcsys", "start_time": 1763759979,
                "start_time_ms": 1763759979250i64, "tdma_slot": 0, "phase2_tdma": 0, "color_code": -1, "audio_type": "digital", "emergency": 0 })
    }

    #[test]
    fn trunk_recorders_examples() {
        let c = call();
        // Local time 5 hours behind.
        let est = -5 * 3600;
        assert_eq!(
            render("{short_name}/{time:%Y}/{time:%m}/{time:%d}/{talkgroup}-{talkgroup_alpha_tag}-{epoch}_{freq}", &c, 0, est),
            "dcsys/2025/11/21/12345-FD_Disp-1763759979_851012500-call_42"
        );
        assert_eq!(
            render("{short_name}/{ztime:%Y-%m-%d}/{talkgroup_group}/{talkgroup}-{ztime:iso}_{freq_mhz}", &c, 0, est),
            "dcsys/2025-11-21/Fire/12345-2025-11-21T21-19-39Z_851.0125-call_42"
        );
        assert_eq!(
            render("{short_name}/{ztime:%Y-%m-%d}/{talkgroup}-{ztime:%Y-%m-%dT%H%M%S.%fZ}_{freq}", &c, 3, est),
            "dcsys/2025-11-21/12345-2025-11-21T211939.250Z_851012500-call_42"
        );
        assert_eq!(render("{talkgroup_tag}/{talkgroup_description}/{tdma_slot}x{sys_num}", &c, 3, 0), "Fire_Dispatch/Fire___EMS__Main/x3-call_42");
        assert_eq!(render("{time:%Y/%m}/../{time:%a %b %e %j %I%p %z}", &c, 0, est), "2025/11/Fri Nov 21 325 04PM -0500-call_42");
        assert_eq!(default_path("dcsys", "12345-1763759979_851012500", 1763759979, est), "dcsys/2025/11/21/12345-1763759979_851012500");
        // Just after midnight UTC is still the day before in New York.
        assert_eq!(default_path("x", "b", 1763769600, est), "x/2025/11/21/b");
        assert_eq!(default_path("x", "b", 1709164800, 0), "x/2024/2/29/b");
    }

    #[test]
    fn no_padding_gives_the_default_layout() {
        let c = json!({ "call_num": 7, "freq": 851012500, "talkgroup": 101, "short_name": "x", "start_time": 1709164800 });
        let f = "{short_name}/{time:%Y}/{time:%-m}/{time:%-d}/{talkgroup}-{epoch}_{freq}";
        assert_eq!(render(f, &c, 0, 0), format!("{}-call_7", default_path("x", "101-1709164800_851012500", 1709164800, 0)));
        assert_eq!(render("{time:%-H}{time:%-M}{time:%-S}{time:%-q}", &c, 0, 0), "000%-q-call_7");
    }

    #[test]
    fn mistakes_are_named() {
        assert_eq!(problem("{short_name}/{time:%Y}/{talkgroup}"), None);
        assert_eq!(problem("{shortName}/{tg}").as_deref(), Some("Unknown tokens: {shortName}, {tg}."));
        assert!(problem("{talkgroup").is_some());
    }
}
