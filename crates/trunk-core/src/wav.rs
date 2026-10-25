//! Mono 16-bit PCM WAV.

pub fn encode(samples: &[f32], rate: u32) -> Vec<u8> {
    let bytes = samples.len() as u64 * 2;
    // Past 4 GB (74 h at 8 kHz) the sizes don't fit: 0xFFFFFFFF, "to the
    // end of the file", as streaming writers do.
    let (riff, data) = if bytes + 36 <= u32::MAX as u64 { (bytes as u32 + 36, bytes as u32) } else { (u32::MAX, u32::MAX) };
    let mut b = Vec::with_capacity(44 + bytes as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&riff.to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for &s in samples {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    b
}

/// A 16-bit PCM WAV's rate, and its samples in [−1, 1] (the first channel).
pub fn decode(b: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let bad = || "not a 16-bit PCM WAV".to_string();
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad());
    }
    let (mut rate, mut channels, mut at) = (0u32, 0usize, 12);
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &b[at + 8..(at + 8).saturating_add(len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            if u16::from_le_bytes([body[14], body[15]]) != 16 {
                return Err(bad());
            }
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
        } else if id == b"data" && channels > 0 {
            return Ok((rate, body.chunks_exact(2 * channels).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect()));
        }
        at = at.saturating_add(8 + len + (len & 1));
    }
    Err(bad())
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_what_it_encodes() {
        let x = [0.0f32, 0.5, -0.5, 1.0];
        let (rate, y) = super::decode(&super::encode(&x, 8000)).unwrap();
        assert_eq!(rate, 8000);
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-4), "{y:?}");
        assert!(super::decode(b"RIFF....WAVE").is_err());
    }
}
