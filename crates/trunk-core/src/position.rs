//! A radio's GPS position, sent while it talks, as any protocol's decoder
//! hands it to the engine ([`crate::trunk::voice::TrackerOut::Position`]).
//! The decoders: P25 (Motorola, L3Harris) in [`crate::p25::gps`].

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub unit: u32,
    /// Decimal degrees, north and east positive.
    pub lat: f64,
    pub lon: f64,
    /// Which decoder ("MotoP25_FDMA", "HarrisP25_TDMA", …).
    pub source: &'static str,
}
