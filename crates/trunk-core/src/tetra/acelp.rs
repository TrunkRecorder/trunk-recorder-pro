//! The TETRA full-rate speech decoder (ACELP, EN 300 395-2 clause 4.2.3): 137 bits each 30 ms to
//! 240 samples at 8 kHz. Integer arithmetic throughout, the same on every platform.
//!
//! A port of mbelib-neo's decoder (src/tetra, GPL-2.0-or-later, © 2026 arancormonk), which was
//! written from the standard's text and passes its decoder test bit for bit.
//!
//! Its codebooks exist only as data files in the archive ETSI publishes with the standard, and
//! whether those may be redistributed isn't established. They aren't here: whoever has the
//! archive supplies the two files ([`Tables::parse`]).

use std::sync::Arc;

use super::speech::{Frame, FRAME_BITS};

/// Samples in a frame, at 8 kHz.
pub const FRAME_SAMPLES: usize = 240;
const SUB: usize = 60;
/// LP order.
const M: usize = 10;
const PIT_MIN: i32 = 20;
const PIT_MAX: i32 = 143;
/// Past excitation kept: the longest lag (with its fraction) and half the interpolation filter.
const HIST: usize = PIT_MAX as usize + 1 + 16 + 1;

type Lsp = [i16; M];
type Lpc = [i16; M + 1];

/// The codebooks: LSPs, split 3 / 3 / 4, and the gains.
#[derive(Clone, PartialEq)]
pub struct Tables {
    lsp1: Vec<[i16; 3]>,
    lsp2: Vec<[i16; 3]>,
    lsp3: Vec<[i16; 4]>,
    gain: Vec<[i16; 2]>,
}

impl std::fmt::Debug for Tables {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("Tables")
    }
}

impl Tables {
    /// The archive's files they are in.
    pub const FILES: [&'static str; 2] = ["CLSP_334.TAB", "ENER_QUA.TAB"];

    /// From those files' text, or what's wrong with them.
    pub fn parse(lsp: &str, gain: &str) -> Result<Tables, String> {
        Ok(Tables {
            lsp1: rows(array(lsp, "dico1_clsp", 256 * 3)?),
            lsp2: rows(array(lsp, "dico2_clsp", 512 * 3)?),
            lsp3: rows(array(lsp, "dico3_clsp", 512 * 4)?),
            gain: rows(array(gain, "t_qua_ener", 64 * 2)?),
        })
    }
}

fn rows<const N: usize>(v: Vec<i16>) -> Vec<[i16; N]> {
    v.as_chunks::<N>().0.to_vec()
}

/// The `count` values of the C array `name` in `text`: integers, or products of two.
fn array(text: &str, name: &str, count: usize) -> Result<Vec<i16>, String> {
    let mut code = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("/*") {
        code.push_str(before);
        rest = after.split_once("*/").map_or("", |x| x.1);
    }
    code.push_str(rest);
    let body = code.split_once(name).and_then(|x| x.1.split_once('{')).and_then(|x| x.1.split_once('}')).ok_or(format!("no table {name}"))?.0;
    let value = |e: &str| -> Option<i16> {
        let product = e.split('*').map(|f| f.trim().parse::<i32>().ok()).try_fold(1i32, |p, f| p.checked_mul(f?))?;
        product.try_into().ok()
    };
    let v = body.split(',').map(str::trim).filter(|e| !e.is_empty()).map(|e| value(e).ok_or(format!("{name}: `{e}` isn't a 16-bit number"))).collect::<Result<Vec<_>, _>>()?;
    if v.len() != count {
        return Err(format!("{name}: {} values, not {count}", v.len()));
    }
    Ok(v)
}

// The standard's fixed-point operators (saturating).
fn sat16(x: i64) -> i16 {
    x.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}
fn sat32(x: i64) -> i32 {
    x.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}
fn l_mult(a: i16, b: i16) -> i32 {
    sat32(a as i64 * b as i64 * 2)
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
fn l_shl(a: i32, n: u32) -> i32 {
    sat32((a as i64) << n.min(32))
}
/// Right by `n`, rounding; left when `n` is negative.
fn l_shr_r(a: i32, n: i32) -> i32 {
    match n {
        32.. => 0,
        1..=31 => ((a as i64 + (1i64 << (n - 1))) >> n) as i32,
        _ => l_shl(a, n.unsigned_abs()),
    }
}
fn high(a: i32) -> i16 {
    (a >> 16) as i16
}
fn round(a: i32) -> i16 {
    high(l_add(a, 0x8000))
}
fn norm_l(a: i32) -> u32 {
    match a {
        0 => 0,
        a => (a ^ (a >> 31)).leading_zeros() - 1,
    }
}

/// round(32767 · log2(1 + i/32)).
const LOG2: [i16; 33] = [
    0, 1455, 2866, 4236, 5568, 6863, 8124, 9352, 10549, 11716, 12855, 13967, 15054, 16117, 17156, 18172, 19167, 20142, 21097, 22033, 22951, 23852, 24735, 25603, 26455, 27291, 28113, 28922, 29716, 30497,
    31266, 32023, 32767,
];
/// round(16384 · 2^(i/32)), the last limited to 32767.
const POW2: [i16; 33] = [
    16384, 16743, 17109, 17484, 17867, 18258, 18658, 19066, 19484, 19911, 20347, 20792, 21247, 21713, 22188, 22674, 23170, 23678, 24196, 24726, 25268, 25821, 26386, 26964, 27554, 28158, 28774, 29405,
    30048, 30706, 31379, 32066, 32767,
];
/// A third of a sample's delay (Hamming-windowed sinc), Q15; backwards, a third's advance.
const THIRD: [i16; 32] = [
    -47, 59, -84, 125, -183, 263, -366, 500, -672, 893, -1185, 1587, -2182, 3179, -5287, 13496, 27072, -6669, 3688, -2452, 1758, -1304, 981, -739, 553, -407, 294, -207, 142, -96, 66, -49,
];
/// 0.75^i and 0.85^i, Q15.
const GAMMA1: [i16; M] = [24576, 18432, 13824, 10368, 7776, 5832, 4374, 3281, 2461, 1846];
const GAMMA2: [i16; M] = [27853, 23675, 20124, 17106, 14540, 12359, 10505, 8929, 7590, 6452];
/// What a reset decoder takes the last frame's LSPs to have been (clause 4.2.2.3).
const LSP_START: Lsp = [30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000];

/// Q8 log2 offset of both gains.
const GAIN_OFFSET: i32 = 4434;
/// The most a pitch gain may be: 1.2, Q12.
const GAIN_P_MAX: i32 = 4915;
const ENERGY_MAX_P: i32 = 6912;
const ENERGY_MAX_C: i32 = 6400;

/// A subframe's indices (table 3).
#[derive(Clone, Copy, Debug, Default)]
struct Sub {
    pitch: i32,
    pulses: [usize; 4],
    sign: bool,
    gain: usize,
}

struct Params {
    lsp: [usize; 3],
    sub: [Sub; 4],
}

fn unpack(bits: &Frame) -> Params {
    let mut pos = 0;
    let mut read = |n: usize| {
        pos += n;
        bits[pos - n..pos].iter().fold(0usize, |w, &b| w << 1 | (b & 1) as usize)
    };
    let lsp = [read(8), read(9), read(9)];
    let sub = std::array::from_fn(|k| {
        let pitch = read(if k == 0 { 8 } else { 5 }) as i32;
        let (p4, p3, p2, p1) = (read(3), read(3), read(3), read(5));
        let (sign, shift) = (read(1) != 0, read(1));
        Sub { pitch, pulses: [2 * p1 + shift, 2 + 8 * p2 + shift, 4 + 8 * p3 + shift, 6 + 8 * p4 + shift], sign, gain: read(6) }
    });
    debug_assert_eq!(pos, FRAME_BITS);
    Params { lsp, sub }
}

/// The LSPs either side of a split come out no nearer than `gap`.
fn widen(lsp: &mut Lsp, i: usize, gap: i32) {
    let d = lsp[i] as i32 - lsp[i + 1] as i32;
    if d < gap {
        let t = ((gap - d) >> 1) as i64;
        (lsp[i], lsp[i + 1]) = (sat16(lsp[i] as i64 + t), sat16(lsp[i + 1] as i64 - t));
    }
}

/// The frame's LSPs; None when they come out crossed (no stable filter).
fn lsps(t: &Tables, at: [usize; 3]) -> Option<Lsp> {
    let mut lsp: Lsp = [0; M];
    lsp[..3].copy_from_slice(&t.lsp1[at[0]]);
    lsp[3..6].copy_from_slice(&t.lsp2[at[1]]);
    lsp[6..].copy_from_slice(&t.lsp3[at[2]]);
    widen(&mut lsp, 2, 916);
    widen(&mut lsp, 5, 1244);
    lsp.windows(2).all(|w| w[0] > w[1]).then_some(lsp)
}

/// (a, split in halves) · q: the low half's top bit's product kept exactly.
fn lsp_mpy(a: i32, q: i16) -> i32 {
    let hi = high(a);
    let lo = l_msu(a >> 1, hi, 16384) as i16;
    let (top, low, q) = ((lo as i64 >> 14) & 1, lo as i64 & 0x3fff, q as i64);
    sat32(hi as i64 * q * 2 + top * q + 2 * ((low * q) >> 15))
}

/// The sum or difference polynomial of every other LSP from `from` (Q24).
fn polynomial(lsp: &Lsp, from: usize) -> [i32; 6] {
    let mut f = [0i32; 6];
    f[0] = l_mult(4096, 2048);
    f[1] = l_msu(0, lsp[from], 512);
    for i in 2..=5 {
        let q = lsp[from + 2 * (i - 1)];
        f[i] = f[i - 2];
        for j in (2..=i).rev() {
            let t = l_shl(lsp_mpy(f[j - 1], q), 1);
            f[j] = l_sub(l_add(f[j], f[j - 2]), t);
        }
        f[1] = l_msu(f[1], q, 512);
    }
    f
}

fn lpc(lsp: &Lsp) -> Lpc {
    let (mut f1, mut f2) = (polynomial(lsp, 0), polynomial(lsp, 1));
    for i in (1..=5).rev() {
        f1[i] = l_add(f1[i], f1[i - 1]);
        f2[i] = l_sub(f2[i], f2[i - 1]);
    }
    let mut a = [0i16; M + 1];
    a[0] = 4096;
    for i in 1..=5 {
        a[i] = l_shr_r(l_add(f1[i], f2[i]), 13) as i16;
        a[M + 1 - i] = l_shr_r(l_sub(f1[i], f2[i]), 13) as i16;
    }
    a
}

/// 1/A(z) over a subframe, from `mem` (oldest first), which it leaves as the output's end.
fn synthesize(a: &Lpc, x: &[i16; SUB], mem: &mut [i16; M]) -> [i16; SUB] {
    let mut buf = [0i16; M + SUB];
    buf[..M].copy_from_slice(mem);
    for n in 0..SUB {
        let acc = (1..=M).fold(l_shl(x[n] as i32, 12), |acc, i| sat32(acc as i64 - a[i] as i64 * buf[M + n - i] as i64));
        buf[M + n] = round(l_shl(acc, 4));
    }
    mem.copy_from_slice(&buf[SUB..]);
    buf[M..].try_into().unwrap()
}

/// The innovation: four pulses through F(z) = A(z/0.75) / A(z/0.85), sharpened at short lags.
fn codevector(a: &Lpc, t0: usize, sp: &Sub) -> [i16; SUB] {
    let weigh = |gamma: &[i16; M]| -> Lpc { std::array::from_fn(|i| if i == 0 { a[0] } else { round(l_mult(a[i], gamma[i - 1])) }) };
    let mut x = [0i16; SUB];
    x[..=M].copy_from_slice(&weigh(&GAMMA1));
    let mut f = synthesize(&weigh(&GAMMA2), &x, &mut [0; M]);
    for n in t0..SUB {
        f[n] = sat16(f[n] as i64 + ((f[n - t0] as i64 * 26216) >> 15));
    }
    // (A pulse past the subframe isn't there. The sum keeps its low 16 bits, as the reference's does.)
    let at = |n: usize, k: usize| n.checked_sub(sp.pulses[k]).map_or(0, |i| f[i] as i64);
    std::array::from_fn(|n| {
        let acc = 23168 * at(n, 0) + ((at(n, 2) - at(n, 1) - at(n, 3)) << 14);
        ((if sp.sign { -acc } else { acc }) >> 14) as i16
    })
}

/// A subframe's lag and its fraction, thirds.
fn lag(k: usize, index: i32, t1: i32) -> (i32, i32) {
    if k == 0 {
        if index < 197 {
            let t0 = (index + 2) / 3 + 19;
            (t0, index - 3 * t0 + 58)
        } else {
            (index - 112, 0)
        }
    } else {
        let tmin = (t1 - 5).clamp(PIT_MIN, PIT_MAX - 9);
        let i = (index + 2) / 3 - 1;
        (tmin + i, index - 2 - 3 * i)
    }
}

fn energy(x: &[i16], init: i64) -> i32 {
    sat32(x.iter().fold(init, |s, &v| s + v as i64 * v as i64))
}

/// log2, Q15.
fn log2(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let e = norm_l(x);
    let x = l_shl(x, e);
    let i = (x >> 25 & 0x1f) as usize;
    let a = (x >> 10 & 0x7fff) as i16;
    let y = l_msu((LOG2[i] as i32) << 16, LOG2[i] - LOG2[i + 1], a);
    (30 - e as i32) * 32768 + high(y) as i32
}

/// A gain, Q12, from a quantized energy (Q8) and an energy product's log2 (Q15).
fn gain(energy: i16, log: i32) -> i32 {
    let z = energy as i32 + GAIN_OFFSET - (log >> 7);
    let (e, frac) = (z >> 9, (z & 0x1ff) << 6);
    let i = (frac >> 10) as usize;
    let x = l_msu((POW2[i] as i32) << 16, POW2[i] - POW2[i + 1], ((frac & 0x3ff) << 5) as i16);
    l_shr_r(x, 30 - e)
}

/// One voice's decoder.
#[derive(Clone)]
pub struct Decoder {
    tables: Arc<Tables>,
    lsp: Lsp,
    /// Past excitation, newest last, and the subframe being made.
    exc: [i16; HIST + SUB],
    mem: [i16; M],
    energy_p: i16,
    energy_c: i16,
    last_lag: i32,
    /// A good frame came since the start.
    good: bool,
    last: [Sub; 4],
}

impl Decoder {
    pub fn new(tables: Arc<Tables>) -> Self {
        Decoder { tables, lsp: LSP_START, exc: [0; HIST + SUB], mem: [0; M], energy_p: 0, energy_c: 0, last_lag: PIT_MIN, good: false, last: Default::default() }
    }

    /// The next frame's audio; None: a lost frame, which the last good ones stand in for, fading.
    pub fn frame(&mut self, bits: Option<&Frame>) -> [i16; FRAME_SAMPLES] {
        let mut pcm = [0i16; FRAME_SAMPLES];
        if bits.is_none() && !self.good {
            // Nothing to go on yet: silence (and the reference's one trace of it).
            self.lsp[0] = 0;
            return pcm;
        }
        let params = bits.map(unpack);
        let new = params.as_ref().and_then(|p| lsps(&self.tables, p.lsp)).unwrap_or(self.lsp);
        let mut t1 = 0;
        for k in 0..4 {
            let sp = params.as_ref().map_or(self.last[k], |p| p.sub[k]);
            let (t0, frac) = match &params {
                Some(_) => lag(k, sp.pitch, t1),
                None => (self.last_lag, 0),
            };
            if k == 0 {
                t1 = t0;
            }
            let old_weight = [24576i16, 16384, 8192, 0][k];
            let lsp: Lsp = if k == 3 { new } else { std::array::from_fn(|i| high(l_mac(l_mult(self.lsp[i], old_weight), new[i], (32768 - old_weight as i32) as i16))) };
            let s = self.subframe(&lpc(&lsp), &sp, t0, frac, params.is_none());
            for (o, v) in pcm[k * SUB..].iter_mut().zip(s) {
                *o = v.saturating_add(v);
            }
            if params.is_some() && k == 3 {
                self.last_lag = t0;
            }
        }
        self.lsp = new;
        if let Some(p) = params {
            self.last = p.sub;
            self.good = true;
        }
        pcm
    }

    fn subframe(&mut self, a: &Lpc, sp: &Sub, t0: i32, frac: i32, lost: bool) -> [i16; SUB] {
        // The past excitation, a lag back (its own start, at lags shorter than the subframe).
        for n in 0..SUB {
            let x = (HIST + n) as i32 - t0;
            self.exc[HIST + n] = if frac == 0 {
                self.exc[x as usize]
            } else {
                let first = if frac > 0 { -16 } else { -15 };
                let acc = (0..32).fold(0i32, |acc, k| {
                    let tap = if frac > 0 { THIRD[k] } else { THIRD[31 - k] };
                    sat32(acc as i64 + self.exc[(x + first + k as i32) as usize] as i64 * tap as i64)
                });
                round(l_add(acc, acc))
            };
        }
        let code = codevector(a, t0 as usize, sp);
        // Quantized energies: predicted and corrected, or fading (equations 35, 42).
        let (p, c) = (self.energy_p as i32, self.energy_c as i32);
        let predicted = |own: i32, other: i32| (((2 * own + other) >> 2) - 768).max(0);
        let (ep, ec) = if lost {
            ((p - 128).max(0), (c - 128).max(0))
        } else {
            let g = self.tables.gain[sp.gain];
            (predicted(p, c) + g[0] as i32, predicted(c, p) + g[1] as i32)
        };
        self.energy_p = sat16(ep.min(ENERGY_MAX_P) as i64);
        self.energy_c = sat16(ec.min(ENERGY_MAX_C) as i64);
        // The gains they and the two vectors' own energies come to, through the filter's (31 to 39).
        let mut impulse = [0i16; SUB];
        impulse[0] = 1024;
        let pg = energy(&synthesize(a, &impulse, &mut [0; M]), 0);
        let mantissa = |x: i32| (high(l_shl(x, norm_l(x))) as i32, norm_l(x) as i32);
        let (mpg, npg) = mantissa(pg);
        let (mv, nv) = mantissa(energy(&self.exc[HIST..], 1));
        let mc = high(energy(&code, 0)) as i32;
        let gp = gain(self.energy_p, log2(mv * mpg) - ((nv + npg) << 15)).min(GAIN_P_MAX) as i64;
        let gc = gain(self.energy_c, log2(mc * mpg) - (npg << 15)) as i16 as i64;
        for (v, c) in self.exc[HIST..].iter_mut().zip(code) {
            *v = l_shr_r(sat32(*v as i64 * gp + c as i64 * gc), 12) as i16;
        }
        let x: [i16; SUB] = self.exc[HIST..].try_into().unwrap();
        self.exc.copy_within(SUB.., 0);
        synthesize(a, &x, &mut self.mem)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Made-up codebooks, the shapes of the real ones: smooth, ordered LSPs and middling gains.
    pub(crate) fn made_up() -> Arc<Tables> {
        let lsp = |base: &[i16], n: usize| -> String { (0..n).flat_map(|i| base.iter().map(move |b| b + (i % 64) as i16 * 8)).map(|v| v.to_string()).collect::<Vec<_>>().join(",\n") };
        let a = format!(
            "/* a comment, 1, 2 */ static Word16 dico1_clsp[] = {{ {} }};\nstatic Word16 dico2_clsp[] = {{ {} }};\nstatic Word16 dico3_clsp[] = {{ {} }};",
            lsp(&[30000, 26000, 21000], 256), lsp(&[15000, 8000, 1000], 512), lsp(&[-8000, -15000, -21000, -26000], 512)
        );
        let g = (0..64).map(|i| format!("{} * 2, {}", i * 12 - 300, i * 20 - 500)).collect::<Vec<_>>().join(", ");
        Arc::new(Tables::parse(&a, &format!("static Word16 t_qua_ener[128] = {{ {g} }};")).unwrap())
    }

    #[test]
    fn tables_are_read_and_checked() {
        let t = made_up();
        assert_eq!((t.lsp1[0], t.lsp3[0], t.gain[0], t.gain[63]), ([30000, 26000, 21000], [-8000, -15000, -21000, -26000], [-600, -500], [912, 760]));
        assert!(Tables::parse("", "").unwrap_err().contains("dico1_clsp"));
        assert!(array("t[] = { 1, 2 };", "t", 3).unwrap_err().contains("2 values, not 3"));
        assert!(array("t[] = { 1, 40000 };", "t", 2).unwrap_err().contains("16-bit"));
        assert!(array("t[] = { 1, x };", "t", 2).is_err());
    }

    /// Any bits, lost frames among them, decode without a panic; lost ones fade.
    #[test]
    fn any_frame_decodes() {
        let mut d = Decoder::new(made_up());
        assert_eq!(d.frame(None), [0; FRAME_SAMPLES]);
        let mut x = 3u64;
        for k in 0..3000 {
            let f: Frame = std::array::from_fn(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 20 & 1) as u8 });
            d.frame((k % 7 != 0).then_some(&f));
        }
        let level = |p: [i16; FRAME_SAMPLES]| p.iter().map(|v| v.unsigned_abs() as u32).max().unwrap();
        let first = level(d.frame(None));
        let last = (0..60).map(|_| level(d.frame(None))).last().unwrap();
        assert!(last < 400 && last <= first, "{first} {last}");
    }

    #[test]
    fn operators() {
        assert_eq!([norm_l(1), norm_l(0x4000_0000), norm_l(-1), norm_l(0), norm_l(-5)], [30, 0, 31, 0, 28]);
        assert_eq!((l_shr_r(3, 1), l_shr_r(-3, 1), l_shr_r(1, 40), l_shr_r(1, -3), l_shl(1 << 30, 2)), (2, -1, 0, 8, i32::MAX));
        assert_eq!((log2(1), log2(1 << 20), log2(0)), (0, 20 << 15, 0));
    }
}
