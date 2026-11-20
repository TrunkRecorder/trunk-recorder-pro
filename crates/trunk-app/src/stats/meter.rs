//! A source's samples as they arrive: how close to full scale they come
//! (headroom), how many hit it (clipping — gain too high, a strong signal
//! nearby), whether they stay so small the ADC hardly moves (quiet — gain too
//! low), plus the counts the session already keeps. Looks at one sample in
//! [`EVERY`], so it costs next to nothing on the sample path.

use std::collections::VecDeque;

use trunk_core::metrics::Sink;
use trunk_core::Complex32;

/// One sample in this many is looked at.
pub const EVERY: usize = 16;

/// An 8-bit radio (RTL-SDR, HackRF) whose peaks average under this, dBFS,
/// over [`QUIET_S`] is too quiet: its samples stay within ±3 of 127 counts, a
/// few of its 256 levels, and what it hears is mostly its own rounding. A
/// quiet band at a working gain still swings tens of counts (−20 to −10 dBFS).
pub const QUIET_DB_8BIT: f64 = -30.0;
/// The same for a float source (12–16-bit ADCs, scaled by their drivers, the
/// bits unknown here): only the clearly under-driven, ±6 counts of 12 bits.
pub const QUIET_DB: f64 = -50.0;
/// Seconds of peaks averaged: a fade or a lull doesn't make it, nor a burst end it.
pub const QUIET_S: usize = 60;
/// Quiet ends only this far, dB, over where it began (no flapping on the line).
const QUIET_HYST_DB: f64 = 3.0;

#[derive(Default, Clone)]
pub struct SampleMeter {
    /// Since the last report: the largest |I| or |Q| (full scale 1), samples looked at, and clipped.
    peak: f32,
    looked: u64,
    clipped: u64,
    /// Running totals.
    pub samples: u64,
    pub dropped: u64,
    pub errors: u64,
    /// Fed 8-bit samples ([`Self::u8`]).
    eight_bit: bool,
    /// The last [`QUIET_S`] reports' peaks, dBFS, and whether it's quiet now.
    peaks: VecDeque<f64>,
    quiet: bool,
}

impl SampleMeter {
    /// RTL-SDR u8 IQ (127.5 is zero; 0 and 255 are the rails).
    pub fn u8(&mut self, bytes: &[u8]) {
        self.samples += bytes.len() as u64 / 2;
        self.eight_bit = true;
        let mut peak = 0u8;
        let mut clipped = 0u64;
        let mut looked = 0u64;
        for iq in bytes.chunks_exact(2).step_by(EVERY) {
            let mut rail = false;
            for &b in iq {
                let d = if b >= 128 { b - 128 } else { 127 - b };
                peak = peak.max(d);
                rail |= b == 0 || b == 255;
            }
            clipped += rail as u64;
            looked += 1;
        }
        self.peak = self.peak.max((peak as f32 + 0.5) / 127.5);
        self.clipped += clipped;
        self.looked += looked;
    }

    /// Float IQ (full scale 1).
    pub fn iq(&mut self, iq: &[Complex32]) {
        self.samples += iq.len() as u64;
        let mut peak = 0f32;
        let mut clipped = 0u64;
        let mut looked = 0u64;
        for s in iq.iter().step_by(EVERY) {
            let m = s.re.abs().max(s.im.abs());
            peak = peak.max(m);
            clipped += (m >= 0.99) as u64;
            looked += 1;
        }
        self.peak = self.peak.max(peak);
        self.clipped += clipped;
        self.looked += looked;
    }

    /// Report under the source's scope and start the next window: `peak`
    /// (dBFS), `clipPct` (% of samples at the rails), `quiet` (1 while it is,
    /// [`Self::quiet`]), and the totals `samples` (→ the measured rate),
    /// `dropped`, `errors`.
    pub fn report(&mut self, sink: &mut dyn Sink) {
        sink.counter("samples", self.samples);
        sink.counter("dropped", self.dropped);
        sink.counter("errors", self.errors);
        if self.looked > 0 {
            let peak = 20.0 * (self.peak.max(1e-6) as f64).log10();
            sink.gauge("peak", peak);
            sink.gauge("clipPct", 100.0 * self.clipped as f64 / self.looked as f64);
            self.peaks.push_back(peak);
            if self.peaks.len() > QUIET_S {
                self.peaks.pop_front();
            }
        } else {
            // Nothing came: not running, so not quiet either.
            self.peaks.clear();
        }
        let line = if self.eight_bit { QUIET_DB_8BIT } else { QUIET_DB };
        self.quiet = self.peaks.len() == QUIET_S && self.peak_avg() < line + if self.quiet { QUIET_HYST_DB } else { 0.0 };
        if self.quiet {
            sink.gauge("quiet", 1.0);
        }
        self.peak = 0.0;
        self.looked = 0;
        self.clipped = 0;
    }

    /// Too quiet (gain too low): the last minute's peaks averaged under the
    /// line for its kind ([`QUIET_DB_8BIT`], [`QUIET_DB`]). That average, dBFS.
    pub fn quiet(&self) -> Option<f64> {
        self.quiet.then(|| self.peak_avg())
    }

    fn peak_avg(&self) -> f64 {
        self.peaks.iter().sum::<f64>() / self.peaks.len().max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct G(Vec<(String, f64)>);
    impl Sink for G {
        fn counter(&mut self, n: &str, t: u64) {
            self.0.push((n.into(), t as f64));
        }
        fn gauge(&mut self, n: &str, v: f64) {
            self.0.push((n.into(), v));
        }
    }

    #[test]
    fn headroom_and_clipping() {
        let mut m = SampleMeter::default();
        // Quiet: ±10 counts. Then rails.
        let quiet: Vec<u8> = (0..4096).map(|i| if i % 2 == 0 { 137 } else { 118 }).collect();
        m.u8(&quiet);
        let mut g = G::default();
        m.report(&mut g);
        let peak = g.0.iter().find(|(n, _)| n == "peak").unwrap().1;
        assert!((-23.0..-21.0).contains(&peak), "{peak}");
        assert_eq!(g.0.iter().find(|(n, _)| n == "clipPct").unwrap().1, 0.0);
        m.u8(&[255u8; 4096]);
        let mut g = G::default();
        m.report(&mut g);
        assert_eq!(g.0.iter().find(|(n, _)| n == "clipPct").unwrap().1, 100.0);
        assert_eq!(g.0.iter().find(|(n, _)| n == "samples").unwrap().1, 4096.0);
    }

    /// `seconds` of RTL samples ±`counts` from the middle, reported each second; whether it's quiet after.
    fn rtl(m: &mut SampleMeter, counts: u8, seconds: usize) -> Option<f64> {
        let iq: Vec<u8> = (0..4096).map(|i| if i % 2 == 0 { 128 + counts } else { 127 - counts }).collect();
        for _ in 0..seconds {
            m.u8(&iq);
            m.report(&mut G::default());
        }
        m.quiet()
    }

    #[test]
    fn an_rtl_that_hardly_moves_is_quiet_after_a_minute() {
        // The corpus capture: peaks of ±3 counts (−31 dBFS).
        let mut m = SampleMeter::default();
        assert_eq!(rtl(&mut m, 3, QUIET_S - 1), None, "not before a minute");
        let db = rtl(&mut m, 3, 1).expect("quiet");
        assert!((-32.0..-30.5).contains(&db), "{db}");
        let mut g = G::default();
        m.u8(&[130u8; 64]);
        m.report(&mut g);
        assert!(g.0.iter().any(|(n, v)| n == "quiet" && *v == 1.0), "its gauge");
        // A working gain in a quiet band (±20 counts): never.
        assert_eq!(rtl(&mut SampleMeter::default(), 20, 2 * QUIET_S), None);
        // Turned up, it ends once the minute's average is over the line and then some.
        assert!(rtl(&mut m, 20, 5).is_some(), "not at once");
        assert_eq!(rtl(&mut m, 20, 30), None);
        // Stopped (nothing comes): not quiet.
        rtl(&mut m, 3, QUIET_S);
        assert!(m.quiet().is_some());
        m.report(&mut G::default());
        assert_eq!(m.quiet(), None, "stopped");
    }

    #[test]
    fn a_float_source_is_quiet_only_far_lower() {
        let mut m = SampleMeter::default();
        let iq = |a: f32| vec![Complex32::new(a, -a); 1024];
        for _ in 0..QUIET_S {
            m.iq(&iq(0.025));
            m.report(&mut G::default());
        }
        assert_eq!(m.quiet(), None, "−32 dBFS from a 12-bit radio is fine");
        for _ in 0..QUIET_S {
            m.iq(&iq(0.002));
            m.report(&mut G::default());
        }
        assert!(m.quiet().is_some_and(|db| db < QUIET_DB));
    }
}
