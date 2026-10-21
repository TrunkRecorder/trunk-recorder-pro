//! EDACS (Enhanced Digital Access Communications System: GE, then Ericsson,
//! then Harris) Standard and Networked sites, and their ProVoice digital voice.
//!
//! ```text
//! control channel IQ → 9600 baud 2FSK → framer (sync, 3× vote, BCH) → Parser → trunk::Message
//! voice channel IQ   → analog FM, or 9600 baud 2FSK → ProVoice framer → IMBE 7100 → P25's vocoder
//! ```
//!
//! A site's channels are numbered (LCNs); the control channel grants a call
//! a channel number, which the site's channel list (Setup) turns into a
//! frequency. Each grant says whether the call is analog or ProVoice.

pub mod bch;
pub mod framer;
pub mod parser;
pub mod provoice;
pub mod site;
#[cfg(test)]
mod provoice_vectors;

pub use parser::{afs, EdacsConfig, Parser};

use num_complex::Complex32;

use crate::mbe;
use crate::smartnet::{Bit, Fsk2, Fsk2Options};
use crate::trunk::frames::{Codec, VoiceFrame};
use crate::trunk::voice::TrackerOut;
use crate::trunk::Message;

/// Symbols a second, control channel and ProVoice alike.
pub const BAUD: f64 = 9600.0;
/// One-sided channel filter cutoff, Hz (9600 baud, about ±2.5 kHz).
pub const CHANNEL_CUTOFF_HZ: f64 = 7500.0;
/// Each tone's nominal distance from the carrier, Hz (the receiver tracks it).
const NOMINAL_DEV_HZ: f32 = 2400.0;

/// The 2FSK receiver both use. At 9600 baud the tones are too close for the
/// tone detectors (modulation index about 0.5): the discriminator decides.
fn receiver(rate: f64) -> Fsk2 {
    Fsk2::with_baud(rate, BAUD, NOMINAL_DEV_HZ, Fsk2Options { tones: false, dc: true, ..Default::default() })
}

/// A control channel: IQ in, messages out.
pub struct ControlChannel {
    rx: Fsk2,
    framer: framer::Framer,
    pub parser: Parser,
    rate: f64,
    bits: Vec<Bit>,
}

impl ControlChannel {
    pub fn new(rate: f64, parser: Parser) -> Self {
        ControlChannel { rx: receiver(rate), framer: framer::Framer::new(), parser, rate, bits: Vec::new() }
    }

    /// Messages decoded / lost so far.
    pub fn counts(&self) -> (u64, u64) {
        (self.framer.good, self.framer.bad)
    }

    pub fn in_sync(&self) -> bool {
        self.framer.in_sync()
    }

    pub fn offset_hz(&self) -> f32 {
        self.rx.offset_hz()
    }

    pub fn deviation_hz(&self) -> f32 {
        self.rx.deviation_hz()
    }

    /// `iq` from the channel's head; `t0` is the air time of its first sample.
    pub fn push(&mut self, iq: &[Complex32], t0: f64, out: &mut Vec<Message>) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        for b in &self.bits {
            let t = t0 + b.sample / self.rate;
            if let Some(f) = self.framer.push(b.soft, t) {
                self.parser.frame(f.msgs[0], f.msgs[1], f.t, out);
            }
        }
    }
}

impl crate::metrics::Instrumented for ControlChannel {
    fn report(&self, sink: &mut dyn crate::metrics::Sink) {
        sink.gauge("inSync", self.in_sync() as u8 as f64);
        sink.gauge("offset", self.offset_hz() as f64);
        sink.gauge("deviation", self.deviation_hz() as f64);
    }
}

/// Sync bits that may be wrong when hunting for a ProVoice frame / when one is due.
const PV_HUNT_ERRORS: u32 = 3;
const PV_DUE_ERRORS: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum PvState {
    Hunt,
    Body { n: usize, inverted: bool },
    Due { n: usize, inverted: bool },
}

/// What [`ProVoice`] heard, at air time `t`.
#[derive(Debug)]
pub struct PvOut {
    pub t: f64,
    pub out: TrackerOut,
}

/// A ProVoice voice channel: IQ in, 20 ms of audio per vocoder frame out.
pub struct ProVoice {
    rx: Fsk2,
    rate: f64,
    sr: u32,
    state: PvState,
    body: Vec<f32>,
    t: f64,
    vocoder: mbe::Decoder,
    layout: [[usize; 142]; 4],
    bits: Vec<Bit>,
    /// Frames decoded, and vocoder frames that weren't spoken as sent.
    pub frames: u64,
    pub bad_vc: u64,
}

impl ProVoice {
    pub fn new(rate: f64, seed: u32, profile: mbe::Profile) -> Self {
        let mut vocoder = mbe::Decoder::new(mbe::lcg(seed), profile);
        vocoder.hard_fec();
        ProVoice {
            rx: receiver(rate),
            rate,
            sr: 0,
            state: PvState::Hunt,
            body: vec![0.0; provoice::BODY_BITS],
            t: 0.0,
            vocoder,
            layout: provoice::layout(),
            bits: Vec::new(),
            frames: 0,
            bad_vc: 0,
        }
    }

    pub fn offset_hz(&self) -> f32 {
        self.rx.offset_hz()
    }

    fn sync(&self, max: u32) -> Option<bool> {
        for s in [provoice::SYNC, provoice::SYNC_EA] {
            let e = (self.sr ^ s).count_ones();
            if e <= max {
                return Some(false);
            }
            if 32 - e <= max {
                return Some(true);
            }
        }
        None
    }

    pub fn push(&mut self, iq: &[Complex32], t0: f64, out: &mut Vec<PvOut>) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        for i in 0..self.bits.len() {
            let b = self.bits[i];
            self.bit(b.soft, t0 + b.sample / self.rate, out);
        }
    }

    fn bit(&mut self, soft: f32, t: f64, out: &mut Vec<PvOut>) {
        self.sr = self.sr << 1 | (soft > 0.0) as u32;
        match self.state {
            PvState::Hunt => {
                if let Some(inverted) = self.sync(PV_HUNT_ERRORS) {
                    self.state = PvState::Body { n: 0, inverted };
                }
            }
            PvState::Body { n, inverted } => {
                if n == 0 {
                    self.t = t;
                }
                self.body[n] = if inverted { -soft } else { soft };
                if n + 1 < provoice::BODY_BITS {
                    self.state = PvState::Body { n: n + 1, inverted };
                } else {
                    self.state = PvState::Due { n: 0, inverted };
                    self.frame(out);
                }
            }
            PvState::Due { n, inverted } => {
                if n + 1 < provoice::SYNC_BITS {
                    self.state = PvState::Due { n: n + 1, inverted };
                } else {
                    self.state = match self.sync(PV_DUE_ERRORS) {
                        Some(inv) if inv == inverted => PvState::Body { n: 0, inverted },
                        _ => PvState::Hunt,
                    };
                }
            }
        }
    }

    /// A whole frame: its four vocoder frames.
    fn frame(&mut self, out: &mut Vec<PvOut>) {
        self.frames += 1;
        let header_s = 144.0 / BAUD;
        for f in 0..4 {
            let at = &self.layout[f];
            let words = provoice::deinterleave(|i| (self.body[at[i]] > 0.0) as u8);
            let (d, e0, et) = provoice::decode(&words);
            let mut buf = [0f32; mbe::FRAME_SAMPLES];
            let kind = self.vocoder.imbe(&d, e0, et, false, &mut buf);
            if !matches!(kind, mbe::Kind::Voice | mbe::Kind::Tone) {
                self.bad_vc += 1;
            }
            mbe::to_limited(&mut buf);
            let vf = VoiceFrame { codec: Codec::Imbe, bits: d.to_vec(), e0, errs: et, erased: false, kind };
            out.push(PvOut { t: self.t + header_s + f as f64 * 0.02, out: TrackerOut::Audio(buf.to_vec(), vf) });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::trunk::MessageType;
    use std::f64::consts::PI;

    /// 9600 baud 2FSK of `bits` (1 = +dev), smoothed over a symbol (roughly GFSK).
    pub fn modulate(bits: &[u8], rate: f64, dev_hz: f64, offset_hz: f64) -> Vec<Complex32> {
        let sps = rate / BAUD;
        let n = (bits.len() as f64 * sps) as usize;
        let nrz: Vec<f64> = (0..n).map(|i| if bits[((i as f64) / sps) as usize % bits.len()] == 1 { 1.0 } else { -1.0 }).collect();
        let w = (sps / 2.0).round().max(1.0) as usize;
        let mut ph = 0.0f64;
        (0..n)
            .map(|i| {
                let lo = i.saturating_sub(w / 2);
                let hi = (i + w - w / 2).min(n);
                let v = nrz[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
                ph = (ph + 2.0 * PI * (v * dev_hz + offset_hz) / rate) % (2.0 * PI);
                Complex32::from_polar(1.0, ph as f32)
            })
            .collect()
    }

    pub fn noise(iq: &mut [Complex32], amp: f32, seed: u64) {
        let mut s = seed | 1;
        let mut r = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        for x in iq.iter_mut() {
            *x += Complex32::new(amp * r(), amp * r());
        }
    }

    #[test]
    fn iq_to_grant() {
        let cfg = EdacsConfig { lcns: [(1, 856_187_500), (3, 857_712_500)].into_iter().collect(), esk: false, ..Default::default() };
        let mut bits = Vec::new();
        for k in 0..12 {
            let (m1, m2) = match k {
                3 => parser::group_grant(457, 1234, 3, true, false),
                _ => parser::site_id(2, 1),
            };
            bits.extend(framer::encode(m1, m2));
        }
        for (rate, offset, flip) in [(37_500.0, 600.0, false), (24_000.0, -900.0, true), (156_250.0, 0.0, false)] {
            let b: Vec<u8> = bits.iter().map(|&b| b ^ flip as u8).collect();
            let mut iq = modulate(&b, rate, 2600.0, offset);
            noise(&mut iq, 0.3, 7);
            let mut cc = ControlChannel::new(rate, Parser::new(cfg.clone()));
            let mut out = Vec::new();
            cc.push(&iq, 0.0, &mut out);
            let g: Vec<&Message> = out.iter().filter(|m| m.kind == MessageType::Grant).collect();
            assert_eq!(g.len(), 1, "rate {rate}: {out:?}");
            assert_eq!((g[0].talkgroup, g[0].source, g[0].freq_hz, g[0].voice), (457, 1234, 857_712_500, crate::trunk::Voice::ProVoice));
            assert_eq!(cc.parser.site, Some(2));
            assert!(cc.counts().0 >= 20 && cc.counts().1 <= 2, "rate {rate}: {:?}", cc.counts());
        }
    }

    /// Test speech: a steady voiced frame (pitch and amplitudes in u0..u7).
    pub fn speech_frames(n: usize) -> Vec<[u8; 88]> {
        let u = [0x5a0u32, 0x6a3, 0x412, 0x7b1, 0x2c4, 0x1d5, 0x0e6, 0x34 << 1];
        let bits = crate::p25::voice::imbe_params_to_bits(&u);
        vec![bits; n]
    }

    #[test]
    fn iq_to_provoice_audio() {
        let frames = speech_frames(40);
        let mut bits = vec![0u8, 1, 0, 1, 0, 1, 0, 1];
        for c in frames.chunks(4) {
            bits.extend(provoice::frame_bits(&[c[0], c[1], c[2], c[3]]));
        }
        for (rate, flip) in [(37_500.0, false), (39_062.5, true)] {
            let b: Vec<u8> = bits.iter().map(|&b| b ^ flip as u8).collect();
            let mut iq = modulate(&b, rate, 2600.0, 400.0);
            noise(&mut iq, 0.2, 3);
            let mut pv = ProVoice::new(rate, 851_000_000, mbe::Profile::Enhanced);
            let mut out = Vec::new();
            pv.push(&iq, 0.0, &mut out);
            let audio: Vec<&VoiceFrame> = out.iter().filter_map(|o| if let TrackerOut::Audio(_, vf) = &o.out { Some(vf) } else { None }).collect();
            assert!(audio.len() >= 36, "rate {rate}: {} vocoder frames", audio.len());
            assert!(audio.iter().all(|vf| vf.bits == frames[0].to_vec() && vf.errs == 0), "rate {rate}: {:?}", audio.iter().map(|v| v.errs).collect::<Vec<_>>());
            // It speaks: the audio isn't silence.
            let power: f32 = out.iter().filter_map(|o| if let TrackerOut::Audio(a, _) = &o.out { Some(a.iter().map(|v| v * v).sum::<f32>()) } else { None }).sum();
            assert!(power > 1.0, "rate {rate}: power {power}");
        }
    }

    /// Wideband IQ: an EDACS control channel granting a call on LCN 3, and
    /// the call's voice there — ProVoice, or analog FM (a 1 kHz tone) — makes
    /// a recorded call of that kind.
    fn engine_call(digital: bool) -> crate::trunk::Concluded {
        use crate::trunk::{Engine, EngineConfig, Event, SiteConfig, SourceConfig, SystemConfig};
        let (fs, center) = (240_000.0, 857_025_000.0);
        let (cc_hz, vc_hz) = (857_000_000.0, 857_062_500.0);
        let cfg = EdacsConfig { lcns: [(1, cc_hz as u64), (3, vc_hz as u64)].into_iter().collect(), esk: false, ..Default::default() };
        // 2.4 s of control channel: the site, then the grant and updates while the call goes on.
        let mut cc_bits = Vec::new();
        for k in 0..80 {
            let (m1, m2) = match k {
                0..=9 => parser::site_id(2, 1),
                10 => parser::group_grant(457, 4321, 3, digital, false),
                11..=60 if k % 2 == 0 => parser::group_update(457, 3, digital),
                _ => parser::site_id(2, 1),
            };
            cc_bits.extend(framer::encode(m1, m2));
        }
        let cc = modulate(&cc_bits, fs, 2600.0, cc_hz - center);
        let n = cc.len();
        // Voice from 0.35 s to 1.6 s.
        let (v0, v1) = ((0.35 * fs) as usize, (1.6 * fs) as usize);
        let vc: Vec<Complex32> = if digital {
            let frames = speech_frames(64);
            let mut bits = Vec::new();
            for c in frames.chunks(4) {
                bits.extend(provoice::frame_bits(&[c[0], c[1], c[2], c[3]]));
            }
            modulate(&bits, fs, 2600.0, vc_hz - center)
        } else {
            let mut ph = 0.0f64;
            (0..v1 - v0)
                .map(|i| {
                    let t = i as f64 / fs;
                    ph = (ph + 2.0 * PI * (vc_hz - center + 2500.0 * (2.0 * PI * 1000.0 * t).sin()) / fs) % (2.0 * PI);
                    Complex32::from_polar(1.0, ph as f32)
                })
                .collect()
        };
        let mut iq: Vec<Complex32> = (0..n).map(|i| cc[i] * 0.3 + if (v0..v1).contains(&i) { vc[(i - v0) % vc.len()] * 0.3 } else { Complex32::default() }).collect();
        noise(&mut iq, 0.02, 11);
        let ecfg = EngineConfig {
            systems: vec![SystemConfig::trunked("nycomco", vec![SiteConfig { control_channels: vec![cc_hz], protocol: crate::trunk::Protocol::Edacs(cfg), ..Default::default() }])],
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            ..Default::default()
        };
        let e = Engine::new(ecfg).unwrap();
        for chunk in iq.chunks(4096) {
            e.push_iq(0, chunk);
        }
        e.finish();
        let events = e.drain_events();
        let done: Vec<_> = events.iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some(k) } else { None }).collect();
        assert_eq!(done.len(), 1, "{:?}", events.iter().filter(|e| !matches!(e, Event::Audio { .. })).collect::<Vec<_>>());
        (*done[0]).clone()
    }

    #[test]
    fn engine_records_a_provoice_call() {
        let k = engine_call(true);
        assert!(k.call.voice == crate::trunk::Voice::ProVoice && k.call.talkgroup == 457 && k.call.freq_hz == 857_062_500, "{:?}", k.call);
        assert_eq!(k.call.sources.first().map(|s| s.src), Some(4321));
        let secs = k.audio.len() as f64 / 8000.0;
        assert!(secs > 1.0, "{secs:.2} s of audio");
    }

    #[test]
    fn engine_records_an_analog_call() {
        let k = engine_call(false);
        assert!(k.call.voice == crate::trunk::Voice::Fm && k.call.talkgroup == 457, "{:?}", k.call);
        let secs = k.audio.len() as f64 / 8000.0;
        assert!(secs > 1.0, "{secs:.2} s of audio");
    }

    /// A site whose list lacks LCN 9: its calls key 857.075 MHz, which is
    /// on the site's 25 kHz raster but in no list. The engine's raster
    /// measurements find it (once the site has identified itself, or the
    /// config says to learn anyway), unless it's another site's frequency.
    /// (Learned: the notes, and the calls' frequencies.)
    fn engine_unlisted(site_ids: bool, learn_unlisted: bool, others: Vec<u64>) -> (Vec<String>, Vec<(u64, usize)>) {
        use crate::trunk::{Engine, EngineConfig, Event, SiteConfig, SourceConfig, SystemConfig};
        let (fs, center) = (240_000.0, 857_040_000.0);
        let cfg = EdacsConfig { lcns: [(1, 857_000_000), (2, 857_025_000)].into_iter().collect(), learn_unlisted, others, ..Default::default() };
        let mut bits = Vec::new();
        let mut keyed = Vec::new();
        for k in 0..600 {
            let t = k as f64 * 0.03;
            let (m1, m2) = if k % 66 == 20 {
                keyed.push(t);
                parser::group_grant(863, 4321, 9, false, false)
            } else if k % 66 > 20 && k % 66 < 50 && k % 4 == 0 {
                parser::group_update(863, 9, false)
            } else if site_ids {
                parser::site_id(5, 1)
            } else {
                // A site that never says who it is: an affiliation instead.
                (6 << 25 | 863 << 14 | 77, 0)
            };
            bits.extend(framer::encode(m1, m2));
        }
        let cc = modulate(&bits, fs, 2600.0, 857_000_000.0 - center);
        let mut ph = 0.0f64;
        let mut iq: Vec<Complex32> = (0..cc.len())
            .map(|i| {
                let t = i as f64 / fs;
                let on = keyed.iter().any(|&k| t >= k + 0.2 && t < k + 1.0);
                ph = (ph + 2.0 * PI * (857_075_000.0 - center + 2500.0 * (2.0 * PI * 1000.0 * t).sin()) / fs) % (2.0 * PI);
                cc[i] * 0.3 + if on { Complex32::from_polar(0.3, ph as f32) } else { Complex32::default() }
            })
            .collect();
        noise(&mut iq, 0.02, 9);
        let ecfg = EngineConfig {
            systems: vec![SystemConfig::trunked("nycomco", vec![SiteConfig { control_channels: vec![857_000_000.0], protocol: crate::trunk::Protocol::Edacs(cfg), ..Default::default() }])],
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: 30_000.0 }],
            ..Default::default()
        };
        let e = Engine::new(ecfg).unwrap();
        for chunk in iq.chunks(4096) {
            e.push_iq(0, chunk);
        }
        e.finish();
        let events = e.drain_events();
        let notes = events.iter().filter_map(|ev| if let Event::Note { text, .. } = ev { Some(text.clone()) } else { None }).collect();
        let done = events.iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some((k.call.freq_hz, k.audio.len())) } else { None }).collect();
        (notes, done)
    }

    #[test]
    fn engine_learns_a_channel_missing_from_every_list() {
        let (notes, done) = engine_unlisted(true, false, vec![]);
        assert!(notes.iter().any(|n| n.contains("LCN 9 is 857.0750 MHz on the air")), "{notes:?}");
        assert!(done.iter().any(|&(f, n)| f == 857_075_000 && n > 4000), "{done:?}");
    }

    #[test]
    fn unlisted_channels_wait_for_the_site_id_unless_told() {
        let (notes, _) = engine_unlisted(false, false, vec![]);
        assert!(!notes.iter().any(|n| n.contains("LCN 9 is")), "a site that hasn't identified itself: {notes:?}");
        let (notes, _) = engine_unlisted(false, true, vec![]);
        assert!(notes.iter().any(|n| n.contains("LCN 9 is 857.0750 MHz on the air")), "learnUnlisted: {notes:?}");
        // Another site's frequency never is this site's.
        let (notes, _) = engine_unlisted(true, true, vec![857_075_000]);
        assert!(!notes.iter().any(|n| n.contains("LCN 9 is")), "another site's: {notes:?}");
    }
}
