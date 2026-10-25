//! The paging tone plans a tone may belong to (frequencies from Midian
//! Electronics' tone signalling charts, data/Tone-signaling-charts.pdf), for
//! guessing which code a heard tone is.

/// A plan: its short name, and its tones (code, Hz).
pub struct Plan {
    pub name: &'static str,
    pub tones: Vec<(&'static str, f32)>,
}

/// Motorola Quick Call II (one plus one) by reed group: tones 1–9 then 0,
/// each its reed code and Hz.
pub const QC2_GROUPS: &[(u8, [(&str, f32); 10])] = &[
    (1, [("111", 349.0), ("112", 368.5), ("113", 389.0), ("114", 410.8), ("115", 433.7), ("116", 457.9), ("117", 483.5), ("118", 510.5), ("119", 539.0), ("110", 330.5)]),
    (2, [("121", 600.9), ("122", 634.5), ("123", 669.9), ("124", 707.3), ("125", 746.8), ("126", 788.5), ("127", 832.5), ("128", 879.0), ("129", 928.1), ("120", 569.1)]),
    (3, [("138", 288.5), ("108", 296.5), ("139", 304.7), ("109", 313.0), ("160", 953.7), ("130", 979.9), ("161", 1006.9), ("131", 1034.7), ("162", 1063.2), ("189", 1092.4)]),
    (4, [("141", 339.6), ("142", 358.6), ("143", 378.6), ("144", 399.8), ("145", 422.1), ("146", 445.7), ("147", 470.5), ("148", 496.8), ("149", 524.6), ("140", 321.7)]),
    (5, [("151", 584.8), ("152", 617.4), ("153", 651.9), ("154", 688.3), ("155", 726.8), ("156", 767.4), ("157", 810.2), ("158", 855.5), ("159", 903.2), ("150", 553.9)]),
    (6, [("191", 1153.4), ("192", 1185.2), ("193", 1217.8), ("194", 1251.4), ("195", 1285.8), ("196", 1321.2), ("197", 1357.6), ("198", 1395.0), ("199", 1433.4), ("190", 1122.5)]),
    (10, [("171", 1513.5), ("172", 1555.2), ("173", 1598.0), ("174", 1642.0), ("175", 1687.2), ("176", 1733.7), ("177", 1781.5), ("178", 1830.5), ("179", 1881.0), ("170", 1472.9)]),
    (11, [("201", 1989.0), ("202", 2043.8), ("203", 2094.5), ("204", 2155.6), ("205", 2212.2), ("206", 2271.7), ("207", 2334.6), ("208", 2401.0), ("209", 2468.2), ("200", 1930.2)]),
];

/// Motorola's general encoding plan (Table 1): a pager code's first digit
/// names the reed groups its A and B tones are from.
const QC2_PLAN: &[(char, u8, u8)] = &[('1', 1, 1), ('2', 2, 2), ('3', 1, 2), ('4', 4, 4), ('5', 5, 5), ('6', 2, 1), ('7', 4, 5), ('8', 5, 4), ('9', 2, 4), ('0', 4, 2), ('A', 3, 3)];

/// Motorola Quick Call I (two plus two): A, B and Z series.
const QC1: &[(&str, f32)] = &[
    ("CA", 358.9), ("DA", 398.1), ("EA", 441.6), ("FA", 489.8), ("GA", 543.3), ("HA", 602.6), ("JA", 668.3), ("KA", 741.3), ("LA", 822.2), ("MA", 912.0), ("NA", 1011.6), ("PA", 1122.1),
    ("CB", 371.5), ("DB", 412.1), ("EB", 457.1), ("FB", 507.0), ("GB", 562.3), ("HB", 623.7), ("JB", 691.8), ("KB", 767.4), ("LB", 851.1), ("MB", 944.1), ("NB", 1047.1), ("PB", 1161.4),
    ("CZ", 346.7), ("DZ", 384.6), ("EZ", 426.6), ("FZ", 473.2), ("GZ", 524.8), ("HZ", 582.1), ("JZ", 645.7), ("KZ", 716.7), ("LZ", 794.3), ("MZ", 881.0), ("NZ", 977.2), ("PZ", 1084.0),
];

/// GE Type 99: groups A, B, C, tones 1–9 and 0, and the diagonal.
const GE99: &[(&str, f32)] = &[
    ("A1", 592.5), ("A2", 757.5), ("A3", 802.5), ("A4", 847.5), ("A5", 892.5), ("A6", 937.5), ("A7", 547.5), ("A8", 727.5), ("A9", 637.5), ("A0", 682.5),
    ("B1", 607.5), ("B2", 787.5), ("B3", 832.5), ("B4", 877.5), ("B5", 922.5), ("B6", 967.5), ("B7", 517.5), ("B8", 562.5), ("B9", 697.5), ("B0", 652.5),
    ("C1", 712.5), ("C2", 772.5), ("C3", 817.5), ("C4", 862.5), ("C5", 907.5), ("C6", 952.5), ("C7", 532.5), ("C8", 577.5), ("C9", 622.5), ("C0", 667.5),
    ("DIA", 742.5),
];

/// Reach two-tone and single-tone paging: channels by group (A–E), and the single-tone-only channels.
const REACH: &[(&str, f32)] = &[
    ("A11", 2704.0), ("A12", 2612.0), ("A13", 2523.0), ("A14", 2437.0), ("A15", 2354.0), ("A16", 2274.0), ("A17", 2196.0), ("A18", 2121.0), ("A19", 2049.0), ("A20", 1980.0),
    ("B21", 1912.0), ("B22", 1847.0), ("B23", 1784.0), ("B24", 1723.0), ("B25", 1664.0), ("B26", 1606.0), ("B27", 1553.0), ("B28", 1500.0), ("B29", 1449.0), ("B30", 1400.0),
    ("C26", 1608.0), ("C27", 1553.0), ("C28", 1500.0), ("C29", 1449.0), ("C30", 1400.0), ("C31", 1352.0), ("C32", 1306.0), ("C33", 1261.0), ("C34", 1219.0), ("C35", 1177.0),
    ("D36", 1137.0), ("D37", 1093.0), ("D38", 1061.0), ("D39", 1025.0), ("D40", 990.0), ("D41", 956.0), ("D42", 923.0), ("D43", 892.0), ("D44", 862.0), ("D45", 832.0),
    ("E46", 804.0), ("E47", 776.0), ("E48", 750.0), ("E49", 725.0), ("E50", 700.0), ("E51", 676.0), ("E52", 653.0), ("E53", 631.0), ("E54", 609.0), ("E55", 588.0),
    ("01", 3824.0), ("02", 3694.0), ("03", 3568.0), ("04", 3446.0), ("05", 3329.0), ("06", 3215.0), ("07", 3106.0), ("08", 3000.0), ("09", 2898.0), ("10", 2799.0),
    ("56", 568.0), ("57", 549.0), ("58", 530.0), ("59", 512.0), ("60", 495.0),
];

/// Aviation AvCall 2 + 2.
const AVCALL: &[(&str, f32)] = &[
    ("A", 312.6), ("B", 346.7), ("C", 384.6), ("D", 426.6), ("E", 473.2), ("F", 524.8), ("G", 582.1), ("H", 645.7),
    ("J", 716.1), ("K", 794.3), ("L", 881.0), ("M", 977.2), ("P", 1083.9), ("Q", 1202.3), ("R", 1333.5), ("S", 1479.1),
];

/// The plans, Quick Call II first (its tones in one list, from [`QC2_GROUPS`]).
pub fn plans() -> &'static [Plan] {
    static PLANS: std::sync::OnceLock<Vec<Plan>> = std::sync::OnceLock::new();
    PLANS.get_or_init(|| {
        let qc2 = QC2_GROUPS.iter().flat_map(|(_, tones)| tones.iter().copied()).collect();
        let plan = |name, tones: &[(&'static str, f32)]| Plan { name, tones: tones.to_vec() };
        vec![Plan { name: "QC2", tones: qc2 }, plan("QC1", QC1), plan("GE99", GE99), plan("Reach", REACH), plan("AvCall", AVCALL)]
    })
}

/// One plan's code for a heard tone, and how far off it is, Hz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub plan: &'static str,
    pub code: &'static str,
    pub hz: f32,
    pub off: f32,
}

/// How far a heard tone may be from a configured one, Hz: from a tone
/// frame, half a 31.25 Hz step and the encoder's own error; from the audio,
/// 1.5 %, what selective-calling decoders must accept (Midian's chart: ZVEI
/// ±1.5 %, CCIR and EEA ±1 %; they must reject from ±3–4.5 %), under QC2's
/// 2.75 % between neighbouring tones.
pub fn tolerance(hz: f32, from_frame: bool) -> f32 {
    if from_frame { 15.7 + 0.005 * hz } else { 0.015 * hz }
}

/// Whether a code's tone, `code` Hz, could have been heard as `heard`. A
/// tone frame names the 31.25 Hz step a tone fell in, nothing finer: any
/// code in it fits as well as any other (3 Hz more for the encoder's
/// error). In audio a tone is within 1.5 % (paging decoders' bandwidth).
pub fn fits(code: f32, heard: f32, from_frame: bool) -> bool {
    (code - heard).abs() <= if from_frame { 15.625 + 3.0 } else { 0.015 * code }
}

/// Each plan's codes within `tol` Hz of `hz`, nearest first.
pub fn candidates(hz: f32, tol: f32) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = plans()
        .iter()
        .flat_map(|p| p.tones.iter().map(move |&(code, f)| Candidate { plan: p.name, code, hz: f, off: (f - hz).abs() }))
        .filter(|c| c.off <= tol)
        .collect();
    out.sort_by(|a, b| a.off.total_cmp(&b.off));
    out
}

fn qc2_fits(heard: f32, from_frame: bool) -> Vec<(u8, char, &'static str)> {
    QC2_GROUPS
        .iter()
        .flat_map(|(g, tones)| tones.iter().enumerate().filter(move |(_, t)| fits(t.1, heard, from_frame)).map(move |(i, t)| (*g, char::from(b'0' + ((i + 1) % 10) as u8), t.0)))
        .collect()
}

/// The paging codes a two-tone page of `a` then `b` may be, likeliest first.
/// No survey says which plans are used where, so this goes by what is
/// known. A page timed as another plan's format ([`super::pattern::FORMATS`]:
/// Quick Call I's gap, GE Type 99's 1.5 s B) is that plan's first when its
/// tones fit. Otherwise Quick Call II, the Minitor's (the most used voice
/// pager in US emergency services): its pairs from Motorola's encoding plan
/// first (named with the pager code, `QC2 126/128 (268)`), then pairs from
/// one reed group, then any pair (consoles send whatever they're set to).
/// Other plans only when no Quick Call II pair fits. None: custom tones.
pub fn identify_pair(a: f32, b: f32, from_frame: bool, timed_as: Option<&str>) -> Vec<String> {
    let plan_pairs = |p: &Plan| -> Vec<String> {
        let x: Vec<&str> = p.tones.iter().filter(|t| fits(t.1, a, from_frame)).map(|t| t.0).collect();
        let y: Vec<&str> = p.tones.iter().filter(|t| fits(t.1, b, from_frame)).map(|t| t.0).collect();
        x.iter().flat_map(|ka| y.iter().map(move |kb| format!("{} {ka}/{kb}", p.name))).collect()
    };
    let timed: Vec<String> = plans().iter().filter(|p| p.name != "QC2" && Some(p.name) == timed_as).flat_map(plan_pairs).collect();
    let mut out = timed;
    out.extend(qc2_pairs(a, b, from_frame));
    if out.is_empty() {
        out = plans().iter().skip(1).flat_map(plan_pairs).collect();
    }
    out
}

fn qc2_pairs(a: f32, b: f32, from_frame: bool) -> Vec<String> {
    let (ca, cb) = (qc2_fits(a, from_frame), qc2_fits(b, from_frame));
    let mut tiers: [Vec<String>; 3] = Default::default();
    for &(ga, ta, ka) in &ca {
        for &(gb, tb, kb) in &cb {
            match QC2_PLAN.iter().find(|p| p.1 == ga && p.2 == gb) {
                Some(p) => tiers[0].push(format!("QC2 {ka}/{kb} ({}{ta}{tb})", p.0)),
                None => tiers[if ga == gb { 1 } else { 2 }].push(format!("QC2 {ka}/{kb}")),
            }
        }
    }
    tiers.concat()
}

/// The codes one long tone may be (a Quick Call II group call is one of its
/// tones held 8 s): Quick Call II's, or another plan's when none fits.
pub fn identify_one(f: f32, from_frame: bool) -> Vec<String> {
    let qc2: Vec<String> = qc2_fits(f, from_frame).into_iter().map(|(_, _, k)| format!("QC2 {k}")).collect();
    if !qc2.is_empty() {
        return qc2;
    }
    plans().iter().skip(1).flat_map(|p| p.tones.iter().filter(|t| fits(t.1, f, from_frame)).map(|t| format!("{} {}", p.name, t.0)).collect::<Vec<_>>()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_is_matched_to_its_codes() {
        let c = candidates(788.5, 1.0);
        assert_eq!((c[0].plan, c[0].code), ("QC2", "126"));
    }

    /// Pages heard on CLMRN's Phase 2 dispatch talkgroups (tone frames), 2026-10-21.
    #[test]
    fn pages_are_named_by_quick_call_first() {
        // Norwalk: only group 2's tones 6 and 8 make a plan pair (GE Type 99 B2/B4 is closer, and not asked).
        assert_eq!(identify_pair(781.25, 875.0, true, None), ["QC2 126/128 (268)", "QC2 156/128"]);
        // Wilton: two plan pairs fit the steps; mixed groups after.
        assert_eq!(identify_pair(593.75, 562.5, true, None)[..2], ["QC2 121/120 (210)", "QC2 151/150 (510)"]);
        // Naugy: group 10, in no plan pair: one group's pair.
        assert_eq!(identify_pair(1593.75, 1468.75, true, None), ["QC2 173/170"]);
        // Weston: no Quick Call II tone that high; Reach doesn't fit either: custom tones.
        assert!(identify_pair(2562.5, 1343.75, true, None).is_empty());
        // Wilton Fire's group call, 812.5 Hz held 8 s.
        assert_eq!(identify_one(812.5, true), ["QC2 157"]);
        // Timed as GE Type 99 (1 s + 1.5 s): its codes first, then Quick Call II's.
        let ge = identify_pair(781.25, 875.0, true, Some("GE99"));
        let qc2 = ge.iter().position(|c| c == "QC2 126/128 (268)").unwrap();
        assert!(qc2 > 0 && ge[..qc2].iter().all(|c| c.starts_with("GE99 ")), "{ge:?}");
        // From the audio a code must be within 1.5 %.
        assert_eq!(identify_pair(788.0, 880.0, false, None), ["QC2 126/128 (268)"]);
    }

    #[test]
    fn codes_are_unique_within_a_plan() {
        for p in plans() {
            let mut codes: Vec<&str> = p.tones.iter().map(|t| t.0).collect();
            codes.sort();
            codes.dedup();
            assert_eq!(codes.len(), p.tones.len(), "{}", p.name);
        }
    }
}
