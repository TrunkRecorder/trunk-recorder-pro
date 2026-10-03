//! Motorola SmartNet / SmartZone (Type II) control channels:
//!
//! ```text
//! channel IQ → rx::Fsk2 (3600 baud 2FSK) → osw::Framer (sync, Viterbi, CRC)
//!   → parser::Parser (OSW sequences, band plan) → trunk::Message
//! ```
//!
//! Voice channels are P25 Phase 1 (digital grants) or narrowband FM (analog
//! grants); the parser says which on each message.

pub mod osw;
pub mod parser;
pub mod plan;
pub mod rx;

pub use osw::{Framer, FramerOut, Osw};
pub use parser::{Bandplan, Parser};
pub use rx::{Bit, Fsk2, Fsk2Options, SYMBOL_RATE};

use num_complex::Complex32;

use crate::trunk::Message;

/// One-sided channel filter cutoff for the control channel, Hz (tones at
/// about ±2.4 kHz, 3600 baud).
pub const CHANNEL_CUTOFF_HZ: f64 = 5000.0;

/// A control channel: IQ in, messages out.
pub struct ControlChannel {
    rx: Fsk2,
    framer: Framer,
    pub parser: Parser,
    rate: f64,
    bits: Vec<Bit>,
    fout: Vec<FramerOut>,
}

impl ControlChannel {
    pub fn new(rate: f64, parser: Parser) -> Self {
        ControlChannel { rx: Fsk2::new(rate), framer: Framer::default(), parser, rate, bits: Vec::new(), fout: Vec::new() }
    }

    /// Good / lost OSWs so far.
    pub fn counts(&self) -> (u64, u64) {
        (self.framer.good, self.framer.bad)
    }

    pub fn in_sync(&self) -> bool {
        self.framer.in_sync()
    }

    /// Carrier offset the receiver measures, Hz (+ = above the channel).
    pub fn offset_hz(&self) -> f32 {
        self.rx.offset_hz()
    }

    /// Tone separation / 2 the receiver measures, Hz (SmartNet's nominal is ~±2.4 kHz).
    pub fn deviation_hz(&self) -> f32 {
        self.rx.deviation_hz()
    }

    /// `iq` from the channel's head; `t0` is the sample-clock time of the
    /// head's first output sample.
    pub fn push(&mut self, iq: &[Complex32], t0: f64, out: &mut Vec<Message>) {
        self.bits.clear();
        self.rx.push(iq, &mut self.bits);
        for b in &self.bits {
            self.fout.clear();
            self.framer.push(b.soft, &mut self.fout);
            let t = t0 + b.sample / self.rate;
            for o in &self.fout {
                match *o {
                    FramerOut::Osw(osw, _) => self.parser.osw(osw, t, out),
                    FramerOut::Bad(_) => self.parser.bad(t, out),
                }
            }
        }
    }
}

/// The OSW framer's view, for the dashboard: whether it holds sync, and the
/// 2FSK receiver's carrier offset and deviation.
impl crate::metrics::Instrumented for ControlChannel {
    fn report(&self, sink: &mut dyn crate::metrics::Sink) {
        sink.gauge("inSync", self.in_sync() as u8 as f64);
        sink.gauge("offset", self.offset_hz() as f64);
        sink.gauge("deviation", self.deviation_hz() as f64);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::trunk::MessageType;

    #[test]
    fn iq_to_grant() {
        let plan = Bandplan::from_config("400_custom", 489_087_500.0, 25_000.0, 380, 496_612_500.0).unwrap();
        let idle = Osw { addr: 0x2468, grp: false, cmd: 0x2f8 };
        let mut osws = vec![idle; 5];
        osws.push(Osw { addr: 40020, grp: false, cmd: 12 });
        osws.push(Osw { addr: 0x8050, grp: true, cmd: 380 + 295 });
        osws.extend([idle; 8]);
        let bits: Vec<u8> = osws.iter().flat_map(|&o| osw::encode(o)).collect();
        let rate = 28_125.0;
        let iq = rx::tests::modulate(&bits, rate, 2300.0, 700.0, 0.2);
        let mut cc = ControlChannel::new(rate, Parser::new(plan));
        let mut out = Vec::new();
        cc.push(&iq, 0.0, &mut out);
        let g: Vec<&Message> = out.iter().filter(|m| m.kind == MessageType::Grant).collect();
        assert_eq!(g.len(), 1, "{out:?}");
        assert_eq!((g[0].talkgroup, g[0].freq_hz, g[0].source), (0x8050, 496_462_500, 40020));
        assert!(cc.counts().0 >= 13, "{:?}", cc.counts());
    }

    /// Wideband IQ: a SmartNet control channel granting an analog call, and
    /// that call's FM voice (a 1 kHz tone) on its channel → a recorded FM call.
    #[test]
    fn engine_records_an_analog_grant() {
        use crate::trunk::{Engine, EngineConfig, Event, SmartnetConfig, SourceConfig, SystemConfig};
        use std::f64::consts::PI;
        let (fs, center) = (240_000.0, 460_000_000.0);
        // OBT: chan 380 = 459.0 MHz, 10 kHz steps; CC 460.03 (chan 483), voice 459.96 (chan 476).
        let bandplan = Bandplan::from_config("400_custom", 459_000_000.0, 10_000.0, 380, 461_000_000.0).unwrap();
        let (cc_hz, vc_hz) = (460_030_000.0, 459_960_000.0);
        let idle = Osw { addr: 0x2468, grp: false, cmd: 0x2f8 };
        let mut osws = Vec::new();
        for _ in 0..8 {
            osws.extend([idle; 6]);
            // Analog group grant: inbound channel (group bit = analog) + outbound channel with the talkgroup.
            osws.push(Osw { addr: 1234, grp: true, cmd: 50 });
            osws.push(Osw { addr: 0x1230, grp: true, cmd: 476 });
            osws.extend([idle; 4]);
        }
        let bits: Vec<u8> = osws.iter().flat_map(|&o| osw::encode(o)).collect();
        let n = (bits.len() as f64 / SYMBOL_RATE * fs) as usize;
        let (mut p_cc, mut p_vc) = (0.0f64, 0.0f64);
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut noise = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let iq: Vec<Complex32> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let b = bits[((t * SYMBOL_RATE) as usize).min(bits.len() - 1)];
                p_cc += 2.0 * PI * (cc_hz - center + if b == 1 { 2400.0 } else { -2400.0 }) / fs;
                p_vc += 2.0 * PI * (vc_hz - center + 2500.0 * (2.0 * PI * 1000.0 * t).sin()) / fs;
                let v = Complex32::from_polar(0.3, p_cc as f32) + if t > 0.3 { Complex32::from_polar(0.3, p_vc as f32) } else { Complex32::default() };
                v + Complex32::new(0.02 * noise() as f32, 0.02 * noise() as f32)
            })
            .collect();
        let cfg = EngineConfig {
            systems: vec![SystemConfig {
                control_channels: vec![cc_hz],
                protocol: crate::trunk::Protocol::SmartNet(SmartnetConfig { bandplan, analog_default: false }),
                ..Default::default()
            }],
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false }],
            ..Default::default()
        };
        let mut e = Engine::new(cfg).unwrap();
        for chunk in iq.chunks(4096) {
            e.push_iq(0, chunk);
        }
        e.finish();
        let events = e.drain_events();
        let done: Vec<_> = events.iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some(k) } else { None }).collect();
        assert_eq!(done.len(), 1, "{:?}", events.iter().filter(|e| !matches!(e, Event::Audio { .. })).collect::<Vec<_>>());
        let k = done[0];
        assert!(k.call.analog && k.call.talkgroup == 0x1230 && k.call.freq_hz == 459_960_000, "{:?}", k.call);
        let secs = k.audio.len() as f64 / 8000.0;
        assert!(secs > 1.5, "{secs:.2} s of audio");
        // The tone is there: Goertzel at 1 kHz carries most of the power.
        let x = &k.audio[k.audio.len() / 4..];
        let w = 2.0 * PI * 1000.0 / 8000.0;
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = v as f64 + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let tone = (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2) * 2.0 / (x.len() as f64).powi(2);
        let total = x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64;
        assert!(tone > 0.8 * total, "tone {tone:.4} of {total:.4}");
    }
}
