//! IMBE 7200x4400 (P25 Phase 1) vocoder: mbelib 1.3.0 (ISC) with Trunk
//! Recorder's "enhanced" synthesis and TIA-102.BABA-A §7.7/§7.8 error
//! concealment. Double precision throughout; the RNG is injectable so output
//! can be compared sample by sample with the reference implementations.
//! AMBE+2 (Phase 2) is still to come.

use std::f64::consts::{E, PI};

use crate::tables::{B2, BA, BO, HOBA, IMBE_JI, QUANTSTEP, STANDDEV, WS};

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

fn spectral_amp_enhance(cur: &mut Parms) {
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

/// mbe_synthesizeSpeechf; `opts` None = mbelib exactly.
fn synthesize_speech(out: &mut [f32; FRAME_SAMPLES], cur: &mut Parms, prev: &mut Parms, uvq: usize, rand: &mut Rng, opts: Option<&SynthOpts>) {
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

    let mut rphase = [0.0f64; 64];
    let mut rphase2 = [0.0f64; 64];
    for l in 1..=maxl {
        let lf = l as f64;
        let (cw0l, pw0l) = (cw0 * lf, pw0 * lf);
        let (cv, pv) = (cur.vl[l], prev.vl[l]);
        if cv == 0 && pv == 1 {
            for r in rphase.iter_mut().take(uvquality) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for n in 0..N {
                let nf = n as f64;
                let c1 = WS[n + N] as f64 * m_old[l] * (pw0l * nf + prev.phil[l]).cos();
                let mut c3 = 0.0;
                for i in 0..uvquality {
                    c3 += (cw0 * nf * (lf + i as f64 * uvstep - uvoffset) + rphase[i]).cos();
                    if cw0l > uvthreshold {
                        c3 += (cw0l - uvthreshold) * uvrand * uv_noise(rand);
                    }
                }
                c3 = c3 * uvsine * WS[n] as f64 * m_new[l] * qfactor;
                out[n] = (out[n] as f64 + (c1 + c3)) as f32;
            }
        } else if cv == 1 && pv == 0 {
            for r in rphase.iter_mut().take(uvquality) {
                *r = rand() * (PI * 2.0) - PI;
            }
            for n in 0..N {
                let nf = n as f64;
                let c1 = WS[n] as f64 * m_new[l] * (cw0l * (nf - N as f64) + cur.phil[l]).cos();
                let mut c3 = 0.0;
                for i in 0..uvquality {
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
        } else {
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
}

/// mbelib's float → short gain (×7, clipped at ±32760), scaled to [−1, 1].
pub fn to_unit(buf: &mut [f32]) {
    for v in buf.iter_mut() {
        *v = ((7.0 * *v as f64).clamp(-32760.0, 32760.0) / 32768.0) as f32;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Voice,
    Repeat,
    Muted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// mbelib's behaviour exactly.
    Mbelib,
    /// Trunk Recorder's synthesis + TIA concealment (the default).
    Enhanced,
}

/// One IMBE stream (a P25 Phase 1 call). Frames must arrive in order; a new
/// call wants [`Decoder::reset`].
pub struct Decoder {
    rand: Rng,
    uvq: usize,
    profile: Profile,
    cur: Parms,
    prev: Parms,
    enh: Parms,
    er: f64,
}

impl Decoder {
    pub fn new(rand: Rng, profile: Profile) -> Self {
        let mut d = Decoder { rand, uvq: 3, profile, cur: Parms::default(), prev: Parms::default(), enh: Parms::default(), er: 0.0 };
        d.reset();
        d
    }

    pub fn reset(&mut self) {
        init_parms(&mut self.cur, &mut self.prev, &mut self.enh);
        self.er = 0.0;
    }

    /// 88 information bits → 160 samples at mbelib's float scale (apply
    /// [`to_unit`] for [−1, 1]). `e0`/`et`: FEC errors in u0 / in total;
    /// `erased`: the frame is known lost.
    pub fn imbe(&mut self, d: &[u8; 88], e0: u32, et: u32, erased: bool, out: &mut [f32; FRAME_SAMPLES]) -> Kind {
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
                synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, None);
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
        if !ok || e0 >= 3 || et as f64 >= 10.0 + 40.0 * self.er {
            if self.prev.repeat >= 4 {
                self.fade_out(out);
                return Kind::Muted;
            }
            move_parms(&self.prev, &mut self.cur);
            self.cur.repeat = self.prev.repeat + 1;
            self.speak(out);
            return Kind::Repeat;
        }
        self.cur.repeat = 0;
        self.speak(out);
        Kind::Voice
    }

    fn speak(&mut self, out: &mut [f32; FRAME_SAMPLES]) {
        move_parms(&self.cur, &mut self.prev);
        spectral_amp_enhance(&mut self.cur);
        synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, Some(&ENHANCED));
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
        synthesize_speech(out, &mut self.cur, &mut self.enh, self.uvq, &mut self.rand, Some(&ENHANCED));
        move_parms(&self.cur, &mut self.enh);
        move_parms(&self.prev, &mut self.cur);
    }
}
