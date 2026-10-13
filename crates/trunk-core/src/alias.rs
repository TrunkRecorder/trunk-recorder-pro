//! A talker alias, the name a radio is programmed with ("E12 CAPT"), as any
//! protocol's decoder hands it to the engine ([`crate::trunk::voice::TrackerOut::Alias`]).
//! The decoders: P25 (Motorola, Harris) in [`crate::p25::alias`], NXDN
//! (Kenwood) in [`crate::nxdn::alias`].

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub unit: u32,
    pub alias: String,
    /// Which decoder ("MotoP25_FDMA", "HarrisP25_TDMA", "KenwoodNXDN", …) — Trunk Recorder's names for P25's.
    pub source: &'static str,
    /// The talkgroup it was sent on, when the payload says.
    pub talkgroup: Option<u32>,
}
