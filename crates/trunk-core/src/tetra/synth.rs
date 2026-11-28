//! A synthetic TETRA downlink, for tests: bursts from their blocks' bits, and their IQ.

use super::burst::{NTS1, NTS2, SLOT_SYMBOLS, STS};
use super::fec::{self, BSCH, SCH_F, SCH_HD};
use super::mac::{Sync, Sysinfo, Time};
use super::speech::{self, Frame};
use crate::dsp::filters::rrc;
use crate::Complex32;

fn dibits(bits: &[u8]) -> Vec<u8> {
    bits.chunks(2).map(|b| b[0] << 1 | b[1]).collect()
}

fn push(v: &mut Vec<u8>, x: u32, n: usize) {
    v.extend((0..n).rev().map(|k| (x >> k & 1) as u8));
}

/// The SYNC PDU's 60 bits.
pub fn sync_bits(s: &Sync) -> Vec<u8> {
    let mut v = Vec::new();
    for (x, n) in [(1, 4), (s.colour as u32, 6), (s.time.slot as u32 - 1, 2), (s.time.frame as u32, 5), (s.time.multiframe as u32, 6), (0, 8), (s.mcc as u32, 10), (s.mnc as u32, 14), (0, 5)] {
        push(&mut v, x, n);
    }
    v
}

/// The SYSINFO PDU's 124 bits.
pub fn sysinfo_bits(s: &Sysinfo) -> Vec<u8> {
    let mut v = Vec::new();
    for (x, n) in [(0b1000, 4), (s.main_carrier as u32, 12), (s.band as u32, 4), (s.offset as u32, 2), (0, 4), (s.secondary as u32, 2), (0, 32), (0, 22), (s.location_area as u32, 14), (s.subscriber_class as u32, 16), (s.services as u32, 12)] {
        push(&mut v, x, n);
    }
    v
}

/// What a slot carries.
pub enum Slot<'a> {
    /// SYNC, and a half slot's 124 bits.
    Sync(Sync, &'a [u8]),
    /// A full slot's 268 bits.
    Full(&'a [u8]),
    /// Two half slots' 124 bits each.
    Halves(&'a [u8], &'a [u8]),
    /// Two speech frames.
    Speech(&'a [Frame; 2]),
}

/// A slot's 255 dibits, its blocks scrambled with `seed` and its AACH saying `access_assign`.
pub fn burst(slot: &Slot, access_assign: u16, seed: u32) -> Vec<u8> {
    let aach = dibits(&fec::aach_encode(access_assign, seed));
    let mut d: Vec<u8> = vec![0, 3, 1, 2, 2, 1, 0];
    let half = |b: &[u8]| dibits(&fec::encode(&SCH_HD, b, seed));
    let (b1, training, b2): (Vec<u8>, &[u8], Vec<u8>) = match slot {
        Slot::Sync(s, b) => {
            d.extend([3; 4].iter().chain(&[0; 32]).chain(&[3; 4]));
            d.extend(dibits(&fec::encode(&BSCH, &sync_bits(s), fec::BSCH_SEED)));
            d.extend(STS);
            d.extend(&aach);
            d.extend(half(b));
            d.extend([0, 1, 3, 2, 2, 1]);
            return d;
        }
        Slot::Full(b) => {
            let all = dibits(&fec::encode(&SCH_F, b, seed));
            (all[..108].to_vec(), &NTS1, all[108..].to_vec())
        }
        Slot::Halves(a, b) => (half(a), &NTS2, half(b)),
        Slot::Speech(f) => {
            let all = dibits(&speech::encode_full(f, seed));
            (all[..108].to_vec(), &NTS1, all[108..].to_vec())
        }
    };
    d.extend(b1);
    d.extend(&aach[..7]);
    d.extend(training);
    d.extend(&aach[7..]);
    d.extend(b2);
    d.extend([0, 1, 3, 2, 2, 1]);
    debug_assert_eq!(d.len(), SLOT_SYMBOLS);
    d
}

/// The cell [`site_downlink`] is of.
pub const CELL: Sync = Sync { mcc: 310, mnc: 42, colour: 17, time: Time { slot: 1, frame: 1, multiframe: 1 } };
pub const INFO: Sysinfo = Sysinfo { main_carrier: 2342, band: 8, offset: 3, secondary: 0, location_area: 401, subscriber_class: 0xffff, services: 0x5 };

/// D-SETUP of call 291 to group 10091 with a channel on slot 2, in a half slot.
fn setup() -> Vec<u8> {
    setup_of(291, 10_091, 2)
}

pub fn setup_of(id: u32, group: u32, slot: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for (x, n) in [(0, 2), (1, 1), (0, 1), (0, 2), (0, 1), (15, 6), (1, 3), (group, 24), (0, 2), (1, 1), (0, 2), (1 << (4 - slot), 4), (3, 2), (0, 2), (2342, 12), (0, 1), (1, 2)] {
        push(&mut v, x, n);
    }
    for (x, n) in [(2, 4), (2, 3), (7, 5), (id, 14), (5, 4), (0, 2), (4, 8), (3, 2), (0, 1), (3, 4), (0, 1)] {
        push(&mut v, x, n);
    }
    v.push(1);
    v.resize(124, 0);
    v
}

/// `n` slots of a cell's main carrier: a SYNC burst on frame 18's slot 1, and D-SETUP in the first half of every other.
pub fn site_downlink(n: u64) -> Vec<u8> {
    let seed = fec::seed(CELL.mcc, CELL.mnc, CELL.colour);
    let (info, call) = (sysinfo_bits(&INFO), setup());
    let null = { let mut v = Vec::new(); for (x, n) in [(0, 2), (1, 1), (0, 4), (2, 6), (0, 3)] { push(&mut v, x, n); } v.push(1); v.resize(124, 0); v };
    // (From frame 18 of a multiframe.)
    (0..n)
        .flat_map(|k| {
            let time = Time { slot: 1, frame: 18, multiframe: 60 }.after(k);
            let slot = if time.frame == 18 && time.slot == 1 { Slot::Sync(Sync { time, ..CELL }, &info) } else { Slot::Halves(&call, &null) };
            burst(&slot, 9 << 6 | 9, seed)
        })
        .collect()
}

/// π/4-DQPSK at `rate` (RRC α 0.35), `offset_hz` off centre, with Gaussian noise of `sigma` a component.
pub fn modulate(dibits: &[u8], rate: f64, offset_hz: f64, sigma: f32) -> Vec<Complex32> {
    use std::f64::consts::PI;
    let sps = rate / super::BAUD;
    let mut ph = 0.0f64;
    let points: Vec<Complex32> = dibits
        .iter()
        .map(|&d| {
            ph += [PI / 4.0, 3.0 * PI / 4.0, -PI / 4.0, -3.0 * PI / 4.0][d as usize];
            Complex32::new(ph.cos() as f32, ph.sin() as f32)
        })
        .collect();
    let mut x = 0x1234_5678_9abc_def1u64;
    let mut uniform = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..(dibits.len() as f64 * sps) as usize)
        .map(|n| {
            let t = n as f64 / sps;
            let k = t.floor() as i64;
            let s: Complex32 = (k - 7..=k + 8).filter_map(|j| points.get(usize::try_from(j).ok()?).map(|p| p * rrc(t - j as f64, 0.35) as f32)).sum();
            let (r, a) = ((-2.0 * (1.0 - uniform()).ln()).sqrt(), 2.0 * PI * uniform());
            let noise = Complex32::new((r * a.cos()) as f32, (r * a.sin()) as f32) * sigma;
            s * Complex32::from_polar(1.0, (2.0 * PI * offset_hz * n as f64 / rate % (2.0 * PI)) as f32) + noise
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::cell::{Carrier, Event};
    use super::super::cmce::{CallKind, Pdu};
    use super::*;


    fn downlink(n: u64) -> Vec<u8> {
        site_downlink(n)
    }

    fn receive(iq: &[Complex32], rate: f64) -> (Carrier, Vec<Event>) {
        let (mut c, mut out) = (Carrier::new(rate), Vec::new());
        for chunk in iq.chunks((rate * 0.005) as usize) {
            c.push(chunk, &mut out);
        }
        (c, out)
    }

    #[test]
    fn a_downlink_is_received() {
        // (Offsets to where the carrier estimate needs the sync's help, and rates heads come out at.)
        for (rate, offset) in [(75_000.0, 0.0), (75_000.0, 700.0), (75_000.0, -2100.0), (62_500.0, 1500.0), (72_000.0, 3000.0)] {
            let iq = modulate(&downlink(220), rate, offset, 0.1);
            let (c, ev) = receive(&iq, rate);
            let id = c.cell.identity().unwrap_or_else(|| panic!("{rate} {offset}: no cell; {} slots, {:?}", c.downlink.framer.good, c.cell.counts));
            assert_eq!((id.mcc, id.mnc, id.colour), (310, 42, 17));
            assert_eq!(c.cell.sysinfo(), Some(INFO));
            let calls: Vec<_> = ev.iter().filter_map(|e| match e { Event::Signalling { sdu, pdu: Some(Pdu::Call(call)), .. } => Some((sdu, call)), _ => None }).collect();
            // Two multiframes from the second SYNC (the first may go to acquisition).
            assert!(calls.len() > 100, "{rate} {offset}: {} calls, {:?}", calls.len(), c.cell.counts);
            let (sdu, call) = calls[0];
            assert_eq!((sdu.to.ssi, call.kind, call.id), (Some(10_091), CallKind::Setup, Some(291)));
            assert_eq!(sdu.allocation.unwrap().hz(&INFO), Some(858_562_500.0));
            assert_eq!(c.cell.counts.bsch_bad, 0);
            let got = c.offset_hz().unwrap() as f64;
            assert!((got - offset).abs() < 100.0, "{rate}: {offset} read as {got}");
        }
    }

    /// A SYSINFO read once that isn't the cell's (a block garbled past its CRC, as on a weak site:
    /// "main carrier 143.3938 MHz") changes nothing; one read twice running is the cell's new one.
    #[test]
    fn a_sysinfo_read_once_is_not_believed() {
        let seed = fec::seed(CELL.mcc, CELL.mnc, CELL.colour);
        let bad = Sysinfo { main_carrier: 1234, band: 1, ..INFO };
        let new = Sysinfo { main_carrier: 2380, ..INFO };
        let null = { let mut v = Vec::new(); for (x, n) in [(0, 2), (1, 1), (0, 4), (2, 6), (0, 3)] { push(&mut v, x, n); } v.push(1); v.resize(124, 0); v };
        // Each multiframe's SYNC carries one SYSINFO, in turn these.
        let said = [INFO, INFO, INFO, bad, INFO, INFO, new, new, new];
        let bits: Vec<Vec<u8>> = said.iter().map(sysinfo_bits).collect();
        let d: Vec<u8> = (0..72 * said.len() as u64 + 8)
            .flat_map(|k| {
                let time = Time { slot: 1, frame: 18, multiframe: 60 }.after(k);
                let slot = if time.frame == 18 && time.slot == 1 { Slot::Sync(Sync { time, ..CELL }, &bits[(k / 72) as usize % said.len()]) } else { Slot::Halves(&null, &null) };
                burst(&slot, 9 << 6 | 9, seed)
            })
            .collect();
        let rate = 75_000.0;
        let (c, ev) = receive(&modulate(&d, rate, 0.0, 0.1), rate);
        let told: Vec<_> = ev.iter().filter_map(|e| match e { Event::Sysinfo(s) => Some(*s), _ => None }).collect();
        assert_eq!(told, [INFO, new]);
        assert_eq!(c.cell.sysinfo(), Some(new));
    }

    /// A SYNC read once that names another cell (garbled past its CRC: "MCC 666 MNC 7859" on a
    /// weak site) changes nothing, the cell's SYSINFO kept; another cell read twice running is the cell.
    #[test]
    fn a_cell_read_once_is_not_believed() {
        let bad = Sync { mcc: 666, mnc: 7859, ..CELL };
        let new = Sync { mnc: 43, ..CELL };
        let null = { let mut v = Vec::new(); for (x, n) in [(0, 2), (1, 1), (0, 4), (2, 6), (0, 3)] { push(&mut v, x, n); } v.push(1); v.resize(124, 0); v };
        let info = sysinfo_bits(&INFO);
        // Each multiframe's SYNC names, in turn, these.
        let said = [CELL, CELL, CELL, bad, CELL, CELL, new, new, new];
        let d: Vec<u8> = (0..72 * said.len() as u64 + 8)
            .flat_map(|k| {
                let time = Time { slot: 1, frame: 18, multiframe: 60 }.after(k);
                let cell = said[(k / 72) as usize % said.len()];
                let slot = if time.frame == 18 && time.slot == 1 { Slot::Sync(Sync { time, ..cell }, &info) } else { Slot::Halves(&null, &null) };
                burst(&slot, 9 << 6 | 9, fec::seed(cell.mcc, cell.mnc, cell.colour))
            })
            .collect();
        let rate = 75_000.0;
        let (c, ev) = receive(&modulate(&d, rate, 0.0, 0.1), rate);
        let cells: Vec<_> = ev.iter().filter_map(|e| match e { Event::Cell(s) => Some((s.mcc, s.mnc)), _ => None }).collect();
        assert_eq!(cells, [(310, 42), (310, 43)]);
        let id = c.cell.identity().unwrap();
        assert_eq!((id.mcc, id.mnc), (310, 43));
        // The cell's SYSINFO wasn't dropped by the garbled one: told once, and again for the new cell.
        assert_eq!(ev.iter().filter(|e| matches!(e, Event::Sysinfo(_))).count(), 2);
    }

    #[test]
    fn noise_is_no_cell() {
        let iq = modulate(&[0; 1], 72_000.0, 0.0, 1.0).into_iter().cycle().take(72_000).collect::<Vec<_>>();
        let (c, ev) = receive(&iq, 72_000.0);
        assert!(ev.is_empty() && c.cell.identity().is_none());
        // And real noise, 10 s of it.
        let mut x = 99u64;
        let mut g = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0 };
        let iq: Vec<Complex32> = (0..720_000).map(|_| Complex32::new(g() + g() + g(), g() + g() + g())).collect();
        let (c, ev) = receive(&iq, 72_000.0);
        assert!(ev.is_empty() && c.downlink.framer.good < 5, "{ev:?} {}", c.downlink.framer.good);
    }
}
