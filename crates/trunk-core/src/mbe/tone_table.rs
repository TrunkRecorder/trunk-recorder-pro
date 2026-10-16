//! AMBE+2 half-rate tone frames: TIA-102.BABA-A Annex T, which maps a tone
//! frame's 8-bit ID to a fundamental and the one or two harmonics that carry
//! the tone.

/// IDs 128–163, the dual tones: fundamental (Hz) and the two harmonics.
/// Copied by script from the table in openBLIP25's blip25-vocoder (MIT).
static DUAL: [(f64, u8, u8); 36] = [
    (78.5000, 12, 17), (173.48, 4, 7), (70.0000, 10, 19), (87.0000, 8, 17),
    (109.95, 7, 11), (191.68, 4, 7), (70.1700, 11, 21), (71.0600, 12, 17),
    (121.58, 7, 11), (212.0000, 4, 7), (116.41, 6, 14), (96.1500, 8, 17),
    (71.0000, 12, 23), (234.26, 4, 7), (134.38, 7, 9), (134.35, 7, 11),
    (68.3300, 12, 17), (150.89, 4, 7), (67.8200, 9, 17), (86.5000, 7, 15),
    (95.7900, 7, 11), (166.92, 4, 7), (67.7000, 10, 19), (74.7400, 10, 14),
    (105.9, 7, 11), (92.7800, 8, 14), (101.55, 6, 14), (84.0200, 8, 17),
    (67.8300, 11, 21), (102.3, 8, 14), (117.0000, 7, 9), (117.49, 7, 11),
    (87.7800, 4, 5), (70.8300, 6, 7), (122.0000, 4, 5), (70.0000, 5, 7),
];

/// Annex T's row for `id`: (fundamental in Hz, harmonic, harmonic). IDs
/// 5–122 are a single tone at 31.25·ID Hz on harmonic ID/13 + 1, which puts
/// the fundamental at about 150–400 Hz (the standard's table rounds its
/// fundamentals to within 0.02 Hz of this). None: a reserved ID, or 255
/// (silence).
pub fn annex_t(id: u8) -> Option<(f64, usize, usize)> {
    match id {
        5..=122 => {
            let h = id as usize / 13 + 1;
            Some((31.25 * id as f64 / h as f64, h, h))
        }
        128..=163 => {
            let (f0, a, b) = DUAL[id as usize - 128];
            Some((f0, a as usize, b as usize))
        }
        _ => None,
    }
}
