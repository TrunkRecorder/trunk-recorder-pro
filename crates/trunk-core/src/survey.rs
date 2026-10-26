//! Finding a P25, SmartNet, trunked DMR or NXDN system with nothing
//! configured: sweep the land-mobile bands for carriers that never key down
//! (a control channel transmits all the time), check each with the P25,
//! SmartNet, DMR and NXDN receivers (a DMR control or rest channel says
//! which kind of trunking its blocks are, an NXDN control channel its RAN,
//! system and site; DMR and NXDN sites aren't monitored further),
//! then sit on the best one and
//! learn the system from its broadcasts — band plan, identity, alternate
//! control channels, neighbouring sites, the voice channels it grants — and
//! the radio's frequency error, from where the control channel is heard
//! versus the frequency it announces for itself.
//!
//! A SmartNet control channel doesn't broadcast its band plan (OBT systems
//! least of all), so the monitor learns it from the air: which carrier comes
//! up when a channel number is granted ([`crate::smartnet::plan`]).
//!
//! ```text
//! scan, per hop:  settle → spectrum: each ~1 kHz cell's 20th-percentile power
//!                 over 0.35 s (a carrier that stays on keeps it high)
//!                 → peaks ≥ 4 dB over the local floor, 3.5–30 kHz wide
//!                 → P25 check: a head + receivers per peak for 0.9 s
//!                 (frames, CRC-valid TSBKs, NAC, identity) → next hop
//! monitor:        control channel head → receivers → TSBKs → SystemInfo;
//!                 mean phase step of the channel → carrier offset → ppm;
//!                 [gain steps: SNR, TSBK rate, clipping → best gain]
//! ```
//!
//! The frequency error: a reference off by ε puts every frequency the radio
//! reports at f/(1+ε), so ε = announced / heard − 1 (plus the correction
//! already applied). Base stations are GPS- or rubidium-disciplined, far
//! better than any SDR's crystal.
//!
//! No I/O: the platform tunes the radio when [`Survey::command`] asks,
//! reports the centre it got (and each applied gain) with [`Survey::tuned`],
//! and feeds samples. Samples before that report are the old frequency and
//! are dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::PI;

use num_complex::Complex32;

use crate::dsp::c4fm::SYMBOL_RATE;
use crate::dsp::{Channelizer, HeadId};
use crate::p25::diversity::{best_frame, control_frame, Bank, BankConfig, Group};
use crate::p25::frame::{LDU1, LDU2};
use crate::trunk::identity::{IdField, Identity};
use crate::smartnet::plan::{self, PlanConfig, PlanFinder};
use crate::smartnet::{self as sn, Word};
use crate::trunk::{Message, MessageType, P25Parser};

/// One-sided channel filter cutoff for P25, Hz.
const CUTOFF_HZ: f64 = 7000.0;
const MIN_CHANNEL_RATE: f64 = 24_000.0;
/// Fraction of a hop's bandwidth searched (the edges are anti-alias roll-off).
pub const USABLE: f64 = 0.85;
const SPECTRUM_S: f64 = 0.35;
const DECODE_S: f64 = 0.9;
const MAX_PROBES: usize = 16;
const DETECT_DB: f64 = 4.0;
const MIN_WIDTH_HZ: f64 = 3500.0;
const MAX_WIDTH_HZ: f64 = 30_000.0;
/// Seconds of carrier averaging before the offset counts.
const PPM_MIN_S: f64 = 1.0;

/// A land-mobile band where P25 systems live (base-station transmit side).
#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub id: &'static str,
    pub label: &'static str,
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// Scanned unless the user chooses otherwise.
    pub default_on: bool,
}

/// In the order they are scanned (where most P25 systems are, first).
pub const BANDS: &[Band] = &[
    Band { id: "800", label: "800 MHz", lo_hz: 851.0e6, hi_hz: 869.0e6, default_on: true },
    Band { id: "700", label: "700 MHz", lo_hz: 769.0e6, hi_hz: 775.0e6, default_on: true },
    Band { id: "900", label: "900 MHz", lo_hz: 935.0e6, hi_hz: 941.0e6, default_on: true },
    Band { id: "uhf", label: "UHF", lo_hz: 450.0e6, hi_hz: 470.0e6, default_on: true },
    Band { id: "vhf", label: "VHF", lo_hz: 136.0e6, hi_hz: 174.0e6, default_on: true },
    Band { id: "uhf-fed", label: "UHF federal", lo_hz: 380.0e6, hi_hz: 420.0e6, default_on: false },
    // Business / industrial (where DMR systems mostly are): inside UHF and VHF,
    // listed on their own for a scan of just them. Overlaps are scanned once.
    Band { id: "biz-uhf", label: "Business UHF", lo_hz: 451.0e6, hi_hz: 470.0e6, default_on: false },
    Band { id: "biz-vhf", label: "Business VHF", lo_hz: 150.8e6, hi_hz: 174.0e6, default_on: false },
    Band { id: "t-band", label: "T-band", lo_hz: 470.0e6, hi_hz: 512.0e6, default_on: false },
];

fn band_of(hz: f64) -> &'static str {
    BANDS.iter().find(|b| hz >= b.lo_hz - 1e6 && hz <= b.hi_hz + 1e6).map_or("", |b| b.id)
}

#[derive(Clone, Debug)]
pub struct SurveyConfig {
    pub rate_hz: f64,
    /// Band ids ([`BANDS`]) to scan.
    pub bands: Vec<String>,
    /// A source that can't be tuned (a capture file): one look at this centre.
    pub fixed_center_hz: Option<f64>,
    /// What the radio can tune.
    pub tune_range_hz: (f64, f64),
    /// The correction the radio already applies, ppm.
    pub ppm: f64,
    /// Gains (dB) to step through while monitoring; empty = leave the gain alone.
    pub gains: Vec<f32>,
    /// Air discarded after each retune (PLL settling, queued samples).
    pub settle_s: f64,
    /// The receivers the monitored control channel runs.
    pub bank: BankConfig,
    /// Frequencies to look for (Hz, as licensed or listed): the scan covers
    /// these instead of the bands, and probes each, whether or not a carrier
    /// stands out there.
    pub targets: Vec<f64>,
    /// How long each hop's signals are probed, s.
    pub probe_s: f64,
    /// The receivers a probe runs (a system of a known kind: its own).
    pub receivers: Receivers,
    /// CPU cores a hop's probes may be shared out over (1: this thread
    /// only). A wide radio's hop holds dozens of them.
    pub threads: usize,
}

impl Default for SurveyConfig {
    fn default() -> Self {
        SurveyConfig {
            rate_hz: 2_400_000.0,
            bands: BANDS.iter().filter(|b| b.default_on).map(|b| b.id.to_string()).collect(),
            fixed_center_hz: None,
            tune_range_hz: (24e6, 1766e6),
            ppm: 0.0,
            gains: vec![],
            settle_s: 0.05,
            bank: BankConfig::default(),
            targets: Vec::new(),
            probe_s: DECODE_S,
            receivers: Receivers::ALL,
            threads: 1,
        }
    }
}

/// What the survey asks of the radio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Tune the centre (Hz), then call [`Survey::tuned`].
    Tune(f64),
    /// Set the gain (dB), then call [`Survey::tuned`].
    Gain(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A continuous signal that isn't P25 (another trunking type, data, …).
    Other,
    /// DMR bursts but no trunking control blocks (a conventional repeater, data).
    Dmr,
    /// NXDN frames but no control channel (a conventional repeater, a busy traffic channel).
    Nxdn,
    /// P25 frames but no control messages (a voice channel, conventional P25).
    P25,
    /// A trunked DMR control or rest channel ([`Candidate::dmr`] says which kind).
    DmrControl,
    /// An NXDN Type-C control channel (CRC-valid CAC; [`Candidate::nxdn`]).
    NxdnControl,
    /// An EDACS control channel (BCH-valid messages; its site ID in the identity).
    Edacs,
    /// A SmartNet / SmartZone control channel (CRC-valid OSWs).
    SmartNet,
    /// A P25 control channel.
    Control,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Other => "other",
            Kind::Dmr => "dmr",
            Kind::Nxdn => "nxdn",
            Kind::NxdnControl => "nxdnControl",
            Kind::P25 => "p25",
            Kind::DmrControl => "dmrControl",
            Kind::Edacs => "edacs",
            Kind::SmartNet => "smartnet",
            Kind::Control => "control",
        }
    }
}

/// A signal the scan found.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Where it was heard, in the radio's (uncorrected) frame.
    pub freq_hz: f64,
    pub band: &'static str,
    pub snr_db: f64,
    pub width_hz: f64,
    pub kind: Kind,
    pub frames: u32,
    pub good: u32,
    pub bad: u32,
    /// "C4FM" | "CQPSK" | "2FSK" (SmartNet) | "4FSK" (DMR) | "".
    pub modulation: &'static str,
    pub identity: Identity,
    /// DMR: the trunking kind its blocks are, and its colour code.
    pub dmr: Option<DmrFound>,
    /// NXDN: its rate, RAN, and (a control channel) its system and site.
    pub nxdn: Option<NxdnFound>,
    /// The frequency looked for it was probed for ([`SurveyConfig::targets`]), if one.
    pub target: Option<f64>,
    /// A target's carrier: the share of the time it was up.
    pub up: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NxdnFound {
    pub rate: crate::nxdn::Rate,
    pub ran: Option<u8>,
    /// SITE_INFO / SRV_INFO: (system code, site code).
    pub location: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DmrFound {
    pub variant: Option<crate::dmr::Variant>,
    pub color_code: Option<u8>,
}

impl Candidate {
    fn ok_ratio(&self) -> f64 {
        self.good as f64 / (self.good + self.bad).max(1) as f64
    }
    fn score(&self) -> f64 {
        self.snr_db + 10.0 * self.ok_ratio()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Adjacent {
    pub rfss: u32,
    pub site: u32,
    pub sys_id: u32,
    pub freq_hz: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceChannel {
    pub freq_hz: u64,
    pub grants: u32,
    pub tdma: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GainState {
    /// Not asked for (no gains configured).
    #[default]
    Off,
    /// Waiting for the control channel to be decoding well.
    Waiting,
    Running,
    Done,
}

#[derive(Clone, Copy, Debug)]
pub struct GainStep {
    pub gain_db: f32,
    pub snr_db: f64,
    /// CRC-valid share of the TSBKs in the window.
    pub ok_ratio: f64,
    /// Share of samples at the ADC's rails.
    pub clipped: f64,
}

#[derive(Clone, Debug, Default)]
pub struct GainReport {
    pub state: GainState,
    pub steps: Vec<GainStep>,
    pub best_db: Option<f32>,
}

/// What the monitored control channel has told us.
#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    /// Where the channel is heard (head centre + measured residual), radio's frame.
    pub heard_hz: f64,
    pub identity: Identity,
    /// The frequency the channel announces for itself (RFSS / network status).
    pub advertised_hz: Option<u64>,
    /// Secondary (alternate) control channels of this site.
    pub secondary: Vec<u64>,
    pub adjacent: Vec<Adjacent>,
    /// Voice channels seen in grants, by frequency.
    pub voice: Vec<VoiceChannel>,
    /// Band plan entries (IDEN_UP*) heard, and the plan as text.
    pub idens: usize,
    pub bandplan: String,
    pub good: u64,
    pub bad: u64,
    /// Seconds of air the decoded messages account for (P25; of `elapsed_s`).
    pub air_s: f64,
    pub modulation: &'static str,
    pub snr_db: Option<f64>,
    /// Heard − advertised, Hz.
    pub offset_hz: Option<f64>,
    /// The total correction to set (already applied + measured), ppm.
    pub ppm: Option<f64>,
    pub gain: GainReport,
    pub elapsed_s: f64,
    /// SmartNet (when its decoder is the one getting through): what the OSWs
    /// named and the band plan learned from them.
    pub smartnet: Option<SmartnetSurvey>,
}

/// What a SmartNet control channel has shown the monitor.
#[derive(Clone, Debug, Default)]
pub struct SmartnetSurvey {
    /// The number this control channel broadcasts for itself.
    pub cc_chan: Option<u16>,
    /// Alternate control channels it names.
    pub alt_chans: Vec<u16>,
    /// Channel numbers granted.
    pub channels: Vec<u16>,
    /// Channels whose carrier was found (radio frame).
    pub points: Vec<plan::Point>,
    pub fit: Option<plan::Fit>,
    /// The band plan, once two channels (the control channel counts) agree.
    pub plan: Option<PlanConfig>,
}

impl SystemInfo {
    /// Enough to record: band plan, where the channel really is, the correction.
    pub fn ready(&self) -> bool {
        (self.idens > 0 || self.smartnet.as_ref().is_some_and(|s| s.plan.is_some())) && self.advertised_hz.is_some() && self.ppm.is_some() && !matches!(self.gain.state, GainState::Running | GainState::Waiting)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Scanning,
    Monitoring,
    Done,
}

/// A narrowband signal in a hop's spectrum.
#[derive(Clone, Copy, Debug)]
pub struct Peak {
    /// Power-weighted centre, Hz from the hop centre.
    pub offset_hz: f64,
    pub snr_db: f64,
    pub width_hz: f64,
    /// Continuity: mean 20th-percentile power over the local floor (linear).
    pub score: f64,
}

/// The narrowband carriers that stayed on through `rows` (spectra from
/// [`Channelizer::cell_powers`], `cells` each, oldest first), strongest first,
/// within ±`max_offset_hz` of the centre.
pub fn find_peaks(rows: &[f32], cells: usize, fs: f64, fft_n: usize, max_offset_hz: f64) -> Vec<Peak> {
    let kept = rows.len() / cells.max(1);
    if kept == 0 || cells < 16 {
        return vec![];
    }
    let cell_hz = fs / cells as f64;
    let bin_hz = fs / fft_n as f64;
    let off = |c: usize| (c as f64 + 0.5) * cell_hz - fs / 2.0 - bin_hz / 2.0;
    let (mut p20, mut mean) = (vec![0f64; cells], vec![0f64; cells]);
    let mut col = vec![0f32; kept];
    for c in 0..cells {
        for (k, v) in col.iter_mut().enumerate() {
            *v = rows[k * cells + c];
        }
        mean[c] = col.iter().map(|&v| v as f64).sum::<f64>() / kept as f64;
        p20[c] = *col.select_nth_unstable_by(kept / 5, f32::total_cmp).1 as f64;
    }
    // The radio's DC offset sits in the cells either side of 0 Hz.
    let d = cells / 2;
    for v in [&mut p20, &mut mean] {
        let fill = (v[d - 2] + v[d + 1]) / 2.0;
        v[d - 1] = fill;
        v[d] = fill;
    }
    // Local floor: the lower quartile of ±80 kHz (follows the passband shape).
    let w = ((80_000.0 / cell_hz).round() as usize).max(8);
    let floor = |v: &[f64]| -> Vec<f64> {
        let mut win = Vec::with_capacity(2 * w + 1);
        (0..cells)
            .map(|c| {
                win.clear();
                win.extend_from_slice(&v[c.saturating_sub(w)..(c + w + 1).min(cells)]);
                let q = win.len() / 4;
                win.select_nth_unstable_by(q, f64::total_cmp).1.max(1e-30)
            })
            .collect()
    };
    let (f20, fmean) = (floor(&p20), floor(&mean));
    let r: Vec<f64> = (0..cells).map(|c| p20[c] / f20[c]).collect();
    let k = ((3500.0 / cell_hz).round() as usize).max(1);
    let s: Vec<f64> = (0..cells)
        .map(|c| {
            let (a, b) = (c.saturating_sub(k), (c + k + 1).min(cells));
            r[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let thr = 10f64.powf(DETECT_DB / 10.0);
    let sup = (9000.0 / cell_hz).ceil() as usize;
    let h = ((6000.0 / cell_hz).round() as usize).max(1);
    let mut taken = vec![false; cells];
    let mut peaks = Vec::new();
    while peaks.len() < MAX_PROBES {
        let Some(c) = (0..cells).filter(|&c| !taken[c] && off(c).abs() <= max_offset_hz).max_by(|&a, &b| s[a].total_cmp(&s[b])) else { break };
        if s[c] < thr {
            break;
        }
        // The strongest cell near the smoothed maximum, and the run of cells
        // around it within 20 dB of it and ≥ 2 dB over the floor (a strong
        // signal's skirts and a spur's leakage don't count as width).
        let m = (c.saturating_sub(k)..(c + k + 1).min(cells)).max_by(|&a, &b| r[a].total_cmp(&r[b])).unwrap_or(c);
        let edge = (r[m] / 100.0).max(10f64.powf(0.2));
        let (mut lo, mut hi) = (m, m);
        while lo > 0 && r[lo - 1] >= edge {
            lo -= 1;
        }
        while hi + 1 < cells && r[hi + 1] >= edge {
            hi += 1;
        }
        for t in taken.iter_mut().take((hi.max(c + sup) + 1).min(cells)).skip(lo.min(c.saturating_sub(sup))) {
            *t = true;
        }
        let width = (hi - lo + 1) as f64 * cell_hz;
        if !(MIN_WIDTH_HZ..=MAX_WIDTH_HZ).contains(&width) {
            continue;
        }
        let (mut sw, mut swf, mut noise) = (0.0, 0.0, 0.0);
        for i in m.saturating_sub(h)..(m + h + 1).min(cells) {
            let e = (mean[i] - fmean[i]).max(0.0);
            sw += e;
            swf += e * off(i);
            noise += fmean[i];
        }
        let offset_hz = if sw > 0.0 { swf / sw } else { off(m) };
        let snr_db = 10.0 * (sw / noise.max(1e-30)).max(1e-3).log10();
        peaks.push(Peak { offset_hz, snr_db, width_hz: width, score: s[c] });
    }
    peaks
}

/// Carrier offset from the mean phase step Σ x[n]·x*[n−1] — unbiased for
/// P25's symmetric symbol sets (C4FM deviations, π/4-DQPSK steps).
#[derive(Clone, Copy, Default)]
struct FreqEst {
    re: f64,
    im: f64,
    last: Complex32,
    n: u64,
}

impl FreqEst {
    fn push(&mut self, iq: &[Complex32]) {
        for &x in iq {
            let d = x * self.last.conj();
            self.re += d.re as f64;
            self.im += d.im as f64;
            self.last = x;
        }
        self.n += iq.len() as u64;
    }
    fn hz(&self, rate: f64) -> Option<f64> {
        (self.n > 0 && (self.re != 0.0 || self.im != 0.0)).then(|| self.im.atan2(self.re) * rate / (2.0 * PI))
    }
}

/// Frames → counts, NAC, identity and messages; shared by the scan's
/// checks and the monitor.
#[derive(Default)]
struct Decode {
    parser: P25Parser,
    nac_votes: BTreeMap<u16, u32>,
    frames: u32,
    voice: u32,
    good: u64,
    bad: u64,
    /// Symbols of air the decoded blocks account for.
    air: u64,
    id: Identity,
}

impl Decode {
    fn group(&mut self, g: &Group, t: f64, out: &mut Vec<Message>) {
        let f = best_frame(g);
        self.frames += 1;
        *self.nac_votes.entry(f.nid.nac).or_default() += 1;
        self.id.set_opt(IdField::Nac, self.nac_votes.iter().max_by_key(|&(_, &c)| c).map(|(&n, _)| n as u32));
        if f.nid.duid == LDU1 || f.nid.duid == LDU2 {
            self.voice += 1;
        }
        let cf = control_frame(g, f.nid.duid);
        self.bad += cf.lost as u64;
        self.air += cf.air;
        for o in &cf.osps {
            self.good += 1;
            for m in self.parser.parse(o, f.nid.nac, t) {
                match m.kind {
                    MessageType::SysId => {
                        self.id.set(IdField::SysId, m.sys_id);
                        self.id.set(IdField::Rfss, m.rfss);
                        self.id.set(IdField::Site, m.site);
                    }
                    MessageType::Status => {
                        self.id.set(IdField::Wacn, m.wacn);
                        self.id.set(IdField::SysId, m.sys_id);
                    }
                    _ => {}
                }
                out.push(m);
            }
        }
    }
}

fn modulation(labels: &[&'static str], frames: &[u64]) -> &'static str {
    let (mut q, mut c) = (0, 0);
    for (l, n) in labels.iter().zip(frames) {
        if *l == "C4FM" {
            c += n;
        } else {
            q += n;
        }
    }
    if q + c < 4 {
        ""
    } else if q > c {
        "CQPSK"
    } else {
        "C4FM"
    }
}

fn bank_labels(cfg: &BankConfig) -> Vec<&'static str> {
    let mut v = Vec::new();
    if cfg.cqpsk {
        v.push("CQPSK");
    }
    if cfg.cqpsk_eq {
        v.push("CQPSK");
    }
    if cfg.c4fm {
        v.push("C4FM");
    }
    v
}

/// Which of a probe's receivers run: each costs CPU on every signal probed,
/// so a system of a known kind runs just its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receivers {
    pub p25: bool,
    pub smartnet: bool,
    pub edacs: bool,
    pub dmr: bool,
    pub nxdn: bool,
}

impl Receivers {
    pub const ALL: Receivers = Receivers { p25: true, smartnet: true, edacs: true, dmr: true, nxdn: true };

    /// Just the receivers of a system type ("p25", "smartnet", "edacs", "dmr", "nxdn"); every one for another.
    pub fn of_type(t: &str) -> Receivers {
        let none = Receivers { p25: false, smartnet: false, edacs: false, dmr: false, nxdn: false };
        match t {
            "p25" => Receivers { p25: true, ..none },
            "smartnet" => Receivers { smartnet: true, ..none },
            "edacs" => Receivers { edacs: true, ..none },
            "dmr" => Receivers { dmr: true, ..none },
            "nxdn" => Receivers { nxdn: true, ..none },
            _ => Receivers::ALL,
        }
    }
}

struct Probe {
    peak: Peak,
    head: HeadId,
    bank: Bank,
    est: FreqEst,
    dec: Decode,
    sn: SnRx,
    edacs: EdacsRx,
    /// The DMR side: one carrier's site decoder (what trunking it is, if any).
    dmr: crate::dmr::Site,
    dmr_samples: u64,
    /// The NXDN side: a receiver per rate.
    nxdn: [NxdnProbe; 2],
    runs: Receivers,
    /// The frequency looked for that it probes ([`SurveyConfig::targets`]), if one.
    target: Option<f64>,
    /// A target's carrier meter: looks it was up, and its strongest over the band's floor (dB).
    up: u64,
    looks: u64,
    peak_db: f64,
}

impl Probe {
    /// A probe of the signal `peak.offset_hz` from `center`: a head on `chz`, and the receivers `runs`.
    fn new(chz: &mut Channelizer, center: f64, peak: Peak, runs: Receivers, target: Option<f64>) -> Self {
        let rate = chz.output_rate();
        let (head, _, _) = chz.add_head(peak.offset_hz, CUTOFF_HZ, 0.0);
        let dmr = crate::dmr::Site::new(&[center + peak.offset_hz], rate, Default::default());
        let nxdn = [NxdnProbe::new(crate::nxdn::Rate::N48, rate), NxdnProbe::new(crate::nxdn::Rate::N96, rate)];
        Probe {
            peak,
            head,
            bank: Bank::new(rate, PROBE_BANK),
            est: FreqEst::default(),
            dec: Decode::default(),
            sn: SnRx::new(rate),
            edacs: EdacsRx::new(rate),
            dmr,
            dmr_samples: 0,
            nxdn,
            runs,
            target,
            up: 0,
            looks: 0,
            peak_db: f64::NEG_INFINITY,
        }
    }

    /// The head's samples of the block `chz` just ran, through the receivers
    /// (`flush`: the last block, so the P25 receivers give up what they hold).
    fn feed(&mut self, chz: &Channelizer, rate: f64, groups: &mut Vec<Group>, msgs: &mut Vec<Message>, flush: bool) {
        let iq = chz.output(self.head).unwrap_or(&[]);
        let r = self.runs;
        if r.p25 || r.smartnet {
            self.est.push(iq);
        }
        if r.smartnet {
            self.sn.push(iq, |_, _| {});
        }
        if r.edacs {
            self.edacs.push(iq, rate);
        }
        if r.dmr {
            let mut dm = Vec::new();
            self.dmr.push(0, iq, 0.0, rate, &mut dm);
            self.dmr_samples += iq.len() as u64;
        }
        if r.nxdn {
            for n in self.nxdn.iter_mut() {
                n.push(iq);
            }
        }
        if !r.p25 {
            return;
        }
        groups.clear();
        self.bank.push(iq, groups);
        if flush {
            self.bank.flush(groups);
        }
        for g in groups.iter() {
            msgs.clear();
            self.dec.group(g, best_frame(g).sample / rate, msgs);
        }
    }

    /// What the receivers made of it, `center` the hop's.
    fn candidate(&self, center: f64, band: &'static str, rate: f64) -> Candidate {
        let p = self;
        // DMR: a continuous carrier frames ~33 bursts a second, most with a sync.
        let dmr_syncs = p.dmr.syncs(0);
        // NXDN: the rate whose receiver framed more (a 9600 signal
        // frames as noise at 2400 and the other way round).
        let nx = p.nxdn.iter().max_by_key(|n| (n.cac, n.lich)).unwrap();
        let kind = if p.dec.good >= 2 {
            Kind::Control
        } else if p.sn.framer.good >= 3 {
            Kind::SmartNet
        } else if p.edacs.framer.good >= 3 {
            Kind::Edacs
        } else if p.dmr.variant.is_some() && dmr_syncs >= 6 {
            Kind::DmrControl
        } else if nx.cac >= 2 {
            Kind::NxdnControl
        } else if p.dec.frames >= 3 {
            Kind::P25
        } else if dmr_syncs >= 6 {
            Kind::Dmr
        } else if nx.lich >= 4 {
            Kind::Nxdn
        } else {
            Kind::Other
        };
        // A target's level: where a peak stood out, the peak's; else what the meter saw.
        let snr_db = if p.peak.width_hz > 0.0 || p.looks == 0 { p.peak.snr_db } else { p.peak_db.max(0.0) };
        let base = Candidate {
            freq_hz: center + p.peak.offset_hz,
            band,
            snr_db,
            width_hz: p.peak.width_hz,
            kind,
            frames: 0,
            good: 0,
            bad: 0,
            modulation: "",
            identity: Identity::default(),
            dmr: None,
            nxdn: None,
            target: p.target,
            up: (p.looks > 0).then(|| p.up as f64 / p.looks as f64),
        };
        match kind {
            Kind::Nxdn | Kind::NxdnControl => Candidate {
                frames: nx.lich,
                good: nx.cac,
                modulation: if nx.rate == crate::nxdn::Rate::N48 { "NXDN48" } else { "NXDN96" },
                nxdn: Some(nx.found()),
                ..base
            },
            Kind::Dmr | Kind::DmrControl => {
                let cc = p.dmr.color_code.or(p.dmr.carriers[0].chan.slots.iter().find_map(|s| s.color_code));
                Candidate { frames: dmr_syncs as u32, modulation: "4FSK", dmr: Some(DmrFound { variant: p.dmr.variant, color_code: cc }), ..base }
            }
            Kind::Edacs => {
                let e = &p.edacs;
                let mut identity = Identity::default();
                identity.set_opt(IdField::Site, e.parser.site);
                Candidate { freq_hz: base.freq_hz + e.rx.offset_hz() as f64, frames: (e.framer.good + e.framer.bad) as u32, good: e.framer.good as u32, bad: e.framer.bad as u32, modulation: "2FSK", identity, ..base }
            }
            // The midpoint of the two tones.
            Kind::SmartNet => Candidate {
                freq_hz: base.freq_hz + p.sn.rx.offset_hz() as f64,
                frames: p.sn.framer.good as u32,
                good: p.sn.framer.good as u32,
                bad: p.sn.framer.bad as u32,
                modulation: "2FSK",
                ..base
            },
            _ => {
                // A decoding signal's own carrier beats the spectrum's centroid.
                let fine = if kind == Kind::Other { 0.0 } else { p.est.hz(rate).filter(|r| r.abs() < 3000.0).unwrap_or(0.0) };
                Candidate {
                    freq_hz: base.freq_hz + fine,
                    frames: p.dec.frames,
                    good: p.dec.good as u32,
                    bad: p.dec.bad as u32,
                    modulation: modulation(&bank_labels(&PROBE_BANK), &p.bank.frames_per_rx()),
                    identity: p.dec.id.clone(),
                    ..base
                }
            }
        }
    }
}

/// An EDACS control channel receiver: 9600 baud 2FSK, the framer, and the
/// parser for the site ID it broadcasts.
struct EdacsRx {
    rx: sn::Fsk2,
    framer: crate::edacs::framer::Framer,
    parser: crate::edacs::Parser,
    bits: Vec<sn::Bit>,
}

impl EdacsRx {
    fn new(rate: f64) -> Self {
        let cfg = crate::edacs::EdacsConfig { lcns: Default::default(), esk: false };
        EdacsRx { rx: crate::edacs::receiver(rate), framer: Default::default(), parser: crate::edacs::Parser::new(cfg), bits: Vec::new() }
    }

    fn push(&mut self, iq: &[Complex32], rate: f64) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        let mut out = Vec::new();
        for b in &self.bits {
            // (The receiver counts its samples from the first.)
            if let Some(f) = self.framer.push(b.soft, b.sample / rate) {
                out.clear();
                self.parser.frame(f.msgs[0], f.msgs[1], f.t, &mut out);
            }
        }
    }
}

/// An NXDN receiver and framer at one rate: frames with a valid LICH, CRC-valid CACs, the RAN and the site.
struct NxdnProbe {
    rate: crate::nxdn::Rate,
    rx: crate::dsp::c4fm::C4fm,
    framer: crate::nxdn::frame::Framer,
    syms: Vec<crate::dsp::Symbol>,
    frames: Vec<crate::nxdn::frame::Frame>,
    lich: u32,
    cac: u32,
    ran: BTreeMap<u8, u32>,
    location: Option<(u32, u32)>,
}

impl NxdnProbe {
    fn new(rate: crate::nxdn::Rate, fs: f64) -> Self {
        let mut o = crate::dsp::c4fm::C4fmOptions::nxdn(rate.baud());
        o.msd = None;
        NxdnProbe {
            rate,
            rx: crate::dsp::c4fm::C4fm::with_options(fs, o),
            framer: crate::nxdn::frame::Framer::new(),
            syms: Vec::new(),
            frames: Vec::new(),
            lich: 0,
            cac: 0,
            ran: BTreeMap::new(),
            location: None,
        }
    }

    fn push(&mut self, iq: &[Complex32]) {
        use crate::dsp::Receiver;
        use crate::nxdn::channel;
        use crate::nxdn::layer3::{Context, Message as L3};
        self.syms.clear();
        self.rx.push(iq, &mut self.syms);
        self.frames.clear();
        for s in &self.syms {
            self.framer.push(s, &mut self.frames);
        }
        for f in &self.frames {
            let Some((l, _)) = f.lich() else { continue };
            self.lich += 1;
            if l.is_cac() {
                let Some((sr, o, _)) = channel::cac(f) else { continue };
                self.cac += 1;
                *self.ran.entry(sr.ran).or_default() += 1;
                let mut msgs = vec![L3::parse(&o, Context::Control, false)];
                if sr.structure & 1 != 0 {
                    msgs.push(L3::parse(&o[9..], Context::Control, false));
                }
                for m in msgs {
                    if let L3::SiteInfo { location, .. } | L3::SrvInfo { location, .. } = m {
                        self.location = Some((location.system(), location.site()));
                    }
                }
            } else if let Some(s) = channel::sacch(f) {
                *self.ran.entry(s.sr.ran).or_default() += 1;
            }
        }
    }

    fn found(&self) -> NxdnFound {
        NxdnFound { rate: self.rate, ran: self.ran.iter().max_by_key(|&(_, n)| n).map(|(&r, _)| r), location: self.location }
    }
}

/// A SmartNet receiver and framer (the probes' and the monitor's).
struct SnRx {
    rx: sn::Fsk2,
    framer: sn::Framer,
    bits: Vec<sn::Bit>,
    out: Vec<Word>,
}

impl SnRx {
    fn new(rate: f64) -> Self {
        SnRx { rx: sn::Fsk2::new(rate), framer: sn::Framer::new(), bits: Vec::new(), out: Vec::new() }
    }

    /// Decode `iq`; each framer output with its channel-sample instant.
    fn push(&mut self, iq: &[Complex32], mut each: impl FnMut(Word, f64)) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        for b in &self.bits {
            self.out.clear();
            self.framer.push(b.soft, &mut self.out);
            for &o in &self.out {
                each(o, b.sample);
            }
        }
    }
}

const PROBE_BANK: BankConfig = BankConfig {
    cqpsk: true,
    cqpsk_eq: false,
    c4fm: true,
    eq_taps: 0,
    eq_mu: 0.02,
    framer: crate::p25::frame::FramerOptions { flywheel: true, flywheel_errs: 12, nid_recover: true, nid_max_errs: 11 },
    soft: true,
    // (A probe is short: it tells the modulation by which kind decodes.)
    detect: false,
};

/// One hop of the scan: the spectrum, then a probe on each carrier that
/// stands out and each frequency looked for. The probes may be shared out
/// over several cores ([`SurveyConfig::threads`]), each share on its own
/// channelizer fed the same samples.
struct HopScan {
    center: f64,
    band: &'static str,
    /// The first share's channelizer makes the spectrum.
    parts: Vec<ProbePart>,
    row: Vec<f32>,
    rows: Vec<f32>,
    blocks: u64,
    stride: u64,
    spectrum_blocks: u64,
    decode_blocks: u64,
    decoding: bool,
    targets: Vec<f64>,
    runs: Receivers,
    threads: usize,
    /// Samples waiting to be handed to the shares together (with more than one).
    held: Vec<Complex32>,
    batch: usize,
}

/// Some of a hop's probes, on a channelizer of their own.
struct ProbePart {
    chz: Channelizer,
    probes: Vec<Probe>,
    blocks: u64,
    /// Blocks between looks at the targets' carriers (~20 ms).
    meter_stride: u64,
    cells: Vec<f32>,
    sorted: Vec<f32>,
    groups: Vec<Group>,
    msgs: Vec<Message>,
}

/// A target's carrier is up when its band is this much over the floor (power ratio, 6 dB).
const UP_RATIO: f64 = 4.0;
/// Half the width a target's carrier is measured over, Hz.
const METER_HALF_HZ: f64 = 5_000.0;
/// A peak this close to a target is its signal, Hz.
const SAME_SIGNAL_HZ: f64 = 6_000.0;

impl ProbePart {
    fn new(chz: Channelizer, cells: usize) -> Self {
        let meter_stride = ((0.02 / chz.block_seconds()).round() as u64).max(1);
        ProbePart { chz, probes: Vec::new(), blocks: 0, meter_stride, cells: vec![0.0; cells], sorted: Vec::with_capacity(cells), groups: Vec::new(), msgs: Vec::new() }
    }

    /// Samples through the probes, until `until` blocks have run (the rest dropped).
    fn push(&mut self, mut iq: &[Complex32], until: u64) {
        while !iq.is_empty() && self.blocks < until {
            let (used, ran) = self.chz.feed(iq);
            iq = &iq[used..];
            if ran {
                self.blocks += 1;
                self.on_block(self.blocks >= until);
            }
        }
    }

    fn on_block(&mut self, last: bool) {
        let rate = self.chz.output_rate();
        for p in &mut self.probes {
            p.feed(&self.chz, rate, &mut self.groups, &mut self.msgs, last);
        }
        if !self.blocks.is_multiple_of(self.meter_stride) || !self.probes.iter().any(|p| p.target.is_some()) {
            return;
        }
        // The floor: the median cell of the band's middle (its edges roll off).
        self.chz.cell_powers(&mut self.cells);
        let n = self.cells.len();
        self.sorted.clear();
        self.sorted.extend_from_slice(&self.cells[n / 10..n - n / 10]);
        let mid = self.sorted.len() / 2;
        let floor = (*self.sorted.select_nth_unstable_by(mid, f32::total_cmp).1 as f64).max(1e-30);
        for p in self.probes.iter_mut().filter(|p| p.target.is_some()) {
            let r = self.chz.band_power(p.peak.offset_hz, METER_HALF_HZ) / floor;
            p.looks += 1;
            p.up += (r >= UP_RATIO) as u64;
            p.peak_db = p.peak_db.max(10.0 * r.max(1e-9).log10());
        }
    }
}

impl HopScan {
    fn new(center: f64, band: &'static str, cfg: &SurveyConfig, targets: Vec<f64>) -> Self {
        let rate = cfg.rate_hz;
        let chz = Channelizer::new(rate, MIN_CHANNEL_RATE, 0.01);
        // Cells of ~1 kHz or wider (a power of two dividing the FFT).
        let mut cells = chz.fft_size();
        while cells > 16 && rate / (cells as f64) < 900.0 {
            cells /= 2;
        }
        let bs = chz.block_seconds();
        HopScan {
            center,
            band,
            row: vec![0.0; cells],
            rows: Vec::new(),
            blocks: 0,
            // At most ~100 spectra: one per ≥ 3.5 ms of air.
            stride: ((0.0035 / bs).ceil() as u64).max(1),
            spectrum_blocks: (SPECTRUM_S / bs).ceil() as u64,
            decode_blocks: (cfg.probe_s / bs).ceil().max(1.0) as u64,
            decoding: false,
            targets,
            runs: cfg.receivers,
            threads: cfg.threads.max(1),
            held: Vec::new(),
            // Handed over every 50 ms of samples: starting the threads is nothing beside that.
            batch: ((rate * 0.05) as usize).max(4096),
            parts: vec![ProbePart::new(chz, cells)],
        }
    }

    /// Samples of the hop; its findings once it is over (the rest of `iq` dropped).
    fn push(&mut self, mut iq: &[Complex32]) -> Option<Vec<Candidate>> {
        if !self.decoding {
            while !iq.is_empty() {
                let chz = &mut self.parts[0].chz;
                let (used, ran) = chz.feed(iq);
                iq = &iq[used..];
                if !ran {
                    continue;
                }
                self.blocks += 1;
                if self.blocks.is_multiple_of(self.stride) {
                    chz.cell_powers(&mut self.row);
                    self.rows.extend_from_slice(&self.row);
                }
                if self.blocks >= self.spectrum_blocks {
                    if !self.start_probes() {
                        return Some(vec![]);
                    }
                    break;
                }
            }
            if !self.decoding {
                return None;
            }
        }
        let until = self.decode_blocks;
        if self.parts.len() == 1 {
            self.parts[0].push(iq, until);
        } else {
            self.held.extend_from_slice(iq);
            if self.held.len() >= self.batch {
                let held = std::mem::take(&mut self.held);
                std::thread::scope(|s| {
                    for p in self.parts.iter_mut() {
                        let h = &held;
                        s.spawn(move || p.push(h, until));
                    }
                });
                self.held = held;
                self.held.clear();
            }
        }
        if self.parts.iter().any(|p| p.blocks < until) {
            return None;
        }
        let rate = self.parts[0].chz.output_rate();
        let mut found: Vec<Candidate> = self.parts.iter().flat_map(|p| p.probes.iter().map(|x| x.candidate(self.center, self.band, rate))).collect();
        found.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
        Some(found)
    }

    /// After the spectrum: a probe on each carrier and each target, shared
    /// out; false when there is nothing to probe.
    fn start_probes(&mut self) -> bool {
        let fs = self.parts[0].chz.fs();
        let mut peaks: Vec<(Peak, Option<f64>)> = find_peaks(&self.rows, self.row.len(), fs, self.parts[0].chz.fft_size(), fs / 2.0 * USABLE).into_iter().map(|p| (p, None)).collect();
        // Each target is the carrier standing out there, else gets a probe of its own.
        for &t in &self.targets {
            let off = t - self.center;
            match peaks.iter_mut().filter(|(p, x)| x.is_none() && (p.offset_hz - off).abs() < SAME_SIGNAL_HZ).min_by(|a, b| (a.0.offset_hz - off).abs().total_cmp(&(b.0.offset_hz - off).abs())) {
                Some(x) => x.1 = Some(t),
                None => peaks.push((Peak { offset_hz: off, snr_db: 0.0, width_hz: 0.0, score: 0.0 }, Some(t))),
            }
        }
        if peaks.is_empty() {
            return false;
        }
        let n = self.threads.clamp(1, peaks.len());
        let per = peaks.len().div_ceil(n);
        let cells = self.row.len();
        let rate = self.parts[0].chz.fs();
        for (k, share) in peaks.chunks(per).enumerate() {
            if k > 0 {
                self.parts.push(ProbePart::new(Channelizer::new(rate, MIN_CHANNEL_RATE, 0.01), cells));
            }
            let part = &mut self.parts[k];
            for &(p, target) in share {
                let probe = Probe::new(&mut part.chz, self.center, p, self.runs, target);
                part.probes.push(probe);
            }
        }
        self.decoding = true;
        true
    }

    fn spectrum(&self, bins: usize) -> Vec<f32> {
        self.parts[0].chz.power_spectrum(bins)
    }
}

enum GainPhase {
    Waiting,
    /// Asked for step `i`; waiting for the radio to confirm.
    Applying { i: usize, since: u64 },
    Settling { i: usize, until: u64 },
    Measuring { i: usize, until: u64, snr: f64, n: u32, good: u64, bad: u64, clip: (u64, u64) },
    Final,
}

/// The monitor's SmartNet side: OSWs decoded with the plan unknown
/// (channel numbers for frequencies), and the plan being learned.
struct SnMonitor {
    rx: SnRx,
    parser: sn::Parser,
    finder: PlanFinder,
    row: Vec<f32>,
    sorted: Vec<f32>,
    cc_votes: BTreeMap<u16, u32>,
    /// System IDs: the OSW before a "this control channel" broadcast.
    sys_votes: BTreeMap<u16, u32>,
    prev: Option<sn::Osw>,
    alt: BTreeSet<u16>,
    grants: BTreeMap<u16, u32>,
    msgs: Vec<Message>,
    info: SmartnetSurvey,
    next_fit: u64,
}

impl SnMonitor {
    fn new(chz: &Channelizer, center: f64) -> Self {
        let mut cells = chz.fft_size();
        while cells > 16 && chz.fs() / (cells as f64) < 900.0 {
            cells /= 2;
        }
        SnMonitor {
            rx: SnRx::new(chz.output_rate()),
            parser: sn::Parser::new(sn::Bandplan::Raw),
            finder: PlanFinder::new(center, chz.fs(), cells, chz.fs() / 2.0 * USABLE),
            row: vec![0.0; cells],
            sorted: vec![0.0; cells],
            cc_votes: BTreeMap::new(),
            sys_votes: BTreeMap::new(),
            prev: None,
            alt: BTreeSet::new(),
            grants: BTreeMap::new(),
            msgs: Vec::new(),
            info: SmartnetSurvey::default(),
            next_fit: 0,
        }
    }

    fn osw(&mut self, o: sn::Osw, t: f64) {
        // This channel's own number, and its alternates.
        if o.cmd == 0x30b && o.grp && o.addr & 0xfc00 == 0x2800 {
            *self.cc_votes.entry(o.addr & 0x3ff).or_default() += 1;
            if let Some(p) = self.prev.filter(|p| p.grp) {
                *self.sys_votes.entry(p.addr).or_default() += 1;
            }
        } else if !o.grp && o.addr & 0xff00 == 0x1f00 && o.cmd < 0x2f8 {
            *self.cc_votes.entry(o.cmd).or_default() += 1;
        } else if o.cmd == 0x30b && !o.grp && o.addr & 0xfc00 == 0x6000 {
            self.alt.insert(o.addr & 0x3ff);
        }
        self.prev = Some(o);
        self.parser.osw(o, t, &mut self.msgs);
        for m in self.msgs.drain(..) {
            if matches!(m.kind, MessageType::Grant | MessageType::Update) {
                let c = m.freq_hz as u16;
                self.finder.heard(c, m.time_s);
                *self.grants.entry(c).or_default() += 1;
            }
        }
    }

    /// One spectrum, scaled to its median (the noise floor), so gain steps don't count as carriers.
    fn spectrum(&mut self, chz: &Channelizer, t: f64) {
        chz.cell_powers(&mut self.row);
        self.sorted.copy_from_slice(&self.row);
        let mid = self.sorted.len() / 2;
        let (_, m, _) = self.sorted.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        let m = m.max(1e-20);
        self.row.iter_mut().for_each(|v| *v /= m);
        self.finder.spectrum(&self.row, t);
    }

    /// Fit the plan through what's been seen, the control channel heard at `heard_hz`.
    fn fit(&mut self, heard_hz: f64) {
        let cc = self.cc_votes.iter().max_by_key(|&(_, &n)| n).map(|(&c, _)| c);
        let i = &mut self.info;
        i.cc_chan = cc;
        i.alt_chans = self.alt.iter().copied().filter(|&c| Some(c) != cc).collect();
        i.channels = self.finder.channels();
        i.points = self.finder.points();
        i.fit = plan::fit(&i.points, cc.map(|c| (c, heard_hz)));
        // Only the points on the line: an OBT inbound channel is granted with
        // its outbound one, so it "finds" the outbound carrier, off the line.
        let mut chans: Vec<u16> = match i.fit {
            Some(f) => i.points.iter().filter(|p| (p.hz - f.hz(p.chan)).abs() <= (f.spacing_hz / 4.0).min(3000.0) + 1.0).map(|p| p.chan).collect(),
            None => vec![],
        };
        chans.extend(cc);
        i.plan = i.fit.map(|f| plan::plan_for(&f, cc, &chans));
    }
}

/// The monitored control channel.
struct Monitor {
    chz: Channelizer,
    center: f64,
    head: HeadId,
    head_hz: f64,
    bank: Bank,
    labels: Vec<&'static str>,
    dec: Decode,
    est: FreqEst,
    est_from: u64,
    blocks: u64,
    recenters: u32,
    noise: Vec<f64>,
    snr: Option<f64>,
    secondary: BTreeSet<u64>,
    adjacent: BTreeMap<(u32, u32, u32), Adjacent>,
    voice: BTreeMap<u64, VoiceChannel>,
    info: SystemInfo,
    gains: Vec<f32>,
    gain: GainPhase,
    pending: Option<Command>,
    groups: Vec<Group>,
    msgs: Vec<Message>,
    ppm_applied: f64,
    sn: SnMonitor,
    /// Its P25 receivers' settings (a re-centre starts them afresh).
    bank_cfg: BankConfig,
}

impl Monitor {
    fn new(center: f64, freq: f64, cfg: &SurveyConfig) -> Self {
        let mut chz = Channelizer::new(cfg.rate_hz, MIN_CHANNEL_RATE, 0.01);
        let (head, _, _) = chz.add_head(freq - center, CUTOFF_HZ, 0.0);
        let rate = chz.output_rate();
        let sn = SnMonitor::new(&chz, center);
        let mut info = SystemInfo { heard_hz: freq, ..Default::default() };
        info.gain.state = if cfg.gains.is_empty() { GainState::Off } else { GainState::Waiting };
        Monitor {
            chz,
            center,
            head,
            head_hz: freq,
            bank: Bank::new(rate, cfg.bank),
            bank_cfg: cfg.bank,
            labels: bank_labels(&cfg.bank),
            dec: Decode::default(),
            est: FreqEst::default(),
            est_from: 2,
            blocks: 0,
            recenters: 0,
            noise: vec![0.0; 64],
            snr: None,
            secondary: BTreeSet::new(),
            adjacent: BTreeMap::new(),
            voice: BTreeMap::new(),
            info,
            gains: cfg.gains.clone(),
            gain: GainPhase::Waiting,
            pending: None,
            groups: Vec::new(),
            msgs: Vec::new(),
            ppm_applied: cfg.ppm,
            sn,
        }
    }

    /// SmartNet is what this channel is (its OSWs outnumber P25 TSBKs).
    fn is_smartnet(&self) -> bool {
        self.sn.rx.framer.good > self.dec.good
    }

    /// Good / bad control messages of whichever protocol it is.
    fn counts(&self) -> (u64, u64) {
        if self.is_smartnet() {
            (self.sn.rx.framer.good, self.sn.rx.framer.bad)
        } else {
            (self.dec.good, self.dec.bad)
        }
    }

    fn seconds(&self, blocks: u64) -> f64 {
        blocks as f64 * self.chz.block_seconds()
    }
    fn blocks_for(&self, s: f64) -> u64 {
        (s / self.chz.block_seconds()).ceil() as u64
    }

    /// The radio applied a gain step.
    fn gain_applied(&mut self) {
        if let GainPhase::Applying { i, .. } = self.gain {
            self.gain = GainPhase::Settling { i, until: self.blocks + self.blocks_for(0.3) };
        }
    }

    fn on_block(&mut self, clip: (u64, u64), flush: bool) {
        self.blocks += 1;
        let rate = self.chz.output_rate();
        let iq = self.chz.output(self.head).unwrap_or(&[]);
        if self.blocks >= self.est_from {
            self.est.push(iq);
        }
        let t = self.seconds(self.blocks);
        let mut outs = Vec::new();
        self.sn.rx.push(iq, |o, _| outs.push(o));
        for o in outs {
            match o {
                Word::Osw(o, _) => self.sn.osw(o, t),
                Word::Bad(_) => self.sn.parser.bad(t, &mut self.sn.msgs),
            }
        }
        self.groups.clear();
        self.bank.push(iq, &mut self.groups);
        if flush {
            self.bank.flush(&mut self.groups);
        }
        for g in std::mem::take(&mut self.groups) {
            self.msgs.clear();
            self.dec.group(&g, t, &mut self.msgs);
            for m in std::mem::take(&mut self.msgs) {
                self.message(&m);
            }
        }
        if self.blocks % 4 == 0 {
            self.chz.noise_profile(&mut self.noise);
            let off = self.head_hz - self.center;
            let fs = self.chz.fs();
            let s = crate::dsp::channelizer::floor_slice(self.noise.len(), fs, off);
            let nb = self.noise[s];
            if nb > 0.0 {
                let r = ((self.chz.band_power(off, 5000.0) - nb) / nb).max(1e-3);
                self.snr = Some(self.snr.map_or(r, |v| v + 0.05 * (r - v)));
                if let GainPhase::Measuring { snr, n, .. } = &mut self.gain {
                    *snr += r;
                    *n += 1;
                }
            }
        }
        if self.sn.rx.framer.good > 0 {
            if self.blocks % 2 == 0 {
                self.sn.spectrum(&self.chz, t);
            }
            if self.blocks >= self.sn.next_fit || flush {
                self.sn.next_fit = self.blocks + self.blocks_for(1.0);
                self.sn.fit(self.info.heard_hz);
            }
        }
        self.frequency(rate);
        self.gain_step(clip);
        if self.is_smartnet() {
            self.smartnet_info(t);
            return;
        }
        let i = &mut self.info;
        i.identity = self.dec.id.clone();
        i.good = self.dec.good;
        i.bad = self.dec.bad;
        i.air_s = self.dec.air as f64 / SYMBOL_RATE;
        i.modulation = modulation(&self.labels, &self.bank.frames_per_rx());
        i.snr_db = self.snr.map(|r| 10.0 * r.log10());
        i.elapsed_s = t;
        i.idens = self.dec.parser.tables.len();
        i.bandplan = self.dec.parser.bandplan_to_string();
        i.secondary = self.secondary.iter().copied().collect();
        i.adjacent = self.adjacent.values().copied().collect();
        i.voice = self.voice.values().copied().collect();
    }

    /// Fill the findings from the SmartNet side.
    fn smartnet_info(&mut self, t: f64) {
        let s = &self.sn;
        let i = &mut self.info;
        let voted = s.sys_votes.iter().max_by_key(|&(_, &n)| n).map(|(&v, _)| v as u32);
        i.identity = Identity::default();
        i.identity.set_opt(IdField::SysId, voted.or(s.parser.sys_id));
        i.identity.set_opt(IdField::Site, s.parser.site);
        i.good = s.rx.framer.good;
        i.bad = s.rx.framer.bad;
        i.modulation = "2FSK";
        i.snr_db = self.snr.map(|r| 10.0 * r.log10());
        i.elapsed_s = t;
        i.idens = 0;
        let info = &s.info;
        match &info.plan {
            Some(p) => {
                let cc = info.cc_chan;
                i.advertised_hz = cc.and_then(|c| p.plan.rx_hz(c));
                i.secondary = info.alt_chans.iter().filter_map(|&c| p.plan.rx_hz(c)).collect();
                i.voice = s
                    .grants
                    .iter()
                    .filter(|&(&c, _)| Some(c) != cc && !info.alt_chans.contains(&c))
                    .filter_map(|(&c, &n)| p.plan.rx_hz(c).map(|f| VoiceChannel { freq_hz: f, grants: n, tdma: false }))
                    .collect();
                i.bandplan = if p.name == "400_custom" {
                    format!("{} base {:.5} MHz, {} kHz steps from channel {}", p.name, p.base_hz / 1e6, p.spacing_hz / 1e3, p.offset)
                } else {
                    p.name.to_string()
                };
            }
            None => {
                i.advertised_hz = None;
                i.bandplan.clear();
            }
        }
        i.smartnet = Some(info.clone());
    }

    fn message(&mut self, m: &Message) {
        match m.kind {
            // This site's control channel (RFSS and network status name it).
            MessageType::SysId | MessageType::Status if m.freq_hz != 0 => self.info.advertised_hz = Some(m.freq_hz),
            MessageType::Adjacent if m.freq_hz != 0 => {
                let a = Adjacent { rfss: m.rfss, site: m.site, sys_id: m.sys_id, freq_hz: m.freq_hz };
                self.adjacent.insert((a.sys_id, a.rfss, a.site), a);
            }
            MessageType::ControlChannel if m.freq_hz != 0 => {
                self.secondary.insert(m.freq_hz);
            }
            MessageType::Grant | MessageType::Update | MessageType::UuVGrant | MessageType::UuVUpdate | MessageType::PhoneGrant | MessageType::PhoneUpdate if m.freq_hz != 0 => {
                let v = self.voice.entry(m.freq_hz).or_insert(VoiceChannel { freq_hz: m.freq_hz, grants: 0, tdma: false });
                v.grants += 1;
                v.tdma |= m.voice.tdma();
            }
            _ => {}
        }
        if let Some(own) = self.info.advertised_hz {
            self.secondary.remove(&own);
        }
    }

    /// Carrier offset → where the channel really is → ppm; re-centre the
    /// head on it once the estimate settles.
    fn frequency(&mut self, rate: f64) {
        let Some(mut res) = self.est.hz(rate) else { return };
        if self.is_smartnet() {
            // The two tones' midpoint: exact for FSK whatever the data.
            res = self.sn.rx.rx.offset_hz() as f64;
        }
        let avg_s = self.est.n as f64 / rate;
        let heard = self.head_hz + res;
        self.info.heard_hz = heard;
        if avg_s >= 1.5 && res.abs() > 250.0 && self.recenters < 3 {
            self.chz.remove_head(self.head);
            let (head, _, _) = self.chz.add_head(heard - self.center, CUTOFF_HZ, 0.0);
            self.head = head;
            self.head_hz = heard;
            // Fresh receivers on the new head: the old ones' carrier tracking,
            // levels and timing were for the old offset. (What was learned stays.)
            self.bank = Bank::new(rate, self.bank_cfg);
            self.sn.rx = SnRx::new(rate);
            self.est = FreqEst::default();
            self.est_from = self.blocks + 2;
            self.recenters += 1;
            return;
        }
        if let (Some(adv), true) = (self.info.advertised_hz, avg_s >= PPM_MIN_S) {
            let adv = adv as f64;
            self.info.offset_hz = Some(heard - adv);
            self.info.ppm = Some(self.ppm_applied + (adv / heard - 1.0) * 1e6);
        }
    }

    fn gain_step(&mut self, clip: (u64, u64)) {
        if self.gains.is_empty() {
            return;
        }
        let now = self.blocks;
        match self.gain {
            GainPhase::Waiting => {
                if self.counts().0 >= 20 && self.seconds(now) >= 4.0 {
                    self.info.gain.state = GainState::Running;
                    self.pending = Some(Command::Gain(self.gains[0]));
                    self.gain = GainPhase::Applying { i: 0, since: now };
                }
            }
            GainPhase::Applying { i, since } => {
                // A radio that never confirms: carry on after 2 s.
                if now - since > self.blocks_for(2.0) {
                    self.gain = GainPhase::Settling { i, until: now + self.blocks_for(0.3) };
                }
            }
            GainPhase::Settling { i, until } => {
                if now >= until {
                    let (good, bad) = self.counts();
                    self.gain = GainPhase::Measuring { i, until: now + self.blocks_for(1.5), snr: 0.0, n: 0, good, bad, clip };
                }
            }
            GainPhase::Measuring { i, until, snr, n, good, bad, clip: c0 } => {
                if now < until {
                    return;
                }
                let (g, b) = (self.counts().0.saturating_sub(good), self.counts().1.saturating_sub(bad));
                let total = clip.1.saturating_sub(c0.1);
                self.info.gain.steps.push(GainStep {
                    gain_db: self.gains[i],
                    snr_db: if n > 0 { 10.0 * (snr / n as f64).max(1e-3).log10() } else { -30.0 },
                    ok_ratio: g as f64 / (g + b).max(1) as f64,
                    clipped: if total > 0 { clip.0.saturating_sub(c0.0) as f64 / total as f64 } else { 0.0 },
                });
                if i + 1 < self.gains.len() {
                    self.pending = Some(Command::Gain(self.gains[i + 1]));
                    self.gain = GainPhase::Applying { i: i + 1, since: now };
                } else {
                    let best = best_gain(&self.info.gain.steps);
                    self.info.gain.best_db = best;
                    self.info.gain.state = GainState::Done;
                    if let Some(g) = best {
                        self.pending = Some(Command::Gain(g));
                    }
                    self.gain = GainPhase::Final;
                }
            }
            GainPhase::Final => {}
        }
    }
}

/// The lowest gain within 1 dB of the best SNR (and decoding as well as the
/// best), not clipping — less gain leaves more headroom for strong neighbours.
pub fn best_gain(steps: &[GainStep]) -> Option<f32> {
    let score = |s: &GainStep| s.snr_db - if s.clipped > 1e-3 { 10.0 } else { 0.0 };
    let top = steps.iter().map(score).fold(f64::NEG_INFINITY, f64::max);
    let top_ok = steps.iter().map(|s| s.ok_ratio).fold(0.0, f64::max);
    let mut ok: Vec<&GainStep> = steps.iter().filter(|s| score(s) >= top - 1.0 && s.ok_ratio >= top_ok - 0.02).collect();
    ok.sort_by(|a, b| a.gain_db.total_cmp(&b.gain_db));
    ok.first().map(|s| s.gain_db)
}

#[derive(Clone, Debug)]
struct Hop {
    center: f64,
    band: &'static str,
    /// The targets it covers.
    targets: Vec<f64>,
}

/// Hop centres covering the chosen bands (in [`BANDS`] order), each hop's
/// searched width overlapping the next by 25 kHz.
fn plan(bands: &[String], rate: f64, range: (f64, f64)) -> Vec<Hop> {
    let span = rate * USABLE;
    let step = span - 25_000.0;
    let round = |f: f64| (f / 1000.0).round() * 1000.0;
    let mut hops = Vec::new();
    // Overlapping bands (business UHF inside UHF, …) are scanned once: each
    // band's range less what the bands before it cover.
    let mut done: Vec<(f64, f64)> = Vec::new();
    let mut ranges: Vec<(f64, f64, &'static str)> = Vec::new();
    for b in BANDS.iter().filter(|b| bands.iter().any(|id| id == b.id)) {
        let mut parts = vec![(b.lo_hz.max(range.0), b.hi_hz.min(range.1))];
        for &(dl, dh) in &done {
            parts = parts.into_iter().flat_map(|(l, h)| [(l, h.min(dl)), (l.max(dh), h)]).filter(|&(l, h)| h > l).collect();
        }
        done.push((b.lo_hz, b.hi_hz));
        ranges.extend(parts.into_iter().map(|(l, h)| (l, h, b.id)));
    }
    for (lo, hi, id) in ranges {
        let b = Band { id, ..BANDS[0] };
        if hi <= lo {
            continue;
        }
        if hi - lo <= span - 25_000.0 {
            hops.push(Hop { center: round((lo + hi) / 2.0), band: b.id, targets: vec![] });
            continue;
        }
        let mut c = lo + span / 2.0 - 12_500.0;
        loop {
            hops.push(Hop { center: round(c), band: b.id, targets: vec![] });
            if c + span / 2.0 >= hi {
                break;
            }
            c += step;
        }
    }
    hops
}

/// Kept clear of the radio's DC offset, Hz.
const DC_CLEAR_HZ: f64 = 20_000.0;

/// Hops covering `targets` (each in one, none on the radio's DC, the ones
/// out of `range` left out), as few as fit.
fn target_plan(targets: &[f64], rate: f64, range: (f64, f64)) -> Vec<Hop> {
    let mut f: Vec<f64> = targets.iter().copied().filter(|&t| t >= range.0 && t <= range.1).collect();
    f.sort_by(f64::total_cmp);
    f.dedup();
    let half = rate * USABLE / 2.0 - METER_HALF_HZ - DC_CLEAR_HZ;
    let mut hops = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let mut j = i;
        while j + 1 < f.len() && f[j + 1] - f[i] <= 2.0 * half {
            j += 1;
        }
        let span = &f[i..=j];
        let mid = (span[0] + span[span.len() - 1]) / 2.0;
        // A centre clear of every frequency, still covering them all.
        let center = [0.0, DC_CLEAR_HZ, -DC_CLEAR_HZ, 2.0 * DC_CLEAR_HZ, -2.0 * DC_CLEAR_HZ]
            .into_iter()
            .map(|d| mid + d)
            .find(|c| span.iter().all(|x| (x - c).abs() >= DC_CLEAR_HZ && (x - c).abs() <= half + DC_CLEAR_HZ))
            .unwrap_or(mid);
        hops.push(Hop { center: (center / 1000.0).round() * 1000.0, band: band_of(center), targets: span.to_vec() });
        i = j + 1;
    }
    hops
}

enum Work {
    None,
    Hop(Box<HopScan>),
    Monitor(Box<Monitor>),
}

enum AfterTune {
    Hop(usize),
    Monitor(f64),
}

pub struct Survey {
    cfg: SurveyConfig,
    hops: Vec<Hop>,
    hop: usize,
    stage: Stage,
    pending: Option<Command>,
    /// Waiting for the radio to confirm a tune; samples until then are dropped.
    awaiting: Option<AfterTune>,
    center_hz: f64,
    skip: u64,
    work: Work,
    candidates: Vec<Candidate>,
    message: String,
    iq: Vec<Complex32>,
    /// Samples at the ADC rails, and all samples (u8 input).
    clip: (u64, u64),
    /// The ppm the monitor measured, kept for correcting the scan's findings.
    ppm: Option<f64>,
    /// The targets the scan has probed so far.
    probed: Vec<f64>,
}

impl Survey {
    pub fn new(cfg: SurveyConfig) -> Self {
        let mut s = Survey {
            hops: Vec::new(),
            hop: 0,
            stage: Stage::Scanning,
            pending: None,
            awaiting: None,
            center_hz: cfg.fixed_center_hz.unwrap_or(0.0),
            skip: 0,
            work: Work::None,
            candidates: Vec::new(),
            message: String::new(),
            iq: Vec::new(),
            clip: (0, 0),
            ppm: None,
            probed: Vec::new(),
            cfg,
        };
        s.rescan();
        s
    }

    /// Start (again) from the first hop, forgetting what was found.
    pub fn rescan(&mut self) {
        self.candidates.clear();
        self.probed.clear();
        self.message.clear();
        self.stage = Stage::Scanning;
        let looking = !self.cfg.targets.is_empty();
        let half = self.cfg.rate_hz / 2.0 * USABLE;
        self.hops = match (self.cfg.fixed_center_hz, looking) {
            (Some(c), true) => target_plan(&self.cfg.targets, self.cfg.rate_hz, (c - half, c + half)).into_iter().take(1).map(|h| Hop { center: c, band: band_of(c), targets: h.targets }).collect(),
            (Some(c), false) => vec![Hop { center: c, band: band_of(c), targets: vec![] }],
            (None, true) => target_plan(&self.cfg.targets, self.cfg.rate_hz, self.cfg.tune_range_hz),
            (None, false) => plan(&self.cfg.bands, self.cfg.rate_hz, self.cfg.tune_range_hz),
        };
        if self.hops.is_empty() {
            self.stage = Stage::Done;
            self.message = if looking { "None of the frequencies looked for is in this radio's tuning range (or the capture)." } else { "None of the chosen bands is in this radio's tuning range." }.into();
            self.work = Work::None;
            return;
        }
        self.go_hop(0);
    }

    fn go_hop(&mut self, i: usize) {
        self.hop = i;
        let h = &self.hops[i];
        if self.cfg.fixed_center_hz.is_some() {
            self.work = Work::Hop(Box::new(HopScan::new(self.center_hz, h.band, &self.cfg, h.targets.clone())));
        } else {
            self.work = Work::None;
            self.pending = Some(Command::Tune(h.center));
            self.awaiting = Some(AfterTune::Hop(i));
        }
    }

    /// Stop scanning and monitor the signal at `freq_hz` (as heard).
    pub fn listen(&mut self, freq_hz: f64) {
        self.stage = Stage::Monitoring;
        self.message.clear();
        if self.cfg.fixed_center_hz.is_some() {
            if (freq_hz - self.center_hz).abs() > self.cfg.rate_hz / 2.0 * USABLE {
                self.stage = Stage::Done;
                self.message = format!("{:.5} MHz is outside the capture.", freq_hz / 1e6);
                self.work = Work::None;
                return;
            }
            self.work = Work::Monitor(Box::new(Monitor::new(self.center_hz, freq_hz, &self.cfg)));
            return;
        }
        // Off the radio's DC offset, with room either side.
        let off = (self.cfg.rate_hz * 0.2).min(500_000.0);
        let center = if freq_hz - off >= self.cfg.tune_range_hz.0 { freq_hz - off } else { freq_hz + off };
        self.work = Work::None;
        self.pending = Some(Command::Tune((center / 1000.0).round() * 1000.0));
        self.awaiting = Some(AfterTune::Monitor(freq_hz));
    }

    /// What the radio should do next (taken once).
    pub fn command(&mut self) -> Option<Command> {
        self.pending.take()
    }

    /// The radio applied the last command; it is now centred on `center_hz`.
    pub fn tuned(&mut self, center_hz: f64) {
        let Some(after) = self.awaiting.take() else {
            if let Work::Monitor(m) = &mut self.work {
                m.gain_applied();
            }
            return;
        };
        self.center_hz = center_hz;
        self.skip = (self.cfg.settle_s * self.cfg.rate_hz) as u64;
        self.work = match after {
            AfterTune::Hop(i) => Work::Hop(Box::new(HopScan::new(center_hz, self.hops[i].band, &self.cfg, self.hops[i].targets.clone()))),
            AfterTune::Monitor(f) => Work::Monitor(Box::new(Monitor::new(center_hz, f, &self.cfg))),
        };
    }

    /// RTL-SDR native unsigned 8-bit interleaved IQ.
    pub fn push_u8(&mut self, bytes: &[u8]) {
        let mut iq = std::mem::take(&mut self.iq);
        iq.clear();
        let mut clipped = 0u64;
        for p in bytes.chunks_exact(2) {
            if p[0] == 0 || p[0] == 255 || p[1] == 0 || p[1] == 255 {
                clipped += 1;
            }
            iq.push(Complex32::new((p[0] as f32 - 127.5) / 127.5, (p[1] as f32 - 127.5) / 127.5));
        }
        self.clip.0 += clipped;
        self.clip.1 += iq.len() as u64;
        self.push_iq(&iq);
        self.iq = iq;
    }

    pub fn push_iq(&mut self, mut iq: &[Complex32]) {
        while !iq.is_empty() {
            if self.awaiting.is_some() {
                return;
            }
            if self.skip > 0 {
                let k = (self.skip as usize).min(iq.len());
                self.skip -= k as u64;
                iq = &iq[k..];
                continue;
            }
            let ran = match &mut self.work {
                Work::Hop(h) => {
                    if let Some(found) = h.push(iq) {
                        let targets = std::mem::take(&mut h.targets);
                        self.probed.extend(targets);
                        self.merge(found);
                        self.next_hop();
                    }
                    return;
                }
                Work::Monitor(m) => {
                    let (used, ran) = m.chz.feed(iq);
                    iq = &iq[used..];
                    ran
                }
                Work::None => return,
            };
            if ran {
                self.on_block(false);
            }
        }
    }

    /// The input ended (a capture file): finish what is in hand.
    pub fn finish(&mut self) {
        match &mut self.work {
            Work::Monitor(m) => {
                m.on_block(self.clip, true);
                self.stage = Stage::Done;
            }
            Work::Hop(_) => {
                self.stage = Stage::Done;
                if self.message.is_empty() {
                    self.message = "The capture ended before the scan finished.".into();
                }
            }
            Work::None => {}
        }
    }

    fn on_block(&mut self, flush: bool) {
        match &mut self.work {
            Work::Monitor(m) => {
                m.on_block(self.clip, flush);
                if let Some(c) = m.pending.take() {
                    self.pending = Some(c);
                }
                if m.info.ppm.is_some() {
                    self.ppm = m.info.ppm;
                }
            }
            Work::Hop(_) | Work::None => {}
        }
    }

    fn merge(&mut self, found: Vec<Candidate>) {
        for c in found {
            // (Two targets close together are kept apart.)
            match self.candidates.iter_mut().find(|x| (x.freq_hz - c.freq_hz).abs() < SAME_SIGNAL_HZ && (x.target.is_none() || c.target.is_none() || x.target == c.target)) {
                Some(x) => {
                    let (target, up) = (x.target.or(c.target), match (x.up, c.up) { (Some(a), Some(b)) => Some(a.max(b)), (a, b) => a.or(b) });
                    if (c.kind, c.score()) > (x.kind, x.score()) {
                        *x = c;
                    }
                    (x.target, x.up) = (target, up);
                }
                None => self.candidates.push(c),
            }
        }
        self.candidates.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
    }

    fn next_hop(&mut self) {
        if self.hop + 1 < self.hops.len() && self.cfg.fixed_center_hz.is_none() {
            self.go_hop(self.hop + 1);
            return;
        }
        match self.best() {
            Some(f) => self.listen(f),
            None => {
                self.stage = Stage::Done;
                self.work = Work::None;
                let dmr = self.candidates.iter().filter(|c| c.kind == Kind::DmrControl).count();
                let nxdn = self.candidates.iter().filter(|c| c.kind == Kind::NxdnControl).count();
                let found: Vec<String> = [(dmr, "trunked DMR control / rest"), (nxdn, "NXDN control")].iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what} channel(s)")).collect();
                let looked = self.cfg.targets.len();
                self.message = if looked > 0 && !found.is_empty() {
                    format!("Found {} on the {looked} frequencies looked for: no P25 or SmartNet control channel to listen to.", found.join(" and "))
                } else if looked > 0 {
                    format!("No P25 or SmartNet control channel on the {looked} frequencies looked for.")
                } else if !found.is_empty() {
                    format!("Found {} — add one below. No P25 or SmartNet control channel.", found.join(" and "))
                } else if self.candidates.iter().any(|c| c.kind != Kind::Other) {
                    "P25, DMR or NXDN signals were found, but no control channel. Scan again, or add more bands.".into()
                } else {
                    "No P25 or SmartNet control channel found. Check the antenna and gain, and add more bands.".into()
                };
            }
        }
    }

    /// The control channel worth monitoring: one looked for first, then the
    /// best SNR among those decoding.
    pub fn best(&self) -> Option<f64> {
        let rank = |c: &Candidate| (c.target.is_some(), c.score());
        self.candidates.iter().filter(|c| matches!(c.kind, Kind::Control | Kind::SmartNet)).max_by(|a, b| rank(a).0.cmp(&rank(b).0).then(rank(a).1.total_cmp(&rank(b).1))).map(|c| c.freq_hz)
    }

    /// The targets probed so far ([`SurveyConfig::targets`]; one out of the radio's range never is).
    pub fn probed(&self) -> &[f64] {
        &self.probed
    }
    /// The targets looked for.
    pub fn targets(&self) -> &[f64] {
        &self.cfg.targets
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }
    /// (hop, hops, band id, centre) while scanning: hop counts from 1.
    pub fn progress(&self) -> Option<(usize, usize, &'static str, f64)> {
        (self.stage == Stage::Scanning && !self.hops.is_empty()).then(|| {
            let h = &self.hops[self.hop];
            (self.hop + 1, self.hops.len(), h.band, h.center)
        })
    }
    /// The monitored channel's findings.
    pub fn system(&self) -> Option<&SystemInfo> {
        match &self.work {
            Work::Monitor(m) => Some(&m.info),
            _ => None,
        }
    }
    /// The frequency being monitored (as heard).
    pub fn monitoring_hz(&self) -> Option<f64> {
        match &self.work {
            Work::Monitor(m) => Some(m.head_hz),
            _ => None,
        }
    }
    /// A frequency as heard, corrected by the measured ppm (once known).
    pub fn corrected(&self, heard_hz: f64) -> Option<f64> {
        self.ppm.map(|p| heard_hz * (1.0 + (p - self.cfg.ppm) * 1e-6))
    }
    /// The current centre and power spectrum (dBFS, `bins`), if tuned.
    pub fn spectrum(&self, bins: usize) -> Option<(f64, Vec<f32>)> {
        let bins = match &self.work {
            Work::Hop(h) => h.spectrum(bins),
            Work::Monitor(m) => m.chz.power_spectrum(bins),
            Work::None => return None,
        };
        Some((self.center_hz, bins))
    }
    pub fn rate_hz(&self) -> f64 {
        self.cfg.rate_hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> f64 {
        let mut s = seed;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
    }

    /// White noise plus: a continuous 8 kHz-wide noise-like signal at +300 kHz
    /// (a control channel), one at −500 kHz keyed only 30 % of the time (voice),
    /// and a CW spur at +100 kHz.
    fn air(fs: f64, secs: f64) -> Vec<Complex32> {
        let n = (fs * secs) as usize;
        let mut u = rng(7);
        let mut gauss = move || {
            let (a, b) = (u(), u());
            Complex32::from_polar((-a.ln()).sqrt() as f32, (2.0 * PI * b) as f32)
        };
        // Band-limited noise: four one-pole low-passes at 3 kHz on white noise.
        let lp = |x: &mut Complex32, st: &mut [Complex32; 4], a: f32| {
            let mut v = *x;
            for s in st.iter_mut() {
                *s += (v - *s) * a;
                v = *s;
            }
            *x = v;
        };
        let a = (2.0 * PI * 3000.0 / fs) as f32;
        let (mut s1, mut s2) = ([Complex32::default(); 4], [Complex32::default(); 4]);
        (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let mut cc = gauss() * 60.0;
                lp(&mut cc, &mut s1, a);
                let mut vo = gauss() * 60.0;
                lp(&mut vo, &mut s2, a);
                let rot = |hz: f64| Complex32::from_polar(1.0, (2.0 * PI * hz * t) as f32);
                let keyed = (t % 0.2) < 0.06;
                gauss() * 0.05 + cc * 0.05 * rot(300_000.0) + if keyed { vo * 0.05 * rot(-500_000.0) } else { Complex32::default() } + rot(100_000.0) * 0.2
            })
            .collect()
    }

    #[test]
    fn an_nxdn_control_channel_is_recognised() {
        use crate::nxdn::channel::Sr;
        use crate::nxdn::layer3::build as l3;
        use crate::nxdn::synth::{cac_frame, dibits, modulate};
        use crate::nxdn::Rate;
        let fs = 37_500.0;
        let site = l3::site_info(0x2a << 12 | 0x07, 0x0200, 0, 0, [1, 0]);
        let idle = l3::idle();
        let frames: Vec<_> = (0..12).map(|k| cac_frame(Sr { structure: 2, ran: 7 }, if k % 3 == 0 { &site } else { &idle })).collect();
        let mut d: Vec<u8> = (0..400u32).map(|i| (i.wrapping_mul(2654435761) >> 30) as u8).collect();
        d.extend(dibits(&frames));
        d.extend((0..800u32).map(|i| (i.wrapping_mul(2246822519) >> 30) as u8));
        let mut ph = 0.0;
        let iq = modulate(&d, None, Rate::N48, fs, 0.0, 1.0, &mut ph);
        let mut probes = [NxdnProbe::new(Rate::N48, fs), NxdnProbe::new(Rate::N96, fs)];
        for p in probes.iter_mut() {
            p.push(&iq);
        }
        let best = probes.iter().max_by_key(|n| (n.cac, n.lich)).unwrap();
        assert!(best.cac >= 10, "{} CACs", best.cac);
        assert_eq!(best.found(), NxdnFound { rate: Rate::N48, ran: Some(7), location: Some((0x2a, 0x07)) });
        assert_eq!(probes[1].cac, 0);
    }

    #[test]
    fn peaks_find_the_continuous_carrier_only() {
        let fs = 2_400_000.0;
        let x = air(fs, SPECTRUM_S + 0.05);
        let mut chz = Channelizer::new(fs, MIN_CHANNEL_RATE, 0.01);
        let mut cells = chz.fft_size();
        while rate_ok(fs, cells) {
            cells /= 2;
        }
        fn rate_ok(fs: f64, cells: usize) -> bool {
            cells > 16 && fs / (cells as f64) < 900.0
        }
        let mut row = vec![0.0; cells];
        let mut rows = Vec::new();
        let mut i = 0;
        while i < x.len() {
            let (used, ran) = chz.feed(&x[i..]);
            i += used;
            if ran {
                chz.cell_powers(&mut row);
                rows.extend_from_slice(&row);
            }
        }
        let peaks = find_peaks(&rows, cells, fs, chz.fft_size(), fs / 2.0 * USABLE);
        assert!(!peaks.is_empty(), "nothing found");
        let p = peaks[0];
        assert!((p.offset_hz - 300_000.0).abs() < 800.0, "centre {:.0}", p.offset_hz);
        assert!(p.snr_db > 10.0, "snr {:.1}", p.snr_db);
        assert!((4000.0..20000.0).contains(&p.width_hz), "width {:.0}", p.width_hz);
        // The intermittent one is below the continuity threshold; the spur too narrow.
        assert!(peaks.iter().all(|p| (p.offset_hz + 500_000.0).abs() > 20_000.0), "voice counted: {peaks:?}");
        assert!(peaks.iter().all(|p| (p.offset_hz - 100_000.0).abs() > 20_000.0), "spur counted: {peaks:?}");
    }

    /// A capture of a DMR repeater 150 kHz above the centre, with a little noise.
    fn dmr_air(fs: f64) -> Vec<Complex32> {
        use crate::dmr::synth::{dibits, modulate, Tx};
        let tx = Tx { cc: 3, slot: 1, talkgroup: 101, source: 2001, superframes: 8, idle_before: 4, idle_after: 4, csbks: vec![], mobile: false };
        let mut ph = 0.0;
        let mut iq = modulate(&dibits(&tx), fs, 150_000.0, 0.3, &mut ph);
        let mut u = rng(0x1234_5678);
        for x in iq.iter_mut() {
            let n = (u() as f32 - 0.5) * 0.02;
            *x += Complex32::new(n, -n);
        }
        iq
    }

    /// Looking for frequencies: each is probed, the repeater's heard and the
    /// quiet ones beside it not, whether on one core or shared out over three.
    #[test]
    fn targets_are_probed_on_one_core_or_several() {
        let fs = 1_200_000.0;
        let iq = dmr_air(fs);
        let secs = iq.len() as f64 / fs;
        let targets = vec![850_800_000.0, 851_150_000.0, 851_300_000.0];
        for threads in [1, 3] {
            let cfg = SurveyConfig {
                rate_hz: fs,
                fixed_center_hz: Some(851_000_000.0),
                targets: targets.clone(),
                probe_s: secs - SPECTRUM_S - 0.15,
                receivers: Receivers::of_type("dmr"),
                threads,
                ..Default::default()
            };
            let mut s = Survey::new(cfg);
            for chunk in iq.chunks(10_000) {
                s.push_iq(chunk);
            }
            assert_eq!(s.stage(), Stage::Done, "{threads} threads: {}", s.message());
            assert_eq!(s.probed(), &targets[..]);
            let on = |t: f64| s.candidates().iter().find(|c| c.target == Some(t)).unwrap_or_else(|| panic!("{t} not probed"));
            assert!(matches!(on(851_150_000.0).kind, Kind::Dmr | Kind::DmrControl), "{threads} threads: {:?}", on(851_150_000.0).kind);
            assert!(on(851_150_000.0).up.unwrap() > 0.5);
            // (The synthetic repeater is unfiltered: its splatter reaches 851.3.)
            assert_eq!(on(851_300_000.0).kind, Kind::Other);
            assert_eq!(on(850_800_000.0).kind, Kind::Other);
            assert!(on(850_800_000.0).up.unwrap() < 0.02, "up {:?}", on(850_800_000.0).up);
        }
    }

    #[test]
    fn an_edacs_control_channel_is_recognised() {
        use crate::edacs::{framer, parser};
        let fs = 1_200_000.0;
        let mut bits = Vec::new();
        for _ in 0..60 {
            let (m1, m2) = parser::site_id(5, 1);
            bits.extend(framer::encode(m1, m2));
        }
        let mut iq = crate::edacs::tests::modulate(&bits, fs, 2600.0, -200_000.0);
        crate::edacs::tests::noise(&mut iq, 0.1, 9);
        let secs = iq.len() as f64 / fs;
        let cfg = SurveyConfig { rate_hz: fs, fixed_center_hz: Some(851_000_000.0), targets: vec![850_800_000.0], probe_s: secs - SPECTRUM_S - 0.1, receivers: Receivers::of_type("edacs"), ..Default::default() };
        let mut s = Survey::new(cfg);
        s.push_iq(&iq);
        let c = s.candidates().iter().find(|c| c.target.is_some()).expect("probed");
        assert_eq!(c.kind, Kind::Edacs);
        assert_eq!(c.identity.site(), Some(5));
    }

    #[test]
    fn hops_cover_the_targets_clear_of_dc() {
        let rate = 2_400_000.0;
        let f = [769_406_250.0, 769_931_250.0, 770_156_250.0, 773_293_750.0, 851_000_000.0, 2_000_000_000.0];
        let hops = target_plan(&f, rate, (24e6, 1766e6));
        let half = rate * USABLE / 2.0 - METER_HALF_HZ;
        for &x in &f[..5] {
            let h: Vec<&Hop> = hops.iter().filter(|h| h.targets.contains(&x)).collect();
            assert_eq!(h.len(), 1, "{x} in {} hops", h.len());
            let d = (x - h[0].center).abs();
            assert!(d <= half && d >= DC_CLEAR_HZ - 500.0, "{x} at {d:.0} from the centre");
        }
        // (Out of the radio's range: not probed.)
        assert!(!hops.iter().any(|h| h.targets.contains(&2e9)));
        assert_eq!(hops.len(), 3);
    }

    #[test]
    fn hops_cover_the_bands() {
        let rate = 2_400_000.0;
        let hops = plan(&["800".into(), "700".into()], rate, (24e6, 1766e6));
        let half = rate * USABLE / 2.0;
        for b in &BANDS[..2] {
            let mut f = b.lo_hz;
            while f <= b.hi_hz {
                assert!(hops.iter().any(|h| (f - h.center).abs() <= half), "{f} uncovered");
                f += 5000.0;
            }
        }
        assert_eq!(hops.iter().filter(|h| h.band == "700").count(), 3);
        assert!(hops.iter().filter(|h| h.band == "800").count() <= 10);
        // A radio that can't reach a band skips it.
        assert!(plan(&["800".into()], rate, (24e6, 500e6)).is_empty());
    }

    #[test]
    fn overlapping_bands_are_scanned_once() {
        let rate = 2_400_000.0;
        let uhf = plan(&["uhf".into()], rate, (24e6, 1766e6));
        let both = plan(&["uhf".into(), "biz-uhf".into()], rate, (24e6, 1766e6));
        assert_eq!(both.len(), uhf.len());
        // Business alone covers its own range.
        let biz = plan(&["biz-uhf".into()], rate, (24e6, 1766e6));
        assert!(!biz.is_empty() && biz.iter().all(|h| h.band == "biz-uhf"));
        assert!(biz.len() <= uhf.len());
    }

    #[test]
    fn gain_choice_prefers_headroom() {
        let st = |g: f32, snr: f64, ok: f64, clip: f64| GainStep { gain_db: g, snr_db: snr, ok_ratio: ok, clipped: clip };
        let steps = [st(20.0, 14.0, 0.97, 0.0), st(30.0, 21.0, 1.0, 0.0), st(38.0, 21.6, 1.0, 0.0), st(44.0, 22.0, 1.0, 0.01)];
        // 44 clips; 38 is best; 30 is within 1 dB of it and decodes as well.
        assert_eq!(best_gain(&steps), Some(30.0));
    }
}
