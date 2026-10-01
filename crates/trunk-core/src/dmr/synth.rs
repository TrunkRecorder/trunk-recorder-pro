//! A synthetic DMR repeater for tests: idle bursts on both slots, then a
//! voice transmission on one (LC headers, voice superframes with embedded
//! LC, terminators), 4FSK-modulated.

use num_complex::Complex32;

use super::burst::tests::{cach_dibits, sync_dibits};
use super::burst::{SyncKind, BURST_DIBITS};
use super::fec::{bptc196_encode, crc_ccitt, embedded_lc_encode, golay20_encode, qr16_encode, rs129_parity, unpack, MASK_CSBK, MASK_TERMINATOR_LC, MASK_VOICE_LC_HEADER};
use super::slot::{DT_CSBK, DT_IDLE, DT_TERMINATOR_LC, DT_VOICE_LC_HEADER};
use crate::p25::fec::{golay23_encode, golay24_encode};
use crate::p25::phase2::{ambe_pn23, VCW_MAP};

pub struct Tx {
    pub cc: u8,
    pub slot: u8,
    pub talkgroup: u32,
    pub source: u32,
    /// Voice superframes (360 ms each).
    pub superframes: usize,
    /// Idle units (30 ms) before and after.
    pub idle_before: usize,
    pub idle_after: usize,
    /// CSBKs (10 bytes, before CRC) sent in place of the other slot's idle bursts.
    pub csbks: Vec<[u8; 10]>,
    /// A mobile (simplex / talkaround): MS syncs, no CACH, nothing on the air
    /// between its bursts and before / after the transmission.
    pub mobile: bool,
}

fn lc_bytes(tg: u32, src: u32) -> [u8; 9] {
    let (t, s) = (tg.to_be_bytes(), src.to_be_bytes());
    [0, 0, 0, t[1], t[2], t[3], s[1], s[2], s[3]]
}

fn bits_to_dibits(bits: &[u8]) -> Vec<u8> {
    bits.chunks(2).map(|c| c[0] << 1 | c[1]).collect()
}

/// A data burst: 196 info bits, slot type, data sync (BS, or MS for a mobile).
fn data_burst(cc: u8, dt: u8, info: &[u8; 196]) -> [u8; BURST_DIBITS] {
    data_burst_sync(cc, dt, info, SyncKind::BsData)
}

fn data_burst_sync(cc: u8, dt: u8, info: &[u8; 196], sync: SyncKind) -> [u8; BURST_DIBITS] {
    let st = golay20_encode((cc as u32) << 4 | dt as u32);
    let mut bits = [0u8; 264];
    bits[..98].copy_from_slice(&info[..98]);
    for i in 0..10 {
        bits[98 + i] = (st >> (19 - i) & 1) as u8;
        bits[156 + i] = (st >> (9 - i) & 1) as u8;
    }
    bits[166..].copy_from_slice(&info[98..]);
    let mut d: [u8; BURST_DIBITS] = bits_to_dibits(&bits).try_into().unwrap();
    d[54..78].copy_from_slice(&sync_dibits(sync));
    d
}

fn lc_burst(cc: u8, dt: u8, lc: &[u8; 9], sync: SyncKind) -> [u8; BURST_DIBITS] {
    let mask = if dt == DT_VOICE_LC_HEADER { MASK_VOICE_LC_HEADER } else { MASK_TERMINATOR_LC };
    let p = rs129_parity(lc);
    let mut bytes = lc.to_vec();
    bytes.extend([p[0] ^ (mask >> 16) as u8, p[1] ^ (mask >> 8) as u8, p[2] ^ mask as u8]);
    let bits: [u8; 96] = unpack(&bytes).try_into().unwrap();
    data_burst_sync(cc, dt, &bptc196_encode(&bits), sync)
}

pub fn csbk_burst(cc: u8, csbk: &[u8; 10]) -> [u8; BURST_DIBITS] {
    let mut bits = unpack(csbk);
    let crc = crc_ccitt(&bits) ^ MASK_CSBK;
    bits.extend((0..16).rev().map(|i| (crc >> i & 1) as u8));
    data_burst(cc, DT_CSBK, &bptc196_encode(&bits.try_into().unwrap()))
}

/// 49 AMBE bits → the 36 dibits of a 72-bit codeword (the inverse of decode_vcw).
fn ambe_dibits(u: &[u8; 49]) -> [u8; 36] {
    let word = |a: usize, n: usize| u[a..a + n].iter().fold(0u32, |v, &b| v << 1 | b as u32);
    let (u0, u1, u2, u3) = (word(0, 12), word(12, 12), word(24, 11), word(35, 14));
    let c = [golay24_encode(u0), golay23_encode(u1) ^ ambe_pn23(u0), u2, u3];
    let mut bits = [0u8; 72];
    for (k, &(w, i)) in VCW_MAP.iter().enumerate() {
        bits[k] = (c[w as usize] >> i & 1) as u8;
    }
    bits_to_dibits(&bits).try_into().unwrap()
}

/// Voice burst `pos` (0 = A … 5 = F) of a superframe, carrying `frames`.
fn voice_burst(cc: u8, pos: usize, frames: &[[u8; 36]; 3], emb_lc: &[u8; 128], vsync: SyncKind) -> [u8; BURST_DIBITS] {
    let mut d = [0u8; BURST_DIBITS];
    d[..36].copy_from_slice(&frames[0]);
    d[36..54].copy_from_slice(&frames[1][..18]);
    d[78..96].copy_from_slice(&frames[1][18..]);
    d[96..].copy_from_slice(&frames[2]);
    if pos == 0 {
        d[54..78].copy_from_slice(&sync_dibits(vsync));
        return d;
    }
    let lcss = [0, 1, 3, 3, 2, 0][pos];
    let emb = qr16_encode((cc as u32) << 3 | lcss);
    let mut mid = [0u8; 48];
    for i in 0..8 {
        mid[i] = (emb >> (15 - i) & 1) as u8;
        mid[40 + i] = (emb >> (7 - i) & 1) as u8;
    }
    if (1..=4).contains(&pos) {
        mid[8..40].copy_from_slice(&emb_lc[(pos - 1) * 32..pos * 32]);
    }
    d[54..78].copy_from_slice(&bits_to_dibits(&mid));
    d
}

/// The transmission's dibits, both slots interleaved, CACH before each burst.
pub fn dibits(tx: &Tx) -> Vec<u8> {
    dibits_on(tx).0
}

/// … and, per dibit, whether anything is on the air (a mobile is off between
/// its bursts and before / after the transmission).
pub fn dibits_on(tx: &Tx) -> (Vec<u8>, Vec<bool>) {
    let (vsync, dsync) = if tx.mobile { (SyncKind::MsVoice, SyncKind::MsData) } else { (SyncKind::BsVoice, SyncKind::BsData) };
    let lc = lc_bytes(tx.talkgroup, tx.source);
    let emb_lc = embedded_lc_encode(&unpack(&lc).try_into().unwrap());
    let idle = data_burst(tx.cc, DT_IDLE, &[0; 196]);
    // The voice slot's bursts, in order.
    // (None: nothing sent — a mobile has no idle bursts.)
    let gap = if tx.mobile { None } else { Some(idle) };
    let mut ours: Vec<Option<[u8; BURST_DIBITS]>> = vec![gap; tx.idle_before];
    ours.extend([Some(lc_burst(tx.cc, DT_VOICE_LC_HEADER, &lc, dsync)); 3]);
    let mut x = 0x1234_5678u32;
    for _ in 0..tx.superframes {
        for pos in 0..6 {
            let frames: [[u8; 36]; 3] = std::array::from_fn(|_| {
                let u: [u8; 49] = std::array::from_fn(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 31) as u8
                });
                ambe_dibits(&u)
            });
            ours.push(Some(voice_burst(tx.cc, pos, &frames, &emb_lc, vsync)));
        }
    }
    ours.extend([Some(lc_burst(tx.cc, DT_TERMINATOR_LC, &lc, dsync)); 3]);
    ours.extend(vec![gap; tx.idle_after]);
    let (mut out, mut on) = (Vec::new(), Vec::new());
    let mut csbks = tx.csbks.iter().cycle();
    let mut x = 0x2468_ace1u32;
    let mut junk = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 30) as u8
            })
            .collect()
    };
    for b in &ours {
        for s in 0..2u8 {
            if tx.mobile {
                // Guard time, then our burst or nothing.
                out.extend(junk(12));
                on.extend([false; 12]);
                match (s == tx.slot, b) {
                    (true, Some(b)) => {
                        out.extend(b);
                        on.extend([true; BURST_DIBITS]);
                    }
                    _ => {
                        out.extend(junk(BURST_DIBITS));
                        on.extend([false; BURST_DIBITS]);
                    }
                }
                continue;
            }
            out.extend(cach_dibits(s));
            if s == tx.slot {
                out.extend(b.unwrap());
            } else {
                let other = csbks.next().map_or(idle, |c| csbk_burst(tx.cc, c));
                out.extend(other);
            }
            on.extend([true; 144]);
        }
    }
    (out, on)
}

/// 4FSK at `fs`, `offset_hz` from the centre: ±648 / ±1944 Hz, 4800 baud.
pub fn modulate(dibits: &[u8], fs: f64, offset_hz: f64, amp: f32, phase: &mut f64) -> Vec<Complex32> {
    modulate_on(dibits, None, fs, offset_hz, amp, phase)
}

/// … with the carrier off where `on` says so.
pub fn modulate_on(dibits: &[u8], on: Option<&[bool]>, fs: f64, offset_hz: f64, amp: f32, phase: &mut f64) -> Vec<Complex32> {
    let n = (dibits.len() as f64 / 4800.0 * fs) as usize;
    (0..n)
        .map(|i| {
            let d = dibits[((i as f64 / fs * 4800.0) as usize).min(dibits.len() - 1)];
            let dev = match d {
                0b01 => 3.0,
                0b00 => 1.0,
                0b10 => -1.0,
                _ => -3.0,
            } * 648.0;
            *phase += 2.0 * std::f64::consts::PI * (offset_hz + dev) / fs;
            let k = ((i as f64 / fs * 4800.0) as usize).min(dibits.len() - 1);
            let a = if on.is_none_or(|o| o[k]) { amp } else { 0.0 };
            Complex32::from_polar(a, *phase as f32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmr::{Channel, Framer, SlotEvent};
    use crate::dsp::c4fm::C4fm;
    use crate::dsp::Receiver;
    use crate::trunk::conventional::{ConvChannel, ConvMode};
    use crate::trunk::{Engine, EngineConfig, Event, SourceConfig, SystemConfig};

    fn tx() -> Tx {
        Tx { cc: 5, slot: 1, talkgroup: 4321, source: 98765, superframes: 6, idle_before: 20, idle_after: 20, csbks: vec![], mobile: false }
    }

    #[test]
    fn a_synthetic_transmission_decodes() {
        let d = dibits(&tx());
        let mut ph = 0.0;
        let iq = modulate(&d, 24_000.0, 0.0, 1.0, &mut ph);
        let mut rx = C4fm::dmr(24_000.0);
        let mut syms = Vec::new();
        for (k, c) in iq.chunks(2400).enumerate() {
            rx.push(c, &mut syms);
            if k % 2 == 0 {
                let l = rx.levels();
                eprintln!("t {:.1} levels {:?} sep {:.1}", k as f64 * 0.1, l.map(|v| v.round()), rx.separation());
            }
        }
        let (mut f, mut ch) = (Framer::default(), Channel::default());
        let mut bursts = Vec::new();
        for s in &syms {
            f.push(s, &mut bursts);
        }
        let mut ev = Vec::new();
        for b in &bursts {
            ch.burst(b, &mut ev);
        }
        let errs: Vec<u32> = ev.iter().filter_map(|(s, e)| if let (1, SlotEvent::Voice { frame, .. }) = (*s, e) { Some(frame.errs) } else { None }).collect();
        assert!(errs.iter().filter(|&&e| e > 0).count() < 20, "{errs:?}");
        let voice = ev.iter().filter(|(s, e)| *s == 1 && matches!(e, SlotEvent::Voice { frame, .. } if frame.errs <= 1)).count();
        assert_eq!(voice, 6 * 6 * 3);
        let lcs: Vec<_> = ev.iter().filter_map(|(s, e)| if let SlotEvent::Lc { lc, from } = e { Some((*s, *from, lc.target(), lc.source())) } else { None }).collect();
        assert!(lcs.iter().all(|&(s, _, tg, src)| s == 1 && tg == 4321 && src == 98765), "{lcs:?}");
        assert_eq!(lcs.iter().filter(|l| l.1 == crate::dmr::LcFrom::Embedded).count(), 6);
        assert_eq!(ch.slots[1].color_code, Some(5));
    }

    fn run(cfg: EngineConfig, iq: &[Complex32]) -> Vec<crate::trunk::Concluded> {
        let mut e = Engine::new(cfg).unwrap();
        for c in iq.chunks(8192) {
            e.push_iq(0, c);
        }
        e.finish();
        e.drain_events().into_iter().filter_map(|ev| if let Event::Concluded(k) = ev { Some(k) } else { None }).collect()
    }

    fn wideband(fs: f64, offset: f64) -> Vec<Complex32> {
        wideband_of(&tx(), fs, offset)
    }

    fn wideband_of(t: &Tx, fs: f64, offset: f64) -> Vec<Complex32> {
        let (d, on) = dibits_on(t);
        let mut ph = 0.0;
        let mut iq = modulate_on(&d, Some(&on), fs, offset, 0.3, &mut ph);
        let mut x = 0x9e37_79b9u32;
        for v in iq.iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let a = (x >> 8) as f32 / 16_777_216.0 - 0.5;
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = (x >> 8) as f32 / 16_777_216.0 - 0.5;
            *v += Complex32::new(0.02 * a, 0.02 * b);
        }
        iq
    }

    #[test]
    fn engine_records_a_conventional_dmr_call_on_its_slot() {
        // The squelch's noise floor is per 1/64 of the band: wide enough here that the carrier isn't it.
        let (fs, center, freq) = (1_200_000.0, 460_000_000.0, 460_050_000.0);
        let iq = wideband(fs, freq - center);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs }],
            conventional: vec![ConvChannel::new(freq, ConvMode::Dmr)],
            ..Default::default()
        };
        let done = run(cfg, &iq);
        assert_eq!(done.len(), 1, "{:?}", done.iter().map(|k| &k.call).collect::<Vec<_>>());
        let c = &done[0].call;
        assert_eq!((c.talkgroup, c.tdma_slot, c.color_code), (4321, 1, Some(5)));
        assert_eq!(c.sources.iter().map(|s| s.src).collect::<Vec<_>>(), [98765]);
        let secs = done[0].audio.len() as f64 / 8000.0;
        assert!((secs - 6.0 * 0.36).abs() < 0.2, "{secs:.2} s of audio");
        assert!(done[0].base_name.ends_with("_460050000.1") && done[0].json.contains("\"color_code\":5"), "{} {}", done[0].base_name, done[0].json);
    }

    #[test]
    fn rows_pick_dmr_calls_by_colour_code_slot_and_talkgroup() {
        use crate::trunk::{Access, Talkgroup};
        // The air: CC 5, slot 2 (tdma_slot 1), TG 4321.
        let (fs, center, freq) = (1_200_000.0, 460_000_000.0, 460_050_000.0);
        let iq = wideband(fs, freq - center);
        let row = |code: &str, tg: u32, name: &str| ConvChannel {
            access: Access::parse(ConvMode::Dmr, code).unwrap(),
            talkgroup: tg,
            info: Some(Talkgroup { number: tg, alpha_tag: name.into(), ..Default::default() }),
            ..ConvChannel::new(freq, ConvMode::Dmr)
        };
        let calls = |rows: Vec<ConvChannel>| {
            let cfg = EngineConfig { sources: vec![SourceConfig { center_hz: center, rate_hz: fs }], conventional: rows, ..Default::default() };
            run(cfg, &iq).into_iter().map(|k| (k.call.talkgroup, k.call.talkgroup_info.map(|t| t.alpha_tag))).collect::<Vec<_>>()
        };
        // The most specific row that fits wins; the talkgroup stays the air's.
        let rows = vec![row("CC5", 1, "Repeater"), row("CC5 TS2 TG4321", 4321, "Ops"), row("CC5 TS1", 2, "Slot 1"), row("", 3, "Other")];
        assert_eq!(calls(rows), [(4321, Some("Ops".to_string()))]);
        // Only the colour code: the row's names for the air's talkgroup.
        assert_eq!(calls(vec![row("CC 5", 1, "Repeater")]), [(4321, Some("Repeater".to_string()))]);
        // Nothing fits, no row without a code: not recorded, reported once.
        let rows = vec![row("CC3", 1, "A"), row("CC5 TS1", 2, "B"), row("CC5 TG 999", 3, "C")];
        assert!(calls(rows.clone()).is_empty());
        let cfg = EngineConfig { sources: vec![SourceConfig { center_hz: center, rate_hz: fs }], conventional: rows, ..Default::default() };
        let mut e = Engine::new(cfg).unwrap();
        for c in iq.chunks(8192) {
            e.push_iq(0, c);
        }
        e.finish();
        let skipped: Vec<String> = e.drain_events().into_iter().filter_map(|ev| if let Event::ConvSkipped { code, .. } = ev { Some(code) } else { None }).collect();
        assert_eq!(skipped, ["CC 5 TS 2 TG 4321"]);
        // … with one: it takes the rest.
        assert_eq!(calls(vec![row("CC3", 1, "A"), row("", 3, "Other")]), [(4321, Some("Other".to_string()))]);
    }

    #[test]
    fn a_mobile_in_simplex_is_recorded() {
        // Bursts on one slot only, nothing on the air between them: talkaround.
        let (fs, center, freq) = (1_200_000.0, 460_000_000.0, 460_050_000.0);
        let t = Tx { mobile: true, slot: 0, ..tx() };
        let iq = wideband_of(&t, fs, freq - center);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs }],
            conventional: vec![ConvChannel::new(freq, ConvMode::Dmr)],
            ..Default::default()
        };
        let done = run(cfg, &iq);
        assert_eq!(done.len(), 1, "{:?}", done.iter().map(|k| (&k.call.talkgroup, k.audio.len())).collect::<Vec<_>>());
        let c = &done[0].call;
        assert_eq!((c.talkgroup, c.color_code), (4321, Some(5)));
        assert_eq!(c.sources.iter().map(|s| s.src).collect::<Vec<_>>(), [98765]);
        let secs = done[0].audio.len() as f64 / 8000.0;
        assert!((secs - 6.0 * 0.36).abs() < 0.2, "{secs:.2} s of audio");
    }

    #[test]
    fn engine_records_a_trunked_dmr_call_found_by_its_link_control() {
        let (fs, center, freq) = (240_000.0, 460_000_000.0, 460_050_000.0);
        let iq = wideband(fs, freq - center);
        let cfg = EngineConfig {
            sources: vec![SourceConfig { center_hz: center, rate_hz: fs }],
            systems: vec![SystemConfig { short_name: "cap".into(), control_channels: vec![freq], dmr: Some(Default::default()), ..Default::default() }],
            ..Default::default()
        };
        let done = run(cfg, &iq);
        assert_eq!(done.len(), 1);
        let c = &done[0].call;
        assert_eq!((c.system, c.talkgroup, c.tdma_slot, c.color_code, c.freq_hz), (0, 4321, 1, Some(5), 460_050_000));
        // The voice head opens at the LC with a second of pre-roll: the whole transmission.
        let secs = done[0].audio.len() as f64 / 8000.0;
        assert!((secs - 6.0 * 0.36).abs() < 0.2, "{secs:.2} s of audio");
    }
}
