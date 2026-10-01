//! IMBE 7200x4400 (P25 Phase 1) and AMBE+2 3600x2450 (P25 Phase 2) vocoders:
//! mbelib 1.3.0 (ISC) with Trunk Recorder's "enhanced" synthesis and
//! TIA-102.BABA-A §7.7/§7.8 error concealment. Double precision throughout;
//! the RNG is injectable so output can be compared sample by sample with the
//! reference implementations. IMBE can also go through Pavel Yazev's
//! fixed-point decoder ([`fixed`], [`Profile::Fixed`]).

pub mod fixed;
mod fixed_tables;

use std::f64::consts::{E, PI};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::tables::{
    AMBE_DG, AMBE_HOCB5, AMBE_HOCB6, AMBE_HOCB7, AMBE_HOCB8, AMBE_LMPRBL, AMBE_LTABLE, AMBE_PRBA24, AMBE_PRBA58, AMBE_VUV, AMBE_W0TABLE, B2, BA, BO, HOBA,
    IMBE_JI, QUANTSTEP, STANDDEV, WS,
};

pub const FRAME_SAMPLES: usize = 160;
pub const SAMPLE_RATE: u32 = 8000;
/// One past mbelib's [57]: the prediction can read log2Ml[57].
const NL: usize = 58;

/// Uniform [0, 1) source (mbelib's `rand()/RAND_MAX`).
pub type Rng = Box<dyn FnMut() -> f64 + Send>;

/// The default RNG: 32-bit LCG, the one the reference comparisons use.
pub fn lcg(seed: u32) -> Rng {
    let mut s = seed;
    Box::new(move || {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        s as f64 / 4294967296.0
    })
}

#[derive(Clone)]
struct Parms {
    w0: f64,
    l: usize,
    k: usize,
    vl: [i8; NL],
    ml: [f64; NL],
    log2ml: [f64; NL],
    phil: [f64; NL],
    psil: [f64; NL],
    gamma: f64,
    repeat: u32,
}

impl Default for Parms {
    fn default() -> Self {
        Parms { w0: 0.0, l: 0, k: 0, vl: [0; NL], ml: [0.0; NL], log2ml: [0.0; NL], phil: [0.0; NL], psil: [0.0; NL], gamma: 0.0, repeat: 0 }
    }
}

fn move_parms(cur: &Parms, prev: &mut Parms) {
    prev.w0 = cur.w0;
    prev.l = cur.l;
    prev.k = cur.k;
    prev.ml[0] = 0.0;
    prev.gamma = cur.gamma;
    prev.repeat = cur.repeat;
    for l in 0..=56 {
        prev.ml[l] = cur.ml[l];
        prev.vl[l] = cur.vl[l];
        prev.log2ml[l] = cur.log2ml[l];
        prev.phil[l] = cur.phil[l];
        prev.psil[l] = cur.psil[l];
    }
}

fn init_parms(cur: &mut Parms, prev: &mut Parms, enh: &mut Parms) {
    prev.w0 = 0.09378;
    prev.l = 30;
    prev.k = 10;
    prev.gamma = 0.0;
    for l in 0..=56 {
        prev.ml[l] = 0.0;
        prev.vl[l] = 0;
        prev.log2ml[l] = 0.0;
        prev.phil[l] = 0.0;
        prev.psil[l] = PI / 2.0;
    }
    prev.repeat = 0;
    move_parms(prev, cur);
    move_parms(prev, enh);
}

/// TIA spectral enhancement; returns R_M0 (the frame's energy before it).
fn spectral_amp_enhance(cur: &mut Parms) -> f64 {
    let (mut rm0, mut rm1) = (0.0, 0.0);
    for l in 1..=cur.l {
        rm0 += cur.ml[l] * cur.ml[l];
        rm1 += cur.ml[l] * cur.ml[l] * (cur.w0 * l as f64).cos();
    }
    let (r2m0, r2m1) = (rm0 * rm0, rm1 * rm1);
    for l in 1..=cur.l {
        if cur.ml[l] != 0.0 {
            let wl = cur.ml[l].sqrt()
                * ((0.96 * PI * (r2m0 + r2m1 - 2.0 * rm0 * rm1 * (cur.w0 * l as f64).cos())) / (cur.w0 * rm0 * (r2m0 - r2m1))).powf(0.25);
            if 8 * l <= cur.l {
            } else if wl > 1.2 {
                cur.ml[l] *= 1.2;
            } else if wl < 0.5 {
                cur.ml[l] *= 0.5;
            } else {
                cur.ml[l] *= wl;
            }
        }
    }
    let sum: f64 = (1..=cur.l).map(|l| cur.ml[l] * cur.ml[l]).sum();
    let gamma = if sum == 0.0 { 1.0 } else { (rm0 / sum).sqrt() };
    for l in 1..=cur.l {
        cur.ml[l] *= gamma;
    }
    rm0
}

/// TIA-102.BABA-A adaptive smoothing (Trunk Recorder's float decoder): once
/// the channel has errors, harmonics louder than a threshold set by the
/// smoothed energy `se` are forced voiced and the total amplitude is capped,
/// so a corrupted frame that got past the repeat rules can't burst.
fn adaptive_smoothing(cur: &mut Parms, se: f64, er: f64, et: u32) {
    let et = et as f64;
    let vm = if er <= 0.005 && et <= 4.0 {
        f64::INFINITY
    } else if er <= 0.0125 && et == 0.0 {
        45.255 * se.powf(0.375) / (277.6 * er).exp()
    } else {
        1.414 * se.powf(0.375)
    };
    let mut am = 0.0;
    for l in 1..=cur.l {
        if cur.ml[l] > vm {
            cur.vl[l] = 1;
        }
        am += cur.ml[l];
    }
    let tm: f64 = if er <= 0.005 && et <= 6.0 { 20480.0 } else { (6000.0 - 300.0 * et).max(0.0) };
    if tm <= am {
        for l in 1..=cur.l {
            cur.ml[l] *= tm / am;
        }
    }
}

/// Trunk Recorder's synthesis improvements ("enhanced" profile).
#[derive(Clone, Copy)]
struct SynthOpts {
    c_env: f64,
    low_blend: f64,
    w_rand: f64,
    kernel_d: usize,
    kernel_gamma: f64,
    interp_tol: f64,
    hf_lift_db: f64,
    hf_lift_f1: f64,
}
const ENHANCED: SynthOpts =
    SynthOpts { c_env: 0.7, low_blend: 0.4, w_rand: 0.25, kernel_d: 19, kernel_gamma: 0.6, interp_tol: 0.2, hf_lift_db: 3.0, hf_lift_f1: 2200.0 };

fn hf_gain(w: f64, o: &SynthOpts) -> f64 {
    if o.hf_lift_db == 0.0 {
        return 1.0;
    }
    let f = w * 8000.0 / (2.0 * PI);
    if f <= o.hf_lift_f1 {
        return 1.0;
    }
    let t = ((f - o.hf_lift_f1) / (3700.0 - o.hf_lift_f1)).min(1.0);
    10f64.powf(o.hf_lift_db * t / 20.0)
}

/// Envelope (minimum) phase per harmonic, after US5701390.
fn envelope_phase(mp: &Parms, maxl: usize, o: &SynthOpts) -> [f64; 58] {
    let d = o.kernel_d.min(19);
    let off = d as isize;
    let blen = 57 + 2 * d + 2;
    let mut b = [0.0f64; 57 + 2 * 19 + 2];
    let l = mp.l;
    let mut mean = 0.0;
    for i in 1..=l {
        let m = mp.ml[i];
        let v = if m > 1e-6 { m.log2() } else { -20.0 };
        b[i + d] = v;
        mean += v;
    }
    if l > 0 {
        mean /= l as f64;
        for i in 1..=l {
            b[i + d] -= mean;
        }
    }
    let mut decay = 1.0;
    let bl = b[l + d];
    let mut i = l + 1;
    while i <= l + d && i + d < blen {
        decay *= o.kernel_gamma;
        b[i + d] = bl * decay;
        i += 1;
    }
    for i in 1..=d {
        b[d - i] = b[i + d];
    }
    let mut env = [0.0f64; 58];
    for ell in 1..=maxl {
        let mut e = 0.0;
        let mut m = 1usize;
        while m <= d {
            let hi = ell + m + d;
            let lo = (ell as isize - m as isize + off) as usize;
            e += (2.0 / PI) * ((if hi < blen { b[hi] } else { 0.0 }) - b[lo]) / m as f64;
            m += 2;
        }
        env[ell] = e;
    }
    env
}

/// Unvoiced synthesis as colored noise (Trunk Recorder's
/// `synth_unvoiced_smooth`). mbelib's multisine noise holds each frame's
/// spectrum between 49-sample cross-fades and redraws its phases every frame,
/// which steps at the boundaries. Here each frame's unvoiced bands are
/// complex Gaussian bins at exactly the band's power (M is a spectral density,
/// as in TIA), inverse-FFT'd to a stationary segment whose first 160 samples
/// cross-fade in over this frame (sin/cos, constant power) and whose next 160
/// continue under the following frame's cross-fade.
struct UvSynth {
    fft: Arc<dyn Fft<f64>>,
    buf: Vec<Complex<f64>>,
    tail: [f64; FRAME_SAMPLES],
}

impl UvSynth {
    const N: usize = 512;
    /// Trunk Recorder's UV_DENSITY (0.85), over its ×4 voiced gain: here a
    /// voiced harmonic's amplitude is M itself.
    const DENSITY: f64 = 0.85 / 4.0;

    fn new() -> Self {
        UvSynth { fft: FftPlanner::new().plan_fft_inverse(Self::N), buf: vec![Complex::default(); Self::N], tail: [0.0; FRAME_SAMPLES] }
    }

    fn reset(&mut self) {
        self.tail = [0.0; FRAME_SAMPLES];
    }

    fn synth(&mut self, out: &mut [f32; FRAME_SAMPLES], cur: &Parms, rand: &mut Rng, o: &SynthOpts) {
        let n = Self::N;
        let nf = n as f64;
        self.buf.fill(Complex::default());
        let bin = |x: f64| (x * cur.w0 * nf / (2.0 * PI)).ceil() as usize;
        for l in 1..=cur.l {
            if cur.vl[l] != 0 {
                continue;
            }
            let lf = l as f64;
            let (lo, hi) = (bin(lf - 0.5).max(1), bin(lf + 0.5).min(n / 2));
            if hi <= lo {
                continue;
            }
            let mut pow = 0.0;
            for k in lo..hi {
                // Box-Muller
                let r = (-(1.0 - rand()).ln()).sqrt();
                let a = 2.0 * PI * rand();
                self.buf[k] = Complex::new(r * a.cos(), r * a.sin());
                pow += r * r;
            }
            let g = Self::DENSITY * cur.ml[l] * hf_gain(cur.w0 * lf, o) * ((hi - lo) as f64 / pow).sqrt();
            for k in lo..hi {
                self.buf[k] *= g;
                self.buf[n - k] = self.buf[k].conj();
            }
        }
        self.fft.process(&mut self.buf);
        for (i, v) in out.iter_mut().enumerate() {
            let p = (i as f64 + 0.5) / FRAME_SAMPLES as f64 * (PI / 2.0);
            *v = (*v as f64 + p.cos() * self.tail[i] + p.sin() * self.buf[i].re) as f32;
            self.tail[i] = self.buf[i + FRAME_SAMPLES].re;
        }
    }
}

/// mbe_synthesizeSpeechf; `opts` None = mbelib exactly. With `uv`, unvoiced
/// harmonics are synthesized as colored noise instead of mbelib's multisines.
fn synthesize_speech(
    out: &mut [f32; FRAME_SAMPLES],
    cur: &mut Parms,
    prev: &mut Parms,
    uvq: usize,
    rand: &mut Rng,
    opts: Option<&SynthOpts>,
    uv: Option<&mut UvSynth>,
) {
    const N: usize = 160;
    let uvthreshold = 2700.0 * PI / 4000.0;
    let uvsine = 1.3591409 * E;
    let uvrand = 2.0;
    let uvquality = if (1..=64).contains(&uvq) { uvq } else { 3 };
    let loguvquality = if uvquality == 1 { 1.0 / E } else { (uvquality as f64).ln() / uvquality as f64 };
    let uvstep = 1.0 / uvquality as f64;
    let qfactor = loguvquality;
    let uvoffset = uvstep * (uvquality - 1) as f64 / 2.0;

    let num_uv = (1..=cur.l).filter(|&l| cur.vl[l] == 0).count();
    let (cw0, pw0) = (cur.w0, prev.w0);
    out.fill(0.0);

    let maxl = if cur.l > prev.l {
        for l in prev.l + 1..=cur.l {
            prev.ml[l] = 0.0;
            prev.vl[l] = 1;
        }
        cur.l
    } else {
        for l in cur.l + 1..=prev.l {
            cur.ml[l] = 0.0;
            cur.vl[l] = 1;
        }
        prev.l
    };

    let env = opts.map(|o| envelope_phase(cur, maxl, o));
    let rho = if cur.l > 0 { num_uv as f64 / cur.l as f64 } else { 0.0 };
    for l in 1..=56 {
        cur.psil[l] = prev.psil[l] + (pw0 + cw0) * ((l * N) as f64 / 2.0);
        if let (Some(o), Some(env)) = (opts, env.as_ref()) {
            cur.phil[l] = if l <= cur.l / 4 {
                cur.psil[l] + o.low_blend * o.c_env * env[l]
            } else {
                cur.psil[l] + o.c_env * env[l] + o.w_rand * rho * (rand() * (PI * 2.0) - PI)
            };
        } else if l <= cur.l / 4 {
            cur.phil[l] = cur.psil[l];
        } else {
            cur.phil[l] = cur.psil[l] + (num_uv as f64 * (rand() * (PI * 2.0) - PI)) / cur.l as f64;
        }
    }

    let mut m_new = [0.0f64; NL];
    let mut m_old = [0.0f64; NL];
    for l in 1..=maxl {
        m_new[l] = cur.ml[l] * opts.map_or(1.0, |o| hf_gain(cw0 * l as f64, o));
        m_old[l] = prev.ml[l] * opts.map_or(1.0, |o| hf_gain(pw0 * l as f64, o));
    }
    let uv_noise = |rand: &mut Rng| if opts.is_some() { rand() - 0.5 } else { rand() };
    let multisine = uv.is_none();

    let mut rphase = [0.0f64; 64];
    let mut rphase2 = [0.0f64; 64];
    for l in 1..=maxl {
        let lf = l as f64;
        let (cw0l, pw0l) = (cw0 * lf, pw0 * lf);
        let (cv, pv) = (cur.vl[l], prev.vl[l]);
        if cv == 0 && pv == 1 {
            for r in rphase.iter_mut().take(if multisine { uvquality } else { 0 }) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for n in 0..N {
                let nf = n as f64;
                let c1 = WS[n + N] as f64 * m_old[l] * (pw0l * nf + prev.phil[l]).cos();
                let mut c3 = 0.0;
                for i in 0..if multisine { uvquality } else { 0 } {
                    c3 += (cw0 * nf * (lf + i as f64 * uvstep - uvoffset) + rphase[i]).cos();
                    if cw0l > uvthreshold {
                        c3 += (cw0l - uvthreshold) * uvrand * uv_noise(rand);
                    }
                }
                c3 = c3 * uvsine * WS[n] as f64 * m_new[l] * qfactor;
                out[n] = (out[n] as f64 + (c1 + c3)) as f32;
            }
        } else if cv == 1 && pv == 0 {
            for r in rphase.iter_mut().take(if multisine { uvquality } else { 0 }) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for n in 0..N {
                let nf = n as f64;
                let c1 = WS[n] as f64 * m_new[l] * (cw0l * (nf - N as f64) + cur.phil[l]).cos();
                let mut c3 = 0.0;
                for i in 0..if multisine { uvquality } else { 0 } {
                    c3 += (pw0 * nf * (lf + i as f64 * uvstep - uvoffset) + rphase[i]).cos();
                    if pw0l > uvthreshold {
                        c3 += (pw0l - uvthreshold) * uvrand * uv_noise(rand);
                    }
                }
                c3 = c3 * uvsine * WS[n + N] as f64 * m_old[l] * qfactor;
                out[n] = (out[n] as f64 + (c1 + c3)) as f32;
            }
        } else if cv == 1 || pv == 1 {
            if let Some(o) = opts.filter(|o| (cw0 - pw0).abs() < o.interp_tol * cw0) {
                let _ = o;
                let mut dpl = cur.phil[l] - prev.phil[l] - (pw0 + cw0) * lf * (N as f64 / 2.0);
                dpl -= 2.0 * PI * ((dpl + PI) / (2.0 * PI)).floor();
                let tha = pw0l + dpl / N as f64;
                let thb = (cw0 - pw0) * lf / (2.0 * N as f64);
                let mb = (m_new[l] - m_old[l]) / N as f64;
                for n in 0..N {
                    let nf = n as f64;
                    out[n] = (out[n] as f64 + (m_old[l] + nf * mb) * (prev.phil[l] + (tha + thb * nf) * nf).cos()) as f32;
                }
            } else {
                for n in 0..N {
                    let nf = n as f64;
                    let c1 = WS[n + N] as f64 * m_old[l] * (pw0l * nf + prev.phil[l]).cos();
                    let c2 = WS[n] as f64 * m_new[l] * (cw0l * (nf - N as f64) + cur.phil[l]).cos();
                    out[n] = (out[n] as f64 + (c1 + c2)) as f32;
                }
            }
        } else if multisine {
            for r in rphase.iter_mut().take(uvquality) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for r in rphase2.iter_mut().take(uvquality) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for n in 0..N {
                let nf = n as f64;
                let mut c3 = 0.0;
                for i in 0..uvquality {
                    c3 += (pw0 * nf * (lf + i as f64 * uvstep - uvoffset) + rphase[i]).cos();
                    if pw0l > uvthreshold {
                        c3 += (pw0l - uvthreshold) * uvrand * uv_noise(rand);
                    }
                }
                c3 = c3 * uvsine * WS[n + N] as f64 * m_old[l] * qfactor;
                let mut c4 = 0.0;
                for i in 0..uvquality {
                    c4 += (cw0 * nf * (lf + i as f64 * uvstep - uvoffset) + rphase2[i]).cos();
                    if cw0l > uvthreshold {
                        c4 += (cw0l - uvthreshold) * uvrand * uv_noise(rand);
                    }
                }
                c4 = c4 * uvsine * WS[n] as f64 * m_new[l] * qfactor;
                out[n] = (out[n] as f64 + (c3 + c4)) as f32;
            }
        }
    }
    if let (Some(u), Some(o)) = (uv, opts) {
        u.synth(out, cur, rand, o);
    }
}

/// mbelib's float → short gain (×7, clipped at ±32760), scaled to [−1, 1].
pub fn to_unit(buf: &mut [f32]) {
    for v in buf.iter_mut() {
        *v = ((7.0 * *v as f64).clamp(-32760.0, 32760.0) / 32768.0) as f32;
    }
}

/// Where [`to_limited`] starts to compress: −3 dBFS.
const LIMIT_KNEE: f64 = 0.7;

/// mbelib's ×7 gain to [−1, 1] through a soft limiter instead of a hard
/// clip: unchanged below the knee, then a tanh curve that meets it with the
/// same slope and approaches full scale. mbelib's gain leaves loud talkers'
/// onsets only a few dB of headroom, and each hard-clipped sample clicks.
pub fn to_limited(buf: &mut [f32]) {
    const K: f64 = LIMIT_KNEE;
    for v in buf.iter_mut() {
        let x = 7.0 * *v as f64 / 32768.0;
        let a = x.abs();
        let y = if a <= K { a } else { K + (1.0 - K) * ((a - K) / (1.0 - K)).tanh() };
        *v = y.copysign(x) as f32;
    }
}

/// mbe_decodeImbe4400Parms: false on an invalid pitch / L (mbelib's `bad`).
fn decode_imbe4400_parms(d: &[u8; 88], cur: &mut Parms, prev: &mut Parms) -> bool {
    let mut bb = [[0u8; 12]; 58];
    let mut cik = [[0.0f64; 11]; 7];
    let (mut gm, mut ri) = ([0.0f64; 7], [0.0f64; 7]);
    let mut tl = [0.0f64; NL];
    let mut flokl = [0.0f64; NL];
    let mut deltal = [0.0f64; NL];
    let mut intkl = [0usize; NL];

    cur.repeat = prev.repeat;
    let b0 = ((d[0] as u32) << 7) | ((d[1] as u32) << 6) | ((d[2] as u32) << 5) | ((d[3] as u32) << 4) | ((d[4] as u32) << 3) | ((d[5] as u32) << 2)
        | ((d[85] as u32) << 1)
        | d[86] as u32;
    if b0 > 207 {
        return false;
    }
    cur.w0 = 4.0 * PI / (b0 as f64 + 39.5);
    let l = (0.9254 * ((PI / cur.w0 + 0.25) as i64) as f64) as usize;
    if !(9..=56).contains(&l) {
        return false;
    }
    cur.l = l;
    let l9 = l - 9;
    let k = if l < 37 { (l + 2) / 3 } else { 12 };
    cur.k = k;

    let bo_base = l9 * 79 * 2;
    for (p, i) in (6..85).enumerate() {
        bb[BO[bo_base + 2 * p] as usize][BO[bo_base + 2 * p + 1] as usize] = d[i];
    }
    let (mut j, mut kk) = (1, k - 1);
    for i in 1..=l {
        cur.vl[i] = bb[1][kk] as i8;
        if j == 3 {
            j = 1;
            kk = kk.saturating_sub(1);
        } else {
            j += 1;
        }
    }
    let b2 = ((bb[2][5] as usize) << 5) | ((bb[2][4] as usize) << 4) | ((bb[2][3] as usize) << 3) | ((bb[2][2] as usize) << 2) | ((bb[2][1] as usize) << 1)
        | bb[2][0] as usize;
    gm[1] = B2[b2] as f64;
    let ba_base = l9 * 5 * 2;
    for i in 2..7 {
        let ba1 = BA[ba_base + (i - 2) * 2] as i32;
        let ba2 = BA[ba_base + (i - 2) * 2 + 1] as f64;
        let mut bm = 0i32;
        for jj in (0..ba1).rev() {
            bm = (bm << 1) | bb[i + 1][jj as usize] as i32;
        }
        gm[i] = ba2 * (bm as f64 - 2f64.powi(ba1 - 1) + 0.5);
    }
    for i in 1..=6 {
        ri[i] = (1..=6).map(|m| (if m == 1 { 1.0 } else { 2.0 }) * gm[m] * (PI * (m - 1) as f64 * (i as f64 - 0.5) / 6.0).cos()).sum();
    }
    let mut m = 8usize;
    for i in 1..=6 {
        cik[i][1] = ri[i];
        let ji = IMBE_JI[l9 * 6 + i - 1] as usize;
        for kk in 2..=ji {
            let bm_bits = HOBA[l9 * 50 + m - 8] as i32;
            cik[i][kk] = if bm_bits == 0 {
                0.0
            } else {
                let mut bm = 0i32;
                for b in 0..bm_bits {
                    bm = (bm << 1) | bb[m][(bm_bits - b - 1) as usize] as i32;
                }
                QUANTSTEP[(bm_bits - 1) as usize] as f64 * STANDDEV[kk - 2] as f64 * (bm as f64 - 2f64.powi(bm_bits - 1) + 0.5)
            };
            m += 1;
        }
    }
    let mut li = 1;
    for i in 1..=6 {
        let ji = IMBE_JI[l9 * 6 + i - 1] as usize;
        for jj in 1..=ji {
            tl[li] = (1..=ji).map(|kk| (if kk == 1 { 1.0 } else { 2.0 }) * cik[i][kk] * (PI * (kk - 1) as f64 * (jj as f64 - 0.5) / ji as f64).cos()).sum();
            li += 1;
        }
    }
    let rho = if cur.l <= 15 { 0.4 } else if cur.l <= 24 { 0.03 * cur.l as f64 - 0.05 } else { 0.7 };
    if cur.l > prev.l {
        for l in prev.l + 1..=cur.l {
            prev.ml[l] = prev.ml[prev.l];
            prev.log2ml[l] = prev.log2ml[prev.l];
        }
    }
    let mut sum77 = 0.0;
    for l in 1..=cur.l {
        flokl[l] = (prev.l as f64 / cur.l as f64) * l as f64;
        intkl[l] = flokl[l] as usize;
        deltal[l] = flokl[l] - intkl[l] as f64;
        sum77 += (1.0 - deltal[l]) * prev.log2ml[intkl[l]] + deltal[l] * prev.log2ml[intkl[l] + 1];
    }
    sum77 *= rho / cur.l as f64;
    for l in 1..=cur.l {
        let c1 = rho * (1.0 - deltal[l]) * prev.log2ml[intkl[l]];
        let c2 = rho * deltal[l] * prev.log2ml[intkl[l] + 1];
        cur.log2ml[l] = tl[l] + c1 + c2 - sum77;
        cur.ml[l] = 2f64.powf(cur.log2ml[l]);
    }
    true
}

/// What mbe_decodeAmbe2450Parms made of a frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ambe {
    Voice,
    Erasure,
    Tone,
}

/// mbe_decodeAmbe2450Parms.
fn decode_ambe2450_parms(d: &[u8; 49], cur: &mut Parms, prev: &mut Parms) -> Ambe {
    let bit = |i: usize| d[i] as usize;
    let mut cik = [[0.0f64; 18]; 5];
    let mut gm = [0.0f64; 9];
    let mut ri = [0.0f64; 9];
    let mut tl = [0.0f64; NL];
    let mut flokl = [0.0f64; NL];
    let mut deltal = [0.0f64; NL];
    let mut intkl = [0usize; NL];
    let mut silence = false;
    let mut f0 = 0.0;
    let mut l_count = 0usize;

    cur.repeat = prev.repeat;
    let b0 = (bit(0) << 6) | (bit(1) << 5) | (bit(2) << 4) | (bit(3) << 3) | (bit(37) << 2) | (bit(38) << 1) | bit(39);
    match b0 {
        120..=123 => return Ambe::Erasure,
        124 | 125 => {
            silence = true;
            cur.w0 = 2.0 * PI / 32.0;
            f0 = 1.0 / 32.0;
            l_count = 14;
            cur.l = 14;
            for l in 1..=l_count {
                cur.vl[l] = 0;
            }
        }
        126 | 127 => return Ambe::Tone,
        _ => {}
    }
    if !silence {
        f0 = AMBE_W0TABLE[b0] as f64;
        cur.w0 = f0 * 2.0 * PI;
    }
    let unvc = 0.2046 / cur.w0.sqrt();
    if !silence {
        l_count = AMBE_LTABLE[b0] as usize;
        cur.l = l_count;
    }

    // V/UV
    let b1 = (bit(4) << 4) | (bit(5) << 3) | (bit(6) << 2) | (bit(7) << 1) | bit(35);
    for l in 1..=l_count {
        let jl = (l as f64 * 16.0 * f0) as usize;
        if !silence {
            cur.vl[l] = AMBE_VUV[b1 * 8 + jl] as i8;
        }
    }
    // gain
    let b2 = (bit(8) << 4) | (bit(9) << 3) | (bit(10) << 2) | (bit(11) << 1) | bit(36);
    cur.gamma = AMBE_DG[b2] as f64 + 0.5 * prev.gamma;

    // PRBA vectors
    let b3 = (bit(12) << 8) | (bit(13) << 7) | (bit(14) << 6) | (bit(15) << 5) | (bit(16) << 4) | (bit(17) << 3) | (bit(18) << 2) | (bit(19) << 1) | bit(40);
    for k in 0..3 {
        gm[2 + k] = AMBE_PRBA24[b3 * 3 + k] as f64;
    }
    let b4 = (bit(20) << 6) | (bit(21) << 5) | (bit(22) << 4) | (bit(23) << 3) | (bit(41) << 2) | (bit(42) << 1) | bit(43);
    for k in 0..4 {
        gm[5 + k] = AMBE_PRBA58[b4 * 4 + k] as f64;
    }
    for i in 1..=8 {
        ri[i] = (1..=8).map(|m| (if m == 1 { 1.0 } else { 2.0 }) * gm[m] * (PI * (m - 1) as f64 * (i as f64 - 0.5) / 8.0).cos()).sum();
    }
    let rconst = 1.0 / (2.0 * std::f64::consts::SQRT_2);
    for i in 1..=4 {
        cik[i][1] = 0.5 * (ri[2 * i - 1] + ri[2 * i]);
        cik[i][2] = rconst * (ri[2 * i - 1] - ri[2 * i]);
    }

    // HOC
    let b5 = (bit(24) << 4) | (bit(25) << 3) | (bit(26) << 2) | (bit(27) << 1) | bit(44);
    let b6 = (bit(28) << 3) | (bit(29) << 2) | (bit(30) << 1) | bit(45);
    let b7 = (bit(31) << 3) | (bit(32) << 2) | (bit(33) << 1) | bit(46);
    let b8 = (bit(34) << 2) | (bit(47) << 1) | bit(48);
    let ji: [usize; 5] = [0, AMBE_LMPRBL[l_count * 4] as usize, AMBE_LMPRBL[l_count * 4 + 1] as usize, AMBE_LMPRBL[l_count * 4 + 2] as usize, AMBE_LMPRBL[l_count * 4 + 3] as usize];
    let hoc: [(&[f32], usize); 4] = [(&AMBE_HOCB5, b5), (&AMBE_HOCB6, b6), (&AMBE_HOCB7, b7), (&AMBE_HOCB8, b8)];
    for i in 1..=4 {
        let (tbl, bi) = hoc[i - 1];
        for k in 3..=ji[i] {
            cik[i][k] = if k > 6 { 0.0 } else { tbl[bi * 4 + k - 3] as f64 };
        }
    }
    // inverse DCT of each C(i,k) → T(l)
    let mut l = 1;
    for i in 1..=4 {
        for j in 1..=ji[i] {
            tl[l] = (1..=ji[i]).map(|k| (if k == 1 { 1.0 } else { 2.0 }) * cik[i][k] * (PI * (k - 1) as f64 * (j as f64 - 0.5) / ji[i] as f64).cos()).sum();
            l += 1;
        }
    }

    if cur.l > prev.l {
        for l in prev.l + 1..=cur.l {
            prev.ml[l] = prev.ml[prev.l];
            prev.log2ml[l] = prev.log2ml[prev.l];
        }
    }
    prev.log2ml[0] = prev.log2ml[1];
    prev.ml[0] = prev.ml[1];

    let mut sum43 = 0.0;
    for l in 1..=cur.l {
        flokl[l] = (prev.l as f64 / cur.l as f64) * l as f64;
        intkl[l] = flokl[l] as usize;
        deltal[l] = flokl[l] - intkl[l] as f64;
        sum43 += (1.0 - deltal[l]) * prev.log2ml[intkl[l]] + deltal[l] * prev.log2ml[intkl[l] + 1];
    }
    sum43 *= 0.65 / cur.l as f64;
    let sum42 = (1..=cur.l).map(|l| tl[l]).sum::<f64>() / cur.l as f64;
    let big_gamma = cur.gamma - 0.5 * ((cur.l as f64).ln() / 2f64.ln()) - sum42;
    for l in 1..=cur.l {
        let c1 = 0.65 * (1.0 - deltal[l]) * prev.log2ml[intkl[l]];
        let c2 = 0.65 * deltal[l] * prev.log2ml[intkl[l] + 1];
        cur.log2ml[l] = tl[l] + c1 + c2 - sum43 + big_gamma;
        cur.ml[l] = if cur.vl[l] == 1 { (0.693 * cur.log2ml[l]).exp() } else { unvc * (0.693 * cur.log2ml[l]).exp() };
    }
    Ambe::Voice
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Voice,
    Repeat,
    Muted,
    /// AMBE erasure / tone frame (silence here).
    Erasure,
    Tone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// mbelib's behaviour exactly.
    Mbelib,
    /// Trunk Recorder's synthesis + TIA concealment, with repeat thresholds
    /// for soft-decision FEC unless [`Decoder::hard_fec`] (the default).
    Enhanced,
    /// IMBE through Pavel Yazev's fixed-point decoder ([`fixed`], Trunk
    /// Recorder's `softVocoder: false`), with the same concealment rules and
    /// thresholds as `Enhanced`; AMBE as `Enhanced`. Listeners on WMATA found
    /// it the most natural-sounding (tools/vocoder-shootout).
    Fixed,
}

impl Profile {
    /// "enhanced", "fixed" or "mbelib".
    pub fn from_name(name: &str) -> Option<Profile> {
        match name.to_ascii_lowercase().as_str() {
            "enhanced" => Some(Profile::Enhanced),
            "fixed" | "fixed-point" => Some(Profile::Fixed),
            "mbelib" => Some(Profile::Mbelib),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Enhanced => "enhanced",
            Profile::Fixed => "fixed",
            Profile::Mbelib => "mbelib",
        }
    }
}

/// One vocoder stream (a P25 Phase 1 call, or one Phase 2 timeslot). Frames
/// must arrive in order; a new call wants [`Decoder::reset`].
pub struct Decoder {
    rand: Rng,
    uvq: usize,
    profile: Profile,
    cur: Parms,
    prev: Parms,
    enh: Parms,
    uv: UvSynth,
    er: f64,
    /// Smoothed spectral energy S_E (TIA), for adaptive smoothing.
    se: f64,
    /// Phase 1 frame repeat (TIA §7.7): E0 ≥ `repeat_e0` or
    /// ET ≥ `repeat_et` + 40·ER.
    repeat_e0: u32,
    repeat_et: f64,
    /// The fixed-point IMBE decoder ([`Profile::Fixed`]).
    fixed: Option<Box<fixed::FixedImbe>>,
}

/// Repeat thresholds for soft-decision FEC counts (the default). A soft
/// decoder corrects past the hard codes' limits, and it counts what it
/// corrected: E0 = 3 (the most a hard Golay(23,12) decoder corrects) or ET
/// of 10–15 is usually still a good frame. Measured on DCFD simulcast
/// against a cleaner receiver's audio of the same calls, playing such frames
/// beat repeating them 72–79 % of the time; at E0 ≥ 4 repeating wins.
const SOFT_REPEAT: (u32, f64) = (4, 16.0);
/// TIA-102.BABA-A's thresholds, for hard-decision counts (Trunk Recorder's).
const HARD_REPEAT: (u32, f64) = (3, 10.0);

impl Decoder {
    pub fn new(rand: Rng, profile: Profile) -> Self {
        let mut d = Decoder {
            rand,
            uvq: 3,
            profile,
            cur: Parms::default(),
            prev: Parms::default(),
            enh: Parms::default(),
            uv: UvSynth::new(),
            er: 0.0,
            se: 0.0,
            repeat_e0: SOFT_REPEAT.0,
            repeat_et: SOFT_REPEAT.1,
            fixed: (profile == Profile::Fixed).then(|| Box::new(fixed::FixedImbe::new())),
        };
        d.reset();
        d
    }

    /// Error counts come from hard-decision FEC: TIA's repeat thresholds.
    pub fn hard_fec(&mut self) {
        (self.repeat_e0, self.repeat_et) = HARD_REPEAT;
    }

    pub fn reset(&mut self) {
        init_parms(&mut self.cur, &mut self.prev, &mut self.enh);
        self.uv.reset();
        self.er = 0.0;
        self.se = 10000.0;
        if let Some(f) = self.fixed.as_mut() {
            f.reset();
        }
    }

    /// 88 information bits → 160 samples at mbelib's float scale (apply
    /// [`to_limited`] or [`to_unit`] for [−1, 1]). `e0`/`et`: FEC errors in u0 / in total;
    /// `erased`: the frame is known lost.
    pub fn imbe(&mut self, d: &[u8; 88], e0: u32, et: u32, erased: bool, out: &mut [f32; FRAME_SAMPLES]) -> Kind {
        if let Some(f) = self.fixed.as_mut() {
            let mut s = [0i16; FRAME_SAMPLES];
            let r = f.decode_checked(&fixed::frame_vector(d), e0, et, erased, self.repeat_e0, self.repeat_et as f32, &mut s);
            // Back to mbelib's float scale (to_limited / to_unit multiply by 7).
            for (o, &v) in out.iter_mut().zip(s.iter()) {
                *o = v as f32 * (32768.0 / 7.0 / 32768.0);
            }
            return match r {
                fixed::Out::Voice => Kind::Voice,
                fixed::Out::Repeat => Kind::Repeat,
                fixed::Out::Muted => Kind::Muted,
            };
        }
        if self.profile == Profile::Mbelib {
            let errs2 = if erased { 99 } else { et };
            let ok = decode_imbe4400_parms(d, &mut self.cur, &mut self.prev);
            let mut kind = Kind::Voice;
            if !ok || errs2 > 5 {
                move_parms(&self.prev, &mut self.cur);
                self.cur.repeat += 1;
                kind = Kind::Repeat;
            } else {
                self.cur.repeat = 0;
            }
            if self.cur.repeat <= 3 {
                move_parms(&self.cur, &mut self.prev);
                spectral_amp_enhance(&mut self.cur);
                synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, None, None);
                move_parms(&self.cur, &mut self.enh);
            } else {
                out.fill(0.0);
                init_parms(&mut self.cur, &mut self.prev, &mut self.enh);
                kind = Kind::Muted;
            }
            return kind;
        }
        if !erased {
            self.er = 0.95 * self.er + 0.000365 * et as f64;
        }
        if self.er > 0.0875 {
            self.fade_out(out);
            return Kind::Muted;
        }
        let ok = !erased && decode_imbe4400_parms(d, &mut self.cur, &mut self.prev);
        if !ok || e0 >= self.repeat_e0 || et as f64 >= self.repeat_et + 40.0 * self.er {
            if self.prev.repeat >= 4 {
                self.fade_out(out);
                return Kind::Muted;
            }
            move_parms(&self.prev, &mut self.cur);
            self.cur.repeat = self.prev.repeat + 1;
            self.speak(out, None);
            return Kind::Repeat;
        }
        self.cur.repeat = 0;
        self.speak(out, Some(et));
        Kind::Voice
    }

    /// 49 AMBE+2 information bits (mbelib's ambe_d, after FEC) → 160 samples.
    /// Repeat rules are mbelib's (`errs` > 3 repeats; Trunk Recorder left
    /// Phase 2's thresholds alone). "Enhanced" fades where mbelib cuts to
    /// silence and resets: past 3 repeats, and on tone / erasure frames.
    pub fn ambe(&mut self, d: &[u8; 49], errs: u32, out: &mut [f32; FRAME_SAMPLES]) -> Kind {
        let enhanced = self.profile != Profile::Mbelib;
        let r = decode_ambe2450_parms(d, &mut self.cur, &mut self.prev);
        let mut kind = Kind::Voice;
        match r {
            Ambe::Erasure => {
                self.cur.repeat = 0;
                kind = Kind::Erasure;
            }
            Ambe::Tone => {
                self.cur.repeat = 0;
                kind = Kind::Tone;
            }
            Ambe::Voice if errs > 3 => {
                move_parms(&self.prev, &mut self.cur);
                self.cur.repeat += 1;
                kind = Kind::Repeat;
            }
            Ambe::Voice => self.cur.repeat = 0,
        }
        if r == Ambe::Voice {
            if self.cur.repeat <= 3 {
                if enhanced {
                    self.speak(out, None);
                } else {
                    move_parms(&self.cur, &mut self.prev);
                    spectral_amp_enhance(&mut self.cur);
                    synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, None, None);
                    move_parms(&self.cur, &mut self.enh);
                }
            } else if enhanced {
                self.fade_out(out);
                kind = Kind::Muted;
            } else {
                out.fill(0.0);
                init_parms(&mut self.cur, &mut self.prev, &mut self.enh);
                kind = Kind::Muted;
            }
        } else if enhanced {
            self.fade_out(out);
        } else {
            out.fill(0.0);
            init_parms(&mut self.cur, &mut self.prev, &mut self.enh);
        }
        kind
    }

    /// `smooth_et`: a freshly decoded Phase 1 frame's error count, for TIA
    /// adaptive smoothing (a repeat already had it). Phase 2 skips it: Trunk
    /// Recorder runs it there with no error rate, where it only caps totals
    /// far above speech.
    fn speak(&mut self, out: &mut [f32; FRAME_SAMPLES], smooth_et: Option<u32>) {
        move_parms(&self.cur, &mut self.prev);
        let rm0 = spectral_amp_enhance(&mut self.cur);
        if let Some(et) = smooth_et {
            self.se = (0.95 * self.se + 0.05 * rm0).max(10000.0);
            adaptive_smoothing(&mut self.cur, self.se, self.er, et);
        }
        synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, Some(&ENHANCED), Some(&mut self.uv));
        move_parms(&self.cur, &mut self.enh);
    }

    /// A zero-amplitude frame holding the last good pitch: the previous
    /// frame fades out through the synthesis window.
    fn fade_out(&mut self, out: &mut [f32; FRAME_SAMPLES]) {
        move_parms(&self.prev, &mut self.cur);
        for l in 0..=56 {
            self.cur.ml[l] = 0.0;
            self.cur.vl[l] = 0;
        }
        synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, Some(&ENHANCED), Some(&mut self.uv));
        move_parms(&self.cur, &mut self.enh);
        move_parms(&self.prev, &mut self.cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 200 frames of one steady frame (harmonics voiced where
    /// `l % voiced_mod == 0`; 0: all unvoiced) through the enhanced synthesis,
    /// with colored-noise or mbelib multisine unvoiced synthesis.
    fn steady(smooth: bool, w0: f64, voiced_mod: usize) -> Vec<f32> {
        let mut rand = lcg(1);
        let (mut cur, mut prev, mut enh) = (Parms::default(), Parms::default(), Parms::default());
        init_parms(&mut cur, &mut prev, &mut enh);
        let mut uv = UvSynth::new();
        let mut audio = Vec::new();
        for _ in 0..200 {
            cur.w0 = w0;
            cur.l = 24;
            for l in 1..=cur.l {
                cur.ml[l] = 1.0;
                cur.vl[l] = (voiced_mod > 0 && l % voiced_mod == 0) as i8;
            }
            let mut out = [0f32; FRAME_SAMPLES];
            synthesize_speech(&mut out, &mut cur, &mut enh, 3, &mut rand, Some(&ENHANCED), smooth.then_some(&mut uv));
            move_parms(&cur, &mut enh);
            audio.extend_from_slice(&out);
        }
        audio.drain(..2 * FRAME_SAMPLES);
        audio
    }

    /// Mean |step| across frame boundaries over the mean |step| inside frames.
    fn boundary_ratio(a: &[f32]) -> f64 {
        let (mut at, mut na, mut inside, mut ni) = (0.0, 0, 0.0, 0);
        for i in 1..a.len() {
            let d = (a[i] - a[i - 1]).abs() as f64;
            if i % FRAME_SAMPLES == 0 {
                at += d;
                na += 1;
            } else {
                inside += d;
                ni += 1;
            }
        }
        (at / na as f64) / (inside / ni as f64)
    }

    fn rms(a: &[f32]) -> f64 {
        (a.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / a.len() as f64).sqrt()
    }

    #[test]
    fn unvoiced_noise_is_continuous_across_frames() {
        for vm in [0, 2] {
            let multisine = boundary_ratio(&steady(false, 0.05, vm));
            let smooth = boundary_ratio(&steady(true, 0.05, vm));
            assert!(multisine > 1.5, "mbelib's multisine noise should step at the boundary: {multisine}");
            assert!(smooth < 1.15, "boundary step with colored noise: {smooth}");
        }
        for w0 in [0.05, 0.08, 0.12] {
            let db = 20.0 * (rms(&steady(true, w0, 0)) / rms(&steady(false, w0, 0))).log10();
            eprintln!("w0 {w0}: colored noise {db:+.1} dB vs multisine");
        }
    }

    #[test]
    fn limiter_is_transparent_below_the_knee_and_smooth_above() {
        let unit = |v: f64| {
            let mut b = [(v * 32768.0 / 7.0) as f32];
            to_limited(&mut b);
            b[0] as f64
        };
        for x in [0.0, 0.1, -0.5, 0.69] {
            assert!((unit(x) - x).abs() < 1e-6, "{x} → {}", unit(x));
        }
        let mut last = 0.0;
        for i in 1..=4000 {
            let x = i as f64 / 1000.0;
            let y = unit(x);
            assert!(y >= last && y <= 1.0, "monotonic, within full scale: {x} → {y}");
            last = y;
        }
        // slope continuous at the knee
        let (h, k) = (1e-4, LIMIT_KNEE);
        assert!(((unit(k + h) - unit(k)) / h - 1.0).abs() < 1e-3);
        assert!((unit(-2.0) + unit(2.0)).abs() < 1e-7);
    }

    #[test]
    fn adaptive_smoothing_only_engages_under_errors() {
        let mut p = Parms { l: 20, ..Default::default() };
        for l in 1..=p.l {
            p.ml[l] = 500.0;
        }
        let clean = {
            let mut q = p.clone();
            adaptive_smoothing(&mut q, 1e4, 0.0, 0);
            q
        };
        assert_eq!(clean.ml, p.ml);
        let mut q = p.clone();
        adaptive_smoothing(&mut q, 1e4, 0.02, 8);
        let am: f64 = (1..=q.l).map(|l| q.ml[l]).sum();
        assert!((am - 3600.0).abs() < 1e-6, "total amplitude capped at 6000 - 300·ET: {am}");
        assert!((1..=q.l).all(|l| q.vl[l] == 1), "loud harmonics forced voiced");
    }
}
