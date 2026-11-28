//! TETRA downlink MAC PDUs (EN 300 392-2 clause 21.4): the SYNC PDU, SYSINFO, the AACH's
//! ACCESS-ASSIGN, MAC-RESOURCE with its channel allocation, and fragments. Bits are one a byte.

/// Reads fields off a run of bits; None past the end.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    bits: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bits: &'a [u8]) -> Self {
        Reader { bits, pos: 0 }
    }
    pub fn read(&mut self, n: usize) -> Option<u32> {
        debug_assert!(n <= 32);
        let b = self.bits.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(b.iter().fold(0, |w, &x| w << 1 | x as u32))
    }
    pub fn flag(&mut self) -> Option<bool> {
        Some(self.read(1)? != 0)
    }
    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.bits.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(())
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.bits[self.pos..]
    }
}

/// The SYNC PDU with its D-MLE-SYNC (tables 21.76 and 18.17): which cell, and where in its time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sync {
    pub mcc: u16,
    pub mnc: u16,
    pub colour: u8,
    pub time: Time,
}

/// A slot's place: timeslot 1–4, frame 1–18, multiframe 1–60.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Time {
    pub slot: u8,
    pub frame: u8,
    pub multiframe: u8,
}

impl Time {
    const SLOTS: u64 = 4 * 18 * 60;

    /// `n` slots on.
    pub fn after(self, n: u64) -> Time {
        let at = ((self.multiframe as u64 - 1) * 18 + self.frame as u64 - 1) * 4 + self.slot as u64 - 1;
        let k = (at + n) % Self::SLOTS;
        Time { slot: (k % 4) as u8 + 1, frame: (k / 4 % 18) as u8 + 1, multiframe: (k / 72) as u8 + 1 }
    }
}

/// Seconds a slot lasts.
pub const SLOT_S: f64 = super::burst::SLOT_SYMBOLS as f64 / super::BAUD;

/// A cell, and the time of one of its slots: its carriers share it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellTime {
    pub mcc: u16,
    pub mnc: u16,
    pub colour: u8,
    /// Air time at which the slot at `time` began, s.
    pub at_s: f64,
    pub time: Time,
}

impl CellTime {
    /// The slot that began at `t`, to the nearest.
    pub fn slot_at(&self, t: f64) -> Time {
        let n = ((t - self.at_s) / SLOT_S).round() as i64;
        self.time.after(n.rem_euclid(Time::SLOTS as i64) as u64)
    }

    pub fn seed(&self) -> u32 {
        super::fec::seed(self.mcc, self.mnc, self.colour)
    }
}

impl Sync {
    pub fn parse(bits: &[u8]) -> Option<Sync> {
        let mut r = Reader::new(bits);
        r.skip(4)?; // system code
        let colour = r.read(6)? as u8;
        let time = Time { slot: r.read(2)? as u8 + 1, frame: r.read(5)? as u8, multiframe: r.read(6)? as u8 };
        r.skip(8)?; // sharing mode, reserved frames, U-plane DTX, frame 18 extension, reserved
        let (mcc, mnc) = (r.read(10)? as u16, r.read(14)? as u16);
        ((1..=18).contains(&time.frame) && (1..=60).contains(&time.multiframe)).then_some(Sync { mcc, mnc, colour, time })
    }
}

/// Downlink frequency of a carrier number, Hz: the band in 100 MHz, 25 kHz carriers, and an offset
/// of 0, +6.25, −6.25 or +12.5 kHz. None for band 0 (a cell that names no main carrier).
pub fn carrier_hz(band: u8, carrier: u16, offset: u8) -> Option<f64> {
    (band != 0).then(|| band as f64 * 100e6 + carrier as f64 * 25e3 + [0.0, 6250.0, -6250.0, 12_500.0][offset as usize & 3])
}

/// SYSINFO with its D-MLE-SYSINFO (tables 21.65 and 18.18).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sysinfo {
    pub main_carrier: u16,
    pub band: u8,
    pub offset: u8,
    /// Secondary control channels on the main carrier (in slots 2, 3, 4).
    pub secondary: u8,
    pub location_area: u16,
    pub subscriber_class: u16,
    /// BS service details: bit 1 is air interface encryption.
    pub services: u16,
}

impl Sysinfo {
    /// A BNCH block, its MAC PDU type and broadcast type included.
    pub fn parse(bits: &[u8]) -> Option<Sysinfo> {
        let mut r = Reader::new(bits);
        if r.read(4)? != 0b1000 {
            return None;
        }
        let (main_carrier, band, offset) = (r.read(12)? as u16, r.read(4)? as u8, r.read(2)? as u8);
        r.skip(4)?; // duplex spacing, reverse operation
        let secondary = r.read(2)? as u8;
        // Power and access parameters, the hyperframe or cipher key number, the optional field.
        r.skip(3 + 4 + 4 + 4 + 1 + 16 + 2 + 20)?;
        Some(Sysinfo { main_carrier, band, offset, secondary, location_area: r.read(14)? as u16, subscriber_class: r.read(16)? as u16, services: r.read(12)? as u16 })
    }
    /// The main carrier, where the control channel is.
    pub fn main_hz(&self) -> Option<f64> {
        carrier_hz(self.band, self.main_carrier, self.offset)
    }
    /// Whether the cell offers air interface encryption (table 18.19: bit 11 of 12, from the first).
    pub fn encryption(&self) -> bool {
        self.services & 0x002 != 0
    }
}

/// What a downlink slot carries, by its ACCESS-ASSIGN (table 21.82; frames 1–17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    Unallocated,
    Control,
    /// Traffic, by its usage marker.
    Traffic(u8),
}

impl Usage {
    pub fn of(access_assign: u16) -> Usage {
        let field1 = (access_assign >> 6 & 0x3f) as u8;
        match (access_assign >> 12 & 3, field1) {
            (0, _) | (_, 1..=3) => Usage::Control,
            (_, 0) => Usage::Unallocated,
            (_, m) => Usage::Traffic(m),
        }
    }
}

/// Who a MAC-RESOURCE is to (table 21.55).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Address {
    /// A group's or radio's SSI, its number encrypted when the PDU is.
    pub ssi: Option<u32>,
    pub event_label: Option<u16>,
    pub usage_marker: Option<u8>,
}

/// A channel allocation (table 21.87).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation {
    /// 0 replace, 1 additional, 2 quit and go, 3 replace and carry signalling.
    pub kind: u8,
    /// Slots 1–4 as bits 3–0; none: the common control channel's.
    pub slots: u8,
    /// Bit 0 downlink, bit 1 uplink; 0 is an augmented allocation.
    pub up_down: u8,
    /// Another cell's channel.
    pub cell_change: bool,
    pub carrier: u16,
    /// The band and offset, when they aren't the cell's own.
    pub band_offset: Option<(u8, u8)>,
}

impl Allocation {
    fn read(r: &mut Reader) -> Option<Allocation> {
        let (kind, slots, up_down) = (r.read(2)? as u8, r.read(4)? as u8, r.read(2)? as u8);
        r.skip(1)?; // CLCH permission
        let cell_change = r.flag()?;
        let carrier = r.read(12)? as u16;
        let band_offset = if r.flag()? {
            let v = (r.read(4)? as u8, r.read(2)? as u8);
            r.skip(4)?; // duplex spacing, reverse operation
            Some(v)
        } else {
            None
        };
        if r.read(2)? == 0 {
            r.skip(2)?; // frame 18 monitoring pattern
        }
        if up_down == 0 {
            // QAM and the like: the augmented part.
            r.skip(2 + 3 + 3 + 3 + 3 + 4 + 5)?;
            if r.read(2)? == 1 {
                r.skip(11)?;
            }
            r.skip(4)?;
            for _ in 0..2 {
                if r.flag()? {
                    r.skip(16)?;
                }
            }
            if r.flag()? {
                return None;
            }
        }
        Some(Allocation { kind, slots, up_down, cell_change, carrier, band_offset })
    }

    /// The channel's frequency, on a cell with `cell`'s band and offset.
    pub fn hz(&self, cell: &Sysinfo) -> Option<f64> {
        let (band, offset) = self.band_offset.unwrap_or((cell.band, cell.offset));
        carrier_hz(band, self.carrier, offset)
    }

    /// Timeslots allocated (1–4).
    pub fn timeslots(&self) -> impl Iterator<Item = u8> + '_ {
        (1..=4).filter(|s| self.slots >> (4 - s) & 1 != 0)
    }
}

/// A whole TM-SDU, or the notice of one that is encrypted.
#[derive(Clone, Debug, PartialEq)]
pub struct Sdu {
    pub to: Address,
    /// Air interface encryption: 0 none, 2 and 3 encrypted. Then there is no allocation or body to read.
    pub encryption: u8,
    pub allocation: Option<Allocation>,
    pub body: Vec<u8>,
}

/// Length indications that aren't octets.
const SECOND_HALF_STOLEN: u32 = 62;
const FRAGMENT: u32 = 63;
/// The longest TM-SDU kept, bits.
const MAX_SDU: usize = 4096;

/// The content of `bits` without its fill bits: the last 1, and the zeros after it.
fn unfilled(bits: &[u8], fill: bool) -> Option<&[u8]> {
    if !fill {
        return Some(bits);
    }
    bits.iter().rposition(|&b| b != 0).map(|k| &bits[..k])
}

/// One timeslot's signalling: puts fragments together.
#[derive(Clone, Debug, Default)]
pub struct Assembler {
    partial: Option<Sdu>,
}

impl Assembler {
    /// A block is missing: what was under way can't be finished.
    pub fn miss(&mut self) {
        self.partial = None;
    }

    /// Every TM-SDU that ends in a signalling block (type-1 bits, CRC checked).
    pub fn block(&mut self, bits: &[u8], out: &mut Vec<Sdu>) {
        let mut at = 0;
        while at < bits.len() {
            match self.pdu(&bits[at..], out) {
                Some(n) if n > 0 => at += n,
                _ => break,
            }
        }
    }

    /// One PDU: its length, or None when no more can be read in this block.
    fn pdu(&mut self, bits: &[u8], out: &mut Vec<Sdu>) -> Option<usize> {
        let mut r = Reader::new(bits);
        match r.read(2)? {
            0 => self.resource(bits, r, out),
            1 if r.flag()? => self.end(bits, r, out),
            1 => {
                let fill = r.flag()?;
                self.append(unfilled(r.rest(), fill));
                None
            }
            _ => None,
        }
    }

    fn append(&mut self, bits: Option<&[u8]>) {
        let fits = self.partial.as_ref().zip(bits).is_some_and(|(p, b)| p.body.len() + b.len() <= MAX_SDU);
        match &mut self.partial {
            Some(p) if fits => p.body.extend_from_slice(bits.unwrap_or_default()),
            _ => self.partial = None,
        }
    }

    fn resource(&mut self, bits: &[u8], mut r: Reader, out: &mut Vec<Sdu>) -> Option<usize> {
        let fill = r.flag()?;
        r.skip(1)?; // position of grant
        let encryption = r.read(2)? as u8;
        r.skip(1)?; // random access
        let length = r.read(6)?;
        let kind = r.read(3)?;
        if kind == 0 {
            return None; // the null PDU: nothing follows
        }
        let len = match length {
            SECOND_HALF_STOLEN | FRAGMENT => bits.len(),
            1..=61 => (length as usize * 8).min(bits.len()),
            _ => return None,
        };
        let pos = r.pos;
        let mut r = Reader { bits: &bits[..len], pos };
        let header = (|| {
            let mut to = Address::default();
            if kind != 2 {
                to.ssi = Some(r.read(24)?);
            }
            if matches!(kind, 2 | 5 | 7) {
                to.event_label = Some(r.read(10)? as u16);
            }
            if kind == 6 {
                to.usage_marker = Some(r.read(6)? as u8);
            }
            if r.flag()? {
                r.skip(4)?; // power control
            }
            if r.flag()? {
                r.skip(8)?; // slot granting
            }
            Some((to, r.flag()?))
        })();
        let (to, allocated) = header?;
        let mut sdu = Sdu { to, encryption, allocation: None, body: Vec::new() };
        if encryption != 0 {
            self.partial = None;
            out.push(sdu);
            return Some(len);
        }
        if allocated {
            match Allocation::read(&mut r) {
                Some(a) => sdu.allocation = Some(a),
                None => return Some(len),
            }
        }
        if let Some(body) = unfilled(r.rest(), fill) {
            sdu.body = body.to_vec();
            if length == FRAGMENT {
                // Its allocation comes in the MAC-END.
                sdu.allocation = None;
                self.partial = Some(sdu);
            } else {
                out.push(sdu);
            }
        }
        Some(len)
    }

    fn end(&mut self, bits: &[u8], mut r: Reader, out: &mut Vec<Sdu>) -> Option<usize> {
        let fill = r.flag()?;
        r.skip(1)?; // position of grant
        let len = match r.read(6)? {
            n @ 1..=61 => (n as usize * 8).min(bits.len()),
            _ => return None,
        };
        let pos = r.pos;
        let mut r = Reader { bits: &bits[..len], pos };
        let allocation = (|| {
            if r.flag()? {
                r.skip(8)?; // slot granting
            }
            match r.flag()? {
                true => Allocation::read(&mut r).map(Some),
                false => Some(None),
            }
        })();
        let Some(allocation) = allocation else {
            self.partial = None;
            return Some(len);
        };
        self.append(unfilled(r.rest(), fill));
        if let Some(mut p) = self.partial.take() {
            p.allocation = allocation;
            out.push(p);
        }
        Some(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(fields: &[(u32, usize)]) -> Vec<u8> {
        fields.iter().flat_map(|&(v, n)| (0..n).rev().map(move |k| (v >> k & 1) as u8)).collect()
    }

    #[test]
    fn time_counts_slots() {
        let t = Time { slot: 4, frame: 18, multiframe: 60 };
        assert_eq!(t.after(1), Time { slot: 1, frame: 1, multiframe: 1 });
        assert_eq!(Time { slot: 2, frame: 18, multiframe: 3 }.after(4), Time { slot: 2, frame: 1, multiframe: 4 });
        assert_eq!(t.after(0), t);
    }

    #[test]
    fn carriers() {
        assert_eq!(carrier_hz(4, 2710, 0), Some(467_750_000.0));
        // The 12.5 kHz offset.
        assert_eq!(carrier_hz(8, 2342, 3), Some(858_562_500.0));
        assert_eq!(carrier_hz(0, 0, 0), None);
    }

    #[test]
    fn usage_markers() {
        assert_eq!(Usage::of(3 << 12 | 51 << 6 | 51), Usage::Traffic(51));
        assert_eq!(Usage::of(9 << 6 | 9), Usage::Control);
        assert_eq!(Usage::of(2 << 12 | 1 << 6 | 9), Usage::Control);
        assert_eq!(Usage::of(1 << 12 | 9), Usage::Unallocated);
    }

    /// Fill bits are the last 1 and the zeros after it: cut only where the header says they are there.
    #[test]
    fn fill_bits_go() {
        assert_eq!(unfilled(&[1, 0, 1, 0, 0], true), Some(&[1, 0][..]));
        assert_eq!(unfilled(&[1, 0, 1, 0, 0], false), Some(&[1, 0, 1, 0, 0][..]));
        assert_eq!(unfilled(&[0, 0], true), None);
    }

    /// A MAC-RESOURCE to a group with a usage marker and a channel, then another to an SSI, then the null PDU.
    #[test]
    fn resources_in_a_block() {
        let first = pack(&[(0, 2), (1, 1), (0, 1), (0, 2), (0, 1), (11, 6), (6, 3), (100_091, 24), (33, 6), (0, 1), (0, 1), (1, 1),
            (0, 2), (0b0100, 4), (3, 2), (0, 1), (0, 1), (2342, 12), (0, 1), (1, 2), (0b10110, 5), (1, 1), (0, 7)]);
        let n = first.len();
        let mut block = first;
        block.extend(vec![0; 88 - n]);
        block.extend(pack(&[(0, 2), (0, 1), (0, 1), (2, 2), (0, 1), (6, 6), (1, 3), (77, 24), (0, 8)]));
        block.extend(pack(&[(0, 2), (1, 1), (0, 1), (0, 2), (0, 1), (2, 6), (0, 3), (0, 32)]));
        let mut out = Vec::new();
        Assembler::default().block(&block, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to, Address { ssi: Some(100_091), usage_marker: Some(33), event_label: None });
        let a = out[0].allocation.unwrap();
        assert_eq!((a.carrier, a.timeslots().collect::<Vec<_>>(), a.up_down), (2342, vec![2], 3));
        assert_eq!(out[0].body, [1, 0, 1, 1, 0]);
        // The encrypted one: who, not what.
        assert_eq!((out[1].encryption, out[1].to.ssi, out[1].body.len()), (2, Some(77), 0));
    }

    #[test]
    fn fragments_join_and_a_miss_drops_them() {
        let start = pack(&[(0, 2), (0, 1), (0, 1), (0, 2), (0, 1), (63, 6), (1, 3), (5, 24), (0, 3), (0xabc, 12)]);
        let frag = pack(&[(1, 2), (0, 1), (1, 1), (0xde, 8), (0b1000, 4)]);
        let end = pack(&[(1, 2), (1, 1), (1, 1), (0, 1), (3, 6), (0, 2), (0xf, 4), (1, 1), (0, 6)]);
        for lost in [false, true] {
            let (mut a, mut out) = (Assembler::default(), Vec::new());
            a.block(&start, &mut out);
            a.block(&frag, &mut out);
            if lost {
                a.miss();
            }
            a.block(&end, &mut out);
            assert_eq!(out.len(), !lost as usize);
            if !lost {
                assert_eq!(out[0].body, pack(&[(0xabcdef, 24)]));
                assert_eq!(out[0].to.ssi, Some(5));
            }
        }
    }

    /// Whatever the bits, nothing panics.
    #[test]
    fn noise_is_harmless() {
        let mut x = 7u64;
        let mut a = Assembler::default();
        for _ in 0..20_000 {
            let bits: Vec<u8> = (0..268).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x & 1) as u8 }).collect();
            let mut out = Vec::new();
            a.block(&bits, &mut out);
            Sync::parse(&bits);
            Sysinfo::parse(&bits);
            for s in &out {
                crate::tetra::cmce::parse(&s.body);
            }
        }
    }
}
