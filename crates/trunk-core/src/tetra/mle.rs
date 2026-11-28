//! The MLE's network broadcast (EN 300 392-2 clause 18.4.1.4.1): the neighbouring cells.

use super::mac::{carrier_hz, Reader, Sync, Sysinfo};

/// D-NWRK-BROADCAST.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broadcast {
    pub neighbours: Vec<Neighbour>,
}

/// A neighbouring cell (table 18.64). What it doesn't say is the serving cell's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Neighbour {
    pub main_carrier: u16,
    /// Band, and offset as [`carrier_hz`] takes it.
    pub band_offset: Option<(u8, u8)>,
    pub mcc: Option<u16>,
    pub mnc: Option<u16>,
    pub location_area: Option<u16>,
}

impl Broadcast {
    /// From after its PDU type.
    pub fn parse(mut r: Reader) -> Option<Broadcast> {
        r.skip(16 + 2)?; // reselection parameters, cell load
        let mut neighbours = Vec::new();
        if r.flag()? {
            optional(&mut r, 48)?; // network time
            let n = if r.flag()? { r.read(3)? } else { 0 };
            for _ in 0..n {
                neighbours.push(neighbour(&mut r)?);
            }
        }
        Some(Broadcast { neighbours })
    }
}

/// A type-2 element of up to 32 bits (a longer one is read past).
fn optional(r: &mut Reader, n: usize) -> Option<Option<u32>> {
    Some(match r.flag()? {
        true if n > 32 => r.skip(n).map(|_| None)?,
        true => Some(r.read(n)?),
        false => None,
    })
}

fn neighbour(r: &mut Reader) -> Option<Neighbour> {
    r.skip(5 + 2 + 1 + 2)?; // cell identifier, reselection types, synchronised, load
    let mut c = Neighbour { main_carrier: r.read(12)? as u16, ..Default::default() };
    if r.flag()? {
        // Band, offset, duplex spacing, reverse operation. (A live cell sends all ones: nothing.)
        c.band_offset = optional(r, 10)?.filter(|&e| e != 0x3ff).map(|e| ((e >> 6) as u8, (e >> 4 & 3) as u8));
        c.mcc = optional(r, 10)?.map(|v| v as u16);
        c.mnc = optional(r, 14)?.map(|v| v as u16);
        c.location_area = optional(r, 14)?.map(|v| v as u16);
        // Power, access, subscriber class, services, timeshare, frame offset.
        for n in [3, 4, 16, 12, 5, 6] {
            optional(r, n)?;
        }
    }
    Some(c)
}

impl Neighbour {
    /// Its main carrier, announced by a cell of `info`.
    pub fn hz(&self, info: &Sysinfo) -> Option<f64> {
        let (band, offset) = self.band_offset.unwrap_or((info.band, info.offset));
        carrier_hz(band, self.main_carrier, offset)
    }

    /// Its country, network and location area, announced by the cell `id`.
    pub fn address(&self, id: &Sync, info: &Sysinfo) -> (u16, u16, u16) {
        (self.mcc.unwrap_or(id.mcc), self.mnc.unwrap_or(id.mnc), self.location_area.unwrap_or(info.location_area))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::mm::tests::pack;
    use super::super::synth::{CELL, INFO};
    use super::*;

    /// `(main carrier, extension, MNC, location area)`.
    type Neighbour = (u32, Option<u32>, Option<u32>, Option<u32>);

    /// After its PDU type, laid out as a live site's: the time, then neighbours of
    /// `(main carrier, extension, MNC, location area)`.
    pub fn broadcast(cells: &[Neighbour]) -> Vec<u8> {
        let mut p = pack(&[(0xa5a5, 16), (0, 2), (1, 1), (1, 1), (0x1234, 24), (0x5678, 24), (1, 1), (cells.len() as u32, 3)]);
        for (k, &(carrier, ext, mnc, area)) in cells.iter().enumerate() {
            p.extend(pack(&[(k as u32 + 1, 5), (2, 2), (0, 1), (0, 2), (carrier, 12), (1, 1)]));
            for (v, n) in [(ext, 10), (None, 10), (mnc, 14), (area, 14), (None, 3), (None, 4), (None, 16), (Some(0xd75), 12), (None, 5), (None, 6)] {
                p.push(v.is_some() as u8);
                p.extend(v.map(|v| pack(&[(v, n)])).unwrap_or_default());
            }
        }
        p
    }

    #[test]
    fn neighbours_are_read() {
        let cells = [(2310, None, None, Some(402)), (2205, Some(0x3ff), None, Some(403)), (400, Some(4 << 6 | 1 << 4), Some(7), None)];
        let b = Broadcast::parse(Reader::new(&broadcast(&cells))).unwrap();
        let got: Vec<_> = b.neighbours.iter().map(|n| (n.hz(&INFO).unwrap(), n.address(&CELL, &INFO))).collect();
        // The cell's own band and offset, unless it says others; all ones says none.
        assert_eq!(got, [(857_762_500.0, (310, 42, 402)), (855_137_500.0, (310, 42, 403)), (410_006_250.0, (310, 7, INFO.location_area))]);
        // None announced.
        assert!(Broadcast::parse(Reader::new(&pack(&[(0, 18), (0, 1)]))).unwrap().neighbours.is_empty());
    }

    /// Cut short anywhere, nothing; any bits at all, no panic.
    #[test]
    fn cut_short() {
        let p = broadcast(&[(2310, None, None, Some(402)), (2205, Some(1), Some(2), None)]);
        for n in 0..p.len() {
            assert!(Broadcast::parse(Reader::new(&p[..n])).is_none(), "{n}");
        }
        let mut x = 7u32;
        for _ in 0..2000 {
            let bits: Vec<u8> = (0..260).map(|_| { x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (x >> 24 & 1) as u8 }).collect();
            Broadcast::parse(Reader::new(&bits));
        }
    }
}
