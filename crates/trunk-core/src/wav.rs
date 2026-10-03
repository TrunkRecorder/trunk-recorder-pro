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
