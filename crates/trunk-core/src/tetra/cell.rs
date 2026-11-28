//! One TETRA carrier, from its bursts: the cell it is of, its time, and what its signalling says.

use super::burst::{Burst, Framer, Kind, SLOT_SYMBOLS};
use super::cmce::{self, Pdu};
use super::fec::{self, BSCH, SCH_F, SCH_HD};
use super::speech;
use super::mac::{Assembler, Sdu, Sync, Sysinfo, Time, Usage};
use crate::dsp::cqpsk::Cqpsk;
use crate::dsp::{Receiver, Symbol};
use crate::Complex32;

/// What a carrier told.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The cell's identity: a first, or another than the last once it's been read twice
    /// running (a weak site read "MCC 666 MNC 7859" once, a block garbled past its CRC).
    Cell(Sync),
    /// A SYSINFO other than the last, once it's been read twice running: a block garbled past
    /// its CRC can't move the cell's main carrier (one weak site read 143.39 MHz).
    Sysinfo(Sysinfo),
    /// A slot's AACH said it carries traffic, by its usage marker.
    Traffic { time: Time, marker: u8 },
    /// A traffic slot's two speech frames, 30 ms each; None: lost, stolen for signalling, or not
    /// speech ([`Cell::speech`] asks for these).
    Speech { time: Time, marker: u8, frames: [Option<speech::Frame>; 2] },
    /// A MAC-RESOURCE, and the PDU in it when it is one that is read.
    Signalling { time: Time, sdu: Sdu, pdu: Option<Pdu> },
}

/// Blocks decoded and not, by what they were.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub bsch: u64,
    pub bsch_bad: u64,
    /// Signalling blocks on slots that aren't traffic.
    pub blocks: u64,
    pub blocks_bad: u64,
    pub aach: u64,
    pub aach_bad: u64,
    /// Slots whose AACH said traffic.
    pub traffic: u64,
}

/// The decoder. Nothing but the BSCH can be read until one names the cell (its scrambling).
#[derive(Default)]
pub struct Cell {
    sync: Option<Sync>,
    /// A SYNC naming another cell than [`Self::sync`], read once: taken when the next says the same.
    other: Option<Sync>,
    /// The time of the burst that started at a symbol count, and its channel sample.
    clock: Option<(u64, Time, f64)>,
    sysinfo: Option<Sysinfo>,
    /// A SYSINFO other than [`Self::sysinfo`], read once: taken when the next says the same.
    pending: Option<Sysinfo>,
    slots: [Assembler; 4],
    /// Decode traffic slots' speech frames.
    pub speech: bool,
    pub counts: Counts,
}

impl Cell {
    pub fn identity(&self) -> Option<Sync> {
        self.sync
    }
    pub fn sysinfo(&self) -> Option<Sysinfo> {
        self.sysinfo
    }

    /// The time last told, and the channel sample its slot began at.
    pub fn clock(&self) -> Option<(Time, f64)> {
        self.clock.map(|(_, t, sample)| (t, sample))
    }

    pub fn time(&self, b: &Burst) -> Option<Time> {
        let (at, t, _) = self.clock?;
        let n = SLOT_SYMBOLS as u64;
        Some(t.after((b.start.checked_sub(at)? + n / 2) / n))
    }

    pub fn push(&mut self, b: &Burst, out: &mut Vec<Event>) {
        if b.kind == Kind::Sync {
            match fec::decode(&BSCH, b.bsch(), fec::BSCH_SEED).as_deref().and_then(Sync::parse) {
                Some(s) => {
                    self.counts.bsch += 1;
                    let id = |x: &Sync| (x.mcc, x.mnc, x.colour);
                    let changed = self.sync.is_some_and(|o| id(&o) != id(&s));
                    if changed && self.other.map(|o| id(&o)) != Some(id(&s)) {
                        // (Its time too may be garbled: the clock stays as the cell's own last set it.)
                        self.other = Some(s);
                    } else {
                        self.other = None;
                        self.clock = Some((b.start, s.time, b.sample));
                        if self.sync.is_none_or(|o| id(&o) != id(&s)) {
                            self.sysinfo = None;
                            self.pending = None;
                            self.slots = Default::default();
                            out.push(Event::Cell(s));
                        }
                        self.sync = Some(s);
                    }
                }
                None => self.counts.bsch_bad += 1,
            }
        }
        let (Some(id), Some(time)) = (self.sync, self.time(b)) else { return };
        let seed = fec::seed(id.mcc, id.mnc, id.colour);
        let usage = fec::aach(&b.aach(), seed).map(Usage::of);
        match usage {
            Some(_) => self.counts.aach += 1,
            None => self.counts.aach_bad += 1,
        }
        // Frame 18 carries signalling only, and its AACH says something else.
        let marker = match usage {
            Some(Usage::Traffic(m)) if time.frame != 18 => Some(m),
            _ => None,
        };
        let traffic = marker.is_some();
        self.counts.traffic += traffic as u64;
        if let Some(marker) = marker {
            out.push(Event::Traffic { time, marker });
            if self.speech {
                out.push(Event::Speech { time, marker, frames: speech::frames(b, seed).map(|f| f.map(|f| f.0)) });
            }
        }
        let full;
        let blocks: &[(&fec::Coding, &[f32])] = match b.kind {
            Kind::Sync => &[(&SCH_HD, b.block2())],
            // Speech under a traffic marker.
            Kind::Full if traffic => &[],
            Kind::Full => {
                full = b.full();
                &[(&SCH_F, &full)]
            }
            Kind::Halves => &[(&SCH_HD, b.block1()), (&SCH_HD, b.block2())],
        };
        for (coding, soft) in blocks {
            let Some(bits) = fec::decode(coding, soft, seed) else {
                // (A stolen half's other half may be speech.)
                if !traffic {
                    self.counts.blocks_bad += 1;
                    self.slots[time.slot as usize - 1].miss();
                }
                continue;
            };
            self.counts.blocks += 1;
            if bits[..2] == [1, 0] {
                if let Some(s) = Sysinfo::parse(&bits) {
                    if self.sysinfo == Some(s) {
                        self.pending = None;
                    } else if self.pending.replace(s) == Some(s) {
                        self.sysinfo = Some(s);
                        self.pending = None;
                        out.push(Event::Sysinfo(s));
                    }
                }
                continue;
            }
            let mut sdus = Vec::new();
            self.slots[time.slot as usize - 1].block(&bits, &mut sdus);
            out.extend(sdus.into_iter().map(|sdu| Event::Signalling { time, pdu: cmce::parse(&sdu.body), sdu }));
        }
    }
}

/// A carrier's receiver and framer: channel IQ in, bursts out.
pub struct Downlink {
    rx: Cqpsk,
    pub framer: Framer,
    symbols: Vec<Symbol>,
    bursts: Vec<Burst>,
    /// Symbols received, and how many there were when the receiver was last turned.
    received: u64,
    turned_at: u64,
}

impl Downlink {
    /// `rate`: the channel's sample rate ([`super::MIN_RATE`] at least).
    pub fn new(rate: f64) -> Self {
        Downlink { rx: super::receiver(rate), framer: Framer::new(), symbols: Vec::new(), bursts: Vec::new(), received: 0, turned_at: 0 }
    }

    pub fn push(&mut self, iq: &[Complex32]) -> &[Burst] {
        self.symbols.clear();
        self.bursts.clear();
        self.rx.push(iq, &mut self.symbols);
        self.received += self.symbols.len() as u64;
        for s in &self.symbols {
            self.framer.push(s, &mut self.bursts);
        }
        for b in &self.bursts {
            // (A burst begun before the last turn may still stand turned.)
            let turn = if b.start >= self.turned_at { b.turn } else { 0 };
            self.rx.synced(turn);
            if turn != 0 {
                self.turned_at = self.received;
            }
        }
        &self.bursts
    }

    pub fn offset_hz(&self) -> Option<f32> {
        self.rx.offset_hz()
    }
    pub fn phase_error_deg(&self) -> Option<f32> {
        self.rx.phase_error_deg()
    }
}

/// A carrier's whole receive chain: channel IQ in, events out.
pub struct Carrier {
    pub downlink: Downlink,
    pub cell: Cell,
}

impl Carrier {
    /// `rate`: the channel's sample rate ([`super::MIN_RATE`] at least).
    pub fn new(rate: f64) -> Self {
        Carrier { downlink: Downlink::new(rate), cell: Cell::default() }
    }

    pub fn push(&mut self, iq: &[Complex32], out: &mut Vec<Event>) {
        for b in self.downlink.push(iq) {
            self.cell.push(b, out);
        }
    }

    pub fn offset_hz(&self) -> Option<f32> {
        self.downlink.offset_hz()
    }
    pub fn phase_error_deg(&self) -> Option<f32> {
        self.downlink.phase_error_deg()
    }
}
