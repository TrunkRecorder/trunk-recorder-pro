//! Checking a list of sites: which of a system's sites can be heard from here.
//!
//! The input is a RadioReference sites export (`trs_sites_NNNN.csv`: one row
//! per site, its frequencies at the end of the row, control channels marked
//! `c`). Each listed frequency gets a probe ([`crate::survey`]'s: P25,
//! SmartNet, DMR and NXDN receivers on a channel head) and a carrier meter,
//! a window of the radio's band at a time, and each site is judged by what
//! its frequencies showed:
//!
//! ```text
//! P25        a control channel decodes (and, where the row gives them, its NAC and site match)
//! SmartNet   a control channel decodes
//! NXDN       a Type-C control channel decodes
//! EDACS      a control channel's carrier stays up (this check doesn't decode EDACS)
//! DMR        DMR bursts on any of its repeaters (a Capacity Plus site's are
//!            quiet between calls, so listen longer)
//! ```
//!
//! A site whose frequencies are quiet is [`Verdict::Inactive`]; one where a
//! carrier is up but nothing of its kind decodes is [`Verdict::Carrier`]
//! (kept: it may be the site, too weak or too busy to decode). No I/O: the
//! caller tunes the radio to each [`Window`]'s centre and feeds samples.

use std::collections::BTreeMap;

use num_complex::Complex32;

use crate::dsp::Channelizer;
use crate::p25::diversity::Group;
use crate::survey::{Candidate, Kind, Peak, Probe, Receivers, USABLE};
use crate::trunk::identity::IdField;
use crate::trunk::Message;

const MIN_CHANNEL_RATE: f64 = 24_000.0;
/// Half the width a carrier is metered over, Hz.
const METER_HALF_HZ: f64 = 5_000.0;
/// Above the band's floor by this much (power ratio, 6 dB), a carrier is up.
const ON_RATIO: f64 = 4.0;
/// Kept clear of the radio's DC offset, Hz.
const DC_CLEAR_HZ: f64 = 20_000.0;
/// A control channel's carrier is up at least this share of the time.
const CONTROL_ON: f64 = 0.8;
/// A carrier counts as present when it's up at least this share of the time.
const SOME_ON: f64 = 0.02;

/// The kind of system the sites belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemType {
    P25,
    SmartNet,
    Edacs,
    Dmr,
    Nxdn,
}

impl SystemType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "p25" | "project25" => SystemType::P25,
            "smartnet" | "smartzone" | "motorola" => SystemType::SmartNet,
            "edacs" => SystemType::Edacs,
            "dmr" | "capacityplus" | "capplus" | "trbo" | "mototrbo" | "connectplus" | "capacitymax" | "tier3" => SystemType::Dmr,
            "nxdn" | "nexedge" | "idas" => SystemType::Nxdn,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SystemType::P25 => "P25",
            SystemType::SmartNet => "SmartNet",
            SystemType::Edacs => "EDACS",
            SystemType::Dmr => "DMR",
            SystemType::Nxdn => "NXDN",
        }
    }
    /// How long a window is listened to by default, s: a control channel
    /// shows itself at once; a DMR site may only key up for a call.
    pub fn default_listen_s(self) -> f64 {
        match self {
            SystemType::Dmr => 120.0,
            _ => 4.0,
        }
    }
}

/// One site of the export.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteRow {
    /// Its line in the file (0 = the header).
    pub line: usize,
    /// "001 Walker Tower (Highland)".
    pub label: String,
    pub rfss: Option<u32>,
    pub site: Option<u32>,
    /// P25 NAC (the export's hex).
    pub nac: Option<u32>,
    /// Hz, and whether it's marked as a control channel.
    pub freqs: Vec<(f64, bool)>,
    /// Where it is (the export's Lat, Lon).
    pub at: Option<(f64, f64)>,
}

impl SiteRow {
    /// The frequencies this check listens to: its control channels where
    /// any is marked (DMR Connect Plus and Capacity Max sites too), else all
    /// of them (a Capacity Plus site has no control channel).
    pub fn checked(&self, _ty: SystemType) -> Vec<f64> {
        let marked: Vec<f64> = self.freqs.iter().filter(|f| f.1).map(|f| f.0).collect();
        if !marked.is_empty() {
            marked
        } else {
            self.freqs.iter().map(|f| f.0).collect()
        }
    }
}

/// A parsed sites export: its lines as they were, and its sites.
#[derive(Clone, Debug)]
pub struct SitesFile {
    pub lines: Vec<String>,
    pub sites: Vec<SiteRow>,
}

/// One CSV line's fields (`"` quotes a field, `""` is a quote in one).
fn fields(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut chars) = (Vec::new(), String::new(), false, line.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|f| f.trim().to_string()).collect()
}

/// Parse a RadioReference sites export (`Frequencies` is the last named
/// column; the row's frequencies run from there to its end, MHz, `c` marking
/// a control channel).
pub fn parse_sites(text: &str) -> Result<SitesFile, String> {
    let lines: Vec<String> = text.trim_start_matches('\u{feff}').lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
    let head = fields(lines.first().ok_or("the file is empty")?);
    let col = |names: &[&str]| head.iter().position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)));
    let fcol = col(&["Frequencies", "Frequency", "Freqs"]).ok_or("no Frequencies column: is this a RadioReference sites export (trs_sites_….csv)?")?;
    let (c_site, c_rfss, c_nac, c_desc) = (col(&["Site Dec", "Site", "Site Number"]), col(&["RFSS"]), col(&["Site NAC", "NAC"]), col(&["Description", "Name", "Site Name"]));
    let (c_lat, c_lon) = (col(&["Lat", "Latitude"]), col(&["Lon", "Long", "Longitude"]));
    let mut sites = Vec::new();
    for (i, l) in lines.iter().enumerate().skip(1) {
        if l.trim().is_empty() {
            continue;
        }
        let f = fields(l);
        let get = |c: Option<usize>| c.and_then(|c| f.get(c)).map(|s| s.as_str()).filter(|s| !s.is_empty());
        let freqs: Vec<(f64, bool)> = f
            .iter()
            .skip(fcol)
            .filter_map(|v| {
                let v = v.trim();
                let control = v.ends_with(['c', 'C']);
                let mhz: f64 = v.trim_end_matches(['c', 'C', 'a', 'A']).trim().parse().ok()?;
                (mhz > 1.0).then_some(((mhz * 1e6).round(), control))
            })
            .collect();
        if freqs.is_empty() {
            continue;
        }
        let site = get(c_site).and_then(|s| s.parse().ok());
        let label = [get(c_site), get(c_desc)].into_iter().flatten().collect::<Vec<_>>().join(" ");
        sites.push(SiteRow {
            line: i,
            label: if label.is_empty() { format!("line {}", i + 1) } else { label },
            rfss: get(c_rfss).and_then(|s| s.parse().ok()),
            site,
            nac: get(c_nac).and_then(|s| u32::from_str_radix(s, 16).ok()),
            freqs,
            at: get(c_lat).zip(get(c_lon)).and_then(|(a, o)| Some((a.parse().ok()?, o.parse().ok()?))),
        });
    }
    if sites.is_empty() {
        return Err("no sites with frequencies in the file".into());
    }
    Ok(SitesFile { lines, sites })
}

/// One tuning of the radio: its centre, and the frequencies it covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub center_hz: f64,
    pub freqs: Vec<f64>,
}

/// Windows covering `freqs` at `rate_hz` (each frequency in one, none at the
/// radio's DC), as few as fit.
pub fn plan(freqs: &[f64], rate_hz: f64) -> Vec<Window> {
    let mut f: Vec<f64> = freqs.to_vec();
    f.sort_by(f64::total_cmp);
    f.dedup();
    let half = rate_hz * USABLE / 2.0 - METER_HALF_HZ - DC_CLEAR_HZ;
    let mut out: Vec<Window> = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let mut j = i;
        while j + 1 < f.len() && f[j + 1] - f[i] <= 2.0 * half {
            j += 1;
        }
        let span = &f[i..=j];
        let mid = (span[0] + span[span.len() - 1]) / 2.0;
        // A centre clear of every frequency (the radio's DC spike), still covering them all.
        let center = [0.0, DC_CLEAR_HZ, -DC_CLEAR_HZ, 2.0 * DC_CLEAR_HZ, -2.0 * DC_CLEAR_HZ]
            .into_iter()
            .map(|d| mid + d)
            .find(|c| span.iter().all(|x| (x - c).abs() >= DC_CLEAR_HZ && (x - c).abs() <= half + DC_CLEAR_HZ))
            .unwrap_or(mid);
        out.push(Window { center_hz: center, freqs: span.to_vec() });
        i = j + 1;
    }
    out
}

/// What one frequency showed.
#[derive(Clone, Debug)]
pub struct FreqReport {
    pub freq_hz: f64,
    pub listened_s: f64,
    /// Share of the time its carrier was up.
    pub on: f64,
    /// Its strongest, dB over the band's floor.
    pub peak_db: f64,
    /// What the receivers made of it.
    pub heard: Candidate,
}

impl FreqReport {
    /// Another listen to the same frequency (a long run's next pass) folded in:
    /// the time up over both, the strongest, and the best decode.
    pub fn merge(&mut self, other: FreqReport) {
        let total = self.listened_s + other.listened_s;
        if total > 0.0 {
            self.on = (self.on * self.listened_s + other.on * other.listened_s) / total;
        }
        self.listened_s = total;
        self.peak_db = self.peak_db.max(other.peak_db);
        if other.heard.kind > self.heard.kind || (other.heard.kind == self.heard.kind && self.heard.identity.is_empty() && !other.heard.identity.is_empty()) {
            self.heard = other.heard;
        }
    }

    /// "P25 control channel, NAC 0x212 site 1" …
    pub fn describe(&self) -> String {
        let c = &self.heard;
        let mut s = match c.kind {
            Kind::Control => "P25 control channel".to_string(),
            Kind::P25 => "P25 (no control messages)".to_string(),
            Kind::SmartNet => "SmartNet control channel".to_string(),
            Kind::DmrControl => format!("DMR {}", c.dmr.and_then(|d| d.variant).map(|v| format!("{v:?}")).unwrap_or_else(|| "trunking".into())),
            Kind::Dmr => "DMR".to_string(),
            Kind::NxdnControl => "NXDN control channel".to_string(),
            Kind::Nxdn => "NXDN".to_string(),
            Kind::Other if self.on >= SOME_ON => "a carrier, not decoded".to_string(),
            Kind::Other => "nothing".to_string(),
        };
        let id: Vec<String> = c
            .identity
            .iter()
            .map(|(f, v)| match f {
                IdField::Nac | IdField::SysId | IdField::Wacn => format!("{} {v:X}", f.label()),
                _ => format!("{} {v}", f.label()),
            })
            .collect();
        if !id.is_empty() {
            s.push_str(&format!(", {}", id.join(" ")));
        }
        if let Some(cc) = c.dmr.and_then(|d| d.color_code) {
            s.push_str(&format!(", colour code {cc}"));
        }
        if let Some(n) = c.nxdn {
            if let Some(r) = n.ran {
                s.push_str(&format!(", RAN {r}"));
            }
            if let Some((sys, site)) = n.location {
                s.push_str(&format!(", system {sys} site {site}"));
            }
        }
        s
    }
}

/// How well a frequency decoded: what the results are coloured by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// Nothing decoded, or most of what framed failed.
    Poor,
    /// It decodes, but weakly: errors, a weak signal, or no control messages.
    Marginal,
    /// It decodes cleanly on a good signal.
    Good,
}

/// A decode on less than this (peak, dB over the floor) is marginal however clean.
const GOOD_DB: f64 = 12.0;

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Good => "good",
            Quality::Marginal => "marginal",
            Quality::Poor => "poor",
        }
    }
}

impl FreqReport {
    /// The share of its frames that decoded (P25 and SmartNet count them), if it framed any.
    pub fn decoded_share(&self) -> Option<f64> {
        let (g, b) = (self.heard.good, self.heard.bad);
        (matches!(self.heard.kind, Kind::Control | Kind::P25 | Kind::SmartNet) && g + b > 0).then(|| g as f64 / (g + b) as f64)
    }

    /// How well it decoded (`ty`: the system's kind; EDACS, not decoded here, by its carrier).
    pub fn quality(&self, ty: SystemType) -> Quality {
        let decoded = match ty {
            SystemType::Edacs => self.on >= CONTROL_ON,
            _ => self.heard.kind != Kind::Other,
        };
        if !decoded {
            return Quality::Poor;
        }
        // A P25 signal with no control messages, or a DMR carrier without trunking, only half decodes.
        let partial = match ty {
            SystemType::P25 => self.heard.kind != Kind::Control,
            SystemType::SmartNet => self.heard.kind != Kind::SmartNet,
            SystemType::Nxdn => self.heard.kind != Kind::NxdnControl,
            SystemType::Dmr => !matches!(self.heard.kind, Kind::Dmr | Kind::DmrControl),
            SystemType::Edacs => false,
        };
        let share = self.decoded_share().unwrap_or(1.0);
        if share < 0.5 {
            Quality::Poor
        } else if partial || share < 0.9 || self.peak_db < GOOD_DB {
            Quality::Marginal
        } else {
            Quality::Good
        }
    }
}

/// Listening to one window: a probe and a carrier meter per frequency.
pub struct WindowCheck {
    center: f64,
    chz: Channelizer,
    probes: Vec<Probe>,
    freqs: Vec<f64>,
    blocks: u64,
    until: u64,
    stride: u64,
    row: Vec<f32>,
    sorted: Vec<f32>,
    metered: u64,
    on: Vec<u64>,
    peak: Vec<f64>,
    groups: Vec<Group>,
    msgs: Vec<Message>,
    iq: Vec<Complex32>,
}

impl WindowCheck {
    /// Listening at `rate_hz` for `listen_s` to the window `w`.
    pub fn new(w: &Window, rate_hz: f64, listen_s: f64) -> Self {
        Self::for_type(w, rate_hz, listen_s, None)
    }

    /// Listening for `ty`'s kind of system only (None: every kind): the other
    /// receivers don't run, which saves most of the CPU on a wide window.
    pub fn for_type(w: &Window, rate_hz: f64, listen_s: f64, ty: Option<SystemType>) -> Self {
        let runs = match ty {
            None => Receivers::ALL,
            Some(t) => Receivers { p25: t == SystemType::P25, smartnet: t == SystemType::SmartNet, dmr: t == SystemType::Dmr, nxdn: t == SystemType::Nxdn },
        };
        let mut chz = Channelizer::new(rate_hz, MIN_CHANNEL_RATE, 0.01);
        let probes = w
            .freqs
            .iter()
            .map(|f| {
                let mut p = Probe::new(&mut chz, w.center_hz, Peak { offset_hz: f - w.center_hz, snr_db: 0.0, width_hz: 0.0, score: 0.0 });
                p.runs = runs;
                p
            })
            .collect();
        let mut cells = chz.fft_size();
        while cells > 16 && rate_hz / (cells as f64) < 900.0 {
            cells /= 2;
        }
        let bs = chz.block_seconds();
        WindowCheck {
            center: w.center_hz,
            probes,
            freqs: w.freqs.clone(),
            blocks: 0,
            until: (listen_s / bs).ceil().max(1.0) as u64,
            // The carriers metered every ~20 ms.
            stride: ((0.02 / bs).round() as u64).max(1),
            row: vec![0.0; cells],
            sorted: Vec::with_capacity(cells),
            metered: 0,
            on: vec![0; w.freqs.len()],
            peak: vec![f64::NEG_INFINITY; w.freqs.len()],
            groups: Vec::new(),
            msgs: Vec::new(),
            iq: Vec::new(),
            chz,
        }
    }

    pub fn done(&self) -> bool {
        self.blocks >= self.until
    }

    /// 8-bit unsigned IQ (an RTL-SDR's).
    pub fn push_u8(&mut self, bytes: &[u8]) {
        let mut iq = std::mem::take(&mut self.iq);
        iq.clear();
        iq.extend(bytes.chunks_exact(2).map(|p| Complex32::new((p[0] as f32 - 127.5) / 127.5, (p[1] as f32 - 127.5) / 127.5)));
        self.push_iq(&iq);
        self.iq = iq;
    }

    pub fn push_iq(&mut self, mut iq: &[Complex32]) {
        while !iq.is_empty() && !self.done() {
            let (used, ran) = self.chz.feed(iq);
            iq = &iq[used..];
            if ran {
                self.on_block();
            }
        }
    }

    fn on_block(&mut self) {
        self.blocks += 1;
        let rate = self.chz.output_rate();
        let last = self.done();
        for p in &mut self.probes {
            p.feed(&self.chz, rate, &mut self.groups, &mut self.msgs, last);
        }
        if !self.blocks.is_multiple_of(self.stride) {
            return;
        }
        // The floor: the median cell of the band's middle (its edges roll off).
        self.chz.cell_powers(&mut self.row);
        let n = self.row.len();
        self.sorted.clear();
        self.sorted.extend_from_slice(&self.row[n / 10..n - n / 10]);
        let mid = self.sorted.len() / 2;
        let floor = (*self.sorted.select_nth_unstable_by(mid, f32::total_cmp).1 as f64).max(1e-30);
        self.metered += 1;
        for (k, f) in self.freqs.iter().enumerate() {
            let r = self.chz.band_power(f - self.center, METER_HALF_HZ) / floor;
            if r >= ON_RATIO {
                self.on[k] += 1;
            }
            self.peak[k] = self.peak[k].max(10.0 * r.max(1e-9).log10());
        }
    }

    /// What each frequency showed so far.
    pub fn reports(&self) -> Vec<FreqReport> {
        let rate = self.chz.output_rate();
        let listened = self.blocks as f64 * self.chz.block_seconds();
        self.probes
            .iter()
            .enumerate()
            .map(|(k, p)| FreqReport {
                freq_hz: self.freqs[k],
                listened_s: listened,
                on: self.on[k] as f64 / self.metered.max(1) as f64,
                peak_db: if self.metered == 0 { 0.0 } else { self.peak[k] },
                heard: p.candidate(self.center, "", rate),
            })
            .collect()
    }
}

impl WindowCheck {
    /// The band as it is now (dB per bin, `bins` bins), for a waterfall.
    pub fn spectrum(&self, bins: usize) -> Vec<f32> {
        self.chz.power_spectrum(bins)
    }
    /// How much air it has listened to, s.
    pub fn listened_s(&self) -> f64 {
        self.blocks as f64 * self.chz.block_seconds()
    }
}

/// A window listened to on several CPU cores: its frequencies split between
/// [`WindowCheck`]s, each with its own channelizer, fed the same samples
/// side by side. A wide window (a bladeRF at 30 MS/s and more, dozens of
/// frequencies) is too much for one core to keep up with.
pub struct ParallelCheck {
    parts: Vec<WindowCheck>,
    buf: Vec<Complex32>,
    batch: usize,
}

impl ParallelCheck {
    /// `threads`: how many cores to use at most (one part each).
    pub fn new(w: &Window, rate_hz: f64, listen_s: f64, ty: Option<SystemType>, threads: usize) -> Self {
        let n = threads.clamp(1, w.freqs.len().max(1));
        let per = w.freqs.len().div_ceil(n).max(1);
        let parts = if w.freqs.is_empty() {
            vec![WindowCheck::for_type(w, rate_hz, listen_s, ty)]
        } else {
            w.freqs.chunks(per).map(|c| WindowCheck::for_type(&Window { center_hz: w.center_hz, freqs: c.to_vec() }, rate_hz, listen_s, ty)).collect()
        };
        // Handed over every 50 ms of samples: the threads' start-up is nothing beside that.
        ParallelCheck { parts, buf: Vec::new(), batch: ((rate_hz * 0.05) as usize).max(4096) }
    }

    /// How many parts (cores) it listens with.
    pub fn parts(&self) -> usize {
        self.parts.len()
    }

    /// Done listening (samples still waiting are pushed by [`ParallelCheck::flush`]).
    pub fn done(&self) -> bool {
        self.parts.iter().all(WindowCheck::done)
    }

    /// 8-bit unsigned IQ (an RTL-SDR's).
    pub fn push_u8(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes.chunks_exact(2).map(|p| Complex32::new((p[0] as f32 - 127.5) / 127.5, (p[1] as f32 - 127.5) / 127.5)));
        self.maybe_flush();
    }

    pub fn push_iq(&mut self, iq: &[Complex32]) {
        self.buf.extend_from_slice(iq);
        self.maybe_flush();
    }

    fn maybe_flush(&mut self) {
        if self.buf.len() >= self.batch {
            self.flush();
        }
    }

    /// Everything waiting, through every part.
    pub fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let buf = std::mem::take(&mut self.buf);
        if self.parts.len() == 1 {
            self.parts[0].push_iq(&buf);
        } else {
            std::thread::scope(|s| {
                for p in self.parts.iter_mut() {
                    let b = &buf;
                    s.spawn(move || p.push_iq(b));
                }
            });
        }
        self.buf = buf;
        self.buf.clear();
    }

    /// What each frequency showed so far, by frequency.
    pub fn reports(&mut self) -> Vec<FreqReport> {
        self.flush();
        let mut v: Vec<FreqReport> = self.parts.iter().flat_map(WindowCheck::reports).collect();
        v.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
        v
    }

    /// The band now, for a waterfall.
    pub fn spectrum(&self, bins: usize) -> Vec<f32> {
        self.parts[0].spectrum(bins)
    }

    /// The air listened to so far, s.
    pub fn listened_s(&self) -> f64 {
        self.parts.iter().map(WindowCheck::listened_s).fold(f64::INFINITY, f64::min)
    }
}

/// What became of a site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Heard: its control channel decodes (DMR without one: bursts on a repeater).
    Active,
    /// Its control channel decodes as another site (P25: another NAC or site number).
    OtherSite,
    /// Something is there, but not confirmed as this site: no decode, another
    /// kind of DMR than the system's, or only on a frequency other sites share.
    Carrier,
    /// Nothing heard.
    Inactive,
    /// None of its frequencies were checked (outside what the radio or capture covers).
    NotChecked,
    /// Farther than asked (`--within`): not listened to.
    OutOfRange,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Active => "active",
            Verdict::OtherSite => "other site",
            Verdict::Carrier => "unconfirmed",
            Verdict::Inactive => "inactive",
            Verdict::NotChecked => "not checked",
            Verdict::OutOfRange => "out of range",
        }
    }
    /// Whether the cleaned file keeps the site.
    pub fn keep(self) -> bool {
        !matches!(self, Verdict::Inactive | Verdict::OtherSite | Verdict::OutOfRange)
    }
}

/// How far apart two places are, statute miles (haversine).
pub fn miles(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let (dp, dl) = (p2 - p1, (b.1 - a.1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 3958.8 * h.sqrt().asin()
}

/// US ZIP code → its centroid (lat, lon): the Census Bureau's ZIP Code
/// Tabulation Areas (public domain), built in.
pub fn zip_location(zip: &str) -> Option<(f64, f64)> {
    static TABLE: &str = include_str!("zcta.txt");
    let want = zip.trim();
    if want.len() != 5 || !want.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let lines: Vec<&str> = TABLE.lines().filter(|l| !l.starts_with('#')).collect();
    let i = lines.binary_search_by(|l| l[..5].cmp(want)).ok()?;
    let mut f = lines[i].split_whitespace().skip(1).map(|v| v.parse::<f64>());
    Some((f.next()?.ok()?, f.next()?.ok()?))
}

/// Every site judged by what its frequencies showed (`heard`: by frequency, Hz).
///
/// A frequency more than one site checks (shared control channels) confirms
/// none of them by itself, except where P25 says which site it is. A DMR
/// system's sites are one kind of trunking: the kind most of its decoded
/// sites are; a site decoding as another kind isn't confirmed (a neighbour
/// on the frequency).
pub fn judge_all(ty: SystemType, sites: &[SiteRow], heard: &BTreeMap<u64, FreqReport>) -> Vec<(Verdict, String)> {
    let mut sharing: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
    for s in sites {
        for f in s.checked(ty) {
            sharing.entry(f as u64).or_default().push(s.label.as_str());
        }
    }
    let decoded = |r: &FreqReport| match ty {
        SystemType::P25 => r.heard.kind == Kind::Control,
        SystemType::SmartNet => r.heard.kind == Kind::SmartNet,
        SystemType::Nxdn => r.heard.kind == Kind::NxdnControl,
        SystemType::Dmr => matches!(r.heard.kind, Kind::Dmr | Kind::DmrControl),
        SystemType::Edacs => r.on >= CONTROL_ON,
    };
    // The system's DMR kind: the most common among the decodes on frequencies only one site has.
    let variant = if ty == SystemType::Dmr {
        let mut votes: Vec<(crate::dmr::Variant, usize)> = Vec::new();
        let alone = heard.iter().filter(|(f, _)| sharing.get(f).is_some_and(|v| v.len() == 1));
        for v in alone.filter_map(|(_, r)| r.heard.dmr.and_then(|d| d.variant)) {
            match votes.iter_mut().find(|(x, _)| *x == v) {
                Some(e) => e.1 += 1,
                None => votes.push((v, 1)),
            }
        }
        votes.into_iter().max_by_key(|(_, n)| *n).map(|(v, _)| v)
    } else {
        None
    };
    sites.iter().map(|s| judge_one(ty, s, heard, &sharing, decoded, variant)).collect()
}

fn judge_one(ty: SystemType, site: &SiteRow, heard: &BTreeMap<u64, FreqReport>, sharing: &BTreeMap<u64, Vec<&str>>, decoded: impl Fn(&FreqReport) -> bool, variant: Option<crate::dmr::Variant>) -> (Verdict, String) {
    let reports: Vec<&FreqReport> = site.checked(ty).iter().filter_map(|f| heard.get(&(*f as u64))).collect();
    if reports.is_empty() {
        return (Verdict::NotChecked, "none of its frequencies were checked".into());
    }
    let mhz = |r: &FreqReport| format!("{:.5}", r.freq_hz / 1e6);
    let others = |r: &FreqReport| -> Vec<&str> { sharing.get(&(r.freq_hz as u64)).map_or(vec![], |v| v.iter().copied().filter(|l| *l != site.label).collect()) };
    let found: Vec<&FreqReport> = reports.iter().copied().filter(|r| decoded(r)).collect();
    if ty == SystemType::P25 {
        // Its own NAC and site, where the row gives them and the air said them.
        let mine = |r: &FreqReport| {
            let id = &r.heard.identity;
            let same = |want: Option<u32>, f: IdField| match (want, id.get(f)) {
                (Some(w), Some(h)) => w == h,
                _ => true,
            };
            same(site.nac, IdField::Nac) && same(site.site, IdField::Site) && same(site.rfss, IdField::Rfss)
        };
        if let Some(r) = found.iter().find(|r| mine(r)) {
            return (Verdict::Active, format!("{} at {}", r.describe(), mhz(r)));
        }
        if let Some(r) = found.first() {
            return (Verdict::OtherSite, format!("{} at {}: not this site's", r.describe(), mhz(r)));
        }
    } else {
        let right_kind = |r: &FreqReport| variant.is_none_or(|v| r.heard.dmr.and_then(|d| d.variant).is_none_or(|x| x == v));
        if let Some(r) = found.iter().find(|r| right_kind(r) && others(r).is_empty()) {
            let what = if ty == SystemType::Edacs { format!("a control channel carrier ({:.0} % of the time; EDACS isn't decoded here)", r.on * 100.0) } else { r.describe() };
            return (Verdict::Active, format!("{what} at {}", mhz(r)));
        }
        if let Some(r) = found.iter().find(|r| !right_kind(r)) {
            return (Verdict::Carrier, format!("{} at {}, but this system is {}", r.describe(), mhz(r), variant.map_or("another kind", |v| v.name())));
        }
        if let Some(r) = found.first() {
            return (Verdict::Carrier, format!("{} at {}, a frequency {} also use{}", r.describe(), mhz(r), others(r).join(", "), if others(r).len() == 1 { "s" } else { "" }));
        }
    }
    let bar = if ty == SystemType::Dmr { SOME_ON } else { CONTROL_ON / 2.0 };
    if let Some(r) = reports.iter().filter(|r| r.on >= bar).max_by(|a, b| a.on.total_cmp(&b.on)) {
        return (Verdict::Carrier, format!("{} at {} (up {:.0} % of the time), but no {} decoded", r.describe(), mhz(r), r.on * 100.0, ty.as_str()));
    }
    let longest = reports.iter().map(|r| r.listened_s).fold(0.0, f64::max);
    (Verdict::Inactive, format!("nothing on {} frequenc{} in {}", reports.len(), if reports.len() == 1 { "y" } else { "ies" }, duration(longest)))
}

/// "45 s", "12 min", "3.5 h".
pub fn duration(s: f64) -> String {
    if s < 90.0 {
        format!("{s:.0} s")
    } else if s < 5400.0 {
        format!("{:.0} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

/// A site judged alone (no other sites to share frequencies with).
pub fn judge(ty: SystemType, site: &SiteRow, heard: &BTreeMap<u64, FreqReport>) -> (Verdict, String) {
    judge_all(ty, std::slice::from_ref(site), heard).remove(0)
}

/// The export without the sites `verdicts` don't keep (lines as they were).
pub fn cleaned(file: &SitesFile, verdicts: &[(usize, Verdict)]) -> String {
    let dropped: std::collections::BTreeSet<usize> = verdicts.iter().filter(|(_, v)| !v.keep()).map(|(line, _)| *line).collect();
    let mut out = String::new();
    for (i, l) in file.lines.iter().enumerate() {
        if !dropped.contains(&i) {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::survey::DmrFound;
    use crate::trunk::identity::Identity;

    const P25_SITES: &str = "Sysid,RFSS,Site Dec,Site Hex,Site NAC,Description,County Name,Lat,Lon,Range,Frequencies\n\
26E,51,051,33,033,\"Zone 2 Simulcast\",\"Putnam\",41.335,-73.72,16,770.156250c,770.431250c,773.293750c\n\
26E,111,111,6F,06F,\"Zone 4, Simulcast\",\"Orange\",41.276,-74.28,28,769.931250c,770.406250c\n\
26E,141,141,8D,08D,\"Zone 7 Simulcast\",\"Dutchess\",41.64,-73.8,20,769.406250c,775.000000\n";

    #[test]
    fn a_radioreference_export_reads() {
        let f = parse_sites(P25_SITES).unwrap();
        assert_eq!(f.sites.len(), 3);
        let s = &f.sites[1];
        assert_eq!((s.line, s.rfss, s.site, s.nac), (2, Some(111), Some(111), Some(0x6f)));
        assert_eq!(s.label, "111 Zone 4, Simulcast");
        assert_eq!(s.freqs, vec![(769_931_250.0, true), (770_406_250.0, true)]);
        // Only its control channels are checked, whatever the kind; a site without any marked: all of them.
        assert_eq!(f.sites[2].checked(SystemType::P25), vec![769_406_250.0]);
        assert_eq!(f.sites[2].checked(SystemType::Dmr), vec![769_406_250.0]);
        assert_eq!(f.sites[2].at, Some((41.64, -73.8)));
        let dmr = parse_sites("Site Dec,Site Hex,Description,County Name,Lat,Lon,Range,Frequencies\n001,1,\"Site 1\",\"Dutchess\",41.5,-73.8,0.5,854.787500,854.812500\n").unwrap();
        assert_eq!((dmr.sites[0].site, dmr.sites[0].nac, dmr.sites[0].freqs.len()), (Some(1), None, 2));
        assert_eq!(dmr.sites[0].checked(SystemType::Dmr), vec![854_787_500.0, 854_812_500.0]);
        assert!(parse_sites("Decimal,Alpha Tag\n1,x\n").is_err(), "a talkgroup file isn't a sites file");
    }

    #[test]
    fn windows_cover_every_frequency_clear_of_dc() {
        let rate = 2_400_000.0;
        let f = [769_406_250.0, 769_931_250.0, 770_156_250.0, 773_293_750.0, 851_000_000.0];
        let w = plan(&f, rate);
        let half = rate * USABLE / 2.0 - METER_HALF_HZ;
        for x in f {
            let n = w.iter().filter(|w| w.freqs.contains(&x)).count();
            assert_eq!(n, 1, "{x} in {n} windows");
            let w = w.iter().find(|w| w.freqs.contains(&x)).unwrap();
            assert!((x - w.center_hz).abs() <= half && (x - w.center_hz).abs() >= DC_CLEAR_HZ, "{x} at {:.0} from centre", x - w.center_hz);
        }
        assert_eq!(w.len(), 3);
        // One frequency alone: a centre beside it.
        let one = plan(&[460_000_000.0], rate);
        assert!((one[0].center_hz - 460_000_000.0).abs() >= DC_CLEAR_HZ);
    }

    fn heard(freq_hz: f64, kind: Kind, on: f64, id: Identity) -> FreqReport {
        FreqReport {
            freq_hz,
            listened_s: 4.0,
            on,
            peak_db: 20.0,
            heard: Candidate { freq_hz, band: "", snr_db: 0.0, width_hz: 0.0, kind, frames: 0, good: 0, bad: 0, modulation: "", identity: id, dmr: (kind == Kind::Dmr).then_some(DmrFound { variant: None, color_code: Some(1) }), nxdn: None },
        }
    }

    #[test]
    fn sites_are_judged_by_their_kind() {
        let f = parse_sites(P25_SITES).unwrap();
        let by = |v: Vec<FreqReport>| v.into_iter().map(|r| (r.freq_hz as u64, r)).collect::<BTreeMap<_, _>>();
        let own = Identity::default().with(IdField::Nac, 0x33).with(IdField::Site, 51).with(IdField::Rfss, 51);
        let other = Identity::default().with(IdField::Nac, 0x51).with(IdField::Site, 81).with(IdField::Rfss, 81);
        let air = by(vec![
            heard(770_156_250.0, Kind::Control, 1.0, own),
            heard(769_931_250.0, Kind::Control, 1.0, other),
            heard(770_406_250.0, Kind::Other, 0.0, Identity::default()),
            heard(769_406_250.0, Kind::Other, 0.0, Identity::default()),
        ]);
        let v: Vec<Verdict> = f.sites.iter().map(|s| judge(SystemType::P25, s, &air).0).collect();
        assert_eq!(v, [Verdict::Active, Verdict::OtherSite, Verdict::Inactive]);
        // A carrier with no decode is kept, unconfirmed.
        let carrier = by(vec![heard(769_406_250.0, Kind::Other, 0.95, Identity::default())]);
        assert_eq!(judge(SystemType::P25, &f.sites[2], &carrier).0, Verdict::Carrier);
        // EDACS: a control channel carrier that stays up is the site.
        assert_eq!(judge(SystemType::Edacs, &f.sites[2], &carrier).0, Verdict::Active);
        // DMR: bursts on one of its own checked frequencies; an unmarked one isn't listened to.
        let dmr = by(vec![heard(769_406_250.0, Kind::Dmr, 0.05, Identity::default())]);
        assert_eq!(judge(SystemType::Dmr, &f.sites[2], &dmr).0, Verdict::Active);
        assert_eq!(judge(SystemType::Dmr, &f.sites[0], &dmr).0, Verdict::NotChecked);
        let unmarked = by(vec![heard(775_000_000.0, Kind::Dmr, 0.05, Identity::default())]);
        assert_eq!(judge(SystemType::Dmr, &f.sites[2], &unmarked).0, Verdict::NotChecked);
        // The cleaned file drops the inactive and other-site rows only.
        let verdicts: Vec<(usize, Verdict)> = f.sites.iter().zip(&v).map(|(s, v)| (s.line, *v)).collect();
        let out = cleaned(&f, &verdicts);
        assert_eq!(out.lines().count(), 2);
        assert!(out.lines().nth(1).unwrap().contains("Zone 2"));
    }

    /// A DMR repeater on the air in one window is heard as DMR; a quiet listed
    /// frequency beside it as nothing.
    #[test]
    fn a_window_hears_a_dmr_repeater_and_a_quiet_frequency() {
        use crate::dmr::synth::{dibits, modulate, Tx};
        let fs = 1_200_000.0;
        let w = Window { center_hz: 851_000_000.0, freqs: vec![851_150_000.0, 850_800_000.0] };
        let tx = Tx { cc: 3, slot: 1, talkgroup: 101, source: 2001, superframes: 8, idle_before: 4, idle_after: 4, csbks: vec![], mobile: false };
        let d = dibits(&tx);
        let mut ph = 0.0;
        let mut iq = modulate(&d, fs, 150_000.0, 0.3, &mut ph);
        let mut s = 0x1234_5678u64;
        for x in iq.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let n = ((s >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.02;
            *x += Complex32::new(n, -n);
        }
        let secs = iq.len() as f64 / fs;
        let mut c = WindowCheck::new(&w, fs, secs * 0.95);
        c.push_iq(&iq);
        let r = c.reports();
        assert!(matches!(r[0].heard.kind, Kind::Dmr | Kind::DmrControl), "{:?} {}", r[0].heard.kind, r[0].describe());
        assert!(r[0].on > 0.5, "on {:.2}", r[0].on);
        assert_eq!(r[1].heard.kind, Kind::Other);
        assert!(r[1].on < SOME_ON, "quiet one on {:.2}", r[1].on);
    }

    /// A DMR system: a shared control channel confirms no site by itself, and
    /// a site decoding as another kind of DMR than the system is unconfirmed.
    #[test]
    fn dmr_sites_need_their_own_frequency_and_the_systems_kind() {
        use crate::dmr::Variant;
        let f = parse_sites("Site Dec,Site Hex,Description,County Name,Lat,Lon,Range,Frequencies\n\
001,1,\"Wurtsboro\",\"Sullivan\",41.57,-74.48,20,464.937500c,463.100000c\n\
002,2,\"Saugerties\",\"Ulster\",42.07,-73.95,20,464.937500c,461.500000c\n\
003,3,\"North East\",\"Dutchess\",41.97,-73.55,20,463.337500c,462.000000c\n\
004,4,\"Tinker Hill\",\"Dutchess\",41.6,-73.6,20,451.000000c\n").unwrap();
        let by = |v: Vec<FreqReport>| v.into_iter().map(|r| (r.freq_hz as u64, r)).collect::<BTreeMap<_, _>>();
        let dmr = |f: f64, v: Variant| {
            let mut r = heard(f, Kind::DmrControl, 0.9, Identity::default());
            r.heard.dmr = Some(DmrFound { variant: Some(v), color_code: Some(1) });
            r
        };
        let air = by(vec![
            dmr(464_937_500.0, Variant::ConnectPlus),
            dmr(463_100_000.0, Variant::ConnectPlus),
            dmr(463_337_500.0, Variant::ConnectPlus),
            dmr(451_000_000.0, Variant::TierIII),
        ]);
        let v = judge_all(SystemType::Dmr, &f.sites, &air);
        assert_eq!(v[0].0, Verdict::Active, "{}", v[0].1);
        assert_eq!(v[1].0, Verdict::Carrier, "only the shared frequency: {}", v[1].1);
        assert!(v[1].1.contains("Wurtsboro"), "{}", v[1].1);
        assert_eq!(v[2].0, Verdict::Active, "{}", v[2].1);
        assert_eq!(v[3].0, Verdict::Carrier, "{}", v[3].1);
        assert!(v[3].1.contains("Connect Plus"), "{}", v[3].1);
    }

    #[test]
    fn zip_codes_and_distances() {
        let p = zip_location("12601").unwrap();
        assert!((p.0 - 41.7078).abs() < 0.01 && (p.1 + 73.9106).abs() < 0.01, "{p:?}");
        assert!(zip_location("00000").is_none() && zip_location("1260").is_none() && zip_location("abcde").is_none());
        assert!(zip_location("99929").is_some(), "the table's last ZIP");
        // Poughkeepsie to Manhattan: about 70 miles.
        let d = miles(p, (40.7128, -74.0060));
        assert!((65.0..75.0).contains(&d), "{d}");
        assert_eq!(miles(p, p), 0.0);
        assert_eq!(duration(45.0), "45 s");
        assert_eq!(duration(720.0), "12 min");
        assert_eq!(duration(12.0 * 3600.0), "12.0 h");
    }

    #[test]
    fn a_long_run_folds_its_passes_together() {
        let mut a = heard(463_337_500.0, Kind::Other, 0.0, Identity::default());
        a.listened_s = 120.0;
        a.peak_db = 3.0;
        let mut b = heard(463_337_500.0, Kind::Dmr, 0.5, Identity::default());
        b.listened_s = 40.0;
        a.merge(b);
        assert_eq!(a.listened_s, 160.0);
        assert!((a.on - 0.125).abs() < 1e-9, "{}", a.on);
        assert_eq!(a.peak_db, 20.0);
        assert_eq!(a.heard.kind, Kind::Dmr, "a decode in any pass counts");
        // A later quiet pass doesn't lose it.
        a.merge(heard(463_337_500.0, Kind::Other, 0.0, Identity::default()));
        assert_eq!(a.heard.kind, Kind::Dmr);
    }

    #[test]
    fn decodes_are_graded() {
        let mut r = heard(770_156_250.0, Kind::Control, 1.0, Identity::default());
        r.heard.good = 95;
        r.heard.bad = 5;
        assert_eq!(r.quality(SystemType::P25), Quality::Good);
        r.peak_db = 8.0;
        assert_eq!(r.quality(SystemType::P25), Quality::Marginal, "clean but weak");
        r.peak_db = 20.0;
        r.heard.bad = 40;
        assert_eq!(r.quality(SystemType::P25), Quality::Marginal, "errors");
        r.heard.bad = 200;
        assert_eq!(r.quality(SystemType::P25), Quality::Poor, "mostly failing");
        assert_eq!(heard(1.0, Kind::Other, 0.9, Identity::default()).quality(SystemType::P25), Quality::Poor);
        assert_eq!(heard(1.0, Kind::Other, 0.9, Identity::default()).quality(SystemType::Edacs), Quality::Good);
        assert_eq!(heard(1.0, Kind::P25, 0.9, Identity::default()).quality(SystemType::P25), Quality::Marginal, "no control messages");
        assert_eq!(heard(1.0, Kind::Dmr, 0.1, Identity::default()).quality(SystemType::Dmr), Quality::Good);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    /// How fast a window of 39 frequencies is listened to, vs real time (cargo test --release -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn listening_keeps_up() {
        let freqs: Vec<f64> = (0..39).map(|k| 451_175_000.0 + k as f64 * 700_000.0).collect();
        for (rate, n) in [(8e6, 39), (20e6, 39), (34e6, 39), (60e6, 39), (34e6, 1), (60e6, 1)] {
            let w = plan(&freqs[..n], rate);
            let w0 = &w[0];
            let secs = 2.0;
            let threads = std::env::var("THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(1);
            let mut c = ParallelCheck::new(w0, rate, secs, Some(SystemType::Dmr), threads);
            let mut s = 1u64;
            let n = (rate * 0.1) as usize;
            let mut iq = vec![Complex32::new(0.0, 0.0); n];
            let t = std::time::Instant::now();
            while !c.done() {
                for x in iq.iter_mut() {
                    s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                    *x = Complex32::new(((s >> 40) as f32 / 16777216.0) - 0.5, ((s >> 16 & 0xffffff) as f32 / 16777216.0) - 0.5);
                }
                c.push_iq(&iq);
            }
            let el = t.elapsed().as_secs_f64();
            println!("{:>4.0} MS/s: {} windows, {} freqs in the first, {} parts, {:.2}x real time", rate / 1e6, w.len(), w0.freqs.len(), c.parts(), secs / el);
        }
    }

    /// Split over cores, a window hears the same: the DMR repeater, and the quiet frequency beside it.
    #[test]
    fn a_window_split_over_cores_hears_the_same() {
        use crate::dmr::synth::{dibits, modulate, Tx};
        let fs = 1_200_000.0;
        let w = Window { center_hz: 851_000_000.0, freqs: vec![850_800_000.0, 851_150_000.0, 851_300_000.0] };
        let tx = Tx { cc: 3, slot: 1, talkgroup: 101, source: 2001, superframes: 8, idle_before: 4, idle_after: 4, csbks: vec![], mobile: false };
        let mut ph = 0.0;
        let mut iq = modulate(&dibits(&tx), fs, 150_000.0, 0.3, &mut ph);
        let mut s = 0x1234_5678u64;
        for x in iq.iter_mut() {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let n = ((s >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.02;
            *x += Complex32::new(n, -n);
        }
        let secs = iq.len() as f64 / fs * 0.95;
        let mut c = ParallelCheck::new(&w, fs, secs, Some(SystemType::Dmr), 3);
        assert_eq!(c.parts(), 3);
        for chunk in iq.chunks(10_000) {
            c.push_iq(chunk);
        }
        c.flush();
        assert!(c.done());
        let r = c.reports();
        assert_eq!(r.iter().map(|r| r.freq_hz).collect::<Vec<_>>(), w.freqs);
        assert!(matches!(r[1].heard.kind, Kind::Dmr | Kind::DmrControl), "{}", r[1].describe());
        assert_eq!(r[0].heard.kind, Kind::Other);
        assert_eq!(r[2].heard.kind, Kind::Other);
        assert!((c.listened_s() - secs).abs() < 0.1, "{}", c.listened_s());
    }
}
