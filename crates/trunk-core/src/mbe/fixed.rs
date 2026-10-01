//! Pavel Yazev's fixed-point IMBE decoder (2009, GPLv3), as vendored by Trunk
//! Recorder in lib/op25_repeater/lib/imbe_vocoder and used there when
//! `softVocoder` is false — ported line for line so its output is
//! sample-identical (`tools/vocoder-shootout` checks it against TR's build).
//!
//! It follows the TIA reference decoder's structure with ETSI basic operators
//! (saturating 16/32-bit arithmetic): parameter unpacking, spectral amplitude
//! prediction, TIA spectral enhancement, voiced synthesis with linear phase
//! plus TIA's random phase on upper harmonics, and unvoiced synthesis from
//! band-limited noise by a 256-point inverse FFT with weighted overlap-add.
//! Listeners on WMATA found it the most natural of the decoders compared.
//!
//! Differences from the C: the noise generator is per decoder (the C keeps
//! one file-static seed shared by every recorder in the process), and a
//! division that would abort() clamps instead.

use super::fixed_tables::*;

pub const FRAME: usize = 160;
const NUM_HARMS_MAX: usize = 56;
const NUM_HARMS_MIN: i16 = 9;
const NUM_BANDS_MAX: i16 = 12;
const MAX_BLOCK_LEN: usize = 10;
const NUM_PRED_RES_BLKS: usize = 6;
const B_NUM: usize = NUM_HARMS_MAX - 1;
const BIT_STREAM_LEN: usize = 3 + 3 * 12 + 3 * 11 + 3;
const FFTLENGTH: usize = 256;

const CNST_0_9254_Q0_16: u32 = 60647;
const CNST_0_33_Q0_16: u32 = 0x5556;
const CNST_ONE_Q8_24: u32 = 0x0100_0000;
const CNST_0_7_Q1_15: i16 = 0x599A;
const CNST_0_4_Q1_15: i16 = 0x3333;
const CNST_0_03_Q1_15: i16 = 0x03D7;
const CNST_0_05_Q1_15: i16 = 0x0666;
const CNST_0_1_Q1_15: i16 = 0x0CCD;
const CNST_0_9898_Q1_15: i16 = 0x7EB3;
const CNST_1_2_Q2_14: i16 = 0x4CCC;
const CNST_0_5_Q2_14: i16 = 0x2000;
const CNST_0_5_Q1_15: u16 = 0x4000;
const CNST_1_0_Q1_15: u16 = 0x7FFF;
const CNST_0_5_Q5_11: i16 = 0x0400;
const ONE_Q15: i16 = 32767;
const X05_Q15: i16 = 16384;
const MAX_32: i32 = 0x7fff_ffff;

// ---------------------------------------------------------------- ETSI basic operators

fn saturate(l: i32) -> i16 {
    l.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}
fn add(a: i16, b: i16) -> i16 {
    saturate(a as i32 + b as i32)
}
fn sub(a: i16, b: i16) -> i16 {
    saturate(a as i32 - b as i32)
}
fn negate(a: i16) -> i16 {
    if a == i16::MIN { i16::MAX } else { -a }
}
fn shl(a: i16, n: i16) -> i16 {
    if n < 0 {
        return shr(a, -(n.max(-16)));
    }
    let r = (a as i32).wrapping_mul(1i32.wrapping_shl(n as u32));
    if (n > 15 && a != 0) || r != r as i16 as i32 { if a > 0 { i16::MAX } else { i16::MIN } } else { r as i16 }
}
fn shr(a: i16, n: i16) -> i16 {
    if n < 0 {
        return shl(a, -(n.max(-16)));
    }
    if n >= 15 { if a < 0 { -1 } else { 0 } } else { a >> n }
}
fn mult(a: i16, b: i16) -> i16 {
    let mut p = (a as i32 * b as i32) & (0xffff_8000u32 as i32);
    p >>= 15;
    if p & 0x0001_0000 != 0 {
        p |= 0xffff_0000u32 as i32;
    }
    saturate(p)
}
fn mult_r(a: i16, b: i16) -> i16 {
    let mut p = a as i32 * b as i32 + 0x4000;
    p &= 0xffff_8000u32 as i32;
    p >>= 15;
    if p & 0x0001_0000 != 0 {
        p |= 0xffff_0000u32 as i32;
    }
    saturate(p)
}
fn l_mult(a: i16, b: i16) -> i32 {
    let p = a as i32 * b as i32;
    if p != 0x4000_0000 { p * 2 } else { MAX_32 }
}
fn l_add(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}
fn l_sub(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}
fn l_mac(acc: i32, a: i16, b: i16) -> i32 {
    l_add(acc, l_mult(a, b))
}
fn l_msu(acc: i32, a: i16, b: i16) -> i32 {
    l_sub(acc, l_mult(a, b))
}
fn l_shl(mut a: i32, n: i16) -> i32 {
    if n <= 0 {
        return l_shr(a, -(n.max(-32)));
    }
    let mut o = 0;
    for _ in 0..n {
        if a > 0x3fff_ffff {
            return MAX_32;
        }
        if a < (0xc000_0000u32 as i32) {
            return i32::MIN;
        }
        a *= 2;
        o = a;
    }
    o
}
fn l_shr(a: i32, n: i16) -> i32 {
    if n < 0 {
        return l_shl(a, -(n.max(-32)));
    }
    if n >= 31 { if a < 0 { -1 } else { 0 } } else { a >> n }
}
fn l_shr_r(a: i32, n: i16) -> i32 {
    if n > 31 {
        return 0;
    }
    let mut o = l_shr(a, n);
    if n > 0 && a & (1i32 << (n - 1)) != 0 {
        o = o.wrapping_add(1);
    }
    o
}
fn l_abs(a: i32) -> i32 {
    if a == i32::MIN { MAX_32 } else { a.abs() }
}
fn extract_h(a: i32) -> i16 {
    (a >> 16) as i16
}
fn extract_l(a: i32) -> i16 {
    a as i16
}
fn l_deposit_h(a: i16) -> i32 {
    (a as i32) << 16
}
fn l_deposit_l(a: i16) -> i32 {
    a as i32
}
fn round(a: i32) -> i16 {
    extract_h(l_add(a, 0x8000))
}
fn norm_s(a: i16) -> i16 {
    if a == 0 {
        return 0;
    }
    if a == -1 {
        return 15;
    }
    let mut v = if a < 0 { !a } else { a };
    let mut n = 0;
    while v < 0x4000 {
        v <<= 1;
        n += 1;
    }
    n
}
fn norm_l(a: i32) -> i16 {
    if a == 0 {
        return 0;
    }
    if a == -1 {
        return 31;
    }
    let mut v = if a < 0 { !a } else { a };
    let mut n = 0;
    while v < 0x4000_0000 {
        v <<= 1;
        n += 1;
    }
    n
}
/// var1 / var2 in Q15 for 0 ≤ var1 ≤ var2; the C aborts outside that, here it clamps.
fn div_s(a: i16, b: i16) -> i16 {
    if b <= 0 || a < 0 {
        return 0;
    }
    if a >= b {
        return i16::MAX;
    }
    if a == 0 {
        return 0;
    }
    let (mut num, den, mut out) = (a as i32, b as i32, 0i16);
    for _ in 0..15 {
        out <<= 1;
        num <<= 1;
        if num >= den {
            num = l_sub(num, den);
            out = add(out, 1);
        }
    }
    out
}

// ---------------------------------------------------------------- math_sub

const POW2_TABLE: [i16; 33] = [
    16384, 16743, 17109, 17484, 17867, 18258, 18658, 19066, 19484, 19911, 20347, 20792, 21247, 21713, 22188, 22674, 23170, 23678, 24196, 24726,
    25268, 25821, 26386, 26964, 27554, 28158, 28774, 29405, 30048, 30706, 31379, 32066, 32767,
];

const COS_TABLE: [i16; 129] = [
    32767, 32766, 32758, 32746, 32729, 32706, 32679, 32647, 32610, 32568, 32522, 32470, 32413, 32352, 32286, 32214, 32138, 32058, 31972, 31881,
    31786, 31686, 31581, 31471, 31357, 31238, 31114, 30986, 30853, 30715, 30572, 30425, 30274, 30118, 29957, 29792, 29622, 29448, 29269, 29086,
    28899, 28707, 28511, 28311, 28106, 27897, 27684, 27467, 27246, 27020, 26791, 26557, 26320, 26078, 25833, 25583, 25330, 25073, 24812, 24548,
    24279, 24008, 23732, 23453, 23170, 22884, 22595, 22302, 22006, 21706, 21403, 21097, 20788, 20475, 20160, 19841, 19520, 19195, 18868, 18538,
    18205, 17869, 17531, 17190, 16846, 16500, 16151, 15800, 15447, 15091, 14733, 14373, 14010, 13646, 13279, 12910, 12540, 12167, 11793, 11417,
    11039, 10660, 10279, 9896, 9512, 9127, 8740, 8351, 7962, 7571, 7180, 6787, 6393, 5998, 5602, 5205, 4808, 4410, 4011, 3612, 3212, 2811, 2411,
    2009, 1608, 1206, 804, 402, 0,
];

const SQRT_TABLE: [i16; 49] = [
    16384, 16888, 17378, 17854, 18318, 18770, 19212, 19644, 20066, 20480, 20886, 21283, 21674, 22058, 22435, 22806, 23170, 23530, 23884, 24232,
    24576, 24915, 25249, 25580, 25905, 26227, 26545, 26859, 27170, 27477, 27780, 28081, 28378, 28672, 28963, 29251, 29537, 29819, 30099, 30377,
    30652, 30924, 31194, 31462, 31727, 31991, 32252, 32511, 32767,
];

/// 2^x, x in signed Q10.22; result Q14.2.
fn pow2(x: i32) -> i16 {
    let mut exponent = extract_h(l_shr(x, 6));
    if exponent < 0 {
        exponent = add(exponent, 1);
    }
    let mut fraction = extract_l(l_shr(l_sub(x, l_shl(l_deposit_l(exponent), 6 + 16)), 7));
    if x < 0 {
        fraction = negate(fraction);
    }
    let mut lx = l_mult(fraction, 32);
    let i = (extract_h(lx) as usize).min(31);
    lx = l_shr(lx, 1);
    let a = extract_l(lx) & 0x7fff;
    lx = l_deposit_h(POW2_TABLE[i]);
    let tmp = sub(POW2_TABLE[i], POW2_TABLE[i + 1]);
    lx = l_msu(lx, tmp, a);
    if x < 0 {
        lx = l_deposit_h(div_s(0x4000, extract_h(lx)));
        exponent = sub(exponent, 1);
    }
    extract_h(l_shr_r(lx, sub(12, exponent)))
}

/// A 32-bit by 16-bit multiply, the 32-bit operand truncated to 31 bits.
fn l_mpy_ls(l: i32, v: i16) -> i32 {
    let sw = shr(extract_l(l), 1) & 32767;
    let o = l_shr(l_mult(v, sw), 15);
    l_mac(o, v, extract_h(l))
}

/// cos(π·x), x in Q1.15.
fn cos_fxp(x: i16) -> i16 {
    let mut tx = if x < 0 { negate(x) } else { x };
    let mut sign = false;
    if tx > X05_Q15 {
        tx = sub(ONE_Q15, tx);
        sign = true;
    }
    let i1 = shr(tx, 7);
    if i1 == 128 {
        return 0;
    }
    let m = shl(sub(tx, shl(i1, 7)), 8);
    let i1 = i1 as usize;
    let ty = add(COS_TABLE[i1], mult(m, sub(COS_TABLE[i1 + 1], COS_TABLE[i1])));
    if sign { negate(ty) } else { ty }
}

fn sin_fxp(x: i16) -> i16 {
    let (mut tx, sign) = if x < 0 { (negate(x), true) } else { (x, false) };
    tx = if tx > X05_Q15 { sub(tx, X05_Q15) } else { sub(X05_Q15, tx) };
    let ty = cos_fxp(tx);
    if sign { negate(ty) } else { ty }
}

/// sqrt(x), x in Q1.31; returns the root and the right shift still to apply.
fn sqrt_l_exp(mut x: i32) -> (i32, i16) {
    if x <= 0 {
        return (0, 0);
    }
    let e = norm_l(x) & !1;
    x = l_shl(x, e);
    x = l_shr(x, 9);
    let i = extract_h(x);
    x = l_shr(x, 1);
    let a = extract_l(x) & 0x7fff;
    let i = (sub(i, 16) as usize).min(47);
    let y = l_deposit_h(SQRT_TABLE[i]);
    let tmp = sub(SQRT_TABLE[i], SQRT_TABLE[i + 1]);
    (l_msu(y, tmp, a), e >> 1)
}

// ---------------------------------------------------------------- aux / qnt

fn l_v_magsq(v: &[i16]) -> i32 {
    v.iter().fold(0, |acc, &x| l_mac(acc, x, x))
}

fn deqnt_by_step(qval: i16, step: u16, bits: i16) -> i32 {
    if bits == 0 {
        return 0;
    }
    let res = (step as i32).wrapping_mul(qval as i32 - (1i32 << (bits - 1)));
    l_add(res, (step as i32 * 0x8000) >> 16)
}

fn bit_allocation(num_harms: i16, out: &mut [i16]) {
    let off = if num_harms == NUM_HARMS_MIN {
        0
    } else {
        let index = (num_harms - NUM_HARMS_MIN - 1) as usize;
        BIT_ALLOCATION_OFFSET_TBL[index >> 2] as usize + (3 + (index >> 2)) * (index & 3)
    };
    for (n, i) in (0..(num_harms - 1).max(0) as usize).step_by(4).enumerate() {
        let mut t = BIT_ALLOCATION_TBL.get(off + n).copied().unwrap_or(0);
        for j in (0..4).rev() {
            if let Some(o) = out.get_mut(i + j) {
                *o = (t & 0xF) as i16;
            }
            t >>= 4;
        }
    }
}

// ---------------------------------------------------------------- the decoder

#[derive(Clone, Copy)]
struct Param {
    fund_freq: i32,
    num_harms: i16,
    num_bands: i16,
    v_uv_dsn: [i16; NUM_HARMS_MAX],
    b_vec: [i16; NUM_HARMS_MAX + 3],
    bit_alloc: [i16; B_NUM + 4],
    sa: [i16; NUM_HARMS_MAX],
    l_uv: i16,
    div_one_by_num_harm: i16,
    div_one_by_num_harm_sh: i16,
}

impl Default for Param {
    fn default() -> Self {
        Param {
            fund_freq: 0,
            num_harms: 0,
            num_bands: 0,
            v_uv_dsn: [0; NUM_HARMS_MAX],
            b_vec: [0; NUM_HARMS_MAX + 3],
            bit_alloc: [0; B_NUM + 4],
            sa: [0; NUM_HARMS_MAX],
            l_uv: 0,
            div_one_by_num_harm: 0,
            div_one_by_num_harm_sh: 0,
        }
    }
}

/// What a frame became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    Voice,
    Repeat,
    Muted,
}

pub struct FixedImbe {
    seed: u32,
    param: Param,
    last: Option<Param>,
    er: f32,
    rpt: u32,
    // v_synt
    ph_mem: [u32; NUM_HARMS_MAX],
    vu_dsn_prev: [i16; NUM_HARMS_MAX],
    sa_prev3: [i16; NUM_HARMS_MAX],
    num_harms_prev3: i16,
    fund_freq_prev: i32,
    // uv_synt
    uv_mem: [i16; 105],
    wr: [i16; FFTLENGTH / 2 + 1],
    wi: [i16; FFTLENGTH / 2 + 1],
    // sa_decode
    num_harms_prev1: i16,
    sa_prev1: [i32; NUM_HARMS_MAX + 2],
}

impl Default for FixedImbe {
    fn default() -> Self {
        Self::new()
    }
}

impl FixedImbe {
    pub fn new() -> Self {
        let mut d = FixedImbe {
            seed: 1,
            param: Param::default(),
            last: None,
            er: 0.0,
            rpt: 0,
            ph_mem: [0; NUM_HARMS_MAX],
            vu_dsn_prev: [0; NUM_HARMS_MAX],
            sa_prev3: [0; NUM_HARMS_MAX],
            num_harms_prev3: 0,
            fund_freq_prev: 0,
            uv_mem: [0; 105],
            wr: [0; FFTLENGTH / 2 + 1],
            wi: [0; FFTLENGTH / 2 + 1],
            num_harms_prev1: 0,
            sa_prev1: [0; NUM_HARMS_MAX + 2],
        };
        d.reset();
        d
    }

    /// decode_init: a fresh stream (the noise generator restarts too).
    pub fn reset(&mut self) {
        self.seed = 1;
        // v_synt_init
        for i in 0..NUM_HARMS_MAX {
            self.ph_mem[i] = l_deposit_h(self.rand_gen()) as u32;
            self.vu_dsn_prev[i] = 0;
        }
        self.sa_prev3 = [0; NUM_HARMS_MAX];
        self.num_harms_prev3 = 0;
        self.fund_freq_prev = 0;
        // uv_synt_init
        self.fft_init();
        self.uv_mem = [0; 105];
        // sa_decode_init
        self.num_harms_prev1 = 30;
        self.sa_prev1 = [0; NUM_HARMS_MAX + 2];
        self.param = Param { fund_freq: 0x0cf6_474a, num_harms: 9, num_bands: 3, ..Param::default() };
        self.last = None;
        self.er = 0.0;
        self.rpt = 0;
    }

    /// Pseudo-random numbers in −1…1 (Park–Miller), Q1.15.
    fn rand_gen(&mut self) -> i16 {
        let s = self.seed;
        let mut lo = 16807u32.wrapping_mul(s & 0xFFFF);
        let hi = 16807u32.wrapping_mul(s >> 16);
        lo = lo.wrapping_add((hi & 0x7FFF) << 16);
        lo = lo.wrapping_add(hi >> 15);
        if lo > 0x7FFF_FFFF {
            lo -= 0x7FFF_FFFF;
        }
        self.seed = lo;
        lo as i16
    }

    /// One frame with TIA repeat and mute (imbe_decode_checked): repeat when
    /// the pitch index is invalid, E0 ≥ `repeat_e0` or ET ≥ ⌊`repeat_et` +
    /// 40·ER⌋; mute after four repeats or when ER > 0.0875. `fv`: u0..u6 and
    /// u7 (7 bits, unshifted); `erased`: the frame is known lost.
    pub fn decode_checked(&mut self, fv: &[i16; 8], e0: u32, et: u32, erased: bool, repeat_e0: u32, repeat_et: f32, snd: &mut [i16; FRAME]) -> Out {
        let b0 = ((fv[0] >> 4) & 0xfc) | ((fv[7] >> 1) & 0x3);
        if !erased {
            self.er = 0.95 * self.er + 0.000365 * et as f32;
        }
        if self.er > 0.0875 {
            self.mute(snd);
            return Out::Muted;
        }
        if erased || b0 > 207 || e0 >= repeat_e0 || et >= (repeat_et + 40.0 * self.er) as u32 {
            self.rpt += 1;
            if self.rpt >= 4 {
                self.mute(snd);
                return Out::Muted;
            }
            self.repeat(snd);
            return Out::Repeat;
        }
        self.rpt = 0;
        self.decode(fv, snd);
        Out::Voice
    }

    /// One frame, no concealment (imbe_decode).
    pub fn decode(&mut self, fv: &[i16; 8], snd: &mut [i16; FRAME]) {
        let mut p = self.param;
        decode_frame_vector(&mut p, fv);
        v_uv_decode(&mut p);
        self.sa_decode(&mut p);
        sa_enh(&mut p);
        self.last = Some(p);
        self.param = p;
        self.synth(snd);
    }

    fn repeat(&mut self, snd: &mut [i16; FRAME]) {
        match self.last {
            Some(l) => {
                self.param = l;
                self.synth(snd);
            }
            None => self.mute(snd),
        }
    }

    fn mute(&mut self, snd: &mut [i16; FRAME]) {
        if let Some(l) = self.last {
            self.param = l;
        }
        self.param.sa = [0; NUM_HARMS_MAX];
        self.param.v_uv_dsn = [0; NUM_HARMS_MAX];
        self.param.l_uv = self.param.num_harms;
        self.synth(snd);
    }

    fn synth(&mut self, snd: &mut [i16; FRAME]) {
        let p = self.param;
        self.v_synt(&p, snd);
        let mut uv = [0i16; FRAME];
        self.uv_synt(&p, &mut uv);
        for (s, u) in snd.iter_mut().zip(uv) {
            *s = add(*s, u);
        }
    }

    fn sa_decode(&mut self, p: &mut Param) {
        let num_harms = p.num_harms;
        let index = (num_harms - NUM_HARMS_MIN) as usize;
        let mut gain_vec = [0i16; 6];
        let mut gain_r = [0i16; 6];
        let mut ba = 0usize;
        let mut bv = 2usize;
        gain_vec[0] = GAIN_QNT_TBL[(p.b_vec[bv] as usize).min(63)];
        bv += 1;
        for i in 1..6 {
            let gss = GAIN_STEP_SIZE_TBL[index * 5 + i - 1];
            gain_vec[i] = extract_l(l_shr(deqnt_by_step(p.b_vec[bv], gss, p.bit_alloc[ba]), 5));
            bv += 1;
            ba += 1;
        }
        idct(&gain_vec, NUM_PRED_RES_BLKS as i16, NUM_PRED_RES_BLKS as i16, &mut gain_r);
        let mut lmprbl = LMPRBL_TBL[index];
        let mut t_vec = [0i16; NUM_HARMS_MAX];
        let mut at = 0usize;
        for &g in gain_r.iter() {
            let bl_len = ((lmprbl >> 28) & 0xF) as usize;
            lmprbl <<= 4;
            let mut c_vec = [0i16; MAX_BLOCK_LEN];
            c_vec[0] = g;
            for j in 1..bl_len {
                let num_bits = p.bit_alloc[ba];
                ba += 1;
                if num_bits > 0 {
                    let step = extract_h(((HI_ORD_STD_TBL[j - 1] as i32).wrapping_mul(HI_ORD_STEP_SIZE_TBL[num_bits as usize - 1] as i32)).wrapping_shl(1));
                    c_vec[j] = extract_l(l_shr(deqnt_by_step(p.b_vec[bv], step as u16, num_bits), 5));
                }
                bv += 1;
            }
            let end = (at + bl_len).min(NUM_HARMS_MAX);
            idct(&c_vec, bl_len as i16, (end - at) as i16, &mut t_vec[at..end]);
            at = end;
        }
        let prev = self.num_harms_prev1;
        let k_coef: u32 = if num_harms == prev {
            CNST_ONE_Q8_24
        } else if num_harms > prev {
            (div_s(prev << 9, num_harms << 9) as i32 as u32) << 9
        } else {
            let (mut k, mut tmp) = (0u32, prev);
            while tmp > num_harms {
                tmp -= num_harms;
                k = k.wrapping_add(CNST_ONE_Q8_24);
            }
            k.wrapping_add((div_s(tmp << 9, num_harms << 9) as i32 as u32) << 9)
        };
        let ro: i16 = if num_harms <= 15 {
            CNST_0_4_Q1_15
        } else if num_harms <= 24 {
            (num_harms as i32 * CNST_0_03_Q1_15 as i32 - CNST_0_05_Q1_15 as i32) as i16
        } else {
            CNST_0_7_Q1_15
        };
        let mut k_acc = k_coef;
        let mut sum = 0i32;
        for i in (prev as usize + 1)..NUM_HARMS_MAX + 2 {
            self.sa_prev1[i] = self.sa_prev1[prev as usize];
        }
        let mut sa_tmp = [0i32; NUM_HARMS_MAX];
        for (i, st) in sa_tmp.iter_mut().enumerate().take(num_harms as usize) {
            let idx = ((k_acc >> 24) as usize).min(NUM_HARMS_MAX);
            let si = ((k_acc.wrapping_sub((idx as u32) << 24)) >> 9) as i16;
            if si == 0 {
                let t = l_mpy_ls(self.sa_prev1[idx], ro);
                *st = l_add(l_shr(l_deposit_h(t_vec[i]), 5), t);
                sum = l_add(sum, self.sa_prev1[idx]);
            } else {
                let t = l_mpy_ls(self.sa_prev1[idx], sub(0x7FFF, si));
                sum = l_add(sum, t);
                *st = l_add(l_shr(l_deposit_h(t_vec[i]), 5), l_mpy_ls(t, ro));
                let t = l_mpy_ls(self.sa_prev1[idx + 1], si);
                sum = l_add(sum, t);
                *st = l_add(*st, l_mpy_ls(t, ro));
            }
            k_acc = k_acc.wrapping_add(k_coef);
        }
        let sh = norm_s(num_harms);
        let inv = div_s(0x4000, num_harms << sh);
        p.div_one_by_num_harm_sh = sh;
        p.div_one_by_num_harm = inv;
        let sum = l_shr(l_mpy_ls(l_mpy_ls(sum, ro), inv), 14 - sh);
        for i in 1..=num_harms as usize {
            self.sa_prev1[i] = l_sub(sa_tmp[i - 1], sum);
            p.sa[i - 1] = pow2(self.sa_prev1[i]);
        }
        self.num_harms_prev1 = num_harms;
    }

    fn v_synt(&mut self, p: &Param, snd: &mut [i16; FRAME]) {
        let fund_freq = p.fund_freq;
        let num_harms = p.num_harms;
        let (inv, inv_sh, num_uv) = (p.div_one_by_num_harm, p.div_one_by_num_harm_sh, p.l_uv);
        let vu = &p.v_uv_dsn;
        let sa = &p.sa;
        let mut l_snd = [0i32; FRAME];
        let mut ph_mem_prev = [0u32; NUM_HARMS_MAX];
        let mut dph = [0u32; NUM_HARMS_MAX];
        // Phases at the frame boundary (integer multiplication mod 1).
        let l_tmp = ((self.fund_freq_prev.wrapping_add(fund_freq) >> 7).wrapping_mul(FRAME as i32) / 2).wrapping_shl(7);
        let mut acc = 0i32;
        for i in 0..NUM_HARMS_MAX {
            ph_mem_prev[i] = self.ph_mem[i];
            acc = acc.wrapping_add(l_tmp);
            self.ph_mem[i] = self.ph_mem[i].wrapping_add(acc as u32);
        }
        let num_harms_max = num_harms.max(self.num_harms_prev3) as usize;
        let num_harms_max_4 = num_harms_max >> 2;
        let freq_flag = l_abs(l_sub(fund_freq, self.fund_freq_prev)) >= l_mpy_ls(fund_freq, CNST_0_1_Q1_15);
        let (mut ph_step, mut ph_step_prev) = (0i32, 0i32);
        for i in 0..num_harms_max.min(NUM_HARMS_MAX) {
            ph_step = ph_step.wrapping_add(fund_freq);
            ph_step_prev = ph_step_prev.wrapping_add(self.fund_freq_prev);
            if i > num_harms_max_4 {
                dph[i] = if num_uv == num_harms {
                    l_deposit_h(self.rand_gen()) as u32
                } else {
                    let t = l_mult(self.rand_gen(), inv);
                    l_shr(t, 15 - inv_sh).wrapping_mul(num_uv as i32) as u32
                };
                self.ph_mem[i] = self.ph_mem[i].wrapping_add(dph[i]);
            }
            let (v, vp) = (vu[i], self.vu_dsn_prev[i]);
            if v == 0 && vp == 0 {
                continue;
            }
            let start = |step: i32, mem: u32| (mem as i32).wrapping_sub((step >> 7).wrapping_mul(104).wrapping_shl(7));
            if v == 1 && vp == 0 {
                // unvoiced → voiced: fade in
                let mut ph = start(ph_step, self.ph_mem[i]);
                for j in 56..=104 {
                    let t = l_mpy_ls(l_mult(WS[j - 56], sa[i]), cos_fxp(extract_h(ph)));
                    l_snd[j] = l_add(l_snd[j], l_shr(t, 1));
                    ph = ph.wrapping_add(ph_step);
                }
                for s in l_snd.iter_mut().take(160).skip(105) {
                    *s = l_add(*s, l_shr(l_mult(sa[i], cos_fxp(extract_h(ph))), 1));
                    ph = ph.wrapping_add(ph_step);
                }
                continue;
            }
            if v == 0 && vp == 1 {
                // voiced → unvoiced: fade out
                let mut ph = ph_mem_prev[i] as i32;
                for s in l_snd.iter_mut().take(56) {
                    *s = l_add(*s, l_shr(l_mult(self.sa_prev3[i], cos_fxp(extract_h(ph))), 1));
                    ph = ph.wrapping_add(ph_step_prev);
                }
                for j in 56..=104 {
                    let t = l_mpy_ls(l_mult(WS[48 - (j - 56)], self.sa_prev3[i]), cos_fxp(extract_h(ph)));
                    l_snd[j] = l_add(l_snd[j], l_shr(t, 1));
                    ph = ph.wrapping_add(ph_step_prev);
                }
                continue;
            }
            if i >= 7 || freq_flag {
                // voiced → voiced, cross-faded
                let mut ph_aux = ph_mem_prev[i] as i32;
                let mut ph = start(ph_step, self.ph_mem[i]);
                for s in l_snd.iter_mut().take(56) {
                    *s = l_add(*s, l_shr(l_mult(self.sa_prev3[i], cos_fxp(extract_h(ph_aux))), 1));
                    ph_aux = ph_aux.wrapping_add(ph_step_prev);
                }
                for j in 56..=104 {
                    let t = l_mpy_ls(l_mult(WS[48 - (j - 56)], self.sa_prev3[i]), cos_fxp(extract_h(ph_aux)));
                    l_snd[j] = l_add(l_snd[j], l_shr(t, 1));
                    let t = l_mpy_ls(l_mult(WS[j - 56], sa[i]), cos_fxp(extract_h(ph)));
                    l_snd[j] = l_add(l_snd[j], l_shr(t, 1));
                    ph_aux = ph_aux.wrapping_add(ph_step_prev);
                    ph = ph.wrapping_add(ph_step);
                }
                for s in l_snd.iter_mut().take(160).skip(105) {
                    *s = l_add(*s, l_shr(l_mult(sa[i], cos_fxp(extract_h(ph))), 1));
                    ph = ph.wrapping_add(ph_step);
                }
                continue;
            }
            // voiced → voiced, low harmonics, steady pitch: interpolated
            let amp_step = l_mpy_ls(l_shr(l_deposit_h(sub(sa[i], self.sa_prev3[i])), 4 + 1), CNST_0_1_Q1_15);
            let mut amp = l_shr(l_deposit_h(self.sa_prev3[i]), 1);
            let mut step_aux = l_mpy_ls(l_shr(fund_freq.wrapping_sub(self.fund_freq_prev), 4 + 1), CNST_0_1_Q1_15);
            step_aux = (step_aux >> 7).wrapping_mul(i as i32 + 1).wrapping_shl(7);
            let mut ph = ph_mem_prev[i] as i32;
            let t1 = l_mpy_ls(l_shr(dph[i] as i32, 4), CNST_0_1_Q1_15);
            for (j, s) in l_snd.iter_mut().enumerate() {
                let mut aux = (step_aux >> 9).wrapping_mul(j as i32).wrapping_shl(9);
                aux = (aux >> 9).wrapping_mul(j as i32).wrapping_shl(9);
                *s = l_add(*s, l_mpy_ls(amp, cos_fxp(extract_h(ph.wrapping_add(aux)))));
                amp = l_add(amp, amp_step);
                ph = ph.wrapping_add(ph_step_prev).wrapping_add(t1);
            }
        }
        for (o, &l) in snd.iter_mut().zip(l_snd.iter()) {
            *o = extract_h(l);
        }
        self.vu_dsn_prev = [0; NUM_HARMS_MAX];
        let n = num_harms as usize;
        self.vu_dsn_prev[..n].copy_from_slice(&vu[..n]);
        self.sa_prev3[..n].copy_from_slice(&sa[..n]);
        self.num_harms_prev3 = num_harms;
        self.fund_freq_prev = fund_freq;
    }

    fn uv_synt(&mut self, p: &Param, snd: &mut [i16; FRAME]) {
        let mut uw = [0i16; 2 * FFTLENGTH];
        let ff = p.fund_freq;
        let ff2 = l_shr(ff, 1);
        let mut acc_a = l_sub(ff, ff2);
        let mut acc_b = l_add(ff, ff2);
        for i in 0..p.num_harms as usize {
            let (ha, hb) = (extract_h(acc_a), extract_h(acc_b));
            let mut ia = (ha >> 8) as i32 + (ha & 0xFF != 0) as i32;
            let ib = (hb >> 8) as i32 + (hb & 0xFF != 0) as i32;
            let mut iaux = 256 - ia;
            let sa = shl(p.sa[i], 3);
            let voiced = p.v_uv_dsn[i] != 0;
            while ia < ib {
                let (re, im) = if voiced { (0, 0) } else { (mult(sa, self.rand_gen()), mult(sa, self.rand_gen())) };
                if (0..FFTLENGTH as i32).contains(&ia) {
                    uw[2 * ia as usize] = re;
                    uw[2 * ia as usize + 1] = im;
                }
                if (0..FFTLENGTH as i32).contains(&iaux) {
                    uw[2 * iaux as usize] = re;
                    uw[2 * iaux as usize + 1] = if voiced { 0 } else { negate(im) };
                }
                ia += 1;
                iaux -= 1;
            }
            acc_a = l_add(acc_a, ff);
            acc_b = l_add(acc_b, ff);
        }
        self.fft(&mut uw, FFTLENGTH, -1);
        let re = |k: usize| uw[2 * k];
        snd[..105].copy_from_slice(&self.uv_mem);
        for (k, s) in snd.iter_mut().enumerate().skip(105) {
            *s = shl(re(73 + k - 105), 3);
        }
        // Weighted overlap-add.
        for (n, i) in (56..105).enumerate() {
            snd[i] = extract_h(l_add(l_mult(snd[i], WS[48 - n]), l_mult(shl(re(24 + n), 3), WS[n])));
        }
        for i in 0..105 {
            self.uv_mem[i] = shl(re(128 + i), 3);
        }
    }

    fn fft_init(&mut self) {
        let len2 = shr(FFTLENGTH as i16, 1);
        let step = shl(2, norm_s(len2));
        let mut theta = 0i16;
        for i in 0..=len2 as usize {
            self.wr[i] = cos_fxp(theta);
            self.wi[i] = sin_fxp(theta);
            theta = if i as i16 >= len2 - 1 { ONE_Q15 } else { add(theta, step) };
        }
    }

    /// In-place radix-2 FFT of `nn` interleaved complex values (Numerical
    /// Recipes, 1-based internally); `isign` −1 for the inverse.
    fn fft(&self, d0: &mut [i16], nn: usize, isign: i16) {
        let n = 2 * nn;
        let idx = |k: usize| k - 1;
        let mut j = 1usize;
        let mut i = 1usize;
        while i < n {
            if j > i {
                d0.swap(idx(j), idx(i));
                d0.swap(idx(j + 1), idx(i + 1));
            }
            let mut m = nn;
            while m >= 2 && j > m {
                j -= m;
                m >>= 1;
            }
            j += m;
            i += 2;
        }
        let mut mmax = 2usize;
        let mut index_step = nn;
        while n > mmax {
            let istep = mmax << 1;
            let mut index = 0usize;
            index_step >>= 1;
            let (mut wr, mut wi) = (ONE_Q15, 0i16);
            let mut m = 1usize;
            while m < mmax {
                let mut i = m;
                while i <= n {
                    let j = i + mmax;
                    let (dj, dj1, di, di1) = (d0[idx(j)], d0[idx(j + 1)], d0[idx(i)], d0[idx(i + 1)]);
                    let tr = l_sub(l_shr(l_mult(wr, dj), 1), l_shr(l_mult(wi, dj1), 1));
                    let ti = l_add(l_shr(l_mult(wr, dj1), 1), l_shr(l_mult(wi, dj), 1));
                    let t1 = l_shr(l_deposit_h(di), 1);
                    d0[idx(j)] = round(l_sub(t1, tr));
                    d0[idx(i)] = round(l_add(t1, tr));
                    let t1 = l_shr(l_deposit_h(di1), 1);
                    d0[idx(j + 1)] = round(l_sub(t1, ti));
                    d0[idx(i + 1)] = round(l_add(t1, ti));
                    i += istep;
                }
                index += index_step;
                wr = self.wr[index];
                wi = if isign < 0 { negate(self.wi[index]) } else { self.wi[index] };
                m += 2;
            }
            mmax = istep;
        }
    }
}

/// mbelib's 88 information bits (u0..u3 12 bits, u4..u6 11, u7 7) → the
/// decoder's frame vector.
pub fn frame_vector(d: &[u8; 88]) -> [i16; 8] {
    const W: [usize; 8] = [12, 12, 12, 12, 11, 11, 11, 7];
    let mut fv = [0i16; 8];
    let mut k = 0;
    for (v, &w) in fv.iter_mut().zip(W.iter()) {
        for _ in 0..w {
            *v = (*v << 1) | (d[k] & 1) as i16;
            k += 1;
        }
    }
    fv
}

/// Unpack u0..u7 into the pitch, voicing and spectral indices (b̂ vector).
/// An invalid pitch index leaves the previous frame's parameters in place.
fn decode_frame_vector(p: &mut Param, fv: &[i16; 8]) {
    let b0 = (shr(fv[0], 4) & 0xFC) | (shr(fv[7], 1) & 0x3);
    p.b_vec[0] = b0;
    if !(0..=207).contains(&b0) {
        return;
    }
    let tmp = ((b0 & 0xFF) << 1) + 0x4F;
    let shift = norm_s(tmp);
    let tmp1 = tmp << shift;
    let tmp2 = div_s(0x4000, tmp1);
    p.fund_freq = l_shr(l_deposit_h(tmp2), 11 - shift);
    let l = l_sub(0x4000_0000, l_mult(tmp1, tmp2));
    let tmp2 = div_s(extract_l(l_shr(l, 2)), tmp1);
    p.fund_freq = l_add(p.fund_freq, l_shr(l_deposit_l(tmp2), 11 - shift - 2));
    let tmp = (tmp + 2) >> 3;
    p.num_harms = ((CNST_0_9254_Q0_16 * tmp as u32) >> 16) as i16;
    p.num_bands = if p.num_harms <= 36 { extract_h(((p.num_harms + 2) as u32 * CNST_0_33_Q0_16) as i32) } else { NUM_BANDS_MAX };

    let mut bs = [0i16; BIT_STREAM_LEN];
    bs[0] = (fv[0] & 0x4 != 0) as i16;
    bs[1] = (fv[0] & 0x2 != 0) as i16;
    bs[2] = (fv[0] & 0x1 != 0) as i16;
    bs[BIT_STREAM_LEN - 3] = (fv[7] & 0x40 != 0) as i16;
    bs[BIT_STREAM_LEN - 2] = (fv[7] & 0x20 != 0) as i16;
    bs[BIT_STREAM_LEN - 1] = (fv[7] & 0x10 != 0) as i16;
    let mut k = 3 + 3 * 12 - 1;
    for v in (1..=3).rev() {
        let mut t = fv[v];
        for _ in 0..12 {
            bs[k] = t & 1;
            t >>= 1;
            k = k.wrapping_sub(1);
        }
    }
    let mut k = 3 + 3 * 12 + 3 * 11 - 1;
    for v in (4..=6).rev() {
        let mut t = fv[v];
        for _ in 0..11 {
            bs[k] = t & 1;
            t >>= 1;
            k -= 1;
        }
    }
    // b1 (voicing), then b2's two bits.
    let mut k = 3 + 3 * 12;
    let mut t = 0i16;
    for _ in 0..p.num_bands {
        t = (t << 1) | bs[k];
        k += 1;
    }
    p.b_vec[1] = t;
    let mut t = bs[k] << 1;
    k += 1;
    t |= bs[k];
    k += 1;
    p.b_vec[2] = (fv[0] & 0x38) | (t << 1) | (shr(fv[7], 3) & 0x01);
    let sh = (p.num_bands + 2) as usize;
    while k < BIT_STREAM_LEN {
        bs[k - sh] = bs[k];
        k += 1;
    }
    // Priority rescanning.
    for i in 0..B_NUM {
        p.bit_alloc[i] = 0;
        p.b_vec[3 + i] = 0;
    }
    bit_allocation(p.num_harms, &mut p.bit_alloc);
    let mut k = 0usize;
    let mut thr = if p.num_harms == 0xb { 9 } else { p.bit_alloc[0] };
    while k < BIT_STREAM_LEN - p.num_bands as usize - 2 {
        for i in 0..(p.num_harms - 1) as usize {
            if thr != 0 && thr <= p.bit_alloc[i] {
                p.b_vec[3 + i] = (p.b_vec[3 + i] << 1) | bs.get(k).copied().unwrap_or(0);
                k += 1;
            }
        }
        thr -= 1;
        if thr < 0 {
            break;
        }
    }
    p.b_vec[p.num_harms as usize + 2] = fv[7] & 1;
}

fn v_uv_decode(p: &mut Param) {
    let mut bands = p.num_bands;
    let vu = p.b_vec[1];
    let mut mask: i32 = 1 << (bands - 1).max(0);
    p.v_uv_dsn = [0; NUM_HARMS_MAX];
    let (mut i, mut uv) = (0, 0i16);
    for h in 0..p.num_harms as usize {
        if vu as i32 & mask != 0 {
            p.v_uv_dsn[h] = 1;
        } else {
            uv += 1;
        }
        i += 1;
        if i == 3 {
            if bands > 1 {
                bands -= 1;
                mask >>= 1;
            }
            i = 0;
        }
    }
    p.l_uv = uv;
}

fn idct(inp: &[i16], m_lim: i16, i_lim: i16, out: &mut [i16]) {
    let (intl, intl2): (u16, u16) = if m_lim == 1 {
        (CNST_0_5_Q1_15, CNST_1_0_Q1_15)
    } else {
        let a = div_s(CNST_0_5_Q5_11, m_lim << 11) as u16;
        (a, shl(a as i16, 1) as u16)
    };
    let mut step = intl;
    for o in out.iter_mut().take(i_lim.max(0) as usize) {
        let mut sum = 0i32;
        let mut a = step;
        for &v in inp.iter().take(m_lim as usize).skip(1) {
            sum = l_add(sum, l_shr(l_mult(v, cos_fxp(a as i16)), 7));
            a = a.wrapping_add(step);
        }
        sum = l_add(sum, l_shr(l_deposit_h(inp[0]), 8));
        *o = extract_l(l_shr_r(sum, 8));
        step = step.wrapping_add(intl2);
    }
}

/// TIA spectral amplitude enhancement, with the C's scaling quirks kept.
fn sa_enh(p: &mut Param) {
    let n = p.num_harms as usize;
    let sa = &mut p.sa;
    let mut sa_tmp = [0i16; NUM_HARMS_MAX];
    sa_tmp[..n].copy_from_slice(&sa[..n]);
    let mut rm0 = l_v_magsq(&sa[..n]);
    if rm0 == 0 {
        return;
    }
    let mut nm = norm_l(rm0);
    if rm0 == MAX_32 {
        nm = 1;
        for i in 0..n {
            sa_tmp[i] = shr(sa[i], nm);
        }
        rm0 = l_v_magsq(&sa_tmp[..n]);
    } else if nm > 2 {
        nm = -(nm >> 1);
        for i in 0..n {
            sa_tmp[i] = shr(sa[i], nm);
        }
        rm0 = l_v_magsq(&sa_tmp[..n]);
    }
    let w0 = p.fund_freq;
    let (mut cos_acc, mut rm1) = (0i32, 0i32);
    let mut cos_w = [0i16; NUM_HARMS_MAX];
    for i in 0..n {
        cos_acc = l_add(cos_acc, w0);
        cos_w[i] = cos_fxp(extract_h(cos_acc));
        rm1 = l_add(rm1, l_mpy_ls(l_mult(sa_tmp[i], sa_tmp[i]), cos_w[i]));
    }
    let (rm0_s, rm1_s) = (extract_h(rm0), extract_h(rm1));
    let l_rm0_2 = l_mult(rm0_s, rm0_s);
    let l_rm1_2 = l_mult(rm1_s, rm1_s);
    let mut den = l_sub(l_rm0_2, l_rm1_2);
    den = l_mult(extract_h(den), rm0_s);
    let mut nm1 = norm_l(den);
    den = l_shl(den, nm1);
    let nm2 = norm_l(w0);
    den = l_mpy_ls(den, extract_h(l_shl(w0, nm2)));
    nm1 += nm2;
    if den < 1 {
        return;
    }
    let sum02_12 = l_add(l_shr(l_rm0_2, 2), l_shr(l_rm1_2, 2));
    let rm0rm1 = shr(mult_r(rm0_s, rm1_s), 1);
    for i in 0..n {
        if ((i + 1) << 3) > n && sa_tmp[i] != 0 {
            let mut num = l_sub(sum02_12, l_mult(rm0rm1, cos_w[i]));
            let mut tot = norm_l(num);
            num = l_shl(num, tot);
            while num >= den {
                num = l_shr(num, 1);
                tot -= 1;
            }
            let mut tmp = div_s(extract_h(num), extract_h(den));
            tot -= nm1;
            let mut l = l_mult(sa_tmp[i], sa_tmp[i]);
            let nm2 = norm_l(l);
            l = l_shl(l, nm2);
            l = l_mult(extract_h(l), tmp);
            tot += nm2;
            tot -= 2;
            if tot <= 0 {
                l = l_shr(l, add(8, tot));
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e);
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e);
                l = l_mult(extract_h(l), CNST_0_9898_Q1_15);
                tmp = extract_h(l_shl(l, 1));
            } else if tot <= 8 {
                l = l_shr(l, tot);
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e);
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e + 1);
                tmp = mult(extract_h(l), CNST_0_9898_Q1_15);
            } else {
                // (nm1 is reused here as in the C, and carries into later harmonics.)
                nm1 = tot & !1;
                l = l_shr(l, tot - nm1);
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e);
                tot = nm1 >> 1;
                nm1 = tot & !1;
                l = l_shr(l, tot - nm1);
                let (r, e) = sqrt_l_exp(l);
                l = l_shr(r, e);
                l = l_mult(extract_h(l), CNST_0_9898_Q1_15);
                tot = nm1 >> 1;
                tmp = extract_h(l_shr(l, tot + 1));
            }
            sa[i] = if tmp > CNST_1_2_Q2_14 {
                extract_h(l_shl(l_mult(sa[i], CNST_1_2_Q2_14), 1))
            } else if tmp < CNST_0_5_Q2_14 {
                shr(sa[i], 1)
            } else {
                extract_h(l_shl(l_mult(sa[i], tmp), 1))
            };
        }
    }
    // Rescale to the original energy.
    for i in 0..n {
        sa_tmp[i] = shr(sa[i], nm);
    }
    let sum_mod = l_v_magsq(&sa_tmp[..n]);
    if sum_mod > rm0 {
        let tmp = div_s(extract_h(rm0), extract_h(sum_mod));
        let (r, e) = sqrt_l_exp(l_deposit_h(tmp));
        let tmp = shr(extract_h(r), e);
        for v in sa.iter_mut().take(n) {
            *v = mult_r(*v, tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_operators_saturate_like_etsi() {
        assert_eq!(add(32767, 1), 32767);
        assert_eq!(sub(-32768, 1), -32768);
        assert_eq!(l_mult(-32768, -32768), MAX_32);
        assert_eq!(shl(0x4000, 1), 32767);
        assert_eq!(shr(-1, 3), -1);
        assert_eq!(norm_s(1), 14);
        assert_eq!(norm_l(1), 30);
        assert_eq!(div_s(1, 2), 16384);
        assert_eq!(mult_r(16384, 16384), 8192);
    }

    #[test]
    fn cos_and_pow2_tables() {
        assert_eq!(cos_fxp(0), 32767);
        assert_eq!(cos_fxp(16384), 0);
        assert_eq!(cos_fxp(32767), -32767);
        // 2^0 = 1.0 in Q14.2 → 4
        assert_eq!(pow2(0), 4);
    }

    #[test]
    fn silence_frame_decodes_quietly() {
        // P25's IMBE silence frame.
        let hex = "040cfd7bfb7df27b3d9e44";
        let mut d = [0u8; 88];
        for (i, b) in d.iter_mut().enumerate() {
            let byte = u8::from_str_radix(&hex[i / 8 * 2..i / 8 * 2 + 2], 16).unwrap();
            *b = (byte >> (7 - i % 8)) & 1;
        }
        let fv = frame_vector(&d);
        let mut dec = FixedImbe::new();
        let mut snd = [0i16; FRAME];
        for _ in 0..20 {
            assert_eq!(dec.decode_checked(&fv, 0, 0, false, 3, 10.0, &mut snd), Out::Voice);
        }
        assert!(snd.iter().all(|s| s.abs() < 400), "{:?}", &snd[..8]);
    }
}
