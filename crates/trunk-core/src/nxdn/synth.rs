//! A synthetic NXDN transmitter, for tests and `tool nxdnsynth`: frames
//! built from layer 3 messages and AMBE voice, 4FSK-modulated. Every
//! encoder here is the inverse of a decoder in this module, so a decode of
//! its output is a check of both against each other; the decoders were
//! first checked on real recordings.

use num_complex::Complex32;

use super::channel::{build, Sr, CAC_AT, HALF_AT, SACCH_AT, UDCH_AT};
use super::frame::{frame_dibits, Lich, BODY_SYMBOLS, FRAME_SYMBOLS};
use super::layer3::{build as l3, CallHead};
use super::Rate;
use crate::ambe::encode_vcw;

/// What one half of a voice frame carries.
#[derive(Clone, Debug)]
pub enum Half {
    /// Two 20 ms AMBE frames (49 bits each).
    Voice([[u8; 49]; 2]),
    Facch1(Vec<u8>),
}

fn put_bits(body: &mut [u8; BODY_SYMBOLS], at: usize, bits: &[u8]) {
    for (i, &b) in bits.iter().enumerate() {
        let k = at + i;
        let d = &mut body[k / 2];
        let sh = 1 - k % 2;
        *d = *d & !(1 << sh) | (b & 1) << sh;
    }
}

fn with_lich(raw: u8) -> [u8; BODY_SYMBOLS] {
    let mut body = [0u8; BODY_SYMBOLS];
    body[..8].copy_from_slice(&Lich { raw }.dibits());
    body
}

/// A voice-channel frame: LICH, SACCH (SR + 18 data bits), two halves.
pub fn voice_frame(lich: u8, sr: Sr, sacch: u32, halves: &[Half; 2]) -> [u8; FRAME_SYMBOLS] {
    let mut body = with_lich(lich);
    put_bits(&mut body, SACCH_AT, &build::sacch(sr, sacch));
    for (h, half) in halves.iter().enumerate() {
        match half {
            Half::Voice(v) => {
                for (k, u) in v.iter().enumerate() {
                    let d = encode_vcw(u);
                    let at = (HALF_AT[h] + 72 * k) / 2;
                    body[at..at + 36].copy_from_slice(&d);
                }
            }
            Half::Facch1(o) => put_bits(&mut body, HALF_AT[h], &build::facch1(o)),
        }
    }
    frame_dibits(&body)
}

/// A control channel frame: LICH 0x01 (CAC, outbound) and the CAC.
pub fn cac_frame(sr: Sr, octets: &[u8]) -> [u8; FRAME_SYMBOLS] {
    let mut body = with_lich(0x01);
    put_bits(&mut body, CAC_AT, &build::cac(sr, octets));
    frame_dibits(&body)
}

/// A FACCH2 frame (LICH `lich`, e.g. 0x29 trunked / 0x49 conventional).
pub fn facch2_frame(lich: u8, sr: Sr, octets: &[u8]) -> [u8; FRAME_SYMBOLS] {
    let mut body = with_lich(lich);
    put_bits(&mut body, UDCH_AT, &build::udch(sr, octets));
    frame_dibits(&body)
}

/// A recognisable AMBE frame: a tone-like voiced frame whose parameters
/// vary with `n` (any 49 bits are a valid frame to the FEC).
pub fn ambe(n: usize) -> [u8; 49] {
    let mut x = 0x9e37_79b9u32 ^ (n as u32).wrapping_mul(0x85eb_ca6b);
    std::array::from_fn(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x & 1) as u8
    })
}

/// A voice transmission.
#[derive(Clone, Debug)]
pub struct Tx {
    pub rate: Rate,
    pub ran: u8,
    pub head: CallHead,
    /// 0 clear, 1 scrambler, 2 DES, 3 AES.
    pub cipher: u8,
    /// Voice superframes (4 frames each: 320 ms at 4800 bps).
    pub superframes: usize,
    /// RF channel of the LICH: 1 trunked traffic, 2 conventional.
    pub rf: u8,
    pub outbound: bool,
}

impl Tx {
    fn lich(&self, fct: u8, option: u8) -> u8 {
        self.rf << 5 | fct << 3 | option << 1 | self.outbound as u8
    }

    /// The frames: a header (VCALL in both FACCH1s), voice superframes
    /// (VCALL in the SACCH; at 9600 EHR every other frame is FACCH1, as the
    /// spec has it), then TX_REL.
    pub fn frames(&self) -> Vec<[u8; FRAME_SYMBOLS]> {
        let vcall = l3::vcall(&self.head, self.cipher, 0);
        let rel = l3::tx_rel(&self.head);
        let sr = |s: u8| Sr { structure: s, ran: self.ran };
        let mut out = vec![voice_frame(self.lich(0, 0), sr(0), 0, &[Half::Facch1(vcall.clone()), Half::Facch1(vcall.clone())])];
        let mut msg = [0u8; 9];
        msg[..vcall.len()].copy_from_slice(&vcall);
        let q = build::sacch_quarters(&msg);
        let mut n = 0;
        for _ in 0..self.superframes {
            for k in 0..4 {
                let halves = if self.rate == Rate::N96 && k % 2 == 1 {
                    [Half::Facch1(vcall.clone()), Half::Facch1(vcall.clone())]
                } else {
                    n += 4;
                    [Half::Voice([ambe(n - 4), ambe(n - 3)]), Half::Voice([ambe(n - 2), ambe(n - 1)])]
                };
                let option = if matches!(halves[0], Half::Facch1(_)) { 0 } else { 3 };
                out.push(voice_frame(self.lich(2, option), sr(3 - k as u8), q[k], &halves));
            }
        }
        out.push(voice_frame(self.lich(0, 0), sr(0), 0, &[Half::Facch1(rel.clone()), Half::Facch1(rel)]));
        out
    }
}

/// Frames' symbols, back to back.
pub fn dibits(frames: &[[u8; FRAME_SYMBOLS]]) -> Vec<u8> {
    frames.iter().flatten().copied().collect()
}

/// 4FSK at the rate's symbol rate and deviation, `fs` samples per second,
/// `offset_hz` off the channel centre; `on`: per symbol, whether the
/// carrier is up (None: always).
pub fn modulate(dibits: &[u8], on: Option<&[bool]>, rate: Rate, fs: f64, offset_hz: f64, amp: f32, phase: &mut f64) -> Vec<Complex32> {
    let baud = rate.baud();
    // Hz per level step (outer: 3×): 350 at 4800 bps, 800 at 9600.
    let unit = if rate == Rate::N48 { 350.0 } else { 800.0 };
    let sps = fs / baud;
    let n = (dibits.len() as f64 * sps) as usize;
    // Frequency pulses smoothed over half a symbol (a gentler spectrum than
    // rectangular, short of the real RRC).
    let w = ((sps / 2.0).round() as usize).max(1);
    let mut f: Vec<f64> = (0..n)
        .map(|i| {
            let k = ((i as f64 / sps) as usize).min(dibits.len() - 1);
            unit * match dibits[k] {
                0b01 => 3.0,
                0b00 => 1.0,
                0b10 => -1.0,
                _ => -3.0,
            }
        })
        .collect();
    let mut acc = 0.0;
    let raw = f.clone();
    for i in 0..n {
        acc += raw[i];
        if i >= w {
            acc -= raw[i - w];
        }
        f[i] = acc / w.min(i + 1) as f64;
    }
    (0..n)
        .map(|i| {
            *phase += 2.0 * std::f64::consts::PI * (offset_hz + f[i]) / fs;
            let k = ((i as f64 / sps) as usize).min(dibits.len() - 1);
            let a = if on.is_none_or(|o| o[k]) { amp } else { 0.0 };
            Complex32::from_polar(a, *phase as f32)
        })
        .collect()
}

/// Gaussian noise of standard deviation `sigma` per component, from a seed.
pub fn add_noise(iq: &mut [Complex32], sigma: f32, seed: &mut u64) {
    let mut u = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        ((*seed >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    for x in iq.iter_mut() {
        let (a, b) = (u(), u());
        let r = (-2.0 * a.ln()).sqrt() * sigma as f64;
        let t = 2.0 * std::f64::consts::PI * b;
        *x += Complex32::new((r * t.cos()) as f32, (r * t.sin()) as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::decode_vcw;
    use crate::dsp::Receiver;
    use crate::nxdn::channel::{facch1, sacch, SacchAssembler};
    use crate::nxdn::frame::{Body, Framer};
    use crate::nxdn::layer3::{Context, Message, CALL_CONFERENCE};

    fn tx(rate: Rate) -> Tx {
        Tx { rate, ran: 9, head: CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: if rate == Rate::N96 { 2 } else { 0 }, source: 1234, destination: 77 }, cipher: 0, superframes: 5, rf: 2, outbound: true }
    }

    /// Through the 4FSK receiver and framer, at both rates, clean and in
    /// noise: the VCALL in FACCH1 and SACCH, and the voice, as sent.
    #[test]
    fn a_synthetic_transmission_decodes() {
        for rate in [Rate::N48, Rate::N96] {
            for snr_noise in [0.0f32, 0.25] {
                let t = tx(rate);
                // Random symbols around it (a receiver needs all four levels to
                // set its thresholds, and has 2 blocks of latency to flush).
                let mut x = 5u32;
                let mut pad = |n: usize| -> Vec<u8> {
                    (0..n)
                        .map(|_| {
                            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                            (x >> 30) as u8
                        })
                        .collect()
                };
                let mut d = pad(600);
                d.extend(dibits(&t.frames()));
                d.extend(pad(1200));
                let fs = 48_000.0;
                let mut ph = 0.0;
                let mut iq = modulate(&d, None, rate, fs, 150.0, 1.0, &mut ph);
                let mut seed = 42;
                add_noise(&mut iq, snr_noise, &mut seed);
                let mut rx = rate.receiver(fs);
                let mut syms = Vec::new();
                rx.push(&iq, &mut syms);
                let mut fr = Framer::new();
                let mut frames = Vec::new();
                for s in &syms {
                    fr.push(s, &mut frames);
                }
                assert!(frames.len() >= t.frames().len() - 1, "{rate:?} noise {snr_noise}: {} frames", frames.len());
                let (mut vcalls, mut rels, mut n_voice, mut voice_ok) = (0, 0, 0, 0);
                let mut sf = SacchAssembler::default();
                let mut voice_n = 0usize;
                for f in &frames {
                    // (The framer flywheels a few frames past the end.)
                    let Some((l, _)) = f.lich() else { continue };
                    let Body::Voice { facch, superframe, .. } = l.body() else { panic!() };
                    if let (true, Some(s)) = (superframe, sacch(f)) {
                        assert_eq!(s.sr.ran, 9);
                        if let Some(o) = sf.push(&s) {
                            assert!(matches!(Message::parse(&o, Context::Traffic, false), Message::VCall { head, .. } if head.source == 1234 && head.destination == 77));
                            vcalls += 1;
                        }
                    }
                    for h in 0..2 {
                        if facch[h] {
                            match facch1(f, h).map(|(o, _)| Message::parse(&o, Context::Traffic, false)) {
                                Some(Message::VCall { .. }) => vcalls += 1,
                                Some(Message::TxRel { .. }) => rels += 1,
                                _ => {}
                            }
                        } else {
                            for k in 0..2 {
                                let (dd, r) = f.voice_frame(2 * h + k);
                                n_voice += 1;
                                voice_ok += (decode_vcw(&dd, Some(&r)).bits == ambe(voice_n)) as usize;
                                voice_n += 1;
                            }
                        }
                    }
                }
                assert!(vcalls >= 5 && rels >= 1, "{rate:?} noise {snr_noise}: vcalls {vcalls} rels {rels}");
                assert!(voice_ok * 10 >= n_voice * 9, "{rate:?} noise {snr_noise}: voice {voice_ok}/{n_voice}");
            }
        }
    }

    use crate::trunk::conventional::{ConvChannel, ConvMode};
    use crate::trunk::{Access, Engine, EngineConfig, Event, SourceConfig, Talkgroup};

    fn run_engine(cfg: EngineConfig, iq: &[Complex32]) -> (Vec<crate::trunk::Concluded>, Vec<String>) {
        let mut e = Engine::new(cfg).unwrap();
        for c in iq.chunks(8192) {
            e.push_iq(0, c);
        }
        e.finish();
        let (mut done, mut skipped) = (Vec::new(), Vec::new());
        for ev in e.drain_events() {
            match ev {
                Event::Concluded(k) => done.push(k),
                Event::ConvSkipped { code, .. } => skipped.push(code),
                _ => {}
            }
        }
        (done, skipped)
    }

    /// A keyed-up transmission (nothing on the air a second before and
    /// after) at `offset` Hz in a `fs` band, with a little noise.
    fn keyed(t: &Tx, fs: f64, offset: f64) -> Vec<Complex32> {
        let pad = (t.rate.baud() * 1.0) as usize;
        let mut d = vec![0u8; pad];
        let mut on = vec![false; pad];
        // A preamble (+3 +3 −3 −3 …), then the frames.
        let pre: Vec<u8> = [1u8, 1, 3, 3].repeat(6);
        let frames = dibits(&t.frames());
        on.extend(std::iter::repeat_n(true, pre.len() + frames.len()));
        d.extend(pre);
        d.extend(frames);
        d.extend(vec![0u8; pad * 3 / 2]);
        on.extend(std::iter::repeat_n(false, pad * 3 / 2));
        let mut ph = 0.0;
        let mut iq = modulate(&d, Some(&on), t.rate, fs, offset, 0.3, &mut ph);
        let mut seed = 99;
        add_noise(&mut iq, 0.01, &mut seed);
        iq
    }

    fn conv_tx(rate: Rate) -> Tx {
        Tx { rate, ran: 3, head: CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: if rate == Rate::N96 { 2 } else { 0 }, source: 501, destination: 3100 }, cipher: 0, superframes: 10, rf: 2, outbound: true }
    }

    #[test]
    fn engine_records_a_conventional_nxdn_call() {
        let (fs, center, freq) = (1_200_000.0, 460_000_000.0, 460_056_250.0);
        for rate in [Rate::N48, Rate::N96] {
            let iq = keyed(&conv_tx(rate), fs, freq - center);
            let cfg = EngineConfig {
                sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
                conventional: vec![ConvChannel::new(freq, ConvMode::Nxdn(rate))],
                ..Default::default()
            };
            let (done, _) = run_engine(cfg, &iq);
            assert_eq!(done.len(), 1, "{rate:?}: {:?}", done.iter().map(|k| &k.call).collect::<Vec<_>>());
            let c = &done[0].call;
            assert_eq!((c.talkgroup, c.ran, c.nxdn), (3100, Some(3), Some(rate)), "{rate:?}");
            assert_eq!(c.sources.iter().map(|s| s.src).collect::<Vec<_>>(), [501]);
            let secs = done[0].audio.len() as f64 / 8000.0;
            // 10 superframes: 160 codewords at 4800, 80 at 9600 (every other frame is FACCH1).
            let want = if rate == Rate::N48 { 3.2 } else { 1.6 };
            assert!((secs - want).abs() < 0.15, "{rate:?}: {secs:.2} s of audio");
            assert!(done[0].json.contains("\"ran\":3"), "{}", done[0].json);
        }
    }

    #[test]
    fn rows_pick_nxdn_calls_by_ran_and_group() {
        let (fs, center, freq) = (1_200_000.0, 460_000_000.0, 460_056_250.0);
        let iq = keyed(&conv_tx(Rate::N48), fs, freq - center);
        let mode = ConvMode::Nxdn(Rate::N48);
        let row = |code: &str, tg: u32, name: &str| ConvChannel {
            access: Access::parse(mode, code).unwrap(),
            talkgroup: tg,
            info: Some(Talkgroup { number: tg, alpha_tag: name.into(), ..Default::default() }),
            ..ConvChannel::new(freq, mode)
        };
        let calls = |rows: Vec<ConvChannel>| {
            let cfg = EngineConfig { sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }], conventional: rows, ..Default::default() };
            let (done, skipped) = run_engine(cfg, &iq);
            (done.into_iter().map(|k| (k.call.talkgroup, k.call.talkgroup_info.map(|t| t.alpha_tag))).collect::<Vec<_>>(), skipped)
        };
        assert_eq!(calls(vec![row("RAN 3", 1, "Ops"), row("RAN 4", 2, "Other")]).0, [(3100, Some("Ops".to_string()))]);
        assert_eq!(calls(vec![row("RAN 3 TG 3100", 1, "Exact"), row("RAN 3", 2, "Site")]).0, [(3100, Some("Exact".to_string()))]);
        let (none, skipped) = calls(vec![row("RAN 9", 1, "A")]);
        assert!(none.is_empty());
        assert_eq!(skipped, ["RAN 3 TG 3100"]);
    }

    #[test]
    fn nxdn_access_codes_parse() {
        let m = ConvMode::Nxdn(Rate::N96);
        assert_eq!(Access::parse(m, "RAN 5"), Ok(Some(Access::Nxdn { ran: Some(5), tg: None })));
        assert_eq!(Access::parse(m, "5"), Ok(Some(Access::Nxdn { ran: Some(5), tg: None })));
        assert_eq!(Access::parse(m, "ran5 tg 201"), Ok(Some(Access::Nxdn { ran: Some(5), tg: Some(201) })));
        assert_eq!(Access::parse(m, "RAN 0"), Ok(None));
        assert!(Access::parse(m, "RAN 64").is_err() && Access::parse(m, "CC 1").is_err());
        assert_eq!(Access::parse(m, "RAN 5 TG 201").unwrap().unwrap().to_string(), "RAN 5 TG 201");
    }

    /// Two carriers' frames side by side in one band: (offset Hz, frames).
    fn band(carriers: &[(f64, Vec<[u8; FRAME_SYMBOLS]>)], rate: Rate, fs: f64) -> Vec<Complex32> {
        let mut sum: Vec<Complex32> = Vec::new();
        for (i, (off, frames)) in carriers.iter().enumerate() {
            let mut x = 11u32 + i as u32;
            let mut d: Vec<u8> = (0..600)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect();
            d.extend(dibits(frames));
            d.extend((0..(rate.baud() * 1.5) as usize).map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 30) as u8
            }));
            let mut ph = 0.0;
            let iq = modulate(&d, None, rate, fs, *off, 0.3, &mut ph);
            if sum.len() < iq.len() {
                sum.resize(iq.len(), Complex32::default());
            }
            for (a, b) in sum.iter_mut().zip(&iq) {
                *a += b;
            }
        }
        let mut seed = 5;
        add_noise(&mut sum, 0.01, &mut seed);
        sum
    }

    #[test]
    fn engine_follows_a_type_c_grant_and_records_the_call() {
        use super::super::channel::Sr as S;
        use super::super::layer3::build as l3;
        use crate::nxdn::trunking::NxdnConfig;
        use crate::trunk::{Protocol, SystemConfig};
        let (fs, center) = (240_000.0, 451_043_750.0);
        let (cc, vc) = (451_018_750.0, 451_068_750.0);
        let h = CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 77, destination: 3001 };
        let t = Tx { rate: Rate::N48, ran: 5, head: h, cipher: 0, superframes: 10, rf: 1, outbound: true };
        let voice = t.frames();
        // Control: site info, the grant to channel 7, then its duplicates while the call lasts.
        let mut ctrl = vec![cac_frame(S { structure: 2, ran: 5 }, &l3::site_info(0x123 << 12 | 0x045, 0x0200, 0, 0, [1, 0]))];
        ctrl.push(cac_frame(S { structure: 2, ran: 5 }, &l3::vcall_assgn(false, &h, 4, 7)));
        while ctrl.len() < voice.len() + 6 {
            ctrl.push(cac_frame(S { structure: 2, ran: 5 }, &l3::vcall_assgn(true, &h, 4, 7)));
        }
        let idle = voice_frame(0x39, Sr { structure: 0, ran: 5 }, 0, &[Half::Facch1(l3::idle()), Half::Facch1(l3::idle())]);
        let mut vframes = vec![idle; 3];
        vframes.extend(voice);
        let iq = band(&[(cc - center, ctrl), (vc - center, vframes)], Rate::N48, fs);
        let mut nc = NxdnConfig::default();
        nc.channel_table.insert(7, vc as u64);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            systems: vec![SystemConfig { short_name: "nx".into(), control_channels: vec![cc], protocol: Protocol::Nxdn(nc), ..Default::default() }],
            ..Default::default()
        };
        let (done, _) = run_engine(cfg, &iq);
        assert_eq!(done.len(), 1, "{:?}", done.iter().map(|k| &k.call).collect::<Vec<_>>());
        let c = &done[0].call;
        assert_eq!((c.talkgroup, c.freq_hz, c.nxdn, c.ran), (3001, vc as u64, Some(Rate::N48), Some(5)));
        assert_eq!(c.sources.first().map(|s| s.src), Some(77));
        let secs = done[0].audio.len() as f64 / 8000.0;
        assert!((secs - 3.2).abs() < 0.2, "{secs:.2} s of audio");
    }

    #[test]
    fn engine_records_a_type_d_call_on_a_watched_repeater() {
        use crate::nxdn::channel::build as cb;
        use crate::nxdn::trunking::{Kind, NxdnConfig};
        use crate::trunk::{Protocol, SystemConfig};
        let (fs, center) = (240_000.0, 452_037_500.0);
        let (r1, r2) = (452_012_500.0, 452_062_500.0);
        let tg: u16 = 3 << 11 | 101;
        let src: u16 = 3 << 11 | 1234;
        // Repeater 1 idle; repeater 2 carries the call (SCCH INFO1–4, FACCH1 VCALL first).
        let idle_frame = |k: usize| {
            let data = if k % 4 == 3 { cb::scch_id(0, 2046, false) } else { 0 };
            type_d_frame(0x7f, 3 - (k % 4) as u8, data, None)
        };
        let r1_frames: Vec<_> = (0..60).map(idle_frame).collect();
        let mut r2_frames: Vec<_> = (0..4).map(idle_frame).collect();
        for n in 0..40usize {
            let k = n % 4;
            let data = match k {
                0 => cb::scch_info1(0, 0, 0, 0, 0),
                1 | 3 => cb::scch_id(2, tg, true),
                _ => cb::scch_id(0, src, false),
            };
            r2_frames.push(type_d_frame(0x77, 3 - k as u8, data, Some(n)));
        }
        let iq = band(&[(r1 - center, r1_frames), (r2 - center, r2_frames)], Rate::N48, fs);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs, auto_tune: false, guard_hz: crate::trunk::DEFAULT_GUARD_HZ }],
            systems: vec![SystemConfig {
                short_name: "idas".into(),
                control_channels: vec![r1, r2],
                protocol: Protocol::Nxdn(NxdnConfig { kind: Kind::TypeD, ..Default::default() }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (done, _) = run_engine(cfg, &iq);
        assert_eq!(done.len(), 1, "{:?}", done.iter().map(|k| &k.call).collect::<Vec<_>>());
        let c = &done[0].call;
        assert_eq!((c.talkgroup, c.freq_hz), (tg as u32, r2 as u64));
        assert!(c.sources.iter().any(|s| s.src == src as u32));
        let secs = done[0].audio.len() as f64 / 8000.0;
        assert!((secs - 3.2).abs() < 0.3, "{secs:.2} s of audio");
    }

    /// A Type-D frame: LICH, SCCH (structure, data), and voice frames `n` (or none).
    fn type_d_frame(lich: u8, structure: u8, data: u32, voice: Option<usize>) -> [u8; FRAME_SYMBOLS] {
        let mut body = with_lich(lich);
        put_bits(&mut body, SACCH_AT, &build::scch(structure, false, data));
        if let Some(n) = voice {
            for h in 0..2 {
                for k in 0..2 {
                    let d = encode_vcw(&ambe(4 * n + 2 * h + k));
                    let at = (HALF_AT[h] + 72 * k) / 2;
                    body[at..at + 36].copy_from_slice(&d);
                }
            }
        }
        frame_dibits(&body)
    }
}

/// Weak-signal curves on synthesized voice (`cargo test -p trunk-core
/// nxdn_snr_curve -- --ignored --nocapture`): the share of voice codewords
/// decoded as sent, per SNR (signal over noise in the channel's own
/// bandwidth: 6.25 / 12.5 kHz), with and without multi-symbol detection.
#[cfg(test)]
mod snr_curve {
    use super::*;
    use crate::ambe::decode_vcw;
    use crate::dsp::c4fm::{C4fm, C4fmOptions};
    use crate::dsp::Receiver;
    use crate::nxdn::frame::{Body, Framer};
    use crate::nxdn::layer3::CALL_CONFERENCE;

    #[test]
    #[ignore]
    fn nxdn_snr_curve() {
        let fs = 48_000.0;
        for rate in [Rate::N48, Rate::N96] {
            let bw = if rate == Rate::N48 { 6250.0 } else { 12_500.0 };
            let t = Tx { rate, ran: 1, head: CallHead { cc_option: 0, call_type: CALL_CONFERENCE, option: 0, source: 1, destination: 2 }, cipher: 0, superframes: 40, rf: 2, outbound: true };
            let mut x = 3u32;
            let mut d: Vec<u8> = (0..1000)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect();
            d.extend(dibits(&t.frames()));
            d.extend(d[..2000].to_vec());
            let mut ph = 0.0;
            let clean = modulate(&d, None, rate, fs, 0.0, 1.0, &mut ph);
            let mut line = format!("{rate:?}:");
            for snr_db in [20.0, 15.0, 12.0, 10.0, 8.0, 6.0, 4.0] {
                // Noise power in the channel bandwidth = 1 / snr: per sample over fs.
                let sigma = ((fs / bw) / 10f64.powf(snr_db / 10.0) / 2.0).sqrt() as f32;
                for msd in [true, false] {
                    let mut iq = clean.clone();
                    let mut seed = 77;
                    add_noise(&mut iq, sigma, &mut seed);
                    // The channel filter a real head would have.
                    let mut chz = crate::dsp::Channelizer::new(fs, 24_000.0, 0.1);
                    let (head, _, _) = chz.add_head(0.0, rate.cutoff_hz(), 0.0);
                    let mut o = C4fmOptions::nxdn(rate.baud());
                    if !msd {
                        o.msd = None;
                    }
                    let mut rx = C4fm::with_options(chz.output_rate(), o);
                    let mut syms = Vec::new();
                    let mut buf = Vec::new();
                    for c in iq.chunks(4096) {
                        let u8s: Vec<u8> = c.iter().flat_map(|v| [((v.re * 40.0 + 127.5).clamp(0.0, 255.0)) as u8, ((v.im * 40.0 + 127.5).clamp(0.0, 255.0)) as u8]).collect();
                        let mut off = 0;
                        while off < u8s.len() {
                            let (used, ran) = chz.feed_u8(&u8s[off..]);
                            off += used;
                            if ran {
                                buf.clear();
                                buf.extend_from_slice(chz.output(head).unwrap());
                                rx.push(&buf, &mut syms);
                            } else if used == 0 {
                                break;
                            }
                        }
                    }
                    let mut fr = Framer::new();
                    let mut frames = Vec::new();
                    for s in &syms {
                        fr.push(s, &mut frames);
                    }
                    // Voice frames as sent: every superframe's 16 codewords at 4800, 8 at 9600 (k odd: FACCH1).
                    let sent: std::collections::HashSet<[u8; 49]> = (0..40 * 16).map(ambe).collect();
                    let mut ok = 0;
                    for f in &frames {
                        let Some((l, _)) = f.lich() else { continue };
                        if let Body::Voice { facch: [false, false], idle: false, .. } = l.body() {
                            for k in 0..4 {
                                let (dd, r) = f.voice_frame(k);
                                ok += sent.contains(&decode_vcw(&dd, Some(&r)).bits) as usize;
                            }
                        }
                    }
                    let total = if rate == Rate::N48 { 40 * 16 } else { 40 * 8 };
                    let _ = write!(line, " {snr_db:>2} dB {}: {:5.1} %", if msd { "msd" } else { "   " }, 100.0 * ok as f64 / total as f64);
                }
            }
            eprintln!("{line}");
        }
    }
    use std::fmt::Write as _;
}
