//! What the analysis tools (`tool.rs`, `dmrtool.rs`, `nxdntool.rs`, `snrtool.rs`) share:
//! reading a capture named on the command line, feeding it through a channelizer,
//! bytes as hex, and the file a tool writes decoded audio or IQ to.

use std::fs::File;
use std::io::{BufWriter, Write};

use trunk_core::dsp::Channelizer;

use crate::{die, Args};

/// A raw `cu8` capture and what the command line says about it.
pub struct Capture {
    pub iq: Vec<u8>,
    /// Sample rate (`--rate`, or `--fs`).
    pub fs: f64,
    /// Centre frequency (`--center`).
    pub center: f64,
}

impl Capture {
    /// The capture named by the first argument after the tool's name, whole.
    pub fn from_args(a: &Args, what: &str) -> Capture {
        let path = a.positional.get(1).unwrap_or_else(|| die(&format!("tool {what}: no capture")));
        let iq = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
        Capture { iq, fs: a.num("fs", a.num("rate", 2_400_000.0)), center: a.num("center", 0.0) }
    }

    /// The same, cut to `--seconds` when that is given.
    pub fn from_args_seconds(a: &Args, what: &str) -> Capture {
        let mut cap = Capture::from_args(a, what);
        if let Some(s) = a.get("seconds").and_then(|v| v.parse::<f64>().ok()) {
            cap.iq.truncate((s * cap.fs) as usize * 2);
        }
        cap
    }

    /// Seconds of air in the capture.
    pub fn air(&self) -> f64 {
        self.iq.len() as f64 / 2.0 / self.fs
    }

    /// Run the capture through the channelizer, calling `each` whenever its heads have new output.
    pub fn channelize(&self, chz: &mut Channelizer, mut each: impl FnMut(&Channelizer)) {
        let mut off = 0;
        while off < self.iq.len() {
            let (used, ran) = chz.feed_u8(&self.iq[off..]);
            off += used;
            if ran {
                each(chz);
            }
            if used == 0 {
                break;
            }
        }
    }
}

/// Bytes as lowercase hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A file a tool writes samples to (`--audio out.f32`, `--iq out.cf32`).
pub struct AudioOut(BufWriter<File>);

impl AudioOut {
    /// The file named by `--<key>`, if the option is given.
    pub fn open(a: &Args, key: &str) -> Option<AudioOut> {
        a.get(key).map(|p| AudioOut(BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}"))))))
    }

    /// Audio samples as 32-bit little-endian floats.
    pub fn samples(&mut self, buf: &[f32]) {
        for v in buf {
            let _ = self.0.write_all(&v.to_le_bytes());
        }
    }

    /// IQ as interleaved 32-bit little-endian floats.
    pub fn iq(&mut self, iq: &[num_complex::Complex32]) {
        for c in iq {
            let _ = self.0.write_all(&c.re.to_le_bytes());
            let _ = self.0.write_all(&c.im.to_le_bytes());
        }
    }
}
