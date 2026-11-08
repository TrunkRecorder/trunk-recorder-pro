//! A source's samples as they arrive: how close to full scale they come
//! (headroom) and how many hit it (clipping — gain too high, a strong signal
//! nearby), plus the counts the session already keeps. Looks at one sample in
//! [`EVERY`], so it costs next to nothing on the sample path.

use trunk_core::metrics::Sink;
use trunk_core::Complex32;

/// One sample in this many is looked at.
pub const EVERY: usize = 16;

/// What an FPGA filter bank's status registers say of the ADC since the last reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BankMeter {
    /// The largest |I| or |Q|, full scale 1.
    pub peak: f32,
    pub clipped: u64,
    pub looked: u64,
}

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
}

impl SampleMeter {
    /// RTL-SDR u8 IQ (127.5 is zero; 0 and 255 are the rails).
    pub fn u8(&mut self, bytes: &[u8]) {
        self.samples += bytes.len() as u64 / 2;
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

    /// A filter bank block: it stands for `samples` input samples. The radio's
    /// own meters come now and then: [`BankMeter`].
    pub fn bank(&mut self, samples: u64, meter: Option<BankMeter>) {
        self.samples += samples;
        if let Some(m) = meter {
            self.peak = self.peak.max(m.peak);
            self.clipped += m.clipped;
            self.looked += m.looked;
        }
    }

    /// Report under the source's scope and start the next window: `peak`
    /// (dBFS), `clipPct` (% of samples at the rails), and the totals
    /// `samples` (→ the measured rate), `dropped`, `errors`.
    pub fn report(&mut self, sink: &mut dyn Sink) {
        sink.counter("samples", self.samples);
        sink.counter("dropped", self.dropped);
        sink.counter("errors", self.errors);
        if self.looked > 0 {
            sink.gauge("peak", 20.0 * (self.peak.max(1e-6) as f64).log10());
            sink.gauge("clipPct", 100.0 * self.clipped as f64 / self.looked as f64);
        }
        self.peak = 0.0;
        self.looked = 0;
        self.clipped = 0;
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
    fn a_filter_banks_meter_reads_like_the_others() {
        let mut m = SampleMeter::default();
        m.bank(12288, None);
        m.bank(12288, Some(BankMeter { peak: 0.5, clipped: 8, looked: 800 }));
        let mut g = G::default();
        m.report(&mut g);
        let get = |n: &str| g.0.iter().find(|(k, _)| k == n).unwrap().1;
        assert_eq!((get("samples"), get("clipPct")), (24576.0, 1.0));
        assert!((get("peak") + 6.02).abs() < 0.01);
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
}
