//! Half-band FIR kernels transcribed from `filters.h` of
//! `airspyone_host` at the all-MIT reference revision `bd15be38`
//! (MIT License, Copyright (C) 2014, Youssef Touil — see NOTICE).

/// `HB_KERNEL_INT16_LEN` — `#define HB_KERNEL_INT16_LEN 47`
/// (`filters.h`, `airspyone_host` @ `bd15be38`).
pub(crate) const HB_KERNEL_INT16_LEN: usize = 47;

/// `HB_KERNEL_INT16` (filters.h) — Q15 half-band low-pass taps; every
/// odd tap is zero apart from the center (16384 = 0.5 in Q15).
#[rustfmt::skip]
pub(crate) const HB_KERNEL_INT16: [i16; HB_KERNEL_INT16_LEN] = [
    -33, 0, 56, 0, -100, 0, 166, 0, -259, 0, 389, 0, -571, 0, 829, 0,
    -1220, 0, 1885, 0, -3353, 0, 10389, 16384, 10389, 0, -3353, 0,
    1885, 0, -1220, 0, 829, 0, -571, 0, 389, 0, -259, 0, 166, 0,
    -100, 0, 56, 0, -33,
];

/// `HB_KERNEL_FLOAT_LEN` — `#define HB_KERNEL_FLOAT_LEN 47`
/// (`filters.h`, `airspyone_host` @ `bd15be38`).
pub(crate) const HB_KERNEL_FLOAT_LEN: usize = 47;

/// `HB_KERNEL_FLOAT` (`filters.h`) — the same half-band low-pass in
/// float. The C initializers are unsuffixed double literals narrowed
/// to float; the `f64 → f32` casts and full-precision digits
/// reproduce that exact conversion path (hence the precision allow).
#[allow(clippy::cast_possible_truncation, clippy::excessive_precision)]
#[rustfmt::skip]
pub(crate) const HB_KERNEL_FLOAT: [f32; HB_KERNEL_FLOAT_LEN] = [
    -0.000_998_606_272_947_510_f64 as f32, 0.0,
    0.001_695_637_278_417_295_f64 as f32, 0.0,
    -0.003_054_430_179_754_289_f64 as f32, 0.0,
    0.005_055_504_379_767_936_f64 as f32, 0.0,
    -0.007_901_319_195_893_647_f64 as f32, 0.0,
    0.011_873_357_051_047_719_f64 as f32, 0.0,
    -0.017_411_159_379_930_066_f64 as f32, 0.0,
    0.025_304_817_427_568_772_f64 as f32, 0.0,
    -0.037_225_225_204_559_217_f64 as f32, 0.0,
    0.057_533_286_997_004_301_f64 as f32, 0.0,
    -0.102_327_462_004_259_350_f64 as f32, 0.0,
    0.317_034_472_508_947_400_f64 as f32,
    0.5,
    0.317_034_472_508_947_400_f64 as f32, 0.0,
    -0.102_327_462_004_259_350_f64 as f32, 0.0,
    0.057_533_286_997_004_301_f64 as f32, 0.0,
    -0.037_225_225_204_559_217_f64 as f32, 0.0,
    0.025_304_817_427_568_772_f64 as f32, 0.0,
    -0.017_411_159_379_930_066_f64 as f32, 0.0,
    0.011_873_357_051_047_719_f64 as f32, 0.0,
    -0.007_901_319_195_893_647_f64 as f32, 0.0,
    0.005_055_504_379_767_936_f64 as f32, 0.0,
    -0.003_054_430_179_754_289_f64 as f32, 0.0,
    0.001_695_637_278_417_295_f64 as f32, 0.0,
    -0.000_998_606_272_947_510_f64 as f32,
];
