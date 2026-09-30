//! Sample formats of captures and drivers → float IQ.

use trunk_core::Complex32;

use crate::config::SampleFormat;

/// Interleaved I/Q bytes in `format` → complex samples in about [−1, 1].
pub fn to_iq(format: SampleFormat, bytes: &[u8]) -> Vec<Complex32> {
    match format {
        SampleFormat::Cu8 => bytes.chunks_exact(2).map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5)).collect(),
        SampleFormat::Cs16 => bytes
            .chunks_exact(4)
            .map(|c| Complex32::new(i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0, i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0))
            .collect(),
        SampleFormat::Cf32 => bytes
            .chunks_exact(8)
            .map(|c| Complex32::new(f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]])))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let u8s = to_iq(SampleFormat::Cu8, &[0, 255, 128, 127]);
        assert_eq!(u8s.len(), 2);
        assert!((u8s[0].re + 1.0).abs() < 1e-6 && (u8s[0].im - 1.0).abs() < 1e-6);
        let s16 = to_iq(SampleFormat::Cs16, &[0x00, 0x40, 0x00, 0xc0]);
        assert_eq!(s16, vec![Complex32::new(0.5, -0.5)]);
        let mut b = Vec::new();
        b.extend_from_slice(&0.25f32.to_le_bytes());
        b.extend_from_slice(&(-0.75f32).to_le_bytes());
        b.push(9); // a partial sample is dropped
        assert_eq!(to_iq(SampleFormat::Cf32, &b), vec![Complex32::new(0.25, -0.75)]);
        assert_eq!(SampleFormat::from_path("x.cfile"), SampleFormat::Cf32);
        assert_eq!(SampleFormat::from_path("x.SC16"), SampleFormat::Cs16);
        assert_eq!(SampleFormat::from_path("x.cu8"), SampleFormat::Cu8);
    }

    #[test]
    fn old_configs_still_load() {
        // A config from before USRP / Airspy / formats: files default to cu8.
        let c: crate::Config = serde_json::from_str(r#"{"sources":[{"kind":"file","path":"a.cu8","centerHz":1,"rateHz":2400000,"realtime":true}]}"#).unwrap();
        assert!(matches!(c.sources[0], crate::config::Source::File { format: SampleFormat::Cu8, .. }));
        let c: crate::Config = serde_json::from_str(r#"{"sources":[{"kind":"airspy","centerHz":1,"rateHz":6000000}]}"#).unwrap();
        assert!(matches!(c.sources[0], crate::config::Source::Airspy { gain: 14, .. }));
    }
}
