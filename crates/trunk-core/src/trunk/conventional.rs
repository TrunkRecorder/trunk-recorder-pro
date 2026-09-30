//! Conventional channels (analog NBFM, P25): energy-detected, so an idle
//! channel costs almost nothing.
//!
//! ```text
//! every block, per channel, from the channelizer's own spectrum (no head):
//!   band power (±5 kHz) vs the local noise floor → SNR (dB)
//! SNR ≥ squelch → open a head with pre-roll (the air from before detection)
//!   → channel filter / carrier meter (±5.5 kHz: confirms the carrier, so FFT
//!     leakage from a strong neighbour doesn't make a call)
//!   → NBFM demod → 8 kHz audio          (fm)
//!   → receiver bank → voice tracker     (p25; talkgroup from link control)
//! a call starts with the first audio (or P25 link control), ends when there
//! has been none for the call timeout; the head closes once the carrier has
//! been gone for CLOSE_HANG_S with no call.
//! ```
//!
//! A detection the carrier meter doesn't confirm raises that channel's open
//! threshold to just above what triggered it, until the band quietens again,
//! so a neighbour's leakage doesn't reopen the head every block.
//!
//! The noise floor is per source: the median of each 1/64 of the band,
//! smoothed over time, which follows the SDR's passband shape and ignores
//! signals filling under half a slice.

use num_complex::Complex32;

use super::calls::{Call, CallId, CallManager, CallSource};
use super::frames::CallFrames;
use super::talkgroups::Talkgroup;
use super::tracker::{TrackerOut, VoiceTracker};
use crate::dsp::fm::{self, ChannelFilter, Nbfm};
use crate::dsp::{Channelizer, HeadId};
use crate::mbe;
use crate::p25::diversity::{best_frame, Bank, BankConfig, Group};

/// Half-width of the band the detector sums, Hz (a 12.5 kHz channel's signal).
const DETECT_HALF_BW: f64 = 5000.0;
/// Head filter cutoff (the carrier meter narrows it further), Hz.
const HEAD_CUTOFF_HZ: f64 = 7000.0;
/// Band power smoothing, s.
const DETECT_TAU_S: f64 = 0.02;
/// Noise floor slices per source.
const FLOOR_SLICES: usize = 64;
/// Noise floor refresh, s, and its smoothing per refresh.
const FLOOR_EVERY_S: f64 = 0.1;
const FLOOR_ALPHA: f64 = 0.3;
/// Carrier-meter threshold below the open threshold (hysteresis), dB.
const CLOSE_BELOW_DB: f64 = 3.0;
/// After a detection the carrier meter didn't confirm, reopen only this far
/// above the level that caused it, dB.
const FALSE_OPEN_RAISE_DB: f64 = 3.0;
/// A head with no call closes once the carrier has been gone this long, s.
const CLOSE_HANG_S: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvMode {
    /// Analog narrowband FM (12.5 kHz).
    Fm,
    /// P25 Phase 1 (C4FM or CQPSK).
    P25,
}

impl ConvMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConvMode::Fm => "fm",
            ConvMode::P25 => "p25",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConvChannel {
    pub freq_hz: f64,
    pub mode: ConvMode,
    /// The talkgroup number calls are filed under (P25: when the air names none).
    pub talkgroup: u32,
    /// Name, description, tag, group for the call record.
    pub info: Option<Talkgroup>,
    /// Open threshold, dB above the noise floor (None: [`ConvConfig::squelch_db`]).
    pub squelch_db: Option<f64>,
}

impl ConvChannel {
    /// A channel filed under its default talkgroup: the frequency in kHz
    /// (154.430 MHz → 154430), stable however the list is ordered.
    pub fn new(freq_hz: f64, mode: ConvMode) -> Self {
        ConvChannel { freq_hz, mode, talkgroup: Self::default_talkgroup(freq_hz), info: None, squelch_db: None }
    }

    pub fn default_talkgroup(freq_hz: f64) -> u32 {
        (freq_hz / 1000.0).round() as u32
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConvConfig {
    /// Default open threshold, dB above the noise floor.
    pub squelch_db: f64,
    /// Air replayed from before detection, s (capped by the engine's history).
    pub preroll_s: f64,
    /// A call longer than this is concluded and a new one started (0: never), s.
    pub max_call_s: f64,
}

impl Default for ConvConfig {
    fn default() -> Self {
        ConvConfig { squelch_db: 8.0, preroll_s: 0.3, max_call_s: 600.0 }
    }
}

/// What the conventional channels did, for the engine to report.
pub enum ConvOut {
    Start(Call),
    Update(Call),
    Audio { call_id: CallId, talkgroup: u32, samples: Vec<f32> },
    End { call: Call, audio: Vec<f32>, frames: CallFrames, recorder_num: u32 },
}

enum Rx {
    Fm(Nbfm),
    /// `t0`: sample-clock time of the head's first output; `rate`: its sample rate.
    P25 { meter: ChannelFilter, bank: Bank, tracker: VoiceTracker, groups: Vec<Group>, t0: f64, rate: f64 },
}

struct Live {
    call: Call,
    audio: Vec<f32>,
    /// P25: the vocoder frames behind `audio`.
    frames: CallFrames,
    /// The talkgroup came from P25 link control (not the channel's default).
    tg_from_air: bool,
}

struct Open {
    head: HeadId,
    rx: Rx,
    opened_s: f64,
    carrier_seen: bool,
    last_carrier_s: f64,
    live: Option<Live>,
}

struct Chan {
    cfg: ConvChannel,
    source: usize,
    offset_hz: f64,
    slice: usize,
    power: f64,
    bar_db: f64,
    raised: bool,
    open: Option<Open>,
}

impl Chan {
    fn base_db(&self, dflt: f64) -> f64 {
        self.cfg.squelch_db.unwrap_or(dflt)
    }
}

struct Floor {
    slices: Vec<f64>,
    scratch: Vec<f64>,
    next_s: f64,
    primed: bool,
}

pub struct Conventional {
    cfg: ConvConfig,
    bank_cfg: BankConfig,
    chans: Vec<Chan>,
    floors: Vec<Floor>,
}

/// What the calls need from the engine's configuration.
pub struct CallRules {
    pub call_timeout_s: f64,
    pub record_encrypted: bool,
    /// Keep each call's vocoder frames (see [`super::frames`]).
    pub capture_frames: bool,
}

impl Conventional {
    /// `sources`: (centre, rate) of each source. Channels outside every
    /// source are an error.
    pub fn new(channels: &[ConvChannel], sources: &[(f64, f64)], cfg: ConvConfig, bank_cfg: BankConfig, usable: f64) -> Result<Self, String> {
        let mut chans = Vec::new();
        let mut outside = Vec::new();
        for c in channels {
            let Some(src) = sources.iter().position(|&(center, rate)| (c.freq_hz - center).abs() <= rate / 2.0 * usable) else {
                outside.push(format!("{:.5}", c.freq_hz / 1e6));
                continue;
            };
            let (center, rate) = sources[src];
            let offset_hz = c.freq_hz - center;
            let slice = (((offset_hz + rate / 2.0) / rate * FLOOR_SLICES as f64) as usize).min(FLOOR_SLICES - 1);
            let bar = c.squelch_db.unwrap_or(cfg.squelch_db);
            chans.push(Chan { cfg: c.clone(), source: src, offset_hz, slice, power: 0.0, bar_db: bar, raised: false, open: None });
        }
        if !outside.is_empty() {
            return Err(format!("Conventional channel(s) outside every source's bandwidth: {} MHz — move a center frequency or disable them.", outside.join(", ")));
        }
        let floors = sources.iter().map(|_| Floor { slices: vec![0.0; FLOOR_SLICES], scratch: vec![0.0; FLOOR_SLICES], next_s: 0.0, primed: false }).collect();
        Ok(Conventional { cfg, bank_cfg, chans, floors })
    }

    pub fn is_empty(&self) -> bool {
        self.chans.is_empty()
    }

    /// Heads open now.
    pub fn open_count(&self) -> usize {
        self.chans.iter().filter(|c| c.open.is_some()).count()
    }

    /// Calls in progress.
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.chans.iter().filter_map(|c| c.open.as_ref().and_then(|o| o.live.as_ref()).map(|l| &l.call))
    }

    /// After a block ran on `source` (whose clock reads `now_s`).
    pub fn on_block(&mut self, source: usize, chz: &mut Channelizer, now_s: f64, calls: &mut CallManager, rules: &CallRules, out: &mut Vec<ConvOut>) {
        if !self.chans.iter().any(|c| c.source == source) {
            return;
        }
        let fl = &mut self.floors[source];
        if now_s >= fl.next_s {
            chz.noise_profile(&mut fl.scratch);
            if fl.primed {
                for (s, v) in fl.slices.iter_mut().zip(&fl.scratch) {
                    *s += FLOOR_ALPHA * (v - *s);
                }
            } else {
                fl.slices.copy_from_slice(&fl.scratch);
                fl.primed = true;
            }
            fl.next_s = now_s + FLOOR_EVERY_S;
        }
        let a = 1.0 - (-chz.block_seconds() / DETECT_TAU_S).exp();
        let dflt = self.cfg.squelch_db;
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            if ch.source != source {
                continue;
            }
            let floor = self.floors[source].slices[ch.slice].max(1e-30);
            let bp = chz.band_power(ch.offset_hz, DETECT_HALF_BW);
            ch.power = if ch.power == 0.0 { bp } else { ch.power + a * (bp - ch.power) };
            let snr_db = 10.0 * (ch.power / floor).log10();
            let base = ch.base_db(dflt);
            let meter_thr = (chz.noise_in_band(floor, ChannelFilter::noise_bandwidth()) * 10f64.powf((base - CLOSE_BELOW_DB).max(3.0) / 10.0)) as f32;
            if ch.open.is_none() {
                if ch.raised && snr_db < base {
                    ch.raised = false;
                    ch.bar_db = base;
                }
                if snr_db >= ch.bar_db {
                    Self::open(ch, chz, now_s, self.cfg.preroll_s, self.bank_cfg, meter_thr, idx as u32, calls, rules, out);
                }
                continue;
            }
            let Some(iq) = chz.output(ch.open.as_ref().unwrap().head).map(|v| v.to_vec()) else { continue };
            Self::run(ch, &iq, now_s, meter_thr, idx as u32, calls, rules, self.cfg.max_call_s, out);
            // Wind down.
            let o = ch.open.as_mut().unwrap();
            if let Some(l) = &o.live {
                if now_s - l.call.last_audio_s > rules.call_timeout_s {
                    let l = o.live.take().unwrap();
                    Self::end(l, idx as u32, out);
                }
            }
            let o = ch.open.as_ref().unwrap();
            if o.live.is_none() && now_s - o.last_carrier_s.max(o.opened_s) > CLOSE_HANG_S {
                if !o.carrier_seen {
                    // The meter never confirmed it: leakage or a spur. Wait
                    // for the band to rise further (or fall back) first.
                    ch.bar_db = snr_db + FALSE_OPEN_RAISE_DB;
                    ch.raised = true;
                }
                let o = ch.open.take().unwrap();
                chz.remove_head(o.head);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open(ch: &mut Chan, chz: &mut Channelizer, now_s: f64, preroll_s: f64, bank_cfg: BankConfig, meter_thr: f32, num: u32, calls: &mut CallManager, rules: &CallRules, out: &mut Vec<ConvOut>) {
        let (head, pre, start_sample) = chz.add_head(ch.offset_hz, HEAD_CUTOFF_HZ, preroll_s);
        let rate = chz.output_rate();
        let rx = match ch.cfg.mode {
            ConvMode::Fm => Rx::Fm(Nbfm::new(rate)),
            ConvMode::P25 => Rx::P25 {
                meter: ChannelFilter::new(rate),
                bank: Bank::new(rate, bank_cfg),
                tracker: VoiceTracker::new(mbe::lcg(ch.cfg.freq_hz as u32)),
                groups: Vec::new(),
                t0: start_sample as f64 / chz.fs(),
                rate,
            },
        };
        ch.open = Some(Open { head, rx, opened_s: now_s, carrier_seen: false, last_carrier_s: now_s, live: None });
        Self::run(ch, &pre, now_s, meter_thr, num, calls, rules, 0.0, out);
    }

    /// Run the open channel's receiver over `iq` (air up to `now_s`).
    #[allow(clippy::too_many_arguments)]
    fn run(ch: &mut Chan, iq: &[Complex32], now_s: f64, meter_thr: f32, num: u32, calls: &mut CallManager, rules: &CallRules, max_call_s: f64, out: &mut Vec<ConvOut>) {
        let o = ch.open.as_mut().unwrap();
        let mut audio = Vec::new();
        let mut vframes = Vec::new();
        let mut infos = Vec::new();
        // P25: air time of the first frame that produced output, and the end of the last.
        let mut air: Option<(f64, f64)> = None;
        let carrier = match &mut o.rx {
            Rx::Fm(fm) => fm.push(iq, meter_thr, &mut audio),
            Rx::P25 { meter, bank, tracker, groups, t0, rate } => {
                let up = meter.meter(iq) > meter_thr;
                groups.clear();
                bank.push(iq, groups);
                let mut tout = Vec::new();
                for g in groups.iter() {
                    let t = *t0 + best_frame(g).sample / *rate;
                    let before = tout.len();
                    tracker.group(g, t, &mut tout);
                    if tout.len() > before {
                        // An LDU is 180 ms of voice.
                        air = Some((air.map_or(t, |a| a.0), t + 0.18));
                    }
                }
                for t in tout {
                    match t {
                        TrackerOut::Audio(a, f) => {
                            audio.extend_from_slice(&a);
                            vframes.push(f);
                        }
                        TrackerOut::Info { source, emergency, encrypted } => infos.push((source, emergency, encrypted)),
                        TrackerOut::AnalogAudio(a) => audio.extend_from_slice(&a),
                    }
                }
                up
            }
        };
        if carrier {
            o.carrier_seen = true;
            o.last_carrier_s = now_s;
        }
        if audio.is_empty() && infos.is_empty() {
            return;
        }
        // The talkgroup: P25 link control's, else the channel's.
        let air_tg = match &o.rx {
            Rx::P25 { tracker, .. } => tracker.talkgroup(),
            Rx::Fm(_) => None,
        };
        let tg = air_tg.unwrap_or(ch.cfg.talkgroup);
        if let Some(l) = &o.live {
            let too_long = max_call_s > 0.0 && now_s - l.call.start_s > max_call_s;
            // A different talkgroup on the air is a new call; the first one
            // named just labels the call it arrived in.
            let new_tg = l.tg_from_air && air_tg.is_some_and(|t| t != l.call.talkgroup);
            if too_long || new_tg {
                let l = o.live.take().unwrap();
                Self::end(l, num, out);
            } else if air_tg.is_some() && !l.tg_from_air {
                let info = Self::info_for(&ch.cfg, tg, calls);
                let l = o.live.as_mut().unwrap();
                l.tg_from_air = true;
                if l.call.talkgroup != tg {
                    l.call.talkgroup = tg;
                    l.call.talkgroup_info = info;
                    out.push(ConvOut::Update(l.call.clone()));
                }
            }
        }
        if o.live.is_none() {
            let dur = audio.len() as f64 / fm::AUDIO_RATE;
            let start = air.map_or_else(|| (now_s - dur).max(o.opened_s - 0.05), |a| a.0);
            let call = Call {
                id: calls.allocate_id(),
                talkgroup: tg,
                freq_hz: ch.cfg.freq_hz.round() as u64,
                phase2_tdma: false,
                tdma_slot: 0,
                unit_to_unit: false,
                recording: true,
                reason: None,
                encrypted: false,
                emergency: false,
                priority: 0,
                duplex: false,
                mode: false,
                analog: ch.cfg.mode == ConvMode::Fm,
                start_s: start,
                last_update_s: now_s,
                last_audio_s: now_s,
                sources: Vec::new(),
                talkgroup_info: Self::info_for(&ch.cfg, tg, calls),
            };
            out.push(ConvOut::Start(call.clone()));
            o.live = Some(Live { call, audio: Vec::new(), frames: CallFrames::new(rules.capture_frames), tg_from_air: air_tg.is_some() });
        }
        let l = o.live.as_mut().unwrap();
        l.call.last_update_s = now_s;
        l.call.last_audio_s = air.map_or(now_s, |a| a.1);
        let mut changed = false;
        for (src, emergency, encrypted) in infos {
            if encrypted && !l.call.encrypted {
                l.call.encrypted = true;
                changed = true;
            }
            if emergency && !l.call.emergency {
                l.call.emergency = true;
                changed = true;
            }
            if let Some(s) = src {
                if s != 0 && l.call.sources.last().is_none_or(|x| x.src != s) {
                    l.call.sources.push(CallSource { src: s, time_s: now_s, emergency });
                    changed = true;
                }
            }
        }
        if changed {
            out.push(ConvOut::Update(l.call.clone()));
        }
        if !audio.is_empty() && !(l.call.encrypted && !rules.record_encrypted) {
            l.audio.extend_from_slice(&audio);
            for f in vframes {
                l.frames.push(f);
            }
            out.push(ConvOut::Audio { call_id: l.call.id, talkgroup: l.call.talkgroup, samples: audio });
        }
    }

    /// The call's names: the talkgroup list's entry for an air talkgroup
    /// other than the channel's, else the channel's own.
    fn info_for(cfg: &ConvChannel, tg: u32, calls: &CallManager) -> Option<Talkgroup> {
        let listed = calls.talkgroups.get(&tg).cloned();
        if tg != cfg.talkgroup && listed.is_some() {
            return listed;
        }
        cfg.info.clone().map(|t| Talkgroup { number: tg, ..t }).or(listed)
    }

    fn end(l: Live, num: u32, out: &mut Vec<ConvOut>) {
        out.push(ConvOut::End { call: l.call, audio: l.audio, frames: l.frames, recorder_num: num });
    }

    /// End of input: flush the receivers and end every call.
    pub fn finish(&mut self, rules: &CallRules, out: &mut Vec<ConvOut>) {
        for (idx, ch) in self.chans.iter_mut().enumerate() {
            let Some(o) = ch.open.as_mut() else { continue };
            if let Rx::P25 { bank, tracker, groups, t0, rate, .. } = &mut o.rx {
                groups.clear();
                bank.flush(groups);
                let mut tout = Vec::new();
                for g in groups.iter() {
                    tracker.group(g, *t0 + best_frame(g).sample / *rate, &mut tout);
                }
                if let Some(l) = o.live.as_mut() {
                    for t in tout {
                        if let TrackerOut::Audio(a, f) = t {
                            if !(l.call.encrypted && !rules.record_encrypted) {
                                l.audio.extend_from_slice(&a);
                                l.frames.push(f);
                            }
                        }
                    }
                }
            }
            let o = ch.open.take().unwrap();
            if let Some(l) = o.live {
                Self::end(l, idx as u32, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::engine::{Engine, EngineConfig, Event, SourceConfig};
    use crate::trunk::CallConfig;
    use std::f64::consts::PI;

    struct Tx {
        offset_hz: f64,
        /// Power relative to the noise in a 10 kHz band, dB.
        snr_db: f64,
        on: Vec<(f64, f64)>,
        ph: f64,
    }

    /// `secs` of air at `fs`: FM transmissions (1 kHz tone, 2.5 kHz deviation) in Gaussian noise.
    fn run(fs: f64, secs: f64, txs: &mut [Tx], channels: Vec<ConvChannel>) -> (Vec<(Call, usize, String)>, usize) {
        let center = 155_000_000.0;
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs }],
            conventional: channels,
            calls: CallConfig { call_timeout_s: 1.0, ..Default::default() },
            ..Default::default()
        };
        let mut e = Engine::new(cfg, Default::default()).unwrap();
        let sigma2 = 0.01f64;
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut u = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            ((rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let total = (fs * secs) as usize;
        let chunk = 32768;
        let mut buf = vec![Complex32::default(); chunk];
        let (mut out, mut starts) = (Vec::new(), 0);
        let mut i0 = 0;
        while i0 < total {
            let n = chunk.min(total - i0);
            for (k, v) in buf[..n].iter_mut().enumerate() {
                let (a, b) = (u(), u());
                let r = ((-a.ln()) * sigma2).sqrt();
                let mut x = Complex32::from_polar(r as f32, (2.0 * PI * b) as f32);
                let t = (i0 + k) as f64 / fs;
                for tx in txs.iter_mut() {
                    let dev = 2500.0 * (2.0 * PI * 1000.0 * t).sin();
                    tx.ph += 2.0 * PI * (tx.offset_hz + dev) / fs;
                    if tx.on.iter().any(|&(s, e)| t >= s && t < e) {
                        let p = sigma2 * 10_000.0 / fs * 10f64.powf(tx.snr_db / 10.0);
                        x += Complex32::from_polar(p.sqrt() as f32, tx.ph as f32);
                    }
                }
                *v = x;
            }
            e.push_iq(0, &buf[..n]);
            i0 += n;
            for ev in e.drain_events() {
                match ev {
                    Event::Concluded(k) => out.push((k.call, k.audio.len(), k.json)),
                    Event::CallStart(_) => starts += 1,
                    _ => {}
                }
            }
        }
        e.finish();
        for ev in e.drain_events() {
            if let Event::Concluded(k) = ev {
                out.push((k.call, k.audio.len(), k.json));
            }
        }
        (out, starts)
    }

    fn fm(freq_hz: f64, tg: u32) -> ConvChannel {
        ConvChannel { freq_hz, mode: ConvMode::Fm, talkgroup: tg, info: None, squelch_db: None }
    }

    #[test]
    fn fm_calls_detected_split_and_leakage_ignored() {
        let fs = 2_400_000.0;
        let c = 155_000_000.0;
        let mut txs = vec![
            // Two transmissions 1.5 s apart (> the 1 s call timeout): two calls.
            Tx { offset_hz: 200_000.0, snr_db: 30.0, on: vec![(0.5, 2.5), (4.0, 5.0)], ph: 0.0 },
            // A weak one, 15 dB above noise.
            Tx { offset_hz: 312_500.0, snr_db: 15.0, on: vec![(1.0, 3.0)], ph: 0.0 },
            // A very strong neighbour (not a configured channel) 12.5 kHz from channel 3.
            Tx { offset_hz: -100_000.0, snr_db: 50.0, on: vec![(0.2, 5.5)], ph: 0.0 },
        ];
        let chans = vec![fm(c + 200_000.0, 1), fm(c + 312_500.0, 2), fm(c - 87_500.0, 3), fm(c - 400_000.0, 4)];
        let (calls, starts) = run(fs, 6.0, &mut txs, chans);
        let by_tg = |tg: u32| calls.iter().filter(|(c, _, _)| c.talkgroup == tg).collect::<Vec<_>>();
        let a = by_tg(1);
        assert_eq!(a.len(), 2, "channel 1 calls: {:?}", calls.iter().map(|(c, n, _)| (c.talkgroup, c.start_s, *n)).collect::<Vec<_>>());
        let secs = |n: usize| n as f64 / 8000.0;
        assert!((secs(a[0].1) - 2.0).abs() < 0.15, "first call {:.2} s of audio", secs(a[0].1));
        assert!((secs(a[1].1) - 1.0).abs() < 0.15, "second call {:.2} s of audio", secs(a[1].1));
        assert!((a[0].0.start_s - 0.5).abs() < 0.1, "first call starts at {:.2} s", a[0].0.start_s);
        assert!(a[0].2.contains("\"audio_type\":\"analog\""));
        assert!(a[0].2.contains("\"error_count\":0,\"spike_count\":0}],\"errorList\":[],\"srcList\":["), "{}", a[0].2);
        let w = by_tg(2);
        assert_eq!(w.len(), 1);
        assert!((secs(w[0].1) - 2.0).abs() < 0.2, "weak call {:.2} s", secs(w[0].1));
        assert!(by_tg(3).is_empty(), "leakage from the neighbour made a call: {:?}", by_tg(3).iter().map(|(c, n, _)| (c.start_s, c.last_audio_s, *n)).collect::<Vec<_>>());
        assert!(by_tg(4).is_empty());
        assert_eq!(starts, 3);
    }
}
