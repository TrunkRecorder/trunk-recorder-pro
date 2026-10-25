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

/// Symbols a second, control channel and ProVoice alike.
pub const BAUD: f64 = 9600.0;
/// One-sided channel filter cutoff, Hz (9600 baud, about ±2.5 kHz).
pub const CHANNEL_CUTOFF_HZ: f64 = 7500.0;
/// Each tone's nominal distance from the carrier, Hz (the receiver tracks it).
const NOMINAL_DEV_HZ: f32 = 2400.0;

/// The 2FSK receiver both use. At 9600 baud the tones are too close for the
/// tone detectors (modulation index about 0.5): the discriminator decides.
fn receiver(rate: f64) -> Fsk2 {
    Fsk2::with_baud(rate, BAUD, NOMINAL_DEV_HZ, Fsk2Options { tones: false, ..Default::default() })
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
    sync: framer::Sync,
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
            // Sync bits that may be wrong when hunting for a frame / when one is due.
            sync: framer::Sync::new(&[provoice::SYNC, provoice::SYNC_EA], provoice::SYNC_BITS, provoice::BODY_BITS, 3, 8),
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

    pub fn push(&mut self, iq: &[Complex32], t0: f64, out: &mut Vec<PvOut>) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        for i in 0..self.bits.len() {
            let b = self.bits[i];
            if self.sync.push(b.soft, t0 + b.sample / self.rate) {
                self.frame(out);
            }
        }
    }

    /// A whole frame: its four vocoder frames.
    fn frame(&mut self, out: &mut Vec<PvOut>) {
        self.frames += 1;
        let header_s = 144.0 / BAUD;
        for f in 0..4 {
            let at = &self.layout[f];
            let words = provoice::deinterleave(|i| (self.sync.body[at[i]] > 0.0) as u8);
            let (d, e0, et) = provoice::decode(&words);
            let mut buf = [0f32; mbe::FRAME_SAMPLES];
            let kind = self.vocoder.imbe(&d, e0, et, false, &mut buf);
            if !matches!(kind, mbe::Kind::Voice | mbe::Kind::Tone) {
                self.bad_vc += 1;
            }
            mbe::to_limited(&mut buf);
            let vf = VoiceFrame { codec: Codec::Imbe, bits: d.to_vec(), e0, errs: et, erased: false, kind, tone: None };
            out.push(PvOut { t: self.sync.t + header_s + f as f64 * 0.02, out: TrackerOut::Audio(buf.to_vec(), vf) });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::trunk::{Message, MessageType};
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
        let cfg = EdacsConfig { lcns: [(1, 856_187_500), (3, 857_712_500)].into_iter().collect(), esk: false };
        let mut bits = Vec::new();
        for k in 0..12 {
            let (m1, m2) = match k {
                8 => parser::group_grant(457, 1234, 3, true, false),
                _ => parser::site_id(2, 1),
            };
            bits.extend(framer::encode(m1, m2));
        }
        for (rate, offset, flip) in [(37_500.0, 600.0, false), (24_000.0, -900.0, true), (156_250.0, 0.0, false)] {
            let b: Vec<u8> = bits.iter().map(|&b| b ^ flip as u8).collect();
            let mut iq = modulate(&b, rate, 2600.0, offset);
            noise(&mut iq, 0.3, 7);
            let mut cc = site::Site::new(cfg.clone(), &[856_187_500], rate);
            let mut out = Vec::new();
            cc.push(0, &iq, 0.0, rate, &mut out);
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
    /// a recorded call of that kind. `seed`: the noise's.
    fn engine_call(digital: bool, seed: u64) -> crate::trunk::Concluded {
        use crate::trunk::{Engine, EngineConfig, Event, SiteConfig, SourceConfig, SystemConfig};
        let (fs, center) = (240_000.0, 857_025_000.0);
        let (cc_hz, vc_hz) = (857_000_000.0, 857_062_500.0);
        let cfg = EdacsConfig { lcns: [(1, cc_hz as u64), (3, vc_hz as u64)].into_iter().collect(), esk: false };
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
        noise(&mut iq, 0.02, seed);
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

    /// Noise seeds for the engine tests. 3 to 197 each left the control
    /// channel's receiver stuck after the start-up transient (one level pulled
    /// most of the way to the other) before the levels were watched against
    /// the deciles; 11 did too, with Linux on aarch64's libm (#76).
    const SEEDS: [u64; 8] = [11, 3, 15, 45, 103, 139, 167, 197];

    #[test]
    fn engine_records_a_provoice_call() {
        for seed in SEEDS {
            let k = engine_call(true, seed);
            assert!(k.call.voice == crate::trunk::Voice::ProVoice && k.call.talkgroup == 457 && k.call.freq_hz == 857_062_500, "seed {seed}: {:?}", k.call);
            assert_eq!(k.call.sources.first().map(|s| s.src), Some(4321), "seed {seed}");
            let secs = k.audio.len() as f64 / 8000.0;
            assert!(secs > 1.0, "seed {seed}: {secs:.2} s of audio");
        }
    }

    #[test]
    fn engine_records_an_analog_call() {
        for seed in SEEDS {
            let k = engine_call(false, seed);
            assert!(k.call.voice == crate::trunk::Voice::Fm && k.call.talkgroup == 457, "seed {seed}: {:?}", k.call);
            let secs = k.audio.len() as f64 / 8000.0;
            assert!(secs > 1.0, "seed {seed}: {secs:.2} s of audio");
        }
    }
}
