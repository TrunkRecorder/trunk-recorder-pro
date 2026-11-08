//! A picture of a capture: its power by frequency over time, one image per
//! radio, for the review before sending and the public page. A plain grid
//! of levels (0–255), drawn by whoever shows it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use rustfft::{num_complex::Complex32, FftPlanner};
use trunk_app::samples::to_iq;

use crate::iq_capture::Recording;

pub const WIDTH: usize = 1024;
const ROWS: usize = 256;
/// FFTs averaged for each row.
const AVERAGE: usize = 8;

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// As a file: width and height (little-endian `u32`), then the rows.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.pixels.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.pixels);
        out
    }
}

/// Rows of powers in dB, as levels: the floor at the bottom of the range and the strongest signals at the top.
fn levels(rows: &[Vec<f32>]) -> Vec<u8> {
    let mut sorted: Vec<f32> = rows.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return vec![0; rows.iter().map(Vec::len).sum()];
    }
    sorted.sort_by(f32::total_cmp);
    let at = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let (low, high) = (at(0.05), at(0.999).max(at(0.05) + 1.0));
    rows.iter().flatten().map(|v| (((v - low) / (high - low)).clamp(0.0, 1.0) * 255.0) as u8).collect()
}

fn image_of(iq_at: &mut dyn FnMut(u64, usize) -> Result<Vec<Complex32>, String>, samples: u64) -> Result<Image, String> {
    let fft = FftPlanner::<f32>::new().plan_fft_forward(WIDTH);
    let window: Vec<f32> = (0..WIDTH).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WIDTH - 1) as f32).cos()).collect();
    let height = ROWS.min((samples as usize / (WIDTH * AVERAGE)).max(1));
    let mut rows = Vec::with_capacity(height);
    for r in 0..height {
        let start = samples * r as u64 / height as u64;
        let mut power = vec![0f32; WIDTH];
        let iq = iq_at(start, WIDTH * AVERAGE)?;
        for block in iq.chunks_exact(WIDTH) {
            let mut b: Vec<Complex32> = block.iter().zip(&window).map(|(v, w)| v * w).collect();
            fft.process(&mut b);
            for (p, v) in power.iter_mut().zip(&b) {
                *p += v.norm_sqr();
            }
        }
        // (DC in the middle, the lowest frequencies at the left.)
        power.rotate_left(WIDTH / 2);
        rows.push(power.iter().map(|p| 10.0 * p.max(1e-20).log10()).collect::<Vec<f32>>());
    }
    Ok(Image { width: WIDTH as u32, height: height as u32, pixels: levels(&rows) })
}

/// The waterfall of one radio's recording.
pub fn of_recording(r: &Recording) -> Result<Image, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", r.data.display());
    let mut file = File::open(&r.data).map_err(fail)?;
    let len = file.metadata().map_err(fail)?.len();
    let bps = r.format.bytes_per_sample() as u64;
    let mut read = |start: u64, n: usize| -> Result<Vec<Complex32>, String> {
        file.seek(SeekFrom::Start(start * bps)).map_err(fail)?;
        let mut buf = Vec::new();
        file.by_ref().take(n as u64 * bps).read_to_end(&mut buf).map_err(fail)?;
        Ok(to_iq(r.format, &buf))
    };
    image_of(&mut read, len / bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_lands_where_its_frequency_is() {
        let n = WIDTH as u64 * AVERAGE as u64 * 4;
        let bin = WIDTH / 4;
        let mut read = |start: u64, count: usize| -> Result<Vec<Complex32>, String> {
            Ok((start..start + count as u64).map(|i| Complex32::from_polar(1.0, 2.0 * std::f32::consts::PI * (i % WIDTH as u64) as f32 * bin as f32 / WIDTH as f32)).collect())
        };
        let im = image_of(&mut read, n).unwrap();
        assert_eq!((im.width, im.height), (WIDTH as u32, 4));
        let first: &[u8] = &im.pixels[..WIDTH];
        assert_eq!(first[WIDTH / 2 + bin], 255);
        assert!(first[WIDTH / 2 - bin] < 255, "the other side of the band is empty");
    }

    #[test]
    fn an_image_says_its_size() {
        let b = Image { width: 2, height: 1, pixels: vec![7, 9] }.to_bytes();
        assert_eq!(b, [2, 0, 0, 0, 1, 0, 0, 0, 7, 9]);
    }

    #[test]
    fn silence_doesnt_divide_by_zero() {
        assert_eq!(levels(&[vec![f32::NEG_INFINITY; 3]]), [0, 0, 0]);
        assert_eq!(levels(&[vec![-5.0; 3]]), [0, 0, 0]);
    }
}
